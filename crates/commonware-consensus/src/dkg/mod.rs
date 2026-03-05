//! DKG manager for the private chain.
//!
//! In Tempo, the DKG manager runs full BLS12-381 distributed key generation
//! ceremonies on-chain. For the private chain, we use a simplified approach:
//!
//! 1. The initial BLS12-381 public polynomial and validator set are read from
//!    the chainspec's genesis `extra_fields`.
//! 2. A `Scheme::verifier(...)` is constructed for epoch 0 and registered with
//!    the `SchemeProvider`.
//! 3. At each epoch boundary, the DKG manager re-registers the same scheme for
//!    the next epoch (no key rotation).
//! 4. The DKG manager notifies the epoch manager to enter/exit epochs.

pub(crate) mod manager;
