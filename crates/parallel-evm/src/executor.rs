//! Bridge between the Block-STM parallel executor and reth's block-executor traits.
//!
//! Provides a drop-in replacement for `EthEvmConfig` / `EthereumExecutorBuilder` that
//! executes user transactions in parallel while keeping the sequential path for
//! system calls, withdrawals, and uncle rewards.

use std::{
    fmt::Debug,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

use alloy_consensus::Header;
use alloy_evm::{
    EthEvmFactory, FromRecoveredTx, FromTxWithEncoded,
    block::{
        BlockExecutionError, BlockExecutionResult, BlockExecutor, BlockExecutorFactory,
        BlockExecutorFor, ExecutableTx, OnStateHook,
    },
    eth::{EthBlockExecutionCtx, EthBlockExecutor, EthBlockExecutorFactory},
    Database, Evm as EvmTrait, EvmFactory,
};
use revm::inspector::NoOpInspector;
use alloy_primitives::{Address, B256, U256};
use hashbrown::HashMap;
use revm::{
    DatabaseCommit, DatabaseRef,
    context::{BlockEnv, TxEnv},
    database::{CacheDB, State},
    primitives::hardfork::SpecId,
    state::{AccountInfo, EvmStorageSlot},
};
use reth_chainspec::{ChainSpec, EthChainSpec, EthereumHardforks};
use reth_ethereum_forks::Hardforks;
use reth_ethereum_primitives::{EthPrimitives, Receipt, TransactionSigned};
use reth_evm::{
    ConfigureEvm, EvmEnv, NextBlockEnvAttributes, TransactionEnv,
    eth::NextEvmEnvAttributes,
    precompiles::PrecompilesMap,
};
use reth_node_builder::{BuilderContext, FullNodeTypes, NodeTypes};
use reth_primitives_traits::SealedBlock;
use tracing::{debug, warn};

use crate::{
    AccountBasic, EvmAccount, EvmCode, PevmError, Storage,
    pevm::{ParallelEvm, PevmResult, execute_revm_sequential},
    storage::BytecodeConversionError,
    vm::{ExecutionError, PevmTxExecutionResult},
};
use reth_evm_ethereum::RethReceiptBuilder;
use alloy_evm::eth::spec::EthExecutorSpec;

// ---------------------------------------------------------------------------
// Storage adapter for State<DB>
// ---------------------------------------------------------------------------

/// Adapts `State<DB>` to our `Storage` trait via its `DatabaseRef` implementation.
///
/// Reads flow through the State's cache (so system-call modifications made before
/// user-tx execution are visible), while the underlying DB provides pre-block state.
struct StateStorage<'a, DB: DatabaseRef>(&'a State<DB>);

