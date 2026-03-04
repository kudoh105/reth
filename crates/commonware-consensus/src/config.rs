//! The non-reth/non-chainspec part of the node configuration.
//!
//! Channel identifiers and rate limits for the Commonware P2P network.

use std::num::NonZeroU32;

use governor::Quota;

// Channel identifiers for Commonware P2P network.
pub const VOTES_CHANNEL_IDENT: commonware_p2p::Channel = 0;
pub const CERTIFICATES_CHANNEL_IDENT: commonware_p2p::Channel = 1;
pub const RESOLVER_CHANNEL_IDENT: commonware_p2p::Channel = 2;
pub const BROADCASTER_CHANNEL_IDENT: commonware_p2p::Channel = 3;
pub const MARSHAL_CHANNEL_IDENT: commonware_p2p::Channel = 4;
pub const DKG_CHANNEL_IDENT: commonware_p2p::Channel = 5;

pub(crate) const NUMBER_CONCURRENT_FETCHES: usize = 4;

pub(crate) const PEERSETS_TO_TRACK: usize = 3;

pub(crate) const BLOCKS_FREEZER_TABLE_INITIAL_SIZE_BYTES: u32 = 2u32.pow(21); // 2MB

pub const BROADCASTER_LIMIT: Quota =
    Quota::per_second(NonZeroU32::new(8).expect("value is not zero"));
pub const DKG_LIMIT: Quota = Quota::per_second(NonZeroU32::new(128).expect("value is not zero"));
pub const MARSHAL_LIMIT: Quota = Quota::per_second(NonZeroU32::new(8).expect("value is not zero"));
pub const VOTES_LIMIT: Quota = Quota::per_second(NonZeroU32::new(128).expect("value is not zero"));
pub const CERTIFICATES_LIMIT: Quota =
    Quota::per_second(NonZeroU32::new(128).expect("value is not zero"));
pub const RESOLVER_LIMIT: Quota =
    Quota::per_second(NonZeroU32::new(128).expect("value is not zero"));

pub const NAMESPACE: &[u8] = b"PRIVATE_CHAIN";
