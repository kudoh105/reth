# Porting Commonware Consensus to Reth

## Overview

This document details the porting of Commonware's BLS12-381 threshold simplex consensus from the [Tempo project](https://github.com/tempo-labs/tempo) to the Reth Ethereum client (v1.10.2).

## Source → Target Mapping

### File Mapping

| Tempo Source | Reth Target | Changes |
|---|---|---|
| `crates/commonware-node/src/lib.rs` | `crates/commonware-consensus/src/lib.rs` | Removed `feed_state`, `subblocks` channel |
| `crates/commonware-node/src/args.rs` | `crates/commonware-consensus/src/args.rs` | Removed `subblock` args |
| `crates/commonware-node/src/config.rs` | `crates/commonware-consensus/src/config.rs` | Removed `SUBBLOCKS_CHANNEL_IDENT` |
| `crates/commonware-node/src/consensus/engine.rs` | `crates/commonware-consensus/src/consensus/engine.rs` | Removed `subblocks` field/channel, `feed_state` |
| `crates/commonware-node/src/consensus/application/actor.rs` | `crates/commonware-consensus/src/consensus/application/actor.rs` | `TempoPayloadBuilderAttributes` → `PayloadAttributes`, removed DKG `extra_data`, removed `subblocks` |
| `crates/commonware-node/src/consensus/block.rs` | `crates/commonware-consensus/src/consensus/block.rs` | `SealedBlock<tempo_primitives::Block>` → `SealedBlock<EthPrimitives::Block>` |
| `crates/commonware-node/src/consensus/digest.rs` | `crates/commonware-consensus/src/consensus/digest.rs` | Unchanged |
| `crates/commonware-node/src/executor/actor.rs` | `crates/commonware-consensus/src/executor/actor.rs` | `TempoFullNode` → `PrivateNodeHandle`, `TempoExecutionData` → `ExecutionData` |
| `crates/commonware-node/src/epoch/` | `crates/commonware-consensus/src/epoch/` | Mostly unchanged |
| `crates/commonware-node/src/dkg/` | `crates/commonware-consensus/src/dkg/` | On-chain contract reads → genesis `extra_fields` reads |
| `crates/commonware-node/src/peer_manager/` | `crates/commonware-consensus/src/peer_manager/` | `TempoFullNode` → `PrivateNodeHandle` |
| `commonware-node-config/src/lib.rs` | `crates/commonware-consensus/src/key_io.rs` | Unchanged, embedded in crate |
| `crates/commonware-node/src/alias.rs` | `crates/commonware-consensus/src/alias.rs` | Unchanged |
| `bin/tempo/src/main.rs` | `bin/private-node/src/main.rs` | `TempoNode` → `EthereumNode`, simplified CLI |

### Type Mapping

| Tempo Type | Reth Type |
|---|---|
| `TempoFullNode` | `PrivateNodeHandle` (custom wrapper) |
| `TempoChainSpec` | `ChainSpec` (standard) |
| `TempoPayloadBuilderAttributes` | `PayloadAttributes` (standard) |
| `TempoExecutionData` | `ExecutionData` (standard) |
| `TempoHeader` | Standard Ethereum header |
| `TempoTxEnvelope` | Standard Ethereum transaction |
| `TempoEvmConfig` | `EthEvmConfig` (standard) |
| `TempoConsensus` | `EthereumConsensus` (eth beacon consensus) |
| `tempo_primitives::Block` | `reth_ethereum_primitives::Block` |

## What Was NOT Ported

### Tempo-Specific Features (Removed)

1. **SubBlocks** - MEV-specific feature for pre-block transactions. Not needed for the private chain.
2. **Feed/FeedStateHandle** - RPC-exposed consensus state feed. Separate concern.
3. **Custom Primitives** - `TempoHeader`, `TempoTxEnvelope`, etc. replaced by standard Ethereum types.
4. **On-Chain DKG Contracts** - Validator set is static from genesis instead of being managed by smart contracts.
5. **Faucet** - Development utility, not a consensus concern.
6. **Telemetry** - Pyroscope profiling, separate concern.
7. **Follow Mode** - Tempo-specific RPC follower mode.

### Using Standard Ethereum Instead