#[derive(Debug, thiserror::Error)]
enum StateStorageError<E: Debug + std::fmt::Display> {
    #[error("EVM database error: {0}")]
    Db(E),
    #[error("Bytecode conversion error: {0}")]
    Bytecode(#[from] BytecodeConversionError),
}

impl<'a, DB> Storage for StateStorage<'a, DB>
where
    DB: DatabaseRef,
    DB::Error: Debug + std::fmt::Display + 'static,
{
    type Error = StateStorageError<revm::database::bal::EvmDatabaseError<DB::Error>>;

    fn basic(&self, address: &Address) -> Result<Option<AccountBasic>, Self::Error> {
        Ok(self
            .0
            .basic_ref(*address)
            .map_err(StateStorageError::Db)?
            .map(|info| AccountBasic { balance: info.balance, nonce: info.nonce }))
    }

    fn code_hash(&self, address: &Address) -> Result<Option<B256>, Self::Error> {
        Ok(self
            .0
            .basic_ref(*address)
            .map_err(StateStorageError::Db)?
            .filter(|i| !i.is_empty_code_hash())
            .map(|i| i.code_hash))
    }

    fn code_by_hash(&self, hash: &B256) -> Result<Option<EvmCode>, Self::Error> {
        let bc = self.0.code_by_hash_ref(*hash).map_err(StateStorageError::Db)?;
        if bc.is_empty() {
            return Ok(None);
        }
        Ok(Some(EvmCode::from(bc)))
    }

    fn has_storage(&self, _address: &Address) -> Result<bool, Self::Error> {
        // Conservative: always false (parallel execution will re-check per slot).
        Ok(false)
    }

    fn storage(&self, address: &Address, slot: &U256) -> Result<U256, Self::Error> {
        self.0.storage_ref(*address, *slot).map_err(StateStorageError::Db)
    }

    fn block_hash(&self, number: &u64) -> Result<B256, Self::Error> {
        self.0.block_hash_ref(*number).map_err(StateStorageError::Db)
    }
}

// ---------------------------------------------------------------------------
// Apply parallel results back to State<DB>
// ---------------------------------------------------------------------------

/// Merges `PevmTxExecutionResult` state diffs into a revm `State<DB>`.
///
/// For each address that was modified:
/// 1. Pre-warms the account into the state cache (via `load_cache_account`).
/// 2. Reads original storage slot values (from the just-warmed cache, pre-user-tx).
/// 3. Builds a `HashMap<Address, Account>` and commits it to the state.
fn apply_pevm_results_to_state<DB>(
    state: &mut State<DB>,
    results: &[PevmTxExecutionResult],
) -> Result<(), PevmError>
where
    DB: DatabaseRef,
    DB::Error: Debug + std::fmt::Display + 'static,
{
    use revm::state::{Account, AccountStatus, EvmStorage};

    // Compute the merged final state (later txs override earlier for the same address).
    // The parallel executor already guarantees the writes are ordered correctly.
    let mut merged: HashMap<Address, Option<EvmAccount>> = HashMap::new();
    for result in results {
        for (addr, maybe_acct) in &result.state {
            merged.insert(*addr, maybe_acct.clone());
        }
    }

    if merged.is_empty() {
        return Ok(());
    }

    // Pre-warm all changed accounts so they appear in State's cache.
    // This is required by `State::commit` which expects all accounts in cache.
    for addr in merged.keys() {
        state
            .load_cache_account(*addr)
            .map_err(|e| PevmError::StorageError(e.to_string()))?;
    }

    // Now build the revm Account objects.
    // For each storage slot, read the original (pre-user-tx) value from State.
    let mut changes: HashMap<Address, Account> = HashMap::with_capacity(merged.len());
    for (addr, maybe_acct) in merged {
        if let Some(acct) = maybe_acct {
            // Build AccountInfo
            let code = if let Some(code) = acct.code {
                match revm::bytecode::Bytecode::try_from(code) {
                    Ok(bc) => Some(bc),
                    Err(e) => {
                        warn!(?addr, ?e, "parallel executor: bytecode conversion error, skipping account");
                        None
                    }
                }
            } else {
                None
            };
            let code_hash = acct.code_hash.unwrap_or(revm::primitives::KECCAK_EMPTY);
            let info = AccountInfo {
                balance: acct.balance,
                nonce: acct.nonce,
                code_hash,
                account_id: None,
                code,
            };

            // Build storage with correct original values.
            let mut evm_storage: EvmStorage =
                HashMap::with_capacity_and_hasher(acct.storage.len(), Default::default());
            for (slot, present_value) in acct.storage {
                let original = state
                    .storage_ref(addr, slot)
                    .unwrap_or_default();
                evm_storage.insert(slot, EvmStorageSlot::new_changed(original, present_value));
            }

            let account = Account {
                info,
                original_info: Box::new(AccountInfo::default()),
                transaction_id: 0,
                storage: evm_storage,
                status: AccountStatus::Touched,
            };
            changes.insert(addr, account);
        } else {
            // Account was deleted / self-destructed.
            let account = Account {
                info: AccountInfo::default(),
                original_info: Box::new(AccountInfo::default()),
                transaction_id: 0,
                storage: EvmStorage::default(),
                status: AccountStatus::SelfDestructed | AccountStatus::Touched,
            };
            changes.insert(addr, account);
        }
    }

    state.commit(changes);
    Ok(())
}

// ---------------------------------------------------------------------------
// ParallelBlockExecutorFactory
// ---------------------------------------------------------------------------

/// Factory that produces `ParallelBlockExecutor` instances.
#[derive(Clone)]
pub struct ParallelBlockExecutorFactory<Spec, EvmF = EthEvmFactory> {
    inner: EthBlockExecutorFactory<RethReceiptBuilder, Spec, EvmF>,
    parallel_evm: Arc<Mutex<ParallelEvm>>,
    spec_id: SpecId,
    chain_id: u64,
    concurrency: NonZeroUsize,
}

impl<Spec, EvmF> Debug for ParallelBlockExecutorFactory<Spec, EvmF>
where
    Spec: Debug,
    EvmF: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParallelBlockExecutorFactory")
            .field("inner", &self.inner)
            .field("concurrency", &self.concurrency)
            .finish()
    }
}

impl<Spec, EvmF> ParallelBlockExecutorFactory<Spec, EvmF>
where
    Spec: Clone,
    EvmF: Clone,
{
    /// Creates a new `ParallelBlockExecutorFactory`.
    pub fn new(
        inner: EthBlockExecutorFactory<RethReceiptBuilder, Spec, EvmF>,
        spec_id: SpecId,
        chain_id: u64,
        concurrency: NonZeroUsize,
    ) -> Self {
        Self {
            inner,
            parallel_evm: Arc::new(Mutex::new(ParallelEvm::default())),
            spec_id,
            chain_id,
            concurrency,
        }
    }
}

