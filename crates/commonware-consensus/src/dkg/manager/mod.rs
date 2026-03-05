//! Simplified DKG manager for the private chain.
//!
//! Unlike Tempo's full DKG ceremony system (which uses on-chain contracts,
//! `DealerPubMsg`/`DealerPrivMsg` exchanges, and player acknowledgements),
//! this manager reads the static validator set from genesis and generates
//! threshold key material using a deterministic RNG seed.
//!
//! # How it works
//!
//! 1. On initialization, reads validators from the chainspec genesis.
//! 2. Uses `dkg::deal()` with a deterministic seed to generate a shared
//!    polynomial and per-validator shares.
//! 3. If this node has a signing share (index-matched), creates a
//!    `Scheme::signer(...)`, otherwise `Scheme::verifier(...)`.
//! 4. Registers the scheme with `SchemeProvider` for epoch 0.
//! 5. Tells the epoch manager to enter epoch 0.
//! 6. Listens for finalized blocks. At epoch boundaries, registers the same
//!    scheme for the next epoch and tells the epoch manager to transition.

use commonware_codec::DecodeExt as _;
use commonware_consensus::{
    marshal::Update,
    types::{Epoch, Epocher as _, FixedEpocher},
    Reporter,
};
use commonware_cryptography::{
    bls12381::{
        dkg,
        primitives::{
            group::Share,
            sharing::{Mode, Sharing},
            variant::MinSig,
        },
    },
    ed25519::{PrivateKey, PublicKey},
    Signer as _,
};
use commonware_runtime::{spawn_cell, Clock, ContextCell, Handle, Metrics, Spawner, Storage};
use commonware_utils::{ordered, Acknowledgement as _, N3f1};
use eyre::WrapErr as _;
use futures::{channel::mpsc, StreamExt as _};
use rand_08::{rngs::StdRng, SeedableRng};
use tracing::{info, warn};

use crate::{
    config,
    epoch::{self, SchemeProvider},
    genesis::PrivateGenesisInfo,
    node_handle::PrivateNodeHandle,
};

/// Configuration for the static DKG manager.
pub(crate) struct Config {
    /// The signer's private key.
    pub(crate) signer: PrivateKey,

    /// Optional BLS12-381 signing share for threshold signing.
    /// This is used if the node was given a pre-computed share file.
    pub(crate) share: Option<Share>,

    /// The epoch strategy.
    pub(crate) epoch_strategy: FixedEpocher,

    /// Handle to the execution node for reading genesis info.
    pub(crate) execution_node: PrivateNodeHandle,

    /// The epoch manager mailbox for entering/exiting epochs.
    pub(crate) epoch_manager: epoch::manager::Mailbox,

    /// Scheme provider for registering BLS threshold schemes.
    pub(crate) scheme_provider: SchemeProvider,
}

/// Mailbox for the DKG manager actor.
///
/// In the private chain, this is passed to the marshal as a reporter
/// to receive finalized block notifications and track epoch boundaries.
#[derive(Clone, Debug)]
pub(crate) struct Mailbox {
    inner: mpsc::UnboundedSender<Message>,
}

impl Mailbox {
    fn new(inner: mpsc::UnboundedSender<Message>) -> Self {
        Self { inner }
    }
}

impl Reporter for Mailbox {
    type Activity = Update<crate::consensus::block::Block>;

    async fn report(&mut self, activity: Self::Activity) {
        if let Err(error) = self
            .inner
            .unbounded_send(Message::Update(Box::new(activity)))
            .wrap_err("dkg manager no longer running")
        {
            warn!(%error, "failed to report finalization activity to dkg manager");
        }
    }
}

enum Message {
    Update(Box<Update<crate::consensus::block::Block>>),
}

/// The DKG manager actor.
pub(crate) struct Actor<TContext> {
    config: Config,
    context: ContextCell<TContext>,
    mailbox_rx: mpsc::UnboundedReceiver<Message>,
}

