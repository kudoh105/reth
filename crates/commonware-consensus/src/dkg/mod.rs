//! DKG manager stub.
//!
//! In the private chain, the validator set is static from genesis.
//! The DKG manager reads the initial share and public polynomial from
//! genesis extra_fields instead of running on-chain DKG ceremonies.

// TODO: Full port of DKG manager from Tempo with genesis-based initialization.
// The DKG manager:
// 1. On init, reads `public_polynomial` and `validators` from genesis extra_fields
// 2. Constructs a Scheme<PublicKey, MinSig> for epoch 0
// 3. Registers the scheme with SchemeProvider
// 4. For subsequent epochs, re-uses the same scheme (no on-chain rotation)

pub(crate) mod manager {
    // Placeholder types for the DKG manager.
    // Full implementation will read from genesis and register schemes.
}
