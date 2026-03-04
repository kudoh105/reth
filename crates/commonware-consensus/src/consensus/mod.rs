//! Consensus module containing block/digest wrappers, the application actor,
//! and the consensus engine.

pub(crate) mod block;
pub(crate) mod digest;
pub mod engine;
pub mod application;

pub use digest::Digest;
