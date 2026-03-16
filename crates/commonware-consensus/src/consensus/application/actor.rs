//! Application actor for proposing and verifying blocks.
//!
//! Ported from Tempo with key changes:
//! - `TempoPayloadBuilderAttributes` → `EthPayloadBuilderAttributes`
//! - `extra_data` is always `Bytes::default()` (no DKG data in blocks)
//! - `subblocks` removed entirely
//! - `TempoFullNode` → `PrivateNodeHandle`

use std::time::Duration;

use alloy_primitives::B256;
use commonware_consensus::{
    types::{Epoch, Epocher as _, Height, Round, View},
    Heightable as _,
};
use commonware_runtime::{
    spawn_cell, Clock, ContextCell, FutureExt, Handle, Metrics, Pacer, Spawner, Storage,
};
use commonware_utils::SystemTimeExt;
use eyre::{OptionExt as _, WrapErr as _};
use futures::{channel::mpsc, StreamExt as _};
use rand_08::{CryptoRng, Rng};
use reth_payload_primitives::{EngineApiMessageVersion, PayloadKind};
use tracing::{info, info_span, warn};

use crate::{
    consensus::{block::Block, Digest},
    epoch::SchemeProvider,
    node_handle::PrivateNodeHandle,
};

use super::ingress::{Broadcast, Genesis, Mailbox, Message, Propose, Verify};

struct State {
    execution_node: PrivateNodeHandle,
    executor: crate::executor::Mailbox,
    marshal: crate::alias::marshal::Mailbox,
    scheme_provider: SchemeProvider,
}

pub(crate) struct Actor<TContext> {
    context: ContextCell<TContext>,
    state: State,
    fee_recipient: alloy_primitives::Address,
    new_payload_wait_time: Duration,
    epoch_strategy: commonware_consensus::types::FixedEpocher,
    mailbox: mpsc::Receiver<Message>,
    mailbox_sender: Mailbox,
}

