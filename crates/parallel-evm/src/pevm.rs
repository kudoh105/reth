//! The Block-STM parallel executor.
//!
//! Adapted from PEVM (https://github.com/risechain/pevm) for revm v34 / reth.

use std::{
    fmt::Debug,
    num::NonZeroUsize,
    sync::{Mutex, OnceLock, mpsc},
    thread,
};

use alloy_primitives::U256;
use hashbrown::HashMap;
use revm::{
    DatabaseCommit,
    context::{BlockEnv, TxEnv, result::EVMError},
    database::CacheDB,
    handler::ExecuteEvm,
    primitives::hardfork::SpecId,
};

use crate::{
    EvmAccount, MemoryEntry, MemoryLocation, MemoryValue, Storage, Task, TxIdx, TxVersion,
    hash_deterministic,
    index_mutex,
    mv_memory::MvMemory,
    scheduler::Scheduler,
    storage::StorageWrapper,
    vm::{
        ExecutionError, ExecutionResult, HaltReason, InvalidTransaction, PevmTxExecutionResult,
        ReadError, Vm, VmExecutionError, VmExecutionResult, build_sequential_evm,
    },
};

/// Errors returned by the parallel executor.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PevmError {
    #[error("Transaction data is missing")]
    MissingTransactionData,
    #[error("Nonce mismatch at tx #{tx_idx}: expected {executed_nonce}, got {tx_nonce}")]
    NonceMismatch {
        tx_idx: TxIdx,
        tx_nonce: u64,
        executed_nonce: u64,
    },
    #[error("Storage error: {0}")]
    StorageError(String),
    #[error("EVM execution error")]
    ExecutionError(#[from] ExecutionError),
    #[error("PEVM internal error (bug — please report)")]
    UnreachableError,
}

/// Result of executing a full block.
pub type PevmResult = Result<Vec<PevmTxExecutionResult>, PevmError>;

#[derive(Debug)]
enum AbortReason {
    FallbackToSequential,
    ExecutionError(ExecutionError),
}

#[derive(Debug)]
struct AsyncDropper<T> {
    sender: mpsc::Sender<T>,
    _handle: thread::JoinHandle<()>,
}

impl<T: Send + 'static> Default for AsyncDropper<T> {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            sender,
            _handle: thread::spawn(move || receiver.into_iter().for_each(drop)),
        }
    }
}

impl<T> AsyncDropper<T> {
    fn drop(&self, t: T) {
        self.sender.send(t).unwrap();
    }
}

/// The main parallel EVM executor struct.
///
/// Reuses internal allocations across blocks for efficiency.
#[derive(Debug, Default)]
pub struct ParallelEvm {
    execution_results: Vec<Mutex<Option<PevmTxExecutionResult>>>,
    abort_reason: OnceLock<AbortReason>,
    dropper: AsyncDropper<(MvMemory, Scheduler, Vec<TxEnv>)>,
}

impl ParallelEvm {
    /// Execute a block's transactions in parallel using Block-STM.
    ///
    /// Falls back to sequential execution for small/low-gas blocks.
    pub fn execute<S: Storage + Send + Sync>(
        &mut self,
        storage: &S,
        block_env: BlockEnv,
        txs: Vec<TxEnv>,
        spec_id: SpecId,
        chain_id: u64,
        concurrency_level: NonZeroUsize,
        force_sequential: bool,
    ) -> PevmResult {
        if force_sequential
            || txs.len() < concurrency_level.into()
            || block_env.gas_limit < 4_000_000
        {
            execute_revm_sequential(storage, block_env, txs, spec_id, chain_id)
        } else {
            self.execute_parallel(storage, block_env, txs, spec_id, chain_id, concurrency_level)
        }
    }

