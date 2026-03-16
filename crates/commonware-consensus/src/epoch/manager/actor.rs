//! Actor implementing the epoch manager logic.
//!
//! Ported from Tempo with simplifications for the private chain:
//! - No subblocks support
//! - No feed actor
//! - No catch-up via vote channel backup (simplified)
//!
//! This actor is responsible for:
//! 1. Entering and exiting epochs on instruction from the DKG manager.
//! 2. Spinning up a new simplex consensus engine for each epoch.

use std::{collections::BTreeMap, num::NonZeroUsize};

use commonware_consensus::{
    marshal::Update,
    simplex::{self, elector, scheme::bls12381_threshold::vrf::Scheme},
    types::{Epoch, Epocher as _, Height},
    Reporters,
};
use commonware_cryptography::ed25519::PublicKey;
use commonware_p2p::{
    utils::mux::{Builder as _, MuxHandle, Muxer},
    Blocker, Receiver, Sender,
};
use commonware_parallel::Sequential;
use commonware_runtime::{
    spawn_cell, Clock, ContextCell, Handle, Metrics as _, Network, Spawner, Storage,
};
use commonware_utils::Acknowledgement as _;
use eyre::{ensure, eyre, WrapErr as _};
use futures::{channel::mpsc, StreamExt as _};
use prometheus_client::metrics::{counter::Counter, gauge::Gauge};
use rand_08::{CryptoRng, Rng};
use tracing::{debug, error, error_span, info, instrument, warn, warn_span, Level, Span};

use crate::epoch::manager::ingress::{EpochTransition, Exit};

use super::ingress::{Content, Message};

const REPLAY_BUFFER: NonZeroUsize = NonZeroUsize::new(8 * 1024 * 1024).expect("value is not zero"); // 8MB
const WRITE_BUFFER: NonZeroUsize = NonZeroUsize::new(1024 * 1024).expect("value is not zero"); // 1MB

pub(crate) struct Actor<TContext, TBlocker> {
    active_epochs: BTreeMap<Epoch, Handle<()>>,
    config: super::Config<TBlocker>,
    context: ContextCell<TContext>,
    mailbox: mpsc::UnboundedReceiver<Message>,
    metrics: Metrics,
}

