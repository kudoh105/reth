//! Command line arguments for configuring the consensus layer of a private node.
//!
//! Ported from Tempo with subblock-related args removed.

use std::{
    net::SocketAddr, num::NonZeroU32, path::PathBuf, str::FromStr, sync::OnceLock, time::Duration,
};

use commonware_cryptography::ed25519::PublicKey;
use eyre::Context;

use crate::key_io::SigningKey;

const DEFAULT_MAX_MESSAGE_SIZE_BYTES: u32 =
    reth_consensus_common::validation::MAX_RLP_BLOCK_SIZE as u32;

/// Command line arguments for configuring the consensus layer of a private node.
#[derive(Debug, Clone, clap::Args)]
pub struct Args {
    /// The file containing the ed25519 signing key for p2p communication.
    #[arg(long = "consensus.signing-key")]
    signing_key: Option<PathBuf>,

    /// The file containing a share of the bls12-381 threshold signing key.
    #[arg(long = "consensus.signing-share")]
    pub signing_share: Option<PathBuf>,

    /// The socket address for consensus P2P communication.
    #[arg(long = "consensus.listen-address", default_value = "127.0.0.1:8000")]
    pub listen_address: SocketAddr,

    /// The socket address for consensus metrics.
    #[arg(long = "consensus.metrics-address", default_value = "127.0.0.1:8001")]
    pub metrics_address: SocketAddr,

    #[arg(long = "consensus.max-message-size-bytes", default_value_t = DEFAULT_MAX_MESSAGE_SIZE_BYTES)]
    pub max_message_size_bytes: u32,

    /// The number of worker threads assigned to consensus.
    #[arg(long = "consensus.worker-threads", default_value_t = 3)]
    pub worker_threads: usize,

    /// Max messages queued on consensus channels before blocking.
    #[arg(long = "consensus.message-backlog", default_value_t = 16_384)]
    pub message_backlog: usize,

    /// Max items on consensus channels before blocking.
    #[arg(long = "consensus.mailbox-size", default_value_t = 16_384)]
    pub mailbox_size: usize,

    /// Max blocks buffered per peer.
    #[arg(long = "consensus.deque-size", default_value_t = 10)]
    pub deque_size: usize,

    /// The fee recipient for built blocks.
    #[arg(long = "consensus.fee-recipient")]
    pub fee_recipient: Option<alloy_primitives::Address>,

    /// Time to wait for a peer response.
    #[arg(long = "consensus.wait-for-peer-response", default_value = "2s")]
    pub wait_for_peer_response: PositiveDuration,

    /// Time to wait for quorum of notarizations before skipping.
    #[arg(long = "consensus.wait-for-notarizations", default_value = "2s")]
    pub wait_for_notarizations: PositiveDuration,

    /// Time to wait for a proposal from the current view leader.
    #[arg(long = "consensus.wait-for-proposal", default_value = "2s")]
    pub wait_for_proposal: PositiveDuration,

    /// Time to wait before retrying a nullify broadcast.
    #[arg(long = "consensus.wait-to-rebroadcast-nullify", default_value = "10s")]
    pub wait_to_rebroadcast_nullify: PositiveDuration,

    /// Number of views to track (activity timeout).
    #[arg(long = "consensus.views-to-track", default_value_t = 256)]
    pub views_to_track: u64,

    /// Views a validator can be inactive before leader-skip.
    #[arg(long = "consensus.inactive-views-until-leader-skip", default_value_t = 32)]
    pub inactive_views_until_leader_skip: u64,

    /// Time to build a proposal block.
    #[arg(long = "consensus.time-to-build-proposal", default_value = "500ms")]
    pub time_to_build_proposal: PositiveDuration,

    /// Use defaults optimized for local networks.
    #[arg(long = "consensus.use-local-defaults", default_value_t = false)]
    pub use_local_defaults: bool,

    /// Disable IP-based connection filtering.
    #[arg(long = "consensus.bypass-ip-check", default_value_t = false)]
    pub bypass_ip_check: bool,

