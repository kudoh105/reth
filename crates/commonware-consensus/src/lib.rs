//! Commonware BLS12-381 threshold simplex consensus ported for Reth private chain.
//!
//! This crate combines Reth's execution layer with Commonware's consensus engine
//! to create a high-performance private blockchain node.
//!
//! # Architecture
//!
//! The consensus runs in a separate OS thread (Commonware `tokio::Runner`) while
//! communicating with the Reth execution layer through in-process channels:
//!
//! ```text
//! ┌─────────────────────┐    mpsc channels     ┌──────────────────────┐
//! │  Thread 1 (Reth)    │◄────────────────────►│  Thread 2 (CW)       │
//! │  - EthereumNode     │  ConsensusEngineHandle│  - Simplex voting    │
//! │  - MDBX DB          │  PayloadBuilderHandle │  - BLS12-381 DKG     │
//! │  - RPC server       │                      │  - P2P oracle         │
//! │  - NoopNetwork      │                      │  - Marshal/Resolver   │
//! └─────────────────────┘                      └──────────────────────┘
//! ```

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod alias;
pub mod args;
mod config;
pub mod consensus;
mod dkg;
mod epoch;
mod executor;
pub mod genesis;
pub mod key_io;
pub mod node_handle;
mod peer_manager;

pub use args::Args;
pub use node_handle::PrivateNodeHandle;

use commonware_codec::DecodeExt as _;
use commonware_p2p::AddressableManager;
use commonware_runtime::{Clock, Metrics, Network, Pacer, Spawner, Storage};
use eyre::WrapErr as _;
use rand_08::{CryptoRng, Rng};
use tracing::{info, info_span, warn};

