//! The parallel EVM execution unit.
//!
//! `VmDb` intercepts database reads for MvMemory tracking.
//! `Vm` orchestrates one transaction incarnation.
//! `NoRewardHandler` skips beneficiary rewards (tracked lazily in the write-set).

use alloy_primitives::{Address, B256, TxKind, U256};
use alloy_rpc_types_eth::Receipt;
use hashbrown::HashMap;
use revm::{
    Database,
    bytecode::Bytecode,
    context::{BlockEnv, CfgEnv, Context, TxEnv, result::EVMError},
    database_interface::DBErrorMarker,
    handler::{
        EvmTrError, FrameResult,
        Handler, MainnetHandler,
    },
    primitives::hardfork::SpecId,
    state::{AccountInfo, EvmState},
    MainBuilder,
};
use smallvec::SmallVec;

use crate::{
    AccountBasic, BuildIdentityHasher, BuildSuffixHasher, EvmAccount, FinishExecFlags,
    MemoryEntry, MemoryLocation, MemoryLocationHash, MemoryValue, ReadOrigin, ReadOrigins,
    ReadSet, Storage, TxIdx, TxVersion, WriteSet,
    hash_deterministic,
    mv_memory::MvMemory,
    storage::BytecodeConversionError,
};

pub use revm::context::result::{
    HaltReason, InvalidTransaction, ExecutionResult,
};
/// The EVM execution error for parallel execution.
pub type ExecutionError = EVMError<ReadError, InvalidTransaction>;

/// State transitions produced by executing one transaction.
type EvmStateTransitions = HashMap<Address, Option<EvmAccount>, BuildSuffixHasher>;

/// Execution result for one transaction: receipt + state diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PevmTxExecutionResult {
    pub receipt: Receipt,
    pub state: EvmStateTransitions,
}

impl PevmTxExecutionResult {
    pub fn from_revm(
        result: ExecutionResult,
        state: EvmState,
        eip_161_enabled: bool,
    ) -> Self {
        let gas_used = result.gas_used();
        let is_success = result.is_success();
        let logs = result.into_logs();
        Self {
            receipt: Receipt {
                status: is_success.into(),
                cumulative_gas_used: gas_used,
                logs,
            },
            state: state
                .into_iter()
                .filter(|(_, account)| account.is_touched())
                .map(|(address, account)| {
                    if account.is_selfdestructed() || (account.is_empty() && eip_161_enabled) {
                        (address, None)
                    } else {
                        (address, Some(EvmAccount::from(account)))
                    }
                })
                .collect(),
        }
    }
}

pub(crate) enum VmExecutionError {
    Retry,
    FallbackToSequential,
    Blocking(TxIdx),
    ExecutionError(ExecutionError),
}

/// Error reading a memory location during parallel execution.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    #[error("Failed reading memory from storage: {0}")]
    StorageError(String),
    #[error("Read of memory location is blocked by tx #{0}")]
    Blocking(TxIdx),
    #[error("Inconsistent read")]
    InconsistentRead,
    #[error("Tx #{0} has invalid nonce")]
    InvalidNonce(TxIdx),
    #[error("Tried to read self-destructed account")]
    SelfDestructedAccount,
    #[error("Invalid bytecode")]
    InvalidBytecode(#[source] BytecodeConversionError),
    #[error("Invalid type of stored memory value")]
    InvalidMemoryValueType,
}

impl DBErrorMarker for ReadError {}

impl From<ReadError> for VmExecutionError {
    fn from(err: ReadError) -> Self {
        match err {
            ReadError::InconsistentRead => Self::Retry,
            ReadError::SelfDestructedAccount => Self::FallbackToSequential,
            ReadError::Blocking(tx_idx) => Self::Blocking(tx_idx),
            _ => Self::ExecutionError(EVMError::Database(err)),
        }
    }
}