| Feature | Tempo (Custom) | Private Chain (Standard) |
|---|---|---|
| Block Header | `TempoHeader` (custom fields) | Standard Ethereum header |
| Transactions | `TempoTxEnvelope` | Standard Ethereum transactions |
| EVM Config | `TempoEvmConfig` | `EthEvmConfig` |
| Consensus Validation | `TempoConsensus` | `EthereumConsensus` |
| Payload Attributes | `TempoPayloadBuilderAttributes` (with `extra_data`) | Standard `PayloadAttributes` |
| Validator Set | On-chain DKG contracts | Genesis `extra_fields` |

## Key Design Decisions

### 1. PrivateNodeHandle

Instead of threading the entire `FullNode<...>` generic type through the consensus layer (which has ~5 levels of generic parameters), we created `PrivateNodeHandle` that extracts just the handles we need:

```rust
struct PrivateNodeHandle {
    beacon_engine: ConsensusEngineHandle<EthEngineTypes>,
    payload_builder: PayloadBuilderHandle<EthEngineTypes>,
    chain_spec: Arc<ChainSpec>,
    provider: ProviderHandle,  // trait object
}
```

This isolates the consensus crate from Reth's complex generic type hierarchy.

### 2. Two-Thread Architecture

Same as Tempo:
- **Thread 1 (Tokio)**: Reth execution layer
- **Thread 2 (Commonware Runtime)**: Consensus engine

Communication is via in-process channels (`ConsensusEngineHandle` uses `mpsc::UnboundedSender`), not HTTP/JSON-RPC.

### 3. Genesis-Based Validator Configuration

Tempo reads validators from on-chain contracts. The private chain reads them from `genesis.config.extra_fields`:

```json
{
  "epochLength": 100,
  "validators": [
    { "pubkey": "0x...", "address": "172.20.0.11:8000" }
  ],
  "publicPolynomial": "0x..."
}
```

### 4. Standard PayloadAttributes

Tempo adds DKG ceremony outcomes to `extra_data` in payload attributes. The private chain uses `Bytes::default()` since the validator set is static.

## Incomplete / TODO Items

1. **Payload Resolution in Propose**: The `handle_propose()` method triggers payload build via FCU with attributes but needs full integration with `PayloadBuilderHandle::resolve()` to get the built block back.

2. **Epoch Manager**: Full porting of the simplex voting round lifecycle from Tempo. Currently a stub.

3. **DKG Manager**: Full porting of the DKG ceremony management. Currently reads from genesis but needs runtime scheme registration.

4. **Keygen Subcommand**: The `private-node keygen` command for generating ed25519 + BLS keys needs to be implemented (use `commonware_cryptography::bls12381::dkg` API).

5. **NoopNetworkBuilder**: Need to verify that disabling discv5 discovery (`enable_discv5_discovery = false`) is sufficient, or if a custom `NoopNetworkBuilder` component is needed.

## Dependency Notes

### Commonware Version

Using `2026.2.0` (matching Tempo's pinned version). Key Commonware crates:
- `commonware-consensus` - Simplex BFT consensus
- `commonware-cryptography` - BLS12-381, ed25519
- `commonware-p2p` - Authenticated P2P networking
- `commonware-runtime` - Async runtime abstraction
- `commonware-storage` - Persistent storage (archives)

### Potential Alloy Version Conflicts

Reth uses `alloy 1.6.3`. Commonware may depend on a different alloy version. If there are conflicts, use `[patch.crates-io]` in the root `Cargo.toml` to align versions. The commented-out patches in `Cargo.toml` show the pattern.

### Minimum Supported Rust Version (MSRV)

**Commonware `2026.2.0` requires Rust >= 1.91.0** because `commonware-p2p` uses `Duration::from_hours()` which was stabilized in Rust 1.91. Tempo uses `rust-version = "1.93.0"`.

Reth v1.10.2 specifies `rust-version = "1.88"`. To build the private chain node:

1. **Update toolchain**: `rustup update stable` (ensure >= 1.91.0)
2. The `rust-version` in `Cargo.toml` only needs updating if you want `cargo check` to enforce it for all workspace members. For building just `private-node`, having a compatible toolchain installed is sufficient.
3. Alternative: Use `[patch.crates-io]` to point Commonware deps to a git revision compatible with Rust 1.88.

## Verification Steps

1. **Build**: `cargo build --release --bin private-node`
2. **Keygen**: `./target/release/private-node keygen --num-validators 4 --threshold 3 --output-dir deploy/keys`
3. **Genesis**: Populate `deploy/genesis.json` with generated keys
4. **Deploy**: `cd deploy && docker compose up -d`
5. **Monitor**: `curl localhost:8545 -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'`
6. **Verify finality**: Block numbers should increase rapidly (~9 blocks/second target)
