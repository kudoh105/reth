//! The foundational data structure the private chain comes to consensus over.
//!
//! The [`Block`] is a thin wrapper around an Ethereum `SealedBlock` using
//! standard `EthPrimitives` (no custom headers or transaction types).

use alloy_consensus::BlockHeader as _;
use alloy_primitives::B256;
use bytes::{Buf, BufMut};
use commonware_codec::{EncodeSize, Read, Write};
use commonware_consensus::{Heightable, types::Height};
use commonware_cryptography::{Committable, Digestible};
use reth_ethereum_primitives::EthPrimitives;
use reth_primitives_traits::SealedBlock;

use super::Digest;

/// A private chain block.
///
/// Thin wrapper around `SealedBlock<reth_ethereum_primitives::Block>` to hold
/// the trait implementations required by commonware.
#[derive(Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub(crate) struct Block(SealedBlock<<EthPrimitives as reth_node_api::NodePrimitives>::Block>);

impl Block {
    /// Wraps an execution layer block.
    pub(crate) fn from_execution_block(
        block: SealedBlock<<EthPrimitives as reth_node_api::NodePrimitives>::Block>,
    ) -> Self {
        Self(block)
    }

    /// Unwraps into the inner sealed block.
    pub(crate) fn into_inner(
        self,
    ) -> SealedBlock<<EthPrimitives as reth_node_api::NodePrimitives>::Block> {
        self.0
    }

    /// Returns the (eth) hash of the wrapped block.
    pub(crate) fn block_hash(&self) -> B256 {
        self.0.hash()
    }

    /// Returns the hash of the wrapped block as a commonware [`Digest`].
    pub(crate) fn digest(&self) -> Digest {
        Digest(self.hash())
    }

    /// Returns the parent hash as a [`Digest`].
    pub(crate) fn parent_digest(&self) -> Digest {
        Digest(self.0.parent_hash())
    }

    /// Returns the parent hash as a raw `B256`.
    pub(crate) fn parent_hash(&self) -> B256 {
        self.0.parent_hash()
    }

    /// Returns the block timestamp.
    pub(crate) fn timestamp(&self) -> u64 {
        self.0.timestamp()
    }
}

impl std::ops::Deref for Block {
    type Target = SealedBlock<<EthPrimitives as reth_node_api::NodePrimitives>::Block>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Write for Block {
    fn write(&self, buf: &mut impl BufMut) {
        use alloy_rlp::Encodable as _;
        self.0.encode(buf);
    }
}

impl Read for Block {
    type Cfg = ();

    fn read_cfg(buf: &mut impl Buf, _cfg: &Self::Cfg) -> Result<Self, commonware_codec::Error> {
        let header = alloy_rlp::Header::decode(&mut buf.chunk()).map_err(|rlp_err| {
            commonware_codec::Error::Wrapped("reading RLP header", rlp_err.into())
        })?;

        if header.length_with_payload() > buf.remaining() {
            return Err(commonware_codec::Error::EndOfBuffer);
        }
        let bytes = buf.copy_to_bytes(header.length_with_payload());

        let inner = alloy_rlp::Decodable::decode(&mut bytes.as_ref()).map_err(|rlp_err| {
            commonware_codec::Error::Wrapped("reading RLP encoded block", rlp_err.into())
        })?;

        Ok(Self::from_execution_block(inner))
    }
}

impl EncodeSize for Block {
    fn encode_size(&self) -> usize {
        use alloy_rlp::Encodable as _;
        self.0.length()
    }
}

impl Committable for Block {
    type Commitment = Digest;

    fn commitment(&self) -> Self::Commitment {
        self.digest()
    }
}

impl Digestible for Block {
    type Digest = Digest;

    fn digest(&self) -> Self::Digest {
        Block::digest(self)
    }
}

impl Heightable for Block {
    fn height(&self) -> Height {
        Height::new(self.0.number())
    }
}

impl commonware_consensus::Block for Block {
    fn parent(&self) -> Digest {
        self.parent_digest()
    }
}
