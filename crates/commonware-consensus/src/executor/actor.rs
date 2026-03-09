//! Drives the actual execution forwarding blocks and setting forkchoice state.
//!
//! Ported from Tempo with `TempoFullNode` → `PrivateNodeHandle` and
//! `TempoExecutionData` → standard `ExecutionData`.

use std::{pin::Pin, sync::Arc, time::Duration};

use alloy_rpc_types_engine::ForkchoiceState;
use commonware_consensus::{marshal::Update, types::Height, Heightable as _};

use alloy_rpc_types_engine::ExecutionData;
use commonware_runtime::{
    spawn_cell, Clock, ContextCell, FutureExt, Handle, Metrics, Pacer, Spawner,
};
use commonware_utils::{acknowledgement::Exact, Acknowledgement};
use eyre::{ensure, OptionExt as _, Report, WrapErr as _};
use futures::{
    channel::{
        mpsc::{self, UnboundedReceiver},
        oneshot,
    },
    select_biased, FutureExt as _, StreamExt as _,
};
use reth_payload_primitives::EngineApiMessageVersion;
use tracing::{
    debug, error, error_span, info, info_span, instrument, warn, warn_span, Level, Span,
};

use crate::{
    consensus::{block::Block, Digest},
    executor::{
        ingress::{CanonicalizeHead, Command, Message},
        Config,
    },
    node_handle::PrivateNodeHandle,
};

/// Tracks the last forkchoice state that the executor sent to the execution layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LastCanonicalized {
    forkchoice: ForkchoiceState,
    head_height: Height,
    finalized_height: Height,
}

impl LastCanonicalized {
    fn update_finalized(self, height: Height, digest: Digest) -> Self {
        let mut this = self;
        if height > this.finalized_height {
            this.finalized_height = height;
            this.forkchoice.safe_block_hash = digest.0;
            this.forkchoice.finalized_block_hash = digest.0;
        }
        if height >= this.head_height {
            this.head_height = height;
            this.forkchoice.head_block_hash = digest.0;
        }
        this
    }

    fn update_head(self, height: Height, digest: Digest) -> Self {
        let mut this = self;
        if height > this.finalized_height {
            this.head_height = height;
            this.forkchoice.head_block_hash = digest.0;
        }
        this
    }
}

pub(crate) struct Actor<TContext> {
    context: ContextCell<TContext>,
    execution_node: PrivateNodeHandle,
    last_consensus_finalized_height: Height,
    last_execution_finalized_height: Height,
    mailbox: mpsc::UnboundedReceiver<Message>,
    marshal: crate::alias::marshal::Mailbox,
    last_canonicalized: LastCanonicalized,
    fcu_heartbeat_interval: Duration,
    fcu_heartbeat_timer: Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
}

