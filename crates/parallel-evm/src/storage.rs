//! Storage abstraction for the parallel EVM.
//! Bridges reth's StateProviderDatabase to our parallel executor.

use std::{fmt::Display, sync::Arc};

use alloy_primitives::{Address, B256, Bytes, U256};
use hashbrown::HashMap;
use revm::{
    Database, DatabaseRef,
    bytecode::{Bytecode, JumpTable, LegacyAnalyzedBytecode, eip7702::Eip7702Bytecode},
    state::AccountInfo,
};
use revm_database::DBErrorMarker;
use rustc_hash::FxBuildHasher;

/// An EVM account state as known to the parallel executor.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EvmAccount {
    pub balance: U256,
    pub nonce: u64,
    pub code_hash: Option<B256>,
    pub code: Option<EvmCode>,
    pub storage: HashMap<U256, U256, FxBuildHasher>,
}

impl From<revm::state::Account> for EvmAccount {
    fn from(account: revm::state::Account) -> Self {
        let has_code = !account.info.is_empty_code_hash();
        Self {
            balance: account.info.balance,
            nonce: account.info.nonce,
            code_hash: has_code.then_some(account.info.code_hash),
            code: has_code.then(|| account.info.code.unwrap().into()),
            storage: account
                .storage
                .into_iter()
                .map(|(k, v)| (k, v.present_value))
                .collect(),
        }
    }
}

/// Basic account info (balance + nonce only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountBasic {
    pub balance: U256,
    pub nonce: u64,
}

impl Default for AccountBasic {
    fn default() -> Self {
        Self { balance: U256::ZERO, nonce: 0 }
    }
}

/// Analyzed legacy bytecode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyCode {
    pub bytecode: Bytes,
    pub original_len: usize,
    /// Pre-analysed jump table (Arc-wrapped bytes inside, cheap to clone).
    pub jump_table: JumpTable,
}

/// EVM code representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvmCode {
    Legacy(LegacyCode),
    Eip7702 { delegated_address: Address, version: u8 },
}

/// Error converting bytecode.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BytecodeConversionError {
    #[error("Invalid EOF bytecode")]
    InvalidEof,
}

impl TryFrom<EvmCode> for Bytecode {
    type Error = BytecodeConversionError;
    fn try_from(code: EvmCode) -> Result<Self, Self::Error> {
        match code {
            EvmCode::Legacy(legacy) => Ok(Bytecode::LegacyAnalyzed(Arc::new(
                LegacyAnalyzedBytecode::new(legacy.bytecode, legacy.original_len, legacy.jump_table),
            ))),
            EvmCode::Eip7702 { delegated_address, version: _ } => {
                Ok(Bytecode::Eip7702(Arc::new(Eip7702Bytecode::new(delegated_address))))
            }
        }
    }
}

impl From<Bytecode> for EvmCode {
    fn from(bytecode: Bytecode) -> Self {
        match bytecode {
            Bytecode::LegacyAnalyzed(analyzed) => EvmCode::Legacy(LegacyCode {
                bytecode: analyzed.bytecode().clone(),
                original_len: analyzed.original_len(),
                jump_table: analyzed.jump_table().clone(),
            }),
            Bytecode::Eip7702(b7702) => EvmCode::Eip7702 {
                delegated_address: b7702.delegated_address,
                version: b7702.version,
            },
        }
    }
}

/// Type aliases used throughout the parallel executor.
pub type ChainState = HashMap<Address, EvmAccount>;
pub type Bytecodes = HashMap<B256, EvmCode>;
pub type BlockHashes = HashMap<u64, B256>;

/// The storage trait the parallel executor uses to read pre-block state.
pub trait Storage {
    type Error: Display + std::fmt::Debug + Send + Sync + 'static;
    fn basic(&self, address: &Address) -> Result<Option<AccountBasic>, Self::Error>;
    fn code_hash(&self, address: &Address) -> Result<Option<B256>, Self::Error>;
    fn code_by_hash(&self, hash: &B256) -> Result<Option<EvmCode>, Self::Error>;
    fn has_storage(&self, address: &Address) -> Result<bool, Self::Error>;
    fn storage(&self, address: &Address, slot: &U256) -> Result<U256, Self::Error>;
    fn block_hash(&self, number: &u64) -> Result<B256, Self::Error>;
}

/// Wraps our `Storage` impl to implement `revm::Database` for use inside the EVM.
pub struct StorageWrapper<'a, S>(pub &'a S);

/// Error type for the storage wrapper.
#[derive(Debug, thiserror::Error)]
pub enum StorageWrapperError<S: Display + std::fmt::Debug + Send + Sync + 'static> {
    #[error("Storage error: {0}")]
    StorageError(S),
    #[error("Bytecode conversion error: {0}")]
    BytecodeConversion(#[from] BytecodeConversionError),
}

impl<S: Display + std::fmt::Debug + Send + Sync + 'static> DBErrorMarker
    for StorageWrapperError<S>
{
}

impl<'a, S: Storage> Database for StorageWrapper<'a, S> {
    type Error = StorageWrapperError<S::Error>;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        let basic = self.0.basic(&address).map_err(StorageWrapperError::StorageError)?;
        let Some(basic) = basic else { return Ok(None) };

        let code_hash = self.0.code_hash(&address).map_err(StorageWrapperError::StorageError)?;
        let code = if let Some(hash) = &code_hash {
            self.0
                .code_by_hash(hash)
                .map_err(StorageWrapperError::StorageError)?
                .map(Bytecode::try_from)
                .transpose()?
        } else {
            None
        };

        Ok(Some(AccountInfo {
            balance: basic.balance,
            nonce: basic.nonce,
            code_hash: code_hash.unwrap_or(revm::primitives::KECCAK_EMPTY),
            account_id: None,
            code,
        }))
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        match self.0.code_by_hash(&code_hash).map_err(StorageWrapperError::StorageError)? {
            Some(code) => Ok(Bytecode::try_from(code)?),
            None => Ok(Bytecode::default()),
        }
    }

    fn storage(&mut self, address: Address, index: U256) -> Result<U256, Self::Error> {
        self.0.storage(&address, &index).map_err(StorageWrapperError::StorageError)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.0.block_hash(&number).map_err(StorageWrapperError::StorageError)
    }
}

impl<'a, S: Storage> DatabaseRef for StorageWrapper<'a, S> {
    type Error = StorageWrapperError<S::Error>;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        let basic = self.0.basic(&address).map_err(StorageWrapperError::StorageError)?;
        let Some(basic) = basic else { return Ok(None) };

        let code_hash = self.0.code_hash(&address).map_err(StorageWrapperError::StorageError)?;
        let code = if let Some(hash) = &code_hash {
            self.0
                .code_by_hash(hash)
                .map_err(StorageWrapperError::StorageError)?
                .map(Bytecode::try_from)
                .transpose()?
        } else {
            None
        };

        Ok(Some(AccountInfo {
            balance: basic.balance,
            nonce: basic.nonce,
            code_hash: code_hash.unwrap_or(revm::primitives::KECCAK_EMPTY),
            account_id: None,
            code,
        }))
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        match self.0.code_by_hash(&code_hash).map_err(StorageWrapperError::StorageError)? {
            Some(code) => Ok(Bytecode::try_from(code)?),
            None => Ok(Bytecode::default()),
        }
    }

    fn storage_ref(&self, address: Address, index: U256) -> Result<U256, Self::Error> {
        self.0.storage(&address, &index).map_err(StorageWrapperError::StorageError)
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.0.block_hash(&number).map_err(StorageWrapperError::StorageError)
    }
}
