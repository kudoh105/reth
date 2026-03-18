//! Block-STM parallel EVM executor for reth (adapted from PEVM).
//!
//! Re-implements PEVM's core algorithm against revm v34 and reth's executor traits.

use std::hash::{BuildHasher, BuildHasherDefault, Hash, Hasher};

use alloy_primitives::{Address, B256, U256};
use bitflags::bitflags;
use hashbrown::HashMap;
use rustc_hash::FxBuildHasher;
use smallvec::SmallVec;

/// Uses the last 8 bytes of an existing hash (address, code hash) instead of
/// rehashing, giving O(1) map operations without a full hash computation.
#[derive(Debug, Default)]
pub struct SuffixHasher(u64);
impl Hasher for SuffixHasher {
    fn write(&mut self, bytes: &[u8]) {
        let mut suffix = [0u8; 8];
        suffix.copy_from_slice(&bytes[bytes.len() - 8..]);
        self.0 = u64::from_be_bytes(suffix);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}
pub type BuildSuffixHasher = BuildHasherDefault<SuffixHasher>;

/// Identity hasher for pre-hashed keys (memory location hashes, tx indexes).
#[derive(Debug, Default)]
pub struct IdentityHasher(u64);
impl Hasher for IdentityHasher {
    fn write_u64(&mut self, id: u64) {
        self.0 = id;
    }
    fn write_usize(&mut self, id: usize) {
        self.0 = id as u64;
    }
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, _: &[u8]) {
        unreachable!()
    }
}
pub type BuildIdentityHasher = BuildHasherDefault<IdentityHasher>;

#[inline(always)]
pub(crate) fn hash_deterministic<T: Hash>(x: T) -> u64 {
    FxBuildHasher.hash_one(x)
}

/// A memory location that a transaction may read or write.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum MemoryLocation {
    Basic(Address),
    CodeHash(Address),
    Storage(Address, U256),
}

/// A hashed form of a memory location, used as the key in MvMemory.
pub type MemoryLocationHash = u64;

/// The value stored at a given memory location in the multi-version data structure.
#[derive(Debug, Clone)]
pub(crate) enum MemoryValue {
    Basic(crate::storage::AccountBasic),
    CodeHash(B256),
    Storage(U256),
    /// Lazy balance increment: beneficiary gas reward or raw-transfer recipient.
    /// Fully evaluated at the end of the block or on an explicit read.
    LazyRecipient(U256),
    /// Lazy balance decrement + implicit nonce increment: raw-transfer sender.
    LazySender(U256),
    /// The account was self-destructed.
    SelfDestructed,
}

/// A single entry in MvMemory: either a committed value or an abort-estimate marker.
#[derive(Debug)]
pub(crate) enum MemoryEntry {
    Data(TxIncarnation, MemoryValue),
    /// Aborted incarnation; the next incarnation is estimated to write here again.
    Estimate,
}

/// Index of a transaction within the block.
pub type TxIdx = usize;

/// Number of times a transaction has been (re-)executed, counting from 0.
pub type TxIncarnation = usize;

#[derive(PartialEq, Debug)]
pub(crate) enum IncarnationStatus {
    ReadyToExecute,
    Executing,
    Executed,
    Validated,
    Aborting,
}

#[derive(PartialEq, Debug)]
pub(crate) struct TxStatus {
    pub(crate) incarnation: TxIncarnation,
    pub(crate) status: IncarnationStatus,
}

/// Identifies a specific (transaction, incarnation) pair.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TxVersion {
    pub(crate) tx_idx: TxIdx,
    pub(crate) tx_incarnation: TxIncarnation,
}

/// The source from which a memory location was read.
#[derive(Debug, PartialEq)]
pub(crate) enum ReadOrigin {
    MvMemory(TxVersion),
    Storage,
}

/// Origins for a single memory location (usually one; lazy locations have more).
pub(crate) type ReadOrigins = SmallVec<[ReadOrigin; 1]>;

/// Read set: maps hashed memory location → its read origins for this execution.
pub(crate) type ReadSet = HashMap<MemoryLocationHash, ReadOrigins, BuildIdentityHasher>;

/// Write set: the updates produced by one transaction incarnation.
pub(crate) type WriteSet = Vec<(MemoryLocationHash, MemoryValue)>;

/// Work item for a scheduler thread.
#[derive(Debug)]
pub(crate) enum Task {
    Execution(TxVersion),
    Validation(TxVersion),
}

bitflags! {
    pub(crate) struct FinishExecFlags: u8 {
        /// The caller needs to schedule a validation task for this tx.
        const NeedValidation = 0;
        /// This execution wrote to a location not written by any previous incarnation.
        const WroteNewLocation = 1;
    }
}

/// Fast unchecked indexing into a vector of mutexes.
///
/// # Safety
/// The caller must guarantee `$index < $vec.len()`.
macro_rules! index_mutex {
    ($vec:expr, $index:expr) => {
        unsafe { $vec.get_unchecked($index).lock().unwrap() }
    };
}
pub(crate) use index_mutex;

pub mod storage;
pub use storage::{
    AccountBasic, BlockHashes, Bytecodes, ChainState, EvmAccount, EvmCode, Storage,
    StorageWrapper,
};

pub(crate) mod mv_memory;
pub(crate) mod scheduler;
pub(crate) mod vm;
pub(crate) mod pevm;
pub use pevm::{ParallelEvm, PevmError, execute_revm_sequential};
pub use vm::{ExecutionError, PevmTxExecutionResult};

pub mod executor;
pub use executor::{ParallelEthEvmConfig, ParallelEthereumExecutorBuilder};
