//! Application ingress messages.
//!
//! Ported from Tempo without changes (subblocks removed upstream in engine).

use commonware_consensus::{
    simplex::types::Context,
    types::{Epoch, Round, View},
    Automaton, CertifiableAutomaton, Relay,
};

use commonware_cryptography::ed25519::PublicKey;
use commonware_utils::channel::oneshot;
use futures::{channel::mpsc, SinkExt as _};

use crate::consensus::Digest;

#[derive(Clone)]
pub(crate) struct Mailbox {
    inner: mpsc::Sender<Message>,
}

impl Mailbox {
    pub(super) fn from_sender(inner: mpsc::Sender<Message>) -> Self {
        Self { inner }
    }
}

/// Messages forwarded from consensus to application.
pub(super) enum Message {
    Broadcast(Broadcast),
    Genesis(Genesis),
    Propose(Propose),
    Verify(Box<Verify>),
}

pub(super) struct Genesis {
    pub(super) epoch: Epoch,
    pub(super) response: oneshot::Sender<Digest>,
}

impl From<Genesis> for Message {
    fn from(value: Genesis) -> Self {
        Self::Genesis(value)
    }
}

pub(super) struct Propose {
    pub(super) parent: (View, Digest),
    pub(super) response: oneshot::Sender<Digest>,
    pub(super) round: Round,
}

impl From<Propose> for Message {
    fn from(value: Propose) -> Self {
        Self::Propose(value)
    }
}

pub(super) struct Broadcast {
    pub(super) payload: Digest,
}

impl From<Broadcast> for Message {
    fn from(value: Broadcast) -> Self {
        Self::Broadcast(value)
    }
}

pub(super) struct Verify {
    pub(super) parent: (View, Digest),
    pub(super) payload: Digest,
    pub(super) proposer: PublicKey,
    pub(super) response: oneshot::Sender<bool>,
    pub(super) round: Round,
}

impl From<Verify> for Message {
    fn from(value: Verify) -> Self {
        Self::Verify(Box::new(value))
    }
}

impl Automaton for Mailbox {
    type Context = Context<Self::Digest, PublicKey>;

    type Digest = Digest;

    async fn genesis(&mut self, epoch: Epoch) -> Self::Digest {
        let (tx, rx) = oneshot::channel();
        self.inner
            .send(Genesis { epoch, response: tx }.into())
            .await
            .expect("application is present and ready to receive genesis");
        rx.await.expect("application returns the digest of the genesis")
    }

    async fn propose(&mut self, context: Self::Context) -> oneshot::Receiver<Self::Digest> {
        let (tx, rx) = oneshot::channel();
        self.inner
            .send(Propose { parent: context.parent, response: tx, round: context.round }.into())
            .await
            .expect("application is present and ready to receive proposals");
        rx
    }

    async fn verify(
        &mut self,
        context: Self::Context,
        payload: Self::Digest,
    ) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.inner
            .send(
                Verify {
                    parent: context.parent,
                    payload,
                    proposer: context.leader,
                    round: context.round,
                    response: tx,
                }
                .into(),
            )
            .await
            .expect("application is present and ready to receive verify requests");
        rx
    }
}

impl CertifiableAutomaton for Mailbox {
    // Uses the default impl which always returns true.
}

impl Relay for Mailbox {
    type Digest = Digest;

    async fn broadcast(&mut self, digest: Self::Digest) {
        self.inner
            .send(Broadcast { payload: digest }.into())
            .await
            .expect("application is present and ready to receive broadcasts");
    }
}
