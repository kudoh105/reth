//! Application actor for proposing and verifying blocks.
//!
//! Ported from Tempo with key changes:
//! - `TempoPayloadBuilderAttributes` → `EthPayloadBuilderAttributes`
//! - `extra_data` is always `Bytes::default()` (no DKG data in blocks)
//! - `subblocks` removed entirely
//! - `TempoFullNode` → `PrivateNodeHandle`

use std::{sync::Arc, time::Duration};

use alloy_consensus::BlockHeader as _;
use alloy_primitives::{B256, U256};
use alloy_rpc_types_engine::PayloadStatusEnum;
use commonware_consensus::{
    marshal,
    types::{Height, Round, View},
    Heightable as _,
};
use commonware_runtime::{
    spawn_cell, Clock, ContextCell, FutureExt, Handle, Metrics, Pacer, Spawner, Storage,
};
use commonware_utils::{channel::oneshot, SystemTimeExt};
use eyre::{OptionExt as _, WrapErr as _};
use futures::{channel::mpsc, StreamExt as _};
use rand_08::{CryptoRng, Rng};
use reth_engine_primitives::ExecutionPayload as _;
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

    async fn handle_genesis(&self, _epoch: commonware_consensus::types::Epoch) -> Digest {
        // Return the genesis block hash from the chain spec.
        let genesis_hash =
            self.state.execution_node.block_hash(0).ok().flatten().unwrap_or(B256::ZERO);
        Digest(genesis_hash)
    }

    async fn handle_propose(
        &mut self,
        parent: (View, Digest),
        round: Round,
    ) -> eyre::Result<Digest> {
        let (parent_view, parent_digest) = parent;

        // Retrieve the parent block from the marshal.
        let parent = self
            .state
            .marshal
            .subscribe(
                Some(commonware_consensus::types::Round::new(round.epoch(), parent_view)),
                parent_digest,
            )
            .await
            .await
            .map_err(|_| eyre::eyre!("failed resolving parent block"))?;

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

        // Register block with marshal for P2P distribution.
        self.state.marshal.verified(round, block).await;

        Ok(digest)
    }

    async fn handle_verify(&mut self, verify: Verify) -> eyre::Result<bool> {
        let Verify { parent, payload: block_digest, proposer: _, response, round } = verify;

        let (parent_view, parent_digest) = parent;

        // Retrieve the block from the marshal.
        let block = self
            .state
            .marshal
            .subscribe(None, block_digest)
            .await
            .await
            .map_err(|_| eyre::eyre!("failed resolving block for verification"))?;

        // Retrieve the parent block.
        let parent = self
            .state
            .marshal
            .subscribe(None, parent_digest)
            .await
            .await
            .map_err(|_| eyre::eyre!("failed resolving parent block for verification"))?;

        // Update canonical head to parent.
        if let Err(error) = self.state.executor.canonicalize_head(parent.height(), parent.digest())
        {
            warn!(
                %error,
                parent.height = %parent.height(),
                parent.digest = %parent.digest(),
                "failed updating canonical head to parent",
            );
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

        // Send verification result
        let _ = response.send(is_valid);

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