impl<TContext> Actor<TContext>
where
    TContext: Clock + Spawner + Metrics + Storage,
{
    pub(crate) fn start(mut self) -> Handle<()> {
        spawn_cell!(self.context, self.run().await)
    }

    async fn run(mut self) {
        // Read genesis info.
        let genesis =
            PrivateGenesisInfo::from_chain_spec_arc(self.config.execution_node.chain_spec());

        let validators = genesis.validators.unwrap_or_default();
        if validators.is_empty() {
            warn!("no validators found in genesis; DKG manager cannot initialize");
            return;
        }

        // Parse validator public keys into an ordered set.
        let mut parsed_keys: Vec<PublicKey> = Vec::new();
        for v in &validators {
            // Strip "0x" prefix if present
            let hex_str = v.pubkey.strip_prefix("0x").unwrap_or(&v.pubkey);
            match const_hex::decode(hex_str) {
                Ok(bytes) => match PublicKey::decode(bytes.as_slice()) {
                    Ok(pk) => parsed_keys.push(pk),
                    Err(e) => {
                        warn!(pubkey = %v.pubkey, "failed to decode ed25519 public key: {e}");
                    }
                },
                Err(e) => {
                    warn!(pubkey = %v.pubkey, "failed to decode hex: {e}");
                }
            }
        }

        if parsed_keys.is_empty() {
            warn!("failed to parse any validator public keys from genesis");
            return;
        }

        let participants: ordered::Set<PublicKey> =
            ordered::Set::from_iter_dedup(parsed_keys.clone());

        info!(
            num_validators = participants.len(),
            identity = %self.config.signer.public_key(),
            "DKG manager initialized with genesis validators",
        );

        // Generate deterministic threshold key material using dkg::deal().
        //
        // All nodes use the same deterministic seed (derived from NAMESPACE),
        // so they all arrive at the same public polynomial. Each node picks
        // its own share based on its position in the ordered participant set.
        let seed: [u8; 32] = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            config::NAMESPACE.hash(&mut hasher);
            let h = hasher.finish();
            let mut s = [0u8; 32];
            s[..8].copy_from_slice(&h.to_le_bytes());
            // Use a secondary hash for more entropy
            b"PRIVATE_CHAIN_DKG_SEED".hash(&mut hasher);
            let h2 = hasher.finish();
            s[8..16].copy_from_slice(&h2.to_le_bytes());
            s
        };
        let mut rng = StdRng::from_seed(seed);

        let deal_result =
            dkg::deal::<MinSig, _, N3f1>(&mut rng, Mode::default(), participants.clone());

        let (output, shares) = match deal_result {
            Ok(result) => result,
            Err(e) => {
                warn!("failed to generate DKG key material: {:?}", e);
                return;
            }
        };

        let polynomial = output.public().clone();

        // Find our share (if we're a validator).
        let my_pubkey = self.config.signer.public_key();
        let my_share = if let Some(pre_share) = self.config.share.clone() {
            // Use the pre-configured share (from --signing-share file).
            Some(pre_share)
        } else {
            // Find our share from the deal result.
            shares.get_value(&my_pubkey).cloned()
        };

        let initial_epoch = Epoch::new(0);

        info!(
            epoch = %initial_epoch,
            is_signer = my_share.is_some(),
            polynomial_threshold = polynomial.total().get(),
            "registering BLS12-381 threshold scheme",
        );

        // Tell the epoch manager to enter epoch 0 with the generated polynomial.
        if let Err(e) = self.config.epoch_manager.enter(
            initial_epoch,
            polynomial.clone(),
            my_share.clone(),
            participants.clone(),
        ) {
            warn!(%e, "failed to instruct epoch manager to enter epoch 0");
            return;
        }

        info!("instructed epoch manager to enter epoch 0");

        // Inform the peer manager about the participants so that P2P tracking is active.
        self.config.peer_manager.track(initial_epoch, participants.clone());
        info!("instructed peer manager to track peers for epoch 0");

        // Main loop: listen for finalized blocks and manage epoch transitions.
        let mut current_epoch = initial_epoch;

        while let Some(msg) = self.mailbox_rx.next().await {
            match msg {
                Message::Update(update) => {
                    match *update {
                        Update::Tip(_, height, _) => {
                            // Check if we've reached an epoch boundary.
                            if let Some(epoch_info) = self.config.epoch_strategy.containing(height)
                            {
                                let block_epoch = epoch_info.epoch();

                                if block_epoch > current_epoch {
                                    let next_epoch = current_epoch.next();

                                    info!(
                                        from = %current_epoch,
                                        to = %next_epoch,
                                        at_height = %height,
                                        "epoch boundary reached; transitioning",
                                    );

                                    // Enter the new epoch with the same polynomial.
                                    if let Err(e) = self.config.epoch_manager.enter(
                                        next_epoch,
                                        polynomial.clone(),
                                        my_share.clone(),
                                        participants.clone(),
                                    ) {
                                        warn!(%e, epoch = %next_epoch, "failed to enter new epoch");
                                    }

                                    // Exit the old epoch.
                                    if let Err(e) = self.config.epoch_manager.exit(current_epoch) {
                                        warn!(%e, epoch = %current_epoch, "failed to exit old epoch");
                                    }

                                    current_epoch = next_epoch;
                                }
                            }
                        }
                        Update::Block(_, ack) => {
                            // Acknowledge each finalized block.
                            ack.acknowledge();
                        }
                    }
                }
            }
        }

        info!("DKG manager exiting");
    }
}

/// Initialize the DKG manager, returning the actor and its mailbox.
pub(crate) fn init<TContext>(context: TContext, config: Config) -> (Actor<TContext>, Mailbox)
where
    TContext: Clock + Spawner + Metrics + Storage,
{
    let (tx, rx) = mpsc::unbounded();
    let actor = Actor { config, context: ContextCell::new(context), mailbox_rx: rx };
    let mailbox = Mailbox::new(tx);
    (actor, mailbox)
}