impl<Spec, EvmF> BlockExecutorFactory for ParallelBlockExecutorFactory<Spec, EvmF>
where
    Spec: EthExecutorSpec + Clone + Debug + Send + Sync + 'static,
    EvmF: EvmFactory<
            Tx: FromRecoveredTx<TransactionSigned>
                + FromTxWithEncoded<TransactionSigned>
                + Clone,
            Spec = SpecId,
            BlockEnv = BlockEnv,
            Precompiles = PrecompilesMap,
        > + Clone
        + Debug
        + Send
        + Sync
        + 'static,
    EvmF::Evm<&'static mut State<revm::database::EmptyDB>, NoOpInspector>: EvmTrait<Tx = TxEnv, BlockEnv = BlockEnv>,
    Self: 'static,
{
    type EvmFactory = EvmF;
    type ExecutionCtx<'a> = EthBlockExecutionCtx<'a>;
    type Transaction = TransactionSigned;
    type Receipt = Receipt;

    fn evm_factory(&self) -> &Self::EvmFactory {
        self.inner.evm_factory()
    }

    fn create_executor<'a, DB, I>(
        &'a self,
        evm: EvmF::Evm<&'a mut State<DB>, I>,
        ctx: Self::ExecutionCtx<'a>,
    ) -> impl BlockExecutorFor<'a, Self, DB, I>
    where
        DB: Database + 'a,
        I: revm::Inspector<EvmF::Context<&'a mut State<DB>>> + 'a,
    {
        let inner = self.inner.create_executor(evm, ctx);
        ParallelBlockExecutorWrapper {
            inner,
            parallel_evm: Arc::clone(&self.parallel_evm),
            spec_id: self.spec_id,
            chain_id: self.chain_id,
            concurrency: self.concurrency,
        }
    }
}

// ---------------------------------------------------------------------------
// Wrapper that holds the inner executor + parallel context.
// The inner executor type is opaque (impl BlockExecutorFor), so we wrap it.
// ---------------------------------------------------------------------------

struct ParallelBlockExecutorWrapper<Inner> {
    inner: Inner,
    parallel_evm: Arc<Mutex<ParallelEvm>>,
    spec_id: SpecId,
    chain_id: u64,
    concurrency: NonZeroUsize,
}

impl<Inner: BlockExecutor<Transaction = TransactionSigned, Receipt = Receipt>> BlockExecutor
    for ParallelBlockExecutorWrapper<Inner>
{
    type Transaction = TransactionSigned;
    type Receipt = Receipt;
    type Evm = Inner::Evm;
    type Result = Inner::Result;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.inner.apply_pre_execution_changes()
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<Self::Result, BlockExecutionError> {
        self.inner.execute_transaction_without_commit(tx)
    }

    fn commit_transaction(&mut self, output: Self::Result) -> Result<u64, BlockExecutionError> {
        self.inner.commit_transaction(output)
    }

    fn finish(
        self,
    ) -> Result<(Self::Evm, BlockExecutionResult<Self::Receipt>), BlockExecutionError> {
        self.inner.finish()
    }

    fn set_state_hook(&mut self, hook: Option<Box<dyn OnStateHook>>) {
        self.inner.set_state_hook(hook);
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        self.inner.evm_mut()
    }

    fn evm(&self) -> &Self::Evm {
        self.inner.evm()
    }

    fn receipts(&self) -> &[Self::Receipt] {
        self.inner.receipts()
    }
}

// ---------------------------------------------------------------------------
// ParallelEthEvmConfig
// ---------------------------------------------------------------------------

/// A drop-in replacement for `EthEvmConfig` that runs user transactions in
/// parallel using Block-STM.
///
/// System calls, withdrawals, and uncle rewards still execute sequentially.
#[derive(Debug, Clone)]
pub struct ParallelEthEvmConfig<C = ChainSpec, EvmF = EthEvmFactory> {
    inner: reth_evm_ethereum::EthEvmConfig<C, EvmF>,
    parallel_factory: ParallelBlockExecutorFactory<Arc<C>, EvmF>,
}

