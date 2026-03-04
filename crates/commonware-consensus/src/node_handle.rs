//! Wrapper around the Reth `FullNode` providing access to the execution layer.
//!
//! This replaces Tempo's `TempoFullNode` with a concrete type using standard
//! Ethereum primitives (no custom headers, tx envelopes, or EVM config).

use std::sync::Arc;

use reth_chainspec::ChainSpec;
use reth_db::DatabaseEnv;
use reth_engine_primitives::ConsensusEngineHandle;
use reth_ethereum::EthEngineTypes;
use reth_node_builder::{
    FullNode, NodeAdapter, RethFullAdapter,
};
use reth_payload_builder::PayloadBuilderHandle;
use reth_provider::{BlockHashReader, BlockNumReader};

use crate::genesis::PrivateGenesisInfo;

/// Concrete Reth node type for standard Ethereum with `EthereumNode`.
///
/// `FullNode<NodeAdapter<RethFullAdapter<DatabaseEnv, EthereumNode>>, EthereumAddOns<...>>`
pub use reth_node_builder::node::FullNodeFor;

/// A handle to the launched Reth execution layer node.
///
/// Provides in-process access to:
/// - `ConsensusEngineHandle` for `new_payload()` / `fork_choice_updated()` via mpsc channel
/// - `PayloadBuilderHandle` for triggering and resolving block builds
/// - `ChainSpec` for chain configuration
/// - Block reader provider
#[derive(Clone)]
pub struct PrivateNodeHandle {
    /// The inner Reth full node handle. We store it as an opaque type
    /// and access specific handles via dedicated methods.
    beacon_engine: ConsensusEngineHandle<EthEngineTypes>,
    payload_builder: PayloadBuilderHandle<EthEngineTypes>,
    chain_spec: Arc<ChainSpec>,
    /// Provider for block hash / number lookups.
    provider: ProviderHandle,
}

/// An opaque provider handle that supports block hash and number lookups.
#[derive(Clone)]
struct ProviderHandle {
    inner: Arc<dyn BlockHashAndNumProvider>,
}

/// Combined trait for what we need from the provider.
trait BlockHashAndNumProvider: BlockHashReader + BlockNumReader + Send + Sync + 'static {}
impl<T: BlockHashReader + BlockNumReader + Send + Sync + 'static> BlockHashAndNumProvider for T {}

impl std::fmt::Debug for ProviderHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderHandle").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for PrivateNodeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrivateNodeHandle")
            .field("chain_id", &self.chain_spec.chain().id())
            .finish_non_exhaustive()
    }
}

impl PrivateNodeHandle {
    /// Creates a new `PrivateNodeHandle` from a launched `FullNode`.
    ///
    /// This extracts the needed handles from the full node so that the consensus
    /// layer can drive the execution layer without going through HTTP.
    pub fn new<Node, AddOns>(node: &FullNode<Node, AddOns>) -> Self
    where
        Node: reth_node_api::FullNodeComponents<
            Types: reth_node_api::NodeTypes<
                Payload = EthEngineTypes,
                ChainSpec = ChainSpec,
            >,
        >,
        Node::Provider: BlockHashReader + BlockNumReader + Clone + 'static,
        AddOns: reth_node_builder::NodeAddOns<Node>,
        AddOns::Handle: AsRef<reth_node_builder::rpc::RpcHandle<Node, reth_rpc::EthApi<Node>>>,
    {
        let rpc_handle: &reth_node_builder::rpc::RpcHandle<Node, reth_rpc::EthApi<Node>> =
            node.add_ons_handle.as_ref();
        let beacon_engine = rpc_handle.beacon_engine_handle.clone();
        let payload_builder = node.payload_builder_handle.clone();
        let chain_spec = node.provider.chain_spec();
        let provider = node.provider.clone();

        Self {
            beacon_engine,
            payload_builder,
            chain_spec,
            provider: ProviderHandle {
                inner: Arc::new(provider),
            },
        }
    }

    /// Returns a handle to the beacon consensus engine for sending
    /// `new_payload` and `fork_choice_updated` messages.
    pub fn beacon_engine_handle(&self) -> &ConsensusEngineHandle<EthEngineTypes> {
        &self.beacon_engine
    }

    /// Returns a handle to the payload builder for triggering and resolving
    /// block builds.
    pub fn payload_builder_handle(&self) -> &PayloadBuilderHandle<EthEngineTypes> {
        &self.payload_builder
    }

    /// Returns the chain spec.
    pub fn chain_spec(&self) -> &Arc<ChainSpec> {
        &self.chain_spec
    }

    /// Reads the genesis-based validator info from the chain spec.
    pub fn genesis_info(&self) -> PrivateGenesisInfo {
        PrivateGenesisInfo::from_chain_spec_arc(&self.chain_spec)
    }

    /// Returns the block hash for a given block number, if available.
    pub fn block_hash(
        &self,
        number: u64,
    ) -> Result<Option<alloy_primitives::B256>, reth_provider::ProviderError> {
        self.provider.inner.block_hash(number)
    }

    /// Returns the last known block number.
    pub fn last_block_number(&self) -> Result<u64, reth_provider::ProviderError> {
        self.provider.inner.last_block_number()
    }
}
