# Private Chain Deployment Guide

## Overview

This guide covers deploying a 4-validator private chain that combines **Reth** (Ethereum execution) with **Commonware** (BLS12-381 threshold simplex consensus) into a single-process, sub-second-finality blockchain.

## Prerequisites

- Docker and Docker Compose v2+
- Machine: 16GB RAM, 256GB disk (4 containers × 3.5GB each)
- Rust 1.88+ (for building from source)

## Architecture

```
┌─────────────────────────────────────────────┐
│              private-node process            │
├─────────────────────┬───────────────────────┤
│  Thread 1 (Reth EL) │  Thread 2 (CW CL)     │
│  - EthereumNode     │  - Simplex voting      │
│  - MDBX + static    │  - BLS12-381 DKG       │
│  - JSON-RPC server  │  - P2P oracle          │
│  - No devp2p        │  - Marshal/Resolver    │
│                     │                        │
│  ← ConsensusEngineHandle (mpsc channel) →    │
│  ← PayloadBuilderHandle (mpsc channel) →     │
└─────────────────────┴───────────────────────┘
```

## Quick Start

### 1. Generate Keys

```bash
# Build the binary
cargo build --release --bin private-node

# Generate keys for 4 validators (threshold = 3)
# This creates ed25519.hex and bls-share.hex for each validator
# and public_polynomial.hex for the genesis
./target/release/private-node keygen \
    --num-validators 4 \
    --threshold 3 \
    --output-dir deploy/keys
```

### 2. Prepare Genesis

```bash
# Copy the template
cp deploy/genesis.template.json deploy/genesis.json

# Replace placeholders with actual keys:
# - VALIDATOR_*_PUBKEY → ed25519 public keys from keygen
# - PUBLIC_POLYNOMIAL_HEX → from keygen output
```

### 3. Start the Network

```bash
cd deploy
docker compose up -d
```

### 4. Monitor

```bash
# Check block production
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'

# Watch logs
docker compose logs -f v1

# Check consensus health
docker compose logs v1 2>&1 | grep "FCU"
```

### 5. Stop

```bash
docker compose down
# To also remove data:
docker compose down -v
```

## Configuration

### Consensus Parameters

| Parameter | Default | Local | Description |
|-----------|---------|-------|-------------|
| `wait-for-proposal` | 2s | 2s | Time to wait for block proposal |
| `wait-for-notarizations` | 2s | 2s | Time for quorum notarizations |
| `time-to-build-proposal` | 500ms | 500ms | Block build time budget |
| `fcu-heartbeat-interval` | 5m | 5m | FCU keepalive interval |
| `views-to-track` | 256 | 256 | Activity timeout in views |
| `allow-private-ips` | false | true | Allow Docker network IPs |

### Genesis Fields

```json
{
  "config": {
    "chainId": 19260817,
    "epochLength": 100,
    "validators": [...],
    "publicPolynomial": "0x..."
  }
}
```

- **`epochLength`**: Blocks per epoch (DKG boundary). Default: 100.
- **`validators`**: Array of `{pubkey, address}` for each validator.
- **`publicPolynomial`**: BLS12-381 threshold public polynomial from DKG.

### Resource Requirements per Container

| Resource | Limit |
|----------|-------|
| Memory | 3500 MB |
| CPU | 1.5 cores |
| Disk | ~50 GB (initial) |

## Performance

### Expected Timing

- **Block latency**: ~110ms per block
- **Epoch finalization**: ~11 seconds (100 blocks)
- **Target finality**: Under 1 second for notarization

### Tuning for Lower Latency

```bash
--consensus.time-to-build-proposal 100ms
--consensus.wait-for-proposal 500ms
--consensus.wait-for-notarizations 500ms
```

## Troubleshooting

### Common Issues

1. **"channel closed before handle"**: Consensus thread started before Reth finished launching. Check for Reth startup errors.

2. **"payload status was neither valid nor syncing"**: EL rejected a finalized block. Check genesis config matches across all validators.

3. **FCU heartbeat warnings**: Normal during initial sync. Should resolve once all validators are connected.

4. **Key mismatch**: Ensure each validator has its own unique `ed25519.hex` and `bls-share.hex`. The shares must come from the same DKG ceremony.

### Logs

```bash
# Reth EL logs
docker compose logs v1 2>&1 | grep -E "execution|rpc|engine"

# Commonware CL logs  
docker compose logs v1 2>&1 | grep -E "consensus|simplex|marshal"
```