pub(crate) struct VmExecutionResult {
    pub(crate) execution_result: PevmTxExecutionResult,
    pub(crate) flags: FinishExecFlags,
}

// ---------------------------------------------------------------------------
// VmDb – implements revm::Database, intercepting reads for MvMemory tracking.
// ---------------------------------------------------------------------------

pub(crate) struct VmDb<'a, S: Storage> {
    vm: &'a Vm<'a, S>,
    tx_idx: TxIdx,
    from_hash: MemoryLocationHash,
    to_hash: Option<MemoryLocationHash>,
    pub(crate) to_code_hash: Option<B256>,
    pub(crate) is_lazy: bool,
    pub(crate) read_set: ReadSet,
    pub(crate) read_accounts:
        HashMap<MemoryLocationHash, (AccountBasic, Option<B256>), BuildIdentityHasher>,
    tx_kind: TxKind,
    tx_caller: Address,
    tx_nonce: u64,
    tx_value: U256,
}

impl<'a, S: Storage> VmDb<'a, S> {
    fn new(
        vm: &'a Vm<'a, S>,
        tx_idx: TxIdx,
        tx: &TxEnv,
        from_hash: MemoryLocationHash,
        to_hash: Option<MemoryLocationHash>,
    ) -> Result<Self, ReadError> {
        let mut db = Self {
            vm,
            tx_idx,
            from_hash,
            to_hash,
            to_code_hash: None,
            is_lazy: false,
            read_set: ReadSet::with_capacity_and_hasher(2, BuildIdentityHasher::default()),
            read_accounts: HashMap::with_capacity_and_hasher(2, BuildIdentityHasher::default()),
            tx_kind: tx.kind,
            tx_caller: tx.caller,
            tx_nonce: tx.nonce,
            tx_value: tx.value,
        };
        if let TxKind::Call(to) = tx.kind {
            db.to_code_hash = db.get_code_hash(to)?;
            db.is_lazy = db.to_code_hash.is_none()
                && (vm.mv_memory.data.contains_key(&from_hash)
                    || vm.mv_memory.data.contains_key(&to_hash.unwrap()));
        }
        Ok(db)
    }

    fn hash_basic(&self, address: &Address) -> MemoryLocationHash {
        if *address == self.tx_caller {
            return self.from_hash;
        }
        if let TxKind::Call(to) = self.tx_kind {
            if to == *address {
                return self.to_hash.unwrap();
            }
        }
        hash_deterministic(MemoryLocation::Basic(*address))
    }

    fn push_origin(origins: &mut ReadOrigins, origin: ReadOrigin) -> Result<(), ReadError> {
        if let Some(prev) = origins.last() {
            if prev != &origin {
                return Err(ReadError::InconsistentRead);
            }
        } else {
            origins.push(origin);
        }
        Ok(())
    }

    fn get_code_hash(&mut self, address: Address) -> Result<Option<B256>, ReadError> {
        let loc = hash_deterministic(MemoryLocation::CodeHash(address));
        let origins = self.read_set.entry(loc).or_default();

        if let Some(written) = self.vm.mv_memory.data.get(&loc) {
            if let Some((tx_idx, MemoryEntry::Data(inc, val))) =
                written.range(..self.tx_idx).next_back()
            {
                match val {
                    MemoryValue::SelfDestructed => return Err(ReadError::SelfDestructedAccount),
                    MemoryValue::CodeHash(h) => {
                        Self::push_origin(
                            origins,
                            ReadOrigin::MvMemory(TxVersion {
                                tx_idx: *tx_idx,
                                tx_incarnation: *inc,
                            }),
                        )?;
                        return Ok(Some(*h));
                    }
                    _ => {}
                }
            }
        }

        Self::push_origin(origins, ReadOrigin::Storage)?;
        self.vm.storage.code_hash(&address).map_err(|e| ReadError::StorageError(e.to_string()))
    }
}

