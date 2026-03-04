# Reth-Commonware 프라이빗 체인 A-to-Z 가이드

이 문서는 Reth-Commonware 프라이빗 노드를 처음부터 빌드하고, 여러 개의 노드를 Docker로 띄워 블록체인 네트워크를 구성하며, 스마트 컨트랙트를 직접 배포하고 테스트하는 전 과정을 매우 상세히 다룹니다.

## 1. 사전 준비 (Prerequisites)

먼저 시스템에 다음 도구들이 설치되어 있어야 합니다.

*   **OS**: Linux (Ubuntu 22.04+ 권장) 또는 macOS
*   **Rust & Cargo**: [공식 설치 가이드](https://rustup.rs/) (최신 안정화 버전)
*   **Docker & Docker Compose**: 노드 다중 구성을 위해 필수
*   **Foundry**: 이더리움 스마트 컨트랙트 배포 및 테스트 프레임워크 (Reth 완벽 호환)
    ```bash
    curl -L https://foundry.paradigm.xyz | bash
    # 쉘 재시작 후
    foundryup
    ```

## 2. 노드 빌드하기 (Build)

Reth의 성능을 최적화하기 위해 반드시 `--release` 모드로 빌드합니다.

```bash
# 프로젝트 루트 디렉토리(/Users/dohyeong/work/reth/)에서 실행
cargo build -p private-node --release
```
빌드가 완료되면 실행 파일은 `target/release/private-node`에 위치하게 됩니다.

## 3. 키 생성 (Key Generation)

Commonware 합의 알고리즘(P2P 인증, 위원회 식별, 블록 서명) 사용을 위해 각 노드는 고유한 **Ed25519 기반 32바이트 Private Key**가 필요합니다. 터미널의 `openssl` 도구를 사용하여 무작위 16진수 키를 생성할 수 있습니다.

```bash
# 키를 보관할 디렉토리 생성
mkdir -p keys

# 노드 1, 2, 3를 위한 고유 합의 서명 키(Signing Key) 생성 (각 노드마다 다르게 실행)
openssl rand -hex 32 > keys/node1.key
openssl rand -hex 32 > keys/node2.key
openssl rand -hex 32 > keys/node3.key

# (선택) 로컬 테스트용 이더리움 JWT 시크릿 생성
openssl rand -hex 32 > keys/jwt.hex
```

## 4. 단일 노드 띄우기 (실행 파라미터 상세)

첫 번째 노드를 OS 상에서 직접 실행해 봅니다. 테스트 계정에 잔액을 쉽게 할당받기 위해 이더리움 개발 모드 플래그(`--dev`)를 활용하는 것이 편리합니다.

```bash
# 터미널 A (노드 1 실행)
export RUST_BACKTRACE=1
export RUST_LOG=info,reth=debug,commonware=info

./target/release/private-node node \
  --dev \
  --datadir ./data/node1/reth \
  --http \
  --http.addr 0.0.0.0 \
  --http.port 8545 \
  --http.api "eth,net,web3,debug,admin,txpool" \
  --http.corsdomain "*" \
  --authrpc.addr 127.0.0.1 \
  --authrpc.port 8551 \
  --authrpc.jwtsecret ./keys/jwt.hex \
  --consensus.signing-key ./keys/node1.key \
  --consensus.listen-address "0.0.0.0:9000" \
  --consensus.datadir ./data/node1/consensus
```

* **Reth 파라미터**:
  * `--dev`: Anvil의 기본 테스트 계정(미리 ETH가 충전됨)들을 사용할 수 있는 로컬 제네시스 모드 활성화.
  * `--http.*`: 사용자가 메타마스크나 Foundry로 접속할 JSON-RPC 설정.
  * `--authrpc.*`: 엔진 API용 내부 통신 설정.
* **Consensus 파라미터 (`--consensus.*`)**:
  * `--consensus.signing-key`: 앞서 만든 노드의 고유 서명 ID(Hex) 파일.
  * `--consensus.listen-address`: Commonware P2P 리슨 주소.
  * `--consensus.datadir`: 합의 내역 저장 파일 시스템 경로.

## 5. Docker를 활용한 3개 노드 멀티 구성 (Docker Compose)

실제 분산 원장처럼 다중 노드(3대)가 서로 통신하며 합의를 이루도록 `docker-compose.yml`을 구성할 수 있습니다. 각 컨테이너 내부 런타임에서 포트 충돌 없이 작동하며 서브넷을 통해 통신합니다.

프로젝트 최상위 디렉토리에 **`Dockerfile.private-node`**를 만듭니다:
```dockerfile
FROM rust:1.80-bullseye as builder
WORKDIR /app
COPY . .
# 릴리즈 모드로 private-node 애플리케이션만 빌드
RUN cargo build -p private-node --release

FROM debian:bullseye-slim
WORKDIR /app
COPY --from=builder /app/target/release/private-node /usr/local/bin/
RUN apt-get update && apt-get install -y openssl && rm -rf /var/lib/apt/lists/*
ENTRYPOINT ["private-node", "node"]
```

동일한 디렉토리에 **`docker-compose.yml`**을 만듭니다:
```yaml
version: '3.8'

services:
  node1:
    build: 
      context: .
      dockerfile: Dockerfile.private-node
    command: >
      --dev
      --datadir /data/reth 
      --http --http.addr 0.0.0.0 --http.port 8545 --http.api "eth,net,web3,debug" --http.corsdomain "*"
      --consensus.signing-key /keys/node1.key 
      --consensus.listen-address 0.0.0.0:9000 
      --consensus.datadir /data/consensus
      --consensus.worker-threads 2
    ports:
      - "8545:8545"   # Node 1 RPC (호스트 접속용)
    volumes:
      - ./keys:/keys
      - ./data/node1:/data

  node2:
    build: 
      context: .
      dockerfile: Dockerfile.private-node
    command: >
      --dev
      --datadir /data/reth 
      --http --http.addr 0.0.0.0 --http.port 8545 --http.api "eth,net,web3,debug" --http.corsdomain "*"
      --consensus.signing-key /keys/node2.key 
      --consensus.listen-address 0.0.0.0:9000 
      --consensus.datadir /data/consensus
      --consensus.worker-threads 2
    ports:
      - "8546:8545"   # Node 2 RPC
    volumes:
      - ./keys:/keys
      - ./data/node2:/data

  node3:
    build: 
      context: .
      dockerfile: Dockerfile.private-node
    command: >
      --dev
      --datadir /data/reth 
      --http --http.addr 0.0.0.0 --http.port 8545 --http.api "eth,net,web3,debug" --http.corsdomain "*"
      --consensus.signing-key /keys/node3.key 
      --consensus.listen-address 0.0.0.0:9000 
      --consensus.datadir /data/consensus
      --consensus.worker-threads 2
    ports:
      - "8547:8545"   # Node 3 RPC
    volumes:
      - ./keys:/keys
      - ./data/node3:/data
```

**실행 방법**:
```bash
docker-compose up -d --build
```
네트워크를 올리면 3대의 노드가 시작되며, 로그(`docker-compose logs -f`)를 통해 P2P 커버리지 구축 및 컨센서스(합의) 블록이 생성되는 속도(약 150~200ms)를 확인할 수 있습니다.

## 6. 스마트 컨트랙트 배포 및 구동 테스트 (Foundry)

성공적으로 다중 노드가 떴다면, 호스트 머신에서 Node1(`http://127.0.0.1:8545`)의 RPC 포트를 이용해 스마트 컨트랙트를 배치하고 상호작용할 수 있습니다. 

### 6.1 Foundry 프로젝트 생성 및 초기화
별도의 임의 디렉토리에서 아래 명령어를 실행합니다.
```bash
# 기본 카운터 컨트랙트 보일러플레이트 생성 
forge init contract_test
cd contract_test
```
`src/Counter.sol` 파일에 기본적인 Counter 스마트 컨트랙트 코드가 생성되어 있음을 알 수 있습니다.

### 6.2 프라이빗 체인에 컨트랙트 배포 (Deploy)
로컬에 띄운 Reth-Commonware 노드 1번 포트(`8545`)에 컨트랙트를 올려봅니다.
`--dev` 체인의 기본 자금 지원 테스트 키(`0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80`)를 사용합니다.

```bash
forge create src/Counter.sol:Counter \
  --rpc-url http://127.0.0.1:8545 \
  --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
```
명령어 실행 후 터미널에 **Deployed to: `0x...`** 형태의 배포된 컨트랙트 주소(Address)가 출력됩니다. (예: `0x5FbDB2315678afecb367f032d93F642f64180aa3`)

### 6.3 상태 변경 (트랜잭션 발생시키기)
배포된 컨트랙트의 숫자를 바꾸는 쓰기(`cast send`) 수행 방법을 보여줍니다. `[DEPLOYED_ADDRESS]` 공간에 방금 얻은 주소를 넣으세요.
```bash
cast send [DEPLOYED_ADDRESS] "setNumber(uint256)" 100 \
  --rpc-url http://127.0.0.1:8545 \
  --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
```
블록 생성 및 투표 합의 시간(150~200ms)이 굉장히 빠르므로, 이더리움 메인넷 대비 즉시 수행되며 성공한 트랜잭션 영수증 로그를 뿜어냅니다.

### 6.4 상태 조회 (A to B 데이터 전파 확인)
이제 데이터를 **입력했던 노드 1번(`8545`)**과, 트랜잭션 수신 이력이 없지만 합의 과정을 거친 **동기화된 노드 2번(`8546`)** 혹은 **3번(`8547`)**에서 각각 상태를 조회 시도해 봅니다.

```bash
# 노드 1에서 조회
cast call [DEPLOYED_ADDRESS] "number()(uint256)" --rpc-url http://127.0.0.1:8545

# 노드 2에서 조회 (P2P 합의 처리가 정상 작동했다면 당연히 같은 값이 출력됩니다)
cast call [DEPLOYED_ADDRESS] "number()(uint256)" --rpc-url http://127.0.0.1:8546
```
양쪽 명령어 모두 결과값에 `100` (hex: `0x0000..0064`)이 출력된다면, **각 노드가 독립적으로 통신하며 Commonware을 통한 P2P 분산 원장 프라이빗 체인이 완벽하게 구축되어 동작하고 있음**을 의미합니다!