    /// Allow connections with private IPs.
    #[arg(
        long = "consensus.allow-private-ips",
        default_value_t = false,
        default_value_if("use_local_defaults", "true", "true")
    )]
    pub allow_private_ips: bool,

    /// Allow DNS-based ingress addresses.
    #[arg(long = "consensus.allow-dns", default_value_t = true)]
    pub allow_dns: bool,

    /// Synchrony bound for timestamps.
    #[arg(long = "consensus.synchrony-bound", default_value = "5s")]
    pub synchrony_bound: PositiveDuration,

    /// Time before redialing peers.
    #[arg(
        long = "consensus.wait-before-peers-redial",
        default_value = "1s",
        default_value_if("use_local_defaults", "true", "500ms")
    )]
    pub wait_before_peers_redial: PositiveDuration,

    /// Time before re-pinging peers.
    #[arg(
        long = "consensus.wait-before-peers-reping",
        default_value = "50s",
        default_value_if("use_local_defaults", "true", "5s")
    )]
    pub wait_before_peers_reping: PositiveDuration,

    /// Time between peer discovery queries.
    #[arg(
        long = "consensus.wait-before-peers-discovery",
        default_value = "60s",
        default_value_if("use_local_defaults", "true", "30s")
    )]
    pub wait_before_peers_discovery: PositiveDuration,

    /// Minimum time between connection attempts to the same peer.
    #[arg(
        long = "consensus.connection-per-peer-min-period",
        default_value = "60s",
        default_value_if("use_local_defaults", "true", "1s")
    )]
    pub connection_per_peer_min_period: PositiveDuration,

    /// Minimum time between handshakes from one IP.
    #[arg(
        long = "consensus.handshake-per-ip-min-period",
        default_value = "5s",
        default_value_if("use_local_defaults", "true", "62ms")
    )]
    pub handshake_per_ip_min_period: PositiveDuration,

    /// Minimum time between handshakes from one subnet.
    #[arg(
        long = "consensus.handshake-per-subnet-min-period",
        default_value = "15ms",
        default_value_if("use_local_defaults", "true", "7ms")
    )]
    pub handshake_per_subnet_min_period: PositiveDuration,

    /// Handshake stale timeout.
    #[arg(long = "consensus.handshake-stale-after", default_value = "10s")]
    pub handshake_stale_after: PositiveDuration,

    /// Handshake timeout.
    #[arg(long = "consensus.handshake-timeout", default_value = "5s")]
    pub handshake_timeout: PositiveDuration,

    /// Max concurrent handshakes.
    #[arg(
        long = "consensus.max-concurrent-handshakes",
        default_value = "512",
        default_value_if("use_local_defaults", "true", "1024")
    )]
    pub max_concurrent_handshakes: NonZeroU32,

    /// Time before a blocked peer can reconnect.
    #[arg(
        long = "consensus.time-to-unblock-byzantine-peer",
        default_value = "4h",
        default_value_if("use_local_defaults", "true", "1h")
    )]
    pub time_to_unblock_byzantine_peer: PositiveDuration,

    /// Rate limit for backfilling (requests/second).
    #[arg(long = "consensus.backfill-frequency", default_value = "8")]
    pub backfill_frequency: std::num::NonZeroU32,

    /// FCU heartbeat interval.
    #[arg(long = "consensus.fcu-heartbeat-interval", default_value = "5m")]
    pub fcu_heartbeat_interval: PositiveDuration,

    /// Comma-separated list of known peers in `pubkey@ip:port` format.
    ///
    /// Each entry registers a validator for P2P authorization. Without known
    /// peers (and without genesis validators), the P2P layer will reject all
    /// inbound connections.
    ///
    /// Example: `0xaabb...@172.20.0.11:8000,0xccdd...@172.20.0.12:8000`
    #[arg(long = "consensus.known-peers", value_delimiter = ',')]
    pub known_peers: Vec<String>,

    /// Cache for the signing key.
    #[clap(skip)]
    loaded_signing_key: OnceLock<Option<SigningKey>>,

    /// Where to store consensus data.
    #[arg(long = "consensus.datadir", value_name = "PATH")]
    pub storage_dir: Option<PathBuf>,
}

/// A jiff::SignedDuration that checks that the duration is positive and not zero.
#[derive(Debug, Clone, Copy)]
pub struct PositiveDuration(jiff::SignedDuration);

impl PositiveDuration {
    /// Converts to a `std::time::Duration`.
    pub fn into_duration(self) -> Duration {
        self.0
            .try_into()
            .expect("must be positive. enforced when cli parsing.")
    }
}

impl FromStr for PositiveDuration {
    type Err = Box<dyn std::error::Error + Send + Sync + 'static>;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let duration = s.parse::<jiff::SignedDuration>()?;
        let _: Duration = duration.try_into().wrap_err("duration must be positive")?;
        Ok(Self(duration))
    }
}

impl Args {
    /// Returns the signing key loaded from the specified file.
    pub fn signing_key(&self) -> eyre::Result<Option<SigningKey>> {
        if let Some(signing_key) = self.loaded_signing_key.get() {
            return Ok(signing_key.clone());
        }

        let signing_key = self
            .signing_key
            .as_ref()
            .map(|path| {
                SigningKey::read_from_file(path).wrap_err_with(|| {
                    format!(
                        "failed reading private ed25519 signing key from file `{}`",
                        path.display()
                    )
                })
            })
            .transpose()?;

        let _ = self.loaded_signing_key.set(signing_key.clone());

        Ok(signing_key)
    }

    /// Returns the public key derived from the configured signing key.
    pub fn public_key(&self) -> eyre::Result<Option<PublicKey>> {
        Ok(self
            .signing_key()?
            .map(|signing_key| signing_key.public_key()))
    }
}
