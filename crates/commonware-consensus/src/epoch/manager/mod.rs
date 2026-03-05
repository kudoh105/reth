//! Epoch manager for the private chain.
//!
//! Ported from Tempo's epoch manager with simplifications:
//! - No subblocks support
//! - No feed actor
//! - Static validator set (no on-chain validator rotation)
//!
//! The epoch manager is responsible for:
//! 1. Entering and exiting epochs on instruction from the DKG manager.
//! 2. Spinning up a new simplex consensus engine for each epoch.
//! 3. Handling votes from unknown epochs (catch-up).

pub(crate) mod actor;
pub(super) mod ingress;

use std::time::Duration;

pub(crate) use actor::Actor;
use commonware_cryptography::{bls12381::primitives::variant::MinSig, ed25519::PublicKey};
pub(crate) use ingress::Mailbox;

use commonware_consensus::{
    marshal,
    simplex::scheme::bls12381_threshold::vrf::Scheme,
    types::{FixedEpocher, ViewDelta},
};
use commonware_p2p::Blocker;
use commonware_runtime::{buffer::paged::CacheRef, Clock, Metrics, Network, Spawner, Storage};
use rand_08::{CryptoRng, Rng};

use crate::{consensus::block::Block, epoch::scheme_provider::SchemeProvider};

pub(crate) struct Config<TBlocker> {
    pub(crate) application: crate::consensus::application::Mailbox,
    pub(crate) blocker: TBlocker,
    pub(crate) page_cache: CacheRef,
    pub(crate) epoch_strategy: FixedEpocher,
    pub(crate) time_for_peer_response: Duration,
    pub(crate) time_to_propose: Duration,
    pub(crate) mailbox_size: usize,
    pub(crate) marshal: marshal::Mailbox<Scheme<PublicKey, MinSig>, Block>,
    pub(crate) scheme_provider: SchemeProvider,
    pub(crate) time_to_collect_notarizations: Duration,
    pub(crate) time_to_retry_nullify_broadcast: Duration,
    pub(crate) partition_prefix: String,
    pub(crate) views_to_track: ViewDelta,
    pub(crate) views_until_leader_skip: ViewDelta,
}

pub(crate) fn init<TContext, TBlocker>(
    context: TContext,
    config: Config<TBlocker>,
) -> (Actor<TContext, TBlocker>, Mailbox)
where
    TBlocker: Blocker<PublicKey = PublicKey>,
    TContext:
        Spawner + Metrics + Rng + CryptoRng + Clock + governor::clock::Clock + Storage + Network,
{
    let (tx, rx) = futures::channel::mpsc::unbounded();
    let actor = Actor::new(config, context, rx);
    let mailbox = Mailbox::new(tx);
    (actor, mailbox)
}