impl<TContext, TBlocker> Actor<TContext, TBlocker>
where
    TBlocker: Blocker<PublicKey = PublicKey>,
    TContext: commonware_runtime::BufferPooler
        + Spawner
        + commonware_runtime::Metrics
        + Rng
        + CryptoRng
        + Clock
        + governor::clock::Clock
        + Storage
        + Network,
{
    pub(super) fn new(
        config: super::Config<TBlocker>,
        context: TContext,
        mailbox: mpsc::UnboundedReceiver<Message>,
    ) -> Self {
        let active_epochs = Gauge::default();
        let latest_epoch = Gauge::default();
        let latest_participants = Gauge::default();
        let how_often_signer = Counter::default();
        let how_often_verifier = Counter::default();

        context.register(
            "active_epochs",
            "the number of epochs currently managed by the epoch manager",
            active_epochs.clone(),
        );
        context.register(
            "latest_epoch",
            "the latest epoch managed by this epoch manager",
            latest_epoch.clone(),
        );
        context.register(
            "latest_participants",
            "the number of participants in the most recently started epoch",
            latest_participants.clone(),
        );
        context.register(
            "how_often_signer",
            "how often a node is a signer; a node is a signer if it has a share",
            how_often_signer.clone(),
        );
        context.register(
            "how_often_verifier",
            "how often a node is a verifier; a node is a verifier if it does not have a share",
            how_often_verifier.clone(),
        );

        Self {
            config,
            context: ContextCell::new(context),
            mailbox,
            metrics: Metrics {
                active_epochs,
                latest_epoch,
                latest_participants,
                how_often_signer,
                how_often_verifier,
            },
            active_epochs: BTreeMap::new(),
        }
    }

    pub(crate) fn start(
        mut self,
        votes: (impl Sender<PublicKey = PublicKey>, impl Receiver<PublicKey = PublicKey>),
        certificates: (impl Sender<PublicKey = PublicKey>, impl Receiver<PublicKey = PublicKey>),
        resolver: (impl Sender<PublicKey = PublicKey>, impl Receiver<PublicKey = PublicKey>),
    ) -> Handle<()> {
        spawn_cell!(self.context, self.run(votes, certificates, resolver).await)
    }

    async fn run(
        mut self,
        (vote_sender, vote_receiver): (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
        (certificate_sender, certificate_receiver): (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
        (resolver_sender, resolver_receiver): (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
    ) {
        let (mux, mut vote_mux) = Muxer::new(
            self.context.with_label("vote_mux"),
            vote_sender,
            vote_receiver,
            self.config.mailbox_size,
        );
        mux.start();

        let (mux, mut certificate_mux) = Muxer::new(
            self.context.with_label("certificate_mux"),
            certificate_sender,
            certificate_receiver,
            self.config.mailbox_size,
        );
        mux.start();

        let (mux, mut resolver_mux) = Muxer::new(
            self.context.with_label("resolver_mux"),
            resolver_sender,
            resolver_receiver,
            self.config.mailbox_size,
        );
        mux.start();

        loop {
            let msg = self.mailbox.next().await;
            let Some(msg) = msg else {
                warn_span!("mailboxes dropped")
                    .in_scope(|| warn!("all mailboxes dropped; exiting actor"));
                break;
            };
            let cause = msg.cause;
            match msg.content {
                Content::Enter(enter) => {
                    let _: Result<_, _> = self
                        .enter(cause, enter, &mut vote_mux, &mut certificate_mux, &mut resolver_mux)
                        .await;
                }
                Content::Exit(exit) => self.exit(cause, exit),
                Content::Update(update) => {
                    match *update {
                        Update::Tip(_, _height, _digest) => {
                            // In the private chain, epoch transitions are driven
                            // by the DKG manager, so we just acknowledge tips.
                        }
                        Update::Block(_block, ack) => {
                            ack.acknowledge();
                        }
                    }
                }
            }
        }
    }

    #[instrument(
        parent = &cause,
        skip_all,
        fields(
            %epoch,
            ?participants,
        ),
        err(level = Level::WARN)
    )]
    async fn enter(
        &mut self,
        cause: Span,
        EpochTransition { epoch, public, share, participants }: EpochTransition,
        vote_mux: &mut MuxHandle<
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        >,
        certificates_mux: &mut MuxHandle<
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        >,
        resolver_mux: &mut MuxHandle<
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        >,
    ) -> eyre::Result<()> {
        if let Some(latest) = self.active_epochs.last_key_value().map(|(k, _)| *k) {
            ensure!(
                epoch > latest,
                "requested to start an epoch `{epoch}` older than the latest \
                running, `{latest}`; refusing",
            );
        }

        let n_participants = participants.len();
        // Register the new signing scheme.
        let is_signer = matches!(share, Some(..));
        let scheme = if let Some(share) = share {
            info!("we have a share for this epoch, participating as a signer");
            Scheme::signer(crate::config::NAMESPACE, participants, public, share)
                .ok_or_else(|| eyre!(
                    "BLS share does not match participant list — \
                     check that the signing share file matches the genesis validator set"
                ))?
        } else {
            info!("we don't have a share for this epoch, participating as a verifier");
            Scheme::verifier(crate::config::NAMESPACE, participants, public)
        };
        self.config.scheme_provider.register(epoch, scheme.clone());

        let engine = simplex::Engine::new(
            self.context.with_label("simplex").with_attribute("epoch", epoch),
            simplex::Config {
                scheme,
                elector: elector::Random,
                blocker: self.config.blocker.clone(),
                automaton: self.config.application.clone(),
                relay: self.config.application.clone(),
                reporter: self.config.marshal.clone(),
                partition: format!(
                    "{partition_prefix}_consensus_epoch_{epoch}",
                    partition_prefix = self.config.partition_prefix
                ),
                mailbox_size: self.config.mailbox_size,
                epoch,

                replay_buffer: REPLAY_BUFFER,
                write_buffer: WRITE_BUFFER,
                page_cache: self.config.page_cache.clone(),

                leader_timeout: self.config.time_to_propose,
                certification_timeout: self.config.time_to_collect_notarizations,
                timeout_retry: self.config.time_to_retry_nullify_broadcast,
                fetch_timeout: self.config.time_for_peer_response,
                activity_timeout: self.config.views_to_track,
                skip_timeout: self.config.views_until_leader_skip,

                fetch_concurrent: crate::config::NUMBER_CONCURRENT_FETCHES,

                strategy: Sequential,
            },
        );

        let vote = vote_mux.register(epoch.get()).await
            .wrap_err("failed to register vote mux channel — P2P network may have closed")?;
        let certificate = certificates_mux.register(epoch.get()).await
            .wrap_err("failed to register certificate mux channel — P2P network may have closed")?;
        let resolver = resolver_mux.register(epoch.get()).await
            .wrap_err("failed to register resolver mux channel — P2P network may have closed")?;

        assert!(
            self.active_epochs.insert(epoch, engine.start(vote, certificate, resolver)).is_none(),
            "there must be no other active engine running: this was ensured at \
            the beginning of this method",
        );

        info!("started consensus engine backing the epoch");

        self.metrics.latest_participants.set(n_participants as i64);
        self.metrics.active_epochs.inc();
        self.metrics.how_often_signer.inc_by(is_signer as u64);
        self.metrics.how_often_verifier.inc_by(!is_signer as u64);

        Ok(())
    }

    #[instrument(parent = &cause, skip_all, fields(epoch))]
    fn exit(&mut self, cause: Span, Exit { epoch }: Exit) {
        if let Some(engine) = self.active_epochs.remove(&epoch) {
            engine.abort();
            info!("stopped engine backing epoch");
        } else {
            warn!(
                "attempted to exit unknown epoch, but epoch was not backed by \
                an active engine",
            );
        }

        if !self.config.scheme_provider.delete(&epoch) {
            warn!(
                "attempted to delete scheme for epoch, but epoch had no scheme \
                registered"
            );
        }
    }
}

struct Metrics {
    active_epochs: Gauge,
    latest_epoch: Gauge,
    latest_participants: Gauge,
    how_often_signer: Counter,
    how_often_verifier: Counter,
}
