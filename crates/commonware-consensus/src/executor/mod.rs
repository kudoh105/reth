//! The executor sends fork-choice-updates to the execution layer.
//!
//! Ported from Tempo with `TempoFullNode` replaced by `PrivateNodeHandle`.

use commonware_consensus::types::Height;
use commonware_runtime::{Clock, Metrics, Pacer, Spawner};

pub(crate) mod actor;
pub(crate) mod ingress;

pub(crate) use actor::Actor;
use eyre::WrapErr as _;
use futures::channel::mpsc;
pub(crate) use ingress::Mailbox;

use crate::node_handle::PrivateNodeHandle;

pub(crate) fn init<TContext>(
    context: TContext,
    config: Config,
) -> eyre::Result<(Actor<TContext>, Mailbox)>
where
    TContext: Clock + Metrics + Pacer + Spawner,
{
    let (tx, rx) = mpsc::unbounded();
    let mailbox = Mailbox { inner: tx };
    let actor = Actor::init(context, config, rx).wrap_err("failed initializing actor")?;
    Ok((actor, mailbox))
}

pub(crate) struct Config {
    /// A handle to the execution node layer.
    pub(crate) execution_node: PrivateNodeHandle,
    /// The last finalized height according to the consensus layer.
    pub(crate) last_finalized_height: Height,
    /// The mailbox of the marshal actor for block backfilling.
    pub(crate) marshal: crate::alias::marshal::Mailbox,
    /// The interval for FCU heartbeats.
    pub(crate) fcu_heartbeat_interval: std::time::Duration,
}