impl<TContext> Actor<TContext>
where
    TContext: Pacer + governor::clock::Clock + Rng + CryptoRng + Spawner + Storage + Metrics,
{
    pub(super) async fn init(config: super::Config<TContext>) -> eyre::Result<Self> {
        let super::Config {
            context,
            fee_recipient,
            mailbox_size,
            marshal,
            executor,
            execution_node,
            new_payload_wait_time,
            epoch_strategy,
            scheme_provider,
        } = config;

        let (tx, rx) = mpsc::channel(mailbox_size);
        let mailbox_sender = Mailbox::from_sender(tx);

        Ok(Self {
            context: ContextCell::new(context),
            state: State { execution_node, executor, marshal, scheme_provider },
            fee_recipient,
            new_payload_wait_time,
            epoch_strategy,
            mailbox: rx,
            mailbox_sender,
        })
    }

    pub(super) fn mailbox(&self) -> &Mailbox {
        &self.mailbox_sender
    }

    pub(crate) fn start(mut self) -> Handle<()> {
        spawn_cell!(self.context, self.run().await)
    }

    async fn run(mut self) {
        while let Some(msg) = self.mailbox.next().await {
            match msg {
                Message::Genesis(Genesis { epoch, response }) => {
                    let digest = self.handle_genesis(epoch).await;
                    let _ = response.send(digest);
                }
                Message::Propose(Propose { parent, response, round }) => {
                    let result = self.handle_propose(parent, round).await;
                    match result {
                        Ok(digest) => {
                            let _ = response.send(digest);
                        }
                        Err(e) => {
                            warn!("failed to propose block: {e:?}");
                        }
                    }
                }
                Message::Verify(verify) => {
                    if let Err(e) = self.handle_verify(*verify).await {
                        warn!("failed to verify block: {e:?}");
                    }
                }
                Message::Broadcast(Broadcast { payload }) => {
                    self.handle_broadcast(payload).await;
                }
            }
        }
        info_span!("application").in_scope(|| info!("mailbox closed, actor shutting down"));
    }

    async fn handle_genesis(&mut self, epoch: Epoch) -> Digest {
        if epoch.get() == 0 {
            // Epoch 0 builds on the EL genesis block.
            let genesis_hash =
                self.state.execution_node.block_hash(0).ok().flatten().unwrap_or(B256::ZERO);
            Digest(genesis_hash)
        } else {
            // Epoch N > 0 must build on the epoch-boundary block: the first block whose
            // height belongs to epoch N. Epoch N's proposals extend the chain from this
            // block, producing new EL blocks at heights (boundary_height + 1, ...).
            //
            // We read the block from the marshal's finalized-block archive rather than
            // querying the EL provider directly.  The marshal guarantees that the block
            // is stored *before* it sends the Update::Tip that triggers the epoch
            // transition, so the block is always present by the time this function runs.
            let boundary_height = self
                .epoch_strategy
                .first(epoch)
                .map(|h| h.get())
                .unwrap_or_else(|| epoch.get().saturating_mul(100));

            let hash = self
                .state
                .marshal
                .get_block(Height::new(boundary_height))
                .await
                .map(|b| b.block_hash())
                .unwrap_or_else(|| {
                    warn!(
                        boundary_height,
                        "epoch genesis: boundary block not in marshal; using B256::ZERO",
                    );
                    B256::ZERO
                });

            Digest(hash)
        }
    }

    async fn handle_propose(
        &mut self,
        parent: (View, Digest),
        round: Round,
    ) -> eyre::Result<Digest> {
        let (parent_view, parent_digest) = parent;

        info!(%round, %parent_view, %parent_digest, "handle_propose: subscribing to parent");

        // Retrieve the parent block from the marshal.
        let parent = self
            .state
            .marshal
            .subscribe_by_digest(
                Some(commonware_consensus::types::Round::new(round.epoch(), parent_view)),
                parent_digest,
            )
            .await
            .await
            .map_err(|_| eyre::eyre!("failed resolving parent block"))?;

        info!(%round, "handle_propose: parent resolved, sending FCU");

        let parent_hash = parent.block_hash();
        let timestamp = self.context.current().epoch_millis() / 1000;

        // Build standard Ethereum payload attributes.
        let payload_attributes = alloy_rpc_types_engine::PayloadAttributes {
            timestamp,
            prev_randao: B256::ZERO, // No beacon randomness in private chain
            suggested_fee_recipient: self.fee_recipient,
            withdrawals: Some(vec![]),
            parent_beacon_block_root: Some(B256::ZERO),
        };

        // Send fork_choice_updated with payload attributes to trigger build.
        let forkchoice_state = alloy_rpc_types_engine::ForkchoiceState {
            head_block_hash: parent_hash,
            safe_block_hash: parent_hash,
            finalized_block_hash: parent_hash,
        };

        let fcu_response = self
            .state
            .execution_node
            .beacon_engine_handle()
            .fork_choice_updated(
                forkchoice_state,
                Some(payload_attributes),
                EngineApiMessageVersion::V3,
            )
            .pace(&self.context, Duration::from_millis(20))
            .await
            .wrap_err("failed sending FCU with payload attributes")?;

        info!(
            %round,
            head_block_hash = %parent_hash,
            %timestamp,
            payload_status = %fcu_response.payload_status,
            payload_id = ?fcu_response.payload_id,
            "FCU response",
        );

        let payload_id =
            fcu_response.payload_id.ok_or_eyre("execution layer did not return payload ID")?;

        // Wait for the payload to be built.
        self.context.sleep(self.new_payload_wait_time).await;

        // Resolve the built payload.
        let resolved = self
            .state
            .execution_node
            .payload_builder_handle()
            .resolve_kind(payload_id, PayloadKind::WaitForPending)
            .await
            .ok_or_eyre("payload builder returned None for payload")?
            .wrap_err("payload builder failed to build payload")?;

        let sealed_block = resolved.block().clone();
        let block = Block::from_execution_block(sealed_block);
        let digest = block.digest();

        info!(
            %payload_id,
            %digest,
            "payload resolved successfully",
        );

        // Send new_payload to EL so the block is known before consensus distributes it.
        let block_inner = block.clone().into_inner();
        let (payload, sidecar) = alloy_rpc_types_engine::ExecutionPayload::from_block_unchecked(
            block_inner.hash(),
            &block_inner.into_block(),
        );
        let execution_data = alloy_rpc_types_engine::ExecutionData { payload, sidecar };
        self.state
            .execution_node
            .beacon_engine_handle()
            .new_payload(execution_data)
            .pace(&self.context, Duration::from_millis(20))
            .await
            .wrap_err("failed sending new_payload for proposed block")?;

        // Broadcast block to all peers and register in marshal.
        // Using `proposed()` (not `verified()`) so the block is distributed
        // via P2P to other nodes before they try to verify it.
        self.state.marshal.proposed(round, block).await;

        Ok(digest)
    }

    async fn handle_verify(&mut self, verify: Verify) -> eyre::Result<bool> {
        let Verify { parent, payload: block_digest, proposer: _, response, round } = verify;

        // parent_view is not used; we subscribe with None to receive the block via P2P.
        let (_parent_view, parent_digest) = parent;

        // Retrieve the proposed block from the marshal.
        // Use `None` for round so the marshal waits for the block to arrive via P2P
        // broadcast (from the proposer's `proposed()` call) rather than requesting a
        // notarized copy from the network (which does not exist yet for a pending proposal).
        let block = self
            .state
            .marshal
            .subscribe_by_digest(None, block_digest)
            .await
            .await
            .map_err(|_| eyre::eyre!("failed resolving block for verification"))?;

        // Retrieve the parent block. Genesis (height 0) is pre-loaded; other parents
        // were distributed when their proposer called `proposed()`. Use `None` so the
        // marshal returns the block from its local cache without requesting notarization.
        let parent = self
            .state
            .marshal
            .subscribe_by_digest(None, parent_digest)
            .await
            .await
            .map_err(|_| eyre::eyre!("failed resolving parent block for verification"))?;

        // Update canonical head to parent and wait for the executor to acknowledge
        // the FCU before sending new_payload, so the execution engine processes
        // the fork-choice update before it validates the block.
        match self.state.executor.canonicalize_head(parent.height(), parent.digest()) {
            Err(error) => {
                warn!(
                    %error,
                    parent.height = %parent.height(),
                    parent.digest = %parent.digest(),
                    "failed to send canonicalize_head request — executor may have exited",
                );
            }
            Ok(rx) => {
                if rx.await.is_err() {
                    warn!(
                        parent.height = %parent.height(),
                        parent.digest = %parent.digest(),
                        "canonicalize_head ack dropped — executor may have exited",
                    );
                }
            }
        }

        // Send the block to the execution engine for validation.
        let block_inner = block.clone().into_inner();
        let (payload, sidecar) = alloy_rpc_types_engine::ExecutionPayload::from_block_unchecked(
            block_inner.hash(),
            &block_inner.into_block(),
        );
        let execution_data = alloy_rpc_types_engine::ExecutionData { payload, sidecar };

        let payload_status = self
            .state
            .execution_node
            .beacon_engine_handle()
            .new_payload(execution_data)
            .pace(&self.context, Duration::from_millis(20))
            .await
            .wrap_err("failed sending new_payload to execution engine")?;

        let is_valid = payload_status.is_valid();

        if !is_valid {
            warn!(
                %payload_status,
                "execution engine rejected block",
            );
        }

        // Send verification result. If the channel is closed, simplex timed out
        // waiting for the result — log so we can detect misconfigured timeouts.
        if response.send(is_valid).is_err() {
            warn!(
                is_valid,
                "verify response channel closed before result could be sent \
                 — simplex engine may have timed out; consider increasing wait-for-proposal",
            );
        }

        // Notify marshal that verification is complete
        if is_valid {
            self.state.marshal.verified(round, block.clone()).await;
        }

        Ok(is_valid)
    }

    async fn handle_broadcast(&self, _payload: Digest) {
        // The marshal handles block distribution.
        // This is a notification that the block should be broadcast.
    }
}
