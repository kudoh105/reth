//! A collection of aliases for frequently used (primarily commonware) types.

pub(crate) mod marshal {
    use commonware_consensus::{
        marshal,
        simplex::{scheme::bls12381_threshold::vrf::Scheme, types::Finalization},
        types::FixedEpocher,
    };
    use commonware_cryptography::{bls12381::primitives::variant::MinSig, ed25519::PublicKey};
    use commonware_parallel::Sequential;
    use commonware_storage::archive::immutable;
    use commonware_utils::acknowledgement::Exact;

    use crate::consensus::{block::Block, Digest};

    pub(crate) type Actor<TContext> = marshal::core::Actor<
        TContext,
        marshal::standard::Standard<Block>,
        crate::epoch::SchemeProvider,
        immutable::Archive<TContext, Digest, Finalization<Scheme<PublicKey, MinSig>, Digest>>,
        immutable::Archive<TContext, Digest, Block>,
        FixedEpocher,
        Sequential,
        Exact,
    >;

    pub(crate) type Mailbox =
        marshal::core::Mailbox<Scheme<PublicKey, MinSig>, marshal::standard::Standard<Block>>;
}
