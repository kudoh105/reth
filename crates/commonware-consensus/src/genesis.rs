//! Genesis-based validator configuration for the private chain.
//!
//! Instead of on-chain DKG contracts (Tempo), validators and epoch parameters
//! are read from the chainspec's `extra_fields`.

use std::sync::Arc;

use reth_chainspec::ChainSpec;

/// Genesis-based configuration for the private validator set.
///
/// These fields are read from `genesis.config.extra_fields` in the chainspec JSON.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivateGenesisInfo {
    /// Number of blocks per epoch (DKG boundary).
    pub epoch_length: Option<u64>,

    /// Static validator set (no on-chain rotation in this ported version).
    pub validators: Option<Vec<ValidatorConfig>>,

    /// Hex-encoded BLS12-381 public polynomial for threshold signing.
    pub public_polynomial: Option<String>,
}

/// Configuration for a single validator in the genesis.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ValidatorConfig {
    /// Hex-encoded ed25519 public key (`"0x..."` format, 32 bytes).
    pub pubkey: String,

    /// Commonware P2P listen address (`"ip:port"` format).
    pub address: String,
}

impl PrivateGenesisInfo {
    /// Reads `PrivateGenesisInfo` from the chainspec's extra fields.
    ///
    /// Returns `Default` if the extra fields are missing or cannot be parsed.
    pub fn from_chain_spec(spec: &ChainSpec) -> Self {
        // Try to deserialize the extra_fields BTreeMap into our struct
        serde_json::from_value(
            serde_json::to_value(&spec.genesis.config.extra_fields).unwrap_or_default(),
        )
        .unwrap_or_default()
    }

    /// Reads from an `Arc<ChainSpec>`.
    pub fn from_chain_spec_arc(spec: &Arc<ChainSpec>) -> Self {
        Self::from_chain_spec(spec.as_ref())
    }

    /// Returns the epoch length, defaulting to 100 if not set.
    pub fn epoch_length(&self) -> u64 {
        self.epoch_length.unwrap_or(100)
    }

    /// Returns the number of validators defined in genesis.
    pub fn num_validators(&self) -> usize {
        self.validators.as_ref().map_or(0, |v| v.len())
    }
}