    fn execute_parallel<S: Storage + Send + Sync>(
        &mut self,
        storage: &S,
        block_env: BlockEnv,
        txs: Vec<TxEnv>,
        spec_id: SpecId,
        chain_id: u64,
        concurrency_level: NonZeroUsize,
    ) -> PevmResult {
        if txs.is_empty() {
            return Ok(Vec::new());
        }

        let block_size = txs.len();
        let scheduler = Scheduler::new(block_size);
        let mv_memory = MvMemory::new(block_size, [], []);
        let vm = Vm::new(storage, &mv_memory, block_env.clone(), &txs, spec_id, chain_id);

        // Grow the results buffer if needed.
        let additional = block_size.saturating_sub(self.execution_results.len());
        if additional > 0 {
            self.execution_results.reserve(additional);
            for _ in 0..additional {
                self.execution_results.push(Mutex::new(None));
            }
        }

        thread::scope(|scope| {
            for _ in 0..concurrency_level.into() {
                scope.spawn(|| {
                    let mut task = scheduler.next_task();
                    while task.is_some() {
                        task = match task.unwrap() {
                            Task::Execution(tv) => self.try_execute(&vm, &scheduler, tv),
                            Task::Validation(tv) => try_validate(&mv_memory, &scheduler, &tv),
                        };
                        if self.abort_reason.get().is_some() {
                            break;
                        }
                        if task.is_none() {
                            task = scheduler.next_task();
                        }
                    }
                });
            }
        });

        if let Some(reason) = self.abort_reason.take() {
            match reason {
                AbortReason::FallbackToSequential => {
                    self.dropper.drop((mv_memory, scheduler, Vec::new()));
                    return execute_revm_sequential(storage, block_env, txs, spec_id, chain_id);
                }
                AbortReason::ExecutionError(err) => {
                    self.dropper.drop((mv_memory, scheduler, txs));
                    return Err(PevmError::ExecutionError(err));
                }
            }
        }

        // Assemble final results and fix up cumulative gas.
        let mut results = Vec::with_capacity(block_size);
        let mut cumulative_gas: u64 = 0;
        for i in 0..block_size {
            let mut r = index_mutex!(self.execution_results, i).take().unwrap();
            cumulative_gas = cumulative_gas.saturating_add(r.receipt.cumulative_gas_used);
            r.receipt.cumulative_gas_used = cumulative_gas;
            results.push(r);
        }

        // Fully evaluate lazy addresses (beneficiary, raw-transfer participants).
        for address in mv_memory.consume_lazy_addresses() {
            let location_hash = hash_deterministic(MemoryLocation::Basic(address));
            if let Some(write_history) = mv_memory.data.get(&location_hash) {
                let mut balance = U256::ZERO;
                let mut nonce: u64 = 0;

                if !matches!(
                    write_history.first_key_value(),
                    Some((_, MemoryEntry::Data(_, MemoryValue::Basic(_))))
                ) {
                    if let Ok(Some(acct)) = storage.basic(&address) {
                        balance = acct.balance;
                        nonce = acct.nonce;
                    }
                }

                let code_hash = match storage.code_hash(&address) {
                    Ok(v) => v,
                    Err(e) => return Err(PevmError::StorageError(e.to_string())),
                };
                let code = if let Some(h) = &code_hash {
                    match storage.code_by_hash(h) {
                        Ok(v) => v,
                        Err(e) => return Err(PevmError::StorageError(e.to_string())),
                    }
                } else {
                    None
                };

                for (tx_idx, entry) in write_history.iter() {
                    let tx = unsafe { txs.get_unchecked(*tx_idx) };
                    match entry {
                        MemoryEntry::Data(_, MemoryValue::Basic(info)) => {
                            debug_assert!(!(info.balance.is_zero() && info.nonce == 0));
                            balance = info.balance;
                            nonce = info.nonce;
                        }
                        MemoryEntry::Data(_, MemoryValue::LazyRecipient(add)) => {
                            balance = balance.saturating_add(*add);
                        }
                        MemoryEntry::Data(_, MemoryValue::LazySender(sub)) => {
                            let max_fee = U256::from(tx.gas_limit)
                                .saturating_mul(U256::from(tx.gas_price))
                                .saturating_add(tx.value);
                            if balance < max_fee {
                                return Err(PevmError::ExecutionError(EVMError::Transaction(
                                    InvalidTransaction::LackOfFundForMaxFee {
                                        balance: Box::new(balance),
                                        fee: Box::new(max_fee),
                                    },
                                )));
                            }
                            balance = balance.saturating_sub(*sub);
                            nonce += 1;
                        }
                        _ => unreachable!(),
                    }

                    if tx.caller == address {
                        let tx_nonce = tx.nonce;
                        let executed_nonce = if nonce == 0 {
                            return Err(PevmError::UnreachableError);
                        } else {
                            nonce - 1
                        };
                        if tx_nonce != executed_nonce {
                            return Err(PevmError::NonceMismatch {
                                tx_idx: *tx_idx,
                                tx_nonce,
                                executed_nonce,
                            });
                        }
                    }

                    let tx_result = unsafe { results.get_unchecked_mut(*tx_idx) };
                    let eip_161 = spec_id.is_enabled_in(revm::primitives::hardfork::SpecId::SPURIOUS_DRAGON);
                    let acct_entry = tx_result.state.entry(address).or_default();
                    if eip_161 && code_hash.is_none() && nonce == 0 && balance == U256::ZERO {
                        *acct_entry = None;
                    } else if let Some(acct) = acct_entry {
                        acct.balance = balance;
                        acct.nonce = nonce;
                    } else {
                        *acct_entry = Some(EvmAccount {
                            balance,
                            nonce,
                            code_hash,
                            code: code.clone(),
                            storage: HashMap::default(),
                        });
                    }
                }
            }
        }

        self.dropper.drop((mv_memory, scheduler, txs));
        Ok(results)
    }

