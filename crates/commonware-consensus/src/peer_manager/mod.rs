//! Peer manager - tracks active peers.
//!
//! Ported from Tempo with `TempoFullNode` replaced by `PrivateNodeHandle`.

use commonware_cryptography::ed25519::PublicKey;
use commonware_p2p::AddressableManager;
use futures::channel::mpsc;

use crate::node_handle::PrivateNodeHandle;

pub(crate) mod actor;
pub(crate) mod ingress;

pub(crate) use actor::Actor;
pub(crate) use ingress::Mailbox;

pub(crate) struct Config<TOracle> {
    pub(crate) oracle: TOracle,
    pub(crate) execution_node: PrivateNodeHandle,
}

pub(crate) fn init<TPeerManager>(
    Config { oracle, execution_node }: Config<TPeerManager>,
) -> (Actor<TPeerManager>, Mailbox)
where
    TPeerManager: AddressableManager<PublicKey = PublicKey>,
{
    let (tx, rx) = mpsc::unbounded();
    let actor = Actor::new(oracle, execution_node, rx);
    let mailbox = Mailbox::new(tx);
    (actor, mailbox)
}