/// Runs the complete consensus stack.
///
/// This function is the entry point called from the consensus thread.
/// It initializes the Commonware P2P network, builds the consensus engine,
/// and runs everything to completion.
pub async fn run_consensus_stack(
    ctx: &commonware_runtime::tokio::Context,
    args: Args,
    node: PrivateNodeHandle,
) -> eyre::Result<()> {
    let signing_key = args
        .signing_key()?
        .ok_or_else(|| eyre::eyre!("consensus.signing-key is required for validator mode"))?;

    let signing_share = args
        .signing_share
        .as_ref()
        .map(|path| {
            crate::key_io::SigningShare::read_from_file(path)
                .map(|s| s.into_inner())
                .map_err(|e| eyre::eyre!("{e}"))
        })
        .transpose()
        .wrap_err("failed reading signing share")?;

    let fee_recipient = args.fee_recipient.unwrap_or_default();

    info_span!("consensus_init").in_scope(|| {
        info!(
            identity = %signing_key.public_key(),
            has_share = signing_share.is_some(),
            fee_recipient = %fee_recipient,
            "initializing consensus engine",
        );
    });

    // Initialize the P2P network.
    let p2p_config = commonware_p2p::authenticated::lookup::Config {
        namespace: commonware_utils::union_unique(config::NAMESPACE, b"_P2P"),
        crypto: signing_key.clone().into_inner(),
        listen: args.listen_address,
        mailbox_size: args.mailbox_size,
        max_message_size: args.max_message_size_bytes,
        synchrony_bound: args.synchrony_bound.into_duration(),
        allow_private_ips: args.allow_private_ips,
        allow_dns: args.allow_dns,
        tracked_peer_sets: config::PEERSETS_TO_TRACK,
        allowed_connection_rate_per_peer: commonware_runtime::Quota::with_period(
            args.connection_per_peer_min_period.into_duration(),
        )
        .unwrap(),
        allowed_handshake_rate_per_ip: commonware_runtime::Quota::with_period(
            args.handshake_per_ip_min_period.into_duration(),
        )
        .unwrap(),
        allowed_handshake_rate_per_subnet: commonware_runtime::Quota::with_period(
            args.handshake_per_subnet_min_period.into_duration(),
        )
        .unwrap(),
        max_handshake_age: args.handshake_stale_after.into_duration(),
        handshake_timeout: args.handshake_timeout.into_duration(),
        max_concurrent_handshakes: args.max_concurrent_handshakes,
        block_duration: args.time_to_unblock_byzantine_peer.into_duration(),
        dial_frequency: args.wait_before_peers_redial.into_duration(),
        ping_frequency: args.wait_before_peers_reping.into_duration(),
        query_frequency: args.wait_before_peers_discovery.into_duration(),
        bypass_ip_check: args.bypass_ip_check,
    };

    let (mut network, mut oracle) = commonware_p2p::authenticated::lookup::Network::new(
        ctx.clone().with_label("network"),
        p2p_config,
    );

    // Register authorized peers from genesis config and/or --consensus.known-peers.
    // Without at least one peer set, commonware-p2p rejects all inbound connections.
    //
    // CLI peers are inserted first so that `from_iter_dedup` keeps them when
    // genesis contains the same pubkey with a stale address (e.g. different
    // Docker subnet).
    let mut peer_entries = Vec::new();

    // Source 1 (higher priority): CLI --consensus.known-peers (pubkey@ip:port format).
    for raw in &args.known_peers {
        let Some((pubkey_str, addr_str)) = raw.split_once('@') else {
            warn!(peer = %raw, "skipping known-peer: expected pubkey@ip:port format");
            continue;
        };
        if let Some(entry) = parse_peer_entry(pubkey_str, addr_str) {
            peer_entries.push(entry);
        }
    }
    if !args.known_peers.is_empty() {
        info!(count = args.known_peers.len(), "parsed CLI known peers");
    }

    // Source 2 (lower priority): Genesis validators (from chainspec extra_fields).
    // If a pubkey was already added from CLI, the genesis entry is discarded
    // by `from_iter_dedup`.
    let genesis_info = node.genesis_info();
    if let Some(validators) = &genesis_info.validators {
        for v in validators {
            if let Some(entry) = parse_peer_entry(&v.pubkey, &v.address) {
                peer_entries.push(entry);
            }
        }
        info!(count = validators.len(), "parsed genesis validators");
    }

    if peer_entries.is_empty() {
        warn!("no authorized peers from genesis or CLI; P2P will reject all connections");
    } else {
        let peer_map = commonware_utils::ordered::Map::from_iter_dedup(peer_entries);
        info!(num_peers = peer_map.len(), "registered initial authorized peer set");
        oracle.track(0, peer_map).await;
    }

    let broadcaster_channel = network.register(
        config::BROADCASTER_CHANNEL_IDENT,
        config::BROADCASTER_LIMIT,
        args.message_backlog,
    );
    let marshal_channel = network.register(
        config::MARSHAL_CHANNEL_IDENT,
        config::MARSHAL_LIMIT,
        args.message_backlog,
    );
    let votes_channel =
        network.register(config::VOTES_CHANNEL_IDENT, config::VOTES_LIMIT, args.message_backlog);
    let certificates_channel = network.register(
        config::CERTIFICATES_CHANNEL_IDENT,
        config::CERTIFICATES_LIMIT,
        args.message_backlog,
    );
    let resolver_channel = network.register(
        config::RESOLVER_CHANNEL_IDENT,
        config::RESOLVER_LIMIT,
        args.message_backlog,
    );

    // Build the consensus engine.
    let builder = consensus::engine::Builder {
        fee_recipient,
        execution_node: None,
        blocker: oracle.clone(),
        peer_manager: oracle.clone(),
        partition_prefix: String::from("private-chain"),
        signer: signing_key.into_inner(),
        share: signing_share,
        mailbox_size: args.mailbox_size,
        deque_size: args.deque_size,
        time_to_propose: args.wait_for_proposal.into_duration(),
        time_to_collect_notarizations: args.wait_for_notarizations.into_duration(),
        time_to_retry_nullify_broadcast: args.wait_to_rebroadcast_nullify.into_duration(),
        time_for_peer_response: args.wait_for_peer_response.into_duration(),
        views_to_track: args.views_to_track,
        views_until_leader_skip: args.inactive_views_until_leader_skip,
        new_payload_wait_time: args.time_to_build_proposal.into_duration(),
        fcu_heartbeat_interval: args.fcu_heartbeat_interval.into_duration(),
    };

    let engine = builder
        .with_execution_node(node)
        .try_init(ctx.clone())
        .await
        .wrap_err("failed to initialize consensus engine")?;

    // Start the network and engine.
    let network_handle = network.start();

    // Start block production and consensus components.
    let engine_handle = engine.start(
        broadcaster_channel,
        marshal_channel,
        votes_channel,
        certificates_channel,
        resolver_channel,
    );

    info!("consensus engine started");

    // Wait for the engine or network to complete.
    tokio::select! {
        ret = network_handle => {
            Err(eyre::eyre!("network task failed: {:?}", ret))
        }
        ret = engine_handle => {
            Err(eyre::eyre!("consensus engine task failed: {:?}", ret))
        }
    }
}

/// Parses a pubkey hex string and address string into a `(PublicKey, Address)` pair.
///
/// Accepts pubkeys with or without `0x` prefix. Returns `None` with a warning on parse errors.
fn parse_peer_entry(
    pubkey_str: &str,
    addr_str: &str,
) -> Option<(commonware_cryptography::ed25519::PublicKey, commonware_p2p::Address)> {
    let pubkey_hex = pubkey_str.strip_prefix("0x").unwrap_or(pubkey_str);
    let pubkey_bytes = match const_hex::decode(pubkey_hex) {
        Ok(b) => b,
        Err(e) => {
            warn!(pubkey = %pubkey_str, %e, "skipping peer with invalid pubkey hex");
            return None;
        }
    };
    let pubkey = match commonware_cryptography::ed25519::PublicKey::decode(&pubkey_bytes[..]) {
        Ok(pk) => pk,
        Err(e) => {
            warn!(pubkey = %pubkey_str, ?e, "skipping peer with invalid ed25519 pubkey");
            return None;
        }
    };
    let addr: std::net::SocketAddr = match addr_str.parse() {
        Ok(a) => a,
        Err(e) => {
            warn!(address = %addr_str, %e, "skipping peer with invalid socket address");
            return None;
        }
    };
    Some((pubkey, commonware_p2p::Address::from(addr)))
}