    fn try_execute<S: Storage>(
        &self,
        vm: &Vm<'_, S>,
        scheduler: &Scheduler,
        tx_version: TxVersion,
    ) -> Option<Task> {
        loop {
            return match vm.execute(&tx_version) {
                Err(VmExecutionError::Retry) => {
                    if self.abort_reason.get().is_none() {
                        continue;
                    }
                    None
                }
                Err(VmExecutionError::FallbackToSequential) => {
                    scheduler.abort();
                    self.abort_reason.get_or_init(|| AbortReason::FallbackToSequential);
                    None
                }
                Err(VmExecutionError::Blocking(idx)) => {
                    if !scheduler.add_dependency(tx_version.tx_idx, idx)
                        && self.abort_reason.get().is_none()
                    {
                        continue;
                    }
                    None
                }
                Err(VmExecutionError::ExecutionError(err)) => {
                    scheduler.abort();
                    self.abort_reason.get_or_init(|| AbortReason::ExecutionError(err));
                    None
                }
                Ok(VmExecutionResult { execution_result, flags }) => {
                    *index_mutex!(self.execution_results, tx_version.tx_idx) =
                        Some(execution_result);
                    scheduler.finish_execution(tx_version, flags)
                }
            };
        }
    }
}

fn try_validate(
    mv_memory: &MvMemory,
    scheduler: &Scheduler,
    tx_version: &TxVersion,
) -> Option<Task> {
    let valid = mv_memory.validate_read_locations(tx_version.tx_idx);
    let aborted = !valid && scheduler.try_validation_abort(tx_version);
    if aborted {
        mv_memory.convert_writes_to_estimates(tx_version.tx_idx);
    }
    scheduler.finish_validation(tx_version, aborted)
}

/// Execute transactions sequentially (fallback for small/simple blocks).
pub fn execute_revm_sequential<S: Storage>(
    storage: &S,
    block_env: BlockEnv,
    txs: Vec<TxEnv>,
    spec_id: SpecId,
    chain_id: u64,
) -> PevmResult {
    let eip_161 = spec_id.is_enabled_in(revm::primitives::hardfork::SpecId::SPURIOUS_DRAGON);
    let wrapper = StorageWrapper(storage);
    let mut db = CacheDB::new(wrapper);
    let mut evm = build_sequential_evm(&mut db, chain_id, spec_id, &block_env);

    let mut results = Vec::with_capacity(txs.len());
    let mut cumulative_gas: u64 = 0;

    for tx in txs {
        let result_and_state = evm.transact(tx).map_err(|e| match e {
            EVMError::Database(db_err) => PevmError::StorageError(db_err.to_string()),
            EVMError::Transaction(tx_err) => {
                PevmError::ExecutionError(EVMError::Transaction(tx_err))
            }
            EVMError::Header(h) => PevmError::ExecutionError(EVMError::Header(h)),
            EVMError::Custom(c) => PevmError::ExecutionError(EVMError::Custom(c)),
        })?;

        evm.db_mut().commit(result_and_state.state.clone());

        let mut r = PevmTxExecutionResult::from_revm(
            result_and_state.result,
            result_and_state.state,
            eip_161,
        );
        cumulative_gas = cumulative_gas.saturating_add(r.receipt.cumulative_gas_used);
        r.receipt.cumulative_gas_used = cumulative_gas;
        results.push(r);
    }

    Ok(results)
}