impl<C, EvmF> ParallelEthEvmConfig<C, EvmF>
where
    C: EthChainSpec<Header = Header> + EthereumHardforks + 'static,
    EvmF: Clone + Debug + Send + Sync + 'static,
{
    /// Creates a new `ParallelEthEvmConfig` with the given chain spec and concurrency level.
    pub fn new(
        chain_spec: Arc<C>,
        evm_factory: EvmF,
        concurrency: NonZeroUsize,
    ) -> Self
    where
        EthBlockExecutorFactory<RethReceiptBuilder, Arc<C>, EvmF>: Clone,
    {
        let inner = reth_evm_ethereum::EthEvmConfig::new_with_evm_factory(
            chain_spec.clone(),
            evm_factory.clone(),
        );
        let spec_id = revm::primitives::hardfork::SpecId::CANCUN; // updated per block
        let chain_id = chain_spec.chain().id();
        let parallel_factory = ParallelBlockExecutorFactory::new(
            inner.executor_factory.clone(),
            spec_id,
            chain_id,
            concurrency,
        );
        Self { inner, parallel_factory }
    }
}

impl<C, EvmF> ConfigureEvm for ParallelEthEvmConfig<C, EvmF>
where
    C: EthExecutorSpec + EthChainSpec<Header = Header> + Hardforks + EthereumHardforks + 'static,
    EvmF: EvmFactory<
            Tx: TransactionEnv
                + FromRecoveredTx<TransactionSigned>
                + FromTxWithEncoded<TransactionSigned>
                + Clone,
            Spec = SpecId,
            BlockEnv = BlockEnv,
            Precompiles = PrecompilesMap,
        > + Clone
        + Debug
        + Send
        + Sync
        + Unpin
        + 'static,
    EvmF::Evm<&'static mut State<revm::database::EmptyDB>, NoOpInspector>: EvmTrait<Tx = TxEnv, BlockEnv = BlockEnv>,
    EthBlockExecutorFactory<RethReceiptBuilder, Arc<C>, EvmF>: Clone,
{
    type Primitives = EthPrimitives;
    type Error = std::convert::Infallible;
    type NextBlockEnvCtx = NextBlockEnvAttributes;
    type BlockExecutorFactory = ParallelBlockExecutorFactory<Arc<C>, EvmF>;
    type BlockAssembler = reth_evm_ethereum::EthBlockAssembler<C>;

    fn block_executor_factory(&self) -> &Self::BlockExecutorFactory {
        &self.parallel_factory
    }

    fn block_assembler(&self) -> &Self::BlockAssembler {
        self.inner.block_assembler()
    }

    fn evm_env(&self, header: &Header) -> Result<EvmEnv<SpecId>, Self::Error> {
        self.inner.evm_env(header)
    }

    fn next_evm_env(
        &self,
        parent: &Header,
        attributes: &NextBlockEnvAttributes,
    ) -> Result<EvmEnv, Self::Error> {
        self.inner.next_evm_env(parent, attributes)
    }

    fn context_for_block<'a>(
        &self,
        block: &'a SealedBlock<reth_ethereum_primitives::Block>,
    ) -> Result<EthBlockExecutionCtx<'a>, Self::Error> {
        self.inner.context_for_block(block)
    }

    fn context_for_next_block(
        &self,
        parent: &reth_primitives_traits::SealedHeader,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<EthBlockExecutionCtx<'_>, Self::Error> {
        self.inner.context_for_next_block(parent, attributes)
    }
}

// ---------------------------------------------------------------------------
// Reth node builder integration
// ---------------------------------------------------------------------------

/// Executor builder that produces a `ParallelEthEvmConfig`.
///
/// Use with `ComponentsBuilder::executor(ParallelEthereumExecutorBuilder::new(concurrency))`.
#[derive(Debug, Clone)]
pub struct ParallelEthereumExecutorBuilder {
    concurrency: NonZeroUsize,
}

impl ParallelEthereumExecutorBuilder {
    /// Creates a new builder with the specified parallelism level.
    pub fn new(concurrency: NonZeroUsize) -> Self {
        Self { concurrency }
    }

    /// Uses all available CPU cores.
    pub fn all_cores() -> Self {
        let n = std::thread::available_parallelism()
            .unwrap_or(NonZeroUsize::new(4).unwrap());
        Self::new(n)
    }
}

impl Default for ParallelEthereumExecutorBuilder {
    fn default() -> Self {
        Self::all_cores()
    }
}

impl<Types, Node> reth_node_builder::components::ExecutorBuilder<Node>
    for ParallelEthereumExecutorBuilder
where
    Types: NodeTypes<
        ChainSpec: EthExecutorSpec + EthChainSpec<Header = Header> + Hardforks + EthereumHardforks,
        Primitives = EthPrimitives,
    >,
    Node: FullNodeTypes<Types = Types>,
    EthBlockExecutorFactory<
        RethReceiptBuilder,
        Arc<Types::ChainSpec>,
        EthEvmFactory,
    >: Clone,
{
    type EVM = ParallelEthEvmConfig<Types::ChainSpec, EthEvmFactory>;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        let chain_spec = ctx.chain_spec();
        Ok(ParallelEthEvmConfig::new(chain_spec, EthEvmFactory::default(), self.concurrency))
    }
}