impl<TContext> Actor<TContext>
where
    TContext: Clock + Metrics + Pacer + Spawner,
{
    pub(super) fn init(
        context: TContext,
        config: super::Config,
        mailbox: UnboundedReceiver<super::ingress::Message>,
    ) -> eyre::Result<Self> {
        let Config { execution_node, last_finalized_height, marshal, fcu_heartbeat_interval } =
            config;
        let last_execution_finalized_height = execution_node
            .last_block_number()
            .wrap_err("unable to read latest block number from execution layer")?;
        let last_finalized_block_hash = execution_node
            .block_hash(last_execution_finalized_height)
            .map_or_else(
                |e| Err(Report::new(e)),
                |hash| hash.ok_or_eyre("execution layer does not have the block hash"),
            )
            .wrap_err("failed to read the last finalized block hash")?;
        let fcu_heartbeat_timer = Box::pin(context.sleep(fcu_heartbeat_interval));
        Ok(Self {
            context: ContextCell::new(context),
            execution_node,
            last_consensus_finalized_height: last_finalized_height,
            last_execution_finalized_height: Height::new(last_execution_finalized_height),
            mailbox,
            marshal,
            last_canonicalized: LastCanonicalized {
                forkchoice: ForkchoiceState {
                    head_block_hash: last_finalized_block_hash,
                    safe_block_hash: last_finalized_block_hash,
                    finalized_block_hash: last_finalized_block_hash,
                },
                head_height: Height::zero(),
                finalized_height: Height::zero(),
            },
            fcu_heartbeat_interval,
            fcu_heartbeat_timer,
        })
    }

    pub(crate) fn start(mut self) -> Handle<()> {
        spawn_cell!(self.context, self.run().await)
    }

    async fn run(mut self) {
        info_span!("start").in_scope(|| {
            info!(
                last_finalized_consensus_height = %self.last_consensus_finalized_height,
                last_finalized_execution_height = %self.last_execution_finalized_height,
                "consensus and execution layers reported last finalized heights; \
                backfilling blocks from consensus to execution if necessary",
            );
        });

        let mut backfill_on_start = {
            let marshal = self.marshal.clone();
            std::pin::pin!(futures::stream::iter(
                self.last_execution_finalized_height.get() + 1..=
                    self.last_consensus_finalized_height.get(),
            )
            .then(move |height| {
                let mut marshal = marshal.clone();
                async move { (height, marshal.get_block(Height::new(height)).await) }
            })
            .fuse())
        };

        loop {
            select_biased! {
                backfill = backfill_on_start.next() => {
                    match backfill {
                        Some((height, Some(block))) => {
                            let (ack, _wait) = Exact::handle();
                            let span = info_span!("backfill_on_start", height);
                            let _ = self.forward_finalized(
                                span,
                                block,
                                ack,
                            ).await;
                        }
                        Some((height, None)) => {
                            warn_span!("backfill_on_start", height)
                            .in_scope(|| warn!(
                                "marshal actor did not have block even though \
                                it must have finalized it previously",
                            ));
                        }
                        None => {
                            info_span!("backfill_on_start")
                            .in_scope(|| info!(
                                "no more blocks to backfill from consensus to \
                                execution layer")
                            );
                        }
                    }
                },

                msg = self.mailbox.next() => {
                    let Some(msg) = msg else { break; };
                    if let Err(error) = self.handle_message(msg).await {
                        error_span!("shutdown").in_scope(|| error!(
                            %error,
                            "executor encountered fatal fork choice update error; \
                            shutting down to prevent consensus-execution divergence"
                        ));
                        break;
                    }
                },

                _ = (&mut self.fcu_heartbeat_timer).fuse() => {
                    self.send_forkchoice_update_heartbeat().await;
                    self.reset_fcu_heartbeat_timer();
                },
            }
        }
    }

    fn reset_fcu_heartbeat_timer(&mut self) {
        self.fcu_heartbeat_timer = Box::pin(self.context.sleep(self.fcu_heartbeat_interval));
    }

    #[instrument(skip_all)]
    async fn send_forkchoice_update_heartbeat(&mut self) {
        info!(
            head_block_hash = %self.last_canonicalized.forkchoice.head_block_hash,
            head_block_height = %self.last_canonicalized.head_height,
            finalized_block_hash = %self.last_canonicalized.forkchoice.finalized_block_hash,
            finalized_block_height = %self.last_canonicalized.finalized_height,
            "sending FCU",
        );

        let fcu_response = self
            .execution_node
            .beacon_engine_handle()
            .fork_choice_updated(
                self.last_canonicalized.forkchoice,
                None,
                EngineApiMessageVersion::V3,
            )
            .pace(&self.context, Duration::from_millis(20))
            .await;

        match fcu_response {
            Ok(response) if response.is_invalid() => {
                warn!(
                    payload_status = %response.payload_status,
                    "execution layer reported FCU status",
                );
            }
            Ok(response) => {
                info!(
                    payload_status = %response.payload_status,
                    "execution layer reported FCU status",
                );
            }
            Err(error) => {
                warn!(
                    error = %Report::new(error),
                    "failed sending FCU to execution layer",
                );
            }
        }
    }

    async fn handle_message(&mut self, message: Message) -> eyre::Result<()> {
        let cause = message.cause;
        match message.command {
            Command::CanonicalizeHead(CanonicalizeHead { height, digest, ack }) => {
                let _ = self.canonicalize(cause, HeadOrFinalized::Head, height, digest, ack).await;
            }
            Command::Finalize(finalized) => {
                self.finalize(cause, *finalized).await.wrap_err("failed handling finalization")?;
            }
        }
        Ok(())
    }

    #[instrument(
        skip_all,
        parent = &cause,
        fields(
            head.height = %height,
            head.digest = %digest,
            %head_or_finalized,
        ),
        err,
    )]
    async fn canonicalize(
        &mut self,
        cause: Span,
        head_or_finalized: HeadOrFinalized,
        height: Height,
        digest: Digest,
        ack: oneshot::Sender<()>,
    ) -> eyre::Result<()> {
        let new_canonicalized = match head_or_finalized {
            HeadOrFinalized::Head => self.last_canonicalized.update_head(height, digest),
            HeadOrFinalized::Finalized => self.last_canonicalized.update_finalized(height, digest),
        };

        if new_canonicalized == self.last_canonicalized {
            info!("would not change forkchoice state; not sending it to the execution layer");
            let _ = ack.send(());
            return Ok(());
        }

        info!(
            head_block_hash = %new_canonicalized.forkchoice.head_block_hash,
            head_block_height = %new_canonicalized.head_height,
            finalized_block_hash = %new_canonicalized.forkchoice.finalized_block_hash,
            finalized_block_height = %new_canonicalized.finalized_height,
            "sending forkchoice-update",
        );
        let fcu_response = self
            .execution_node
            .beacon_engine_handle()
            .fork_choice_updated(new_canonicalized.forkchoice, None, EngineApiMessageVersion::V3)
            .pace(&self.context, Duration::from_millis(20))
            .await
            .wrap_err("failed requesting execution layer to update forkchoice state")?;

        debug!(
            payload_status = %fcu_response.payload_status,
            "execution layer reported FCU status",
        );

        if fcu_response.is_invalid() {
            return Err(Report::msg(fcu_response.payload_status)
                .wrap_err("execution layer responded with error for forkchoice-update"));
        }

        let _ = ack.send(());
        self.last_canonicalized = new_canonicalized;
        self.reset_fcu_heartbeat_timer();

        Ok(())
    }

    #[instrument(parent = &cause, skip_all)]
    async fn finalize(&mut self, cause: Span, finalized: Update<Block>) -> eyre::Result<()> {
        match finalized {
            Update::Tip(_, height, digest) => {
                self.canonicalize(
                    Span::current(),
                    HeadOrFinalized::Finalized,
                    height,
                    digest,
                    oneshot::channel().0,
                )
                .await
                .wrap_err("failed canonicalizing finalization tip")?;
            }
            Update::Block(block, acknowledgment) => {
                self.forward_finalized(Span::current(), block, acknowledgment)
                    .await
                    .wrap_err("failed forwarding finalized block to execution layer")?;
            }
        }
        Ok(())
    }

    #[instrument(
        skip_all,
        parent = &cause,
        fields(
            block.digest = %block.digest(),
            block.height = %block.height(),
        ),
        err(level = Level::WARN),
        ret,
    )]
    async fn forward_finalized(
        &mut self,
        cause: Span,
        block: Block,
        acknowledgment: Exact,
    ) -> eyre::Result<()> {
        self.canonicalize(
            Span::current(),
            HeadOrFinalized::Finalized,
            block.height(),
            block.digest(),
            oneshot::channel().0,
        )
        .await
        .wrap_err("failed canonicalizing finalized block")?;

        let block_inner = block.clone().into_inner();
        let (payload, sidecar) = alloy_rpc_types_engine::ExecutionPayload::from_block_unchecked(
            block_inner.hash(),
            &block_inner.into_block(),
        );
        let execution_data = ExecutionData { payload, sidecar };
        let payload_status = self
            .execution_node
            .beacon_engine_handle()
            .new_payload(execution_data)
            .pace(&self.context, Duration::from_millis(20))
            .await
            .wrap_err(
                "failed sending new-payload request to execution engine to \
                query payload status of finalized block",
            )?;

        ensure!(
            payload_status.is_valid() || payload_status.is_syncing(),
            "this is a problem: payload status of block-to-be-finalized was \
            neither valid nor syncing: `{payload_status}`"
        );

        acknowledgment.acknowledge();

        Ok(())
    }
}

/// Marker to indicate whether the head hash or finalized hash should be updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeadOrFinalized {
    Head,
    Finalized,
}

impl std::fmt::Display for HeadOrFinalized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::Head => "head",
            Self::Finalized => "finalized",
        };
        f.write_str(msg)
    }
}