impl<S: Storage> Database for VmDb<'_, S> {
    type Error = ReadError;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, ReadError> {
        let location_hash = self.hash_basic(&address);

        if self.is_lazy {
            if location_hash == self.from_hash {
                return Ok(Some(AccountInfo {
                    nonce: self.tx_nonce + 1,
                    balance: U256::MAX,
                    code: None,
                    code_hash: revm::primitives::KECCAK_EMPTY,
                    account_id: None,
                }));
            } else if Some(location_hash) == self.to_hash {
                return Ok(None);
            }
        }

        let origins = self.read_set.entry(location_hash).or_default();
        let has_prev = !origins.is_empty();
        let mut new_origins: ReadOrigins = SmallVec::new();

        let mut base_account: Option<AccountBasic> = None;
        let mut balance_add = U256::ZERO;
        let mut positive = true;
        let mut nonce_add: u64 = 0;

        if self.tx_idx > 0 {
            if let Some(written) = self.vm.mv_memory.data.get(&location_hash) {
                let mut iter = written.range(..self.tx_idx);
                loop {
                    match iter.next_back() {
                        Some((idx, MemoryEntry::Estimate)) => {
                            return Err(ReadError::Blocking(*idx));
                        }
                        Some((idx, MemoryEntry::Data(inc, val))) => {
                            if has_prev && origins.len() == new_origins.len() {
                                return Err(ReadError::InconsistentRead);
                            }
                            let origin = ReadOrigin::MvMemory(TxVersion {
                                tx_idx: *idx,
                                tx_incarnation: *inc,
                            });
                            if has_prev
                                && unsafe { origins.get_unchecked(new_origins.len()) } != &origin
                            {
                                return Err(ReadError::InconsistentRead);
                            }
                            new_origins.push(origin);
                            match val {
                                MemoryValue::Basic(b) => {
                                    base_account = Some(b.clone());
                                    break;
                                }
                                MemoryValue::LazyRecipient(add) => {
                                    if positive {
                                        balance_add = balance_add.saturating_add(*add);
                                    } else {
                                        positive = *add >= balance_add;
                                        balance_add = balance_add.abs_diff(*add);
                                    }
                                }
                                MemoryValue::LazySender(sub) => {
                                    if positive {
                                        positive = balance_add >= *sub;
                                        balance_add = balance_add.abs_diff(*sub);
                                    } else {
                                        balance_add = balance_add.saturating_add(*sub);
                                    }
                                    nonce_add += 1;
                                }
                                _ => return Err(ReadError::InvalidMemoryValueType),
                            }
                        }
                        None => break,
                    }
                }
            }
        }

        if base_account.is_none() {
            if !has_prev {
                new_origins.push(ReadOrigin::Storage);
            } else if origins.len() != new_origins.len() + 1
                || origins.last() != Some(&ReadOrigin::Storage)
            {
                return Err(ReadError::InconsistentRead);
            }
            base_account = match self.vm.storage.basic(&address) {
                Ok(Some(b)) => Some(b),
                Ok(None) => (balance_add > U256::ZERO).then(AccountBasic::default),
                Err(e) => return Err(ReadError::StorageError(e.to_string())),
            };
        }

        if !has_prev {
            *origins = new_origins;
        }

        let Some(mut account) = base_account else { return Ok(None) };

        account.nonce += nonce_add;
        if location_hash == self.from_hash && self.tx_nonce != account.nonce {
            return if self.tx_idx > 0 {
                Err(ReadError::Blocking(self.tx_idx - 1))
            } else {
                Err(ReadError::InvalidNonce(self.tx_idx))
            };
        }

        if positive {
            account.balance = account.balance.saturating_add(balance_add);
        } else {
            account.balance = account.balance.saturating_sub(balance_add);
        }

        let code_hash = if Some(location_hash) == self.to_hash {
            self.to_code_hash
        } else {
            self.get_code_hash(address)?
        };
        let code = if let Some(hash) = &code_hash {
            if let Some(bc) = self.vm.mv_memory.new_bytecodes.get(hash) {
                Some(bc.clone())
            } else {
                match self.vm.storage.code_by_hash(hash) {
                    Ok(Some(c)) => {
                        Some(Bytecode::try_from(c).map_err(ReadError::InvalidBytecode)?)
                    }
                    Ok(None) => None,
                    Err(e) => return Err(ReadError::StorageError(e.to_string())),
                }
            }
        } else {
            None
        };

        self.read_accounts.insert(location_hash, (account.clone(), code_hash));

        Ok(Some(AccountInfo {
            balance: account.balance,
            nonce: account.nonce,
            code_hash: code_hash.unwrap_or(revm::primitives::KECCAK_EMPTY),
            account_id: None,
            code,
        }))
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, ReadError> {
        match self
            .vm
            .storage
            .code_by_hash(&code_hash)
            .map_err(|e| ReadError::StorageError(e.to_string()))?
        {
            Some(c) => Bytecode::try_from(c).map_err(ReadError::InvalidBytecode),
            None => Ok(Bytecode::default()),
        }
    }

    fn storage(&mut self, address: Address, index: U256) -> Result<U256, ReadError> {
        let loc = hash_deterministic(MemoryLocation::Storage(address, index));
        let origins = self.read_set.entry(loc).or_default();

        if self.tx_idx > 0 {
            if let Some(written) = self.vm.mv_memory.data.get(&loc) {
                if let Some((idx, entry)) = written.range(..self.tx_idx).next_back() {
                    match entry {
                        MemoryEntry::Data(inc, MemoryValue::Storage(v)) => {
                            Self::push_origin(
                                origins,
                                ReadOrigin::MvMemory(TxVersion {
                                    tx_idx: *idx,
                                    tx_incarnation: *inc,
                                }),
                            )?;
                            return Ok(*v);
                        }
                        MemoryEntry::Estimate => return Err(ReadError::Blocking(*idx)),
                        _ => return Err(ReadError::InvalidMemoryValueType),
                    }
                }
            }
        }

        Self::push_origin(origins, ReadOrigin::Storage)?;
        self.vm
            .storage
            .storage(&address, &index)
            .map_err(|e| ReadError::StorageError(e.to_string()))
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, ReadError> {
        self.vm
            .storage
            .block_hash(&number)
            .map_err(|e| ReadError::StorageError(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Vm – orchestrates parallel execution of a single incarnation.
// ---------------------------------------------------------------------------

pub(crate) struct Vm<'a, S: Storage> {
    pub(crate) storage: &'a S,
    pub(crate) mv_memory: &'a MvMemory,
    pub(crate) block_env: BlockEnv,
    pub(crate) txs: &'a [TxEnv],
    pub(crate) spec_id: SpecId,
    pub(crate) beneficiary_location_hash: MemoryLocationHash,
    pub(crate) chain_id: u64,
    pub(crate) eip_161_enabled: bool,
    pub(crate) eip_1559_enabled: bool,
}

impl<'a, S: Storage> Vm<'a, S> {
    pub(crate) fn new(
        storage: &'a S,
        mv_memory: &'a MvMemory,
        block_env: BlockEnv,
        txs: &'a [TxEnv],
        spec_id: SpecId,
        chain_id: u64,
    ) -> Self {
        let beneficiary_location_hash =
            hash_deterministic(MemoryLocation::Basic(block_env.beneficiary));
        Self {
            storage,
            mv_memory,
            block_env,
            txs,
            spec_id,
            beneficiary_location_hash,
            chain_id,
            eip_161_enabled: spec_id.is_enabled_in(SpecId::SPURIOUS_DRAGON),
            eip_1559_enabled: spec_id.is_enabled_in(SpecId::LONDON),
        }
    }

    pub(crate) fn execute(
        &self,
        tx_version: &TxVersion,
    ) -> Result<VmExecutionResult, VmExecutionError> {
        let tx = unsafe { self.txs.get_unchecked(tx_version.tx_idx) };
        let from_hash = hash_deterministic(MemoryLocation::Basic(tx.caller));
        let to_hash = tx.kind.to().map(|to| hash_deterministic(MemoryLocation::Basic(*to)));

        let db = VmDb::new(self, tx_version.tx_idx, tx, from_hash, to_hash)
            .map_err(VmExecutionError::from)?;

        let block_env = self.block_env.clone();
        let ctx = Context::new(db, self.spec_id)
            .modify_block_chained(|b| *b = block_env)
            .modify_cfg_chained(|c| c.chain_id = self.chain_id);
        let mut evm = ctx.build_mainnet();
        evm.ctx.tx = tx.clone();

        type MyError = EVMError<ReadError, InvalidTransaction>;
        let exec_result =
            match NoRewardHandler::<_, MyError, _>::default().run(&mut evm) {
                Ok(r) => r,
                Err(EVMError::Database(e)) => return Err(VmExecutionError::from(e)),
                Err(EVMError::Transaction(
                    InvalidTransaction::LackOfFundForMaxFee { .. }
                    | InvalidTransaction::NonceTooHigh { .. },
                )) if tx_version.tx_idx > 0 => {
                    return Err(VmExecutionError::Blocking(tx_version.tx_idx - 1));
                }
                Err(e) => return Err(VmExecutionError::ExecutionError(e)),
            };

        let state = evm.ctx.journaled_state.finalize();
        let is_lazy = evm.ctx.journaled_state.database.is_lazy;
        let read_accounts = &evm.ctx.journaled_state.database.read_accounts;

        let mut write_set = WriteSet::with_capacity(3);
        for (address, account) in &state {
            if account.is_selfdestructed() {
                write_set.push((
                    hash_deterministic(MemoryLocation::CodeHash(*address)),
                    MemoryValue::SelfDestructed,
                ));
                continue;
            }

            if account.is_touched() {
                let acct_hash = hash_deterministic(MemoryLocation::Basic(*address));
                let read_acct = read_accounts.get(&acct_hash);
                let has_code = !account.info.is_empty_code_hash();
                let is_new_code = has_code && read_acct.is_none_or(|(_, ch)| ch.is_none());

                if is_new_code
                    || read_acct.is_none()
                    || read_acct.is_some_and(|(b, _)| {
                        b.nonce != account.info.nonce || b.balance != account.info.balance
                    })
                {
                    if is_lazy {
                        if acct_hash == from_hash {
                            write_set.push((
                                acct_hash,
                                MemoryValue::LazySender(U256::MAX - account.info.balance),
                            ));
                        } else if Some(acct_hash) == to_hash {
                            write_set.push((acct_hash, MemoryValue::LazyRecipient(tx.value)));
                        }
                    } else if !self.eip_161_enabled || !account.is_empty() {
                        write_set.push((
                            acct_hash,
                            MemoryValue::Basic(AccountBasic {
                                balance: account.info.balance,
                                nonce: account.info.nonce,
                            }),
                        ));
                    }
                }

                if is_new_code {
                    write_set.push((
                        hash_deterministic(MemoryLocation::CodeHash(*address)),
                        MemoryValue::CodeHash(account.info.code_hash),
                    ));
                    self.mv_memory
                        .new_bytecodes
                        .entry(account.info.code_hash)
                        .or_insert_with(|| account.info.code.clone().unwrap());
                }
            }

            for (slot, value) in account.changed_storage_slots() {
                write_set.push((
                    hash_deterministic(MemoryLocation::Storage(*address, *slot)),
                    MemoryValue::Storage(value.present_value),
                ));
            }
        }

        self.apply_rewards(&mut write_set, tx, U256::from(exec_result.gas_used()))?;

        let read_set = std::mem::take(&mut evm.ctx.journaled_state.database.read_set);

        if is_lazy {
            self.mv_memory.add_lazy_addresses([tx.caller, *tx.kind.to().unwrap()]);
        }

        let mut flags = if tx_version.tx_idx > 0 && !is_lazy {
            FinishExecFlags::NeedValidation
        } else {
            FinishExecFlags::empty()
        };
        if self.mv_memory.record(tx_version, read_set, write_set) {
            flags |= FinishExecFlags::WroteNewLocation;
        }

        Ok(VmExecutionResult {
            execution_result: PevmTxExecutionResult::from_revm(
                exec_result,
                state,
                self.eip_161_enabled,
            ),
            flags,
        })
    }

    fn apply_rewards(
        &self,
        write_set: &mut WriteSet,
        tx: &TxEnv,
        gas_used: U256,
    ) -> Result<(), VmExecutionError> {
        let mut gas_price = if let Some(pf) = tx.gas_priority_fee {
            pf.saturating_add(self.block_env.basefee as u128).min(tx.gas_price)
        } else {
            tx.gas_price
        };
        if self.eip_1559_enabled {
            gas_price = gas_price.saturating_sub(self.block_env.basefee as u128);
        }
        let reward = U256::from(gas_price).saturating_mul(gas_used);
        let loc = self.beneficiary_location_hash;

        if let Some((_, val)) = write_set.iter_mut().find(|(l, _)| *l == loc) {
            match val {
                MemoryValue::Basic(b) => b.balance = b.balance.saturating_add(reward),
                MemoryValue::LazySender(sub) => *sub = sub.saturating_sub(reward),
                MemoryValue::LazyRecipient(add) => *add = add.saturating_add(reward),
                _ => return Err(VmExecutionError::from(ReadError::InvalidMemoryValueType)),
            }
        } else {
            write_set.push((loc, MemoryValue::LazyRecipient(reward)));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Sequential EVM helper.
// ---------------------------------------------------------------------------

pub(crate) fn build_sequential_evm<DB: Database>(
    db: DB,
    chain_id: u64,
    spec_id: SpecId,
    block_env: &BlockEnv,
) -> revm::handler::MainnetEvm<revm::context::Context<
    BlockEnv,
    TxEnv,
    CfgEnv,
    DB,
    revm::context::Journal<DB>,
    (),
>> {
    Context::new(db, spec_id)
        .modify_block_chained(|b| *b = block_env.clone())
        .modify_cfg_chained(|c| c.chain_id = chain_id)
        .build_mainnet()
}

// ---------------------------------------------------------------------------
// NoRewardHandler.
// ---------------------------------------------------------------------------

/// Handler that skips beneficiary reward during parallel execution.
///
/// Gas rewards are accumulated as lazy writes in the Block-STM write-set,
/// breaking the serial dependency chain through the block coinbase.
pub(crate) struct NoRewardHandler<EVM, ERROR, FRAME> {
    _phantom: std::marker::PhantomData<(EVM, ERROR, FRAME)>,
}

impl<EVM, ERROR, FRAME> Default for NoRewardHandler<EVM, ERROR, FRAME> {
    fn default() -> Self {
        Self { _phantom: std::marker::PhantomData }
    }
}

impl<EVM, ERROR, FRAME> Handler for NoRewardHandler<EVM, ERROR, FRAME>
where
    EVM: revm::handler::EvmTr<
        Context: revm::context_interface::ContextTr<
            Journal: revm::context_interface::JournalTr<State = EvmState>,
        >,
        Frame = FRAME,
    >,
    ERROR: EvmTrError<EVM>,
    FRAME: revm::handler::FrameTr<
        FrameResult = FrameResult,
        FrameInit = revm::interpreter::interpreter_action::FrameInit,
    >,
{
    type Evm = EVM;
    type Error = ERROR;
    type HaltReason = HaltReason;

    fn reward_beneficiary(
        &self,
        _evm: &mut Self::Evm,
        _exec_result: &mut FrameResult,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}
