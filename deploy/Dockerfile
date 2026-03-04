# Multi-stage build for the private-node binary
FROM rust:1.88-bookworm AS builder

WORKDIR /usr/src/reth

# Copy workspace files
COPY . .

# Build only the private-node binary
RUN cargo build --release --bin private-node

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/src/reth/target/release/private-node /usr/local/bin/

RUN mkdir -p /data /keys

EXPOSE 8545 8546 8000 8001

ENTRYPOINT ["private-node"]
