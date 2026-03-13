# Private Chain — 별도 서버 배포 가이드

각 노드를 독립 서버에 배포하기 위한 전체 절차.

---

## 디렉토리 구조

```
stg/
├── SETUP_GUIDE.md        ← 이 파일
├── build.sh              ← 이미지 빌드 + 배포 스크립트
├── v1/                   ← V1 서버에 전송하는 패키지
│   ├── docker-compose.yml
│   ├── .env.example      ← IP 입력 후 .env로 복사
│   ├── genesis.json      ← 배포 시 수동 복사 필요 (하단 참고)
│   ├── keys/
│   │   ├── ed25519.hex   ← Consensus 서명 키 (고정)
│   │   └── nodekey       ← ETH P2P 키 (고정)
│   └── README.md
├── v2/  (동일 구조)
├── v3/  (동일 구조)
└── v4/  (동일 구조)
```

---

## 변경 가능한 설정값 vs 고정값

### ★ 수정 필요 — .env 파일 (4개 서버 모두 동일하게 입력)

| 변수 | 설명 | 예시 |
|------|------|------|
| `V1_IP` | V1 서버 공인 IP | `203.0.113.1` |
| `V2_IP` | V2 서버 공인 IP | `203.0.113.2` |
| `V3_IP` | V3 서버 공인 IP | `203.0.113.3` |
| `V4_IP` | V4 서버 공인 IP | `203.0.113.4` |

> 4개 서버의 .env 파일은 모두 동일한 값을 입력합니다.

### 수정 가능 — docker-compose.yml 내 성능 파라미터

노드별 성능 튜닝이 필요한 경우 docker-compose.yml에서 직접 수정:

| 파라미터 | 현재값 | 설명 |
|----------|--------|------|
| `--consensus.wait-for-proposal` | `2000ms` | Voter proposal 대기 시간 |
| `--consensus.time-to-build-proposal` | `1000ms` | 블록 빌드 대기 시간 |
| `--builder.gaslimit` | `220000000` | 블록 최대 가스 한도 |
| `memory` | `8000m` | 컨테이너 메모리 제한 |
| `cpus` | `"3"` | 컨테이너 CPU 제한 |

### 절대 수정 금지

| 파일/변수 | 이유 |
|-----------|------|
| `keys/ed25519.hex` | Consensus 서명 키 - 노드 식별자 |
| `keys/nodekey` | ETH P2P 식별자 - enode 주소 변경됨 |
| `.env`의 `V*_CONSENSUS_PUBKEY` | ed25519.hex에서 수학적으로 고정 |
| `.env`의 `V*_ETH_NODEID` | nodekey에서 수학적으로 고정 |

---

## trusted-peers enode — 최초 기동 시 설정 가능?

### ✅ 가능 — 이 패키지는 최초 기동 시 설정 완료

각 패키지에 `keys/nodekey`가 사전 포함되어 있어, 기동 전에 ETH Node ID가 이미 확정됩니다.

| 설정 | 기동 전 설정 가능 여부 | 근거 |
|------|:---------------------:|------|
| `--consensus.known-peers` | ✅ 가능 | ed25519.hex → 공개키 결정론적 |
| `--trusted-peers` (enode) | ✅ 가능 | nodekey → node ID 결정론적 |

> nodekey 없이 reth를 기동하면 자동 생성되어 node ID가 매번 바뀝니다.
> 이 패키지는 nodekey를 사전 포함하여 이 문제를 해결합니다.

---

## 전체 배포 절차

### Phase 1: 빌드 서버 준비 (1회)

```bash
# reth 소스 루트에서
cd /path/to/reth

# 이미지 빌드 (약 10~20분)
docker build -t private-node:latest -f deploy/Dockerfile .

# 이미지 파일로 저장 (약 200~400MB)
docker save private-node:latest | gzip > private-node.tar.gz

# genesis.json을 각 패키지에 복사
cp deploy/genesis.json deploy/stg/v1/genesis.json
cp deploy/genesis.json deploy/stg/v2/genesis.json
cp deploy/genesis.json deploy/stg/v3/genesis.json
cp deploy/genesis.json deploy/stg/v4/genesis.json
```

또는 `build.sh` 스크립트 사용:
```bash
cd /path/to/reth/deploy/stg
bash build.sh
```

### Phase 2: 각 서버에 패키지 전송

```bash
# V1 서버로 전송
scp -r deploy/stg/v1/ user@<V1_IP>:~/private-chain/
scp deploy/private-node.tar.gz user@<V1_IP>:~/private-chain/

# V2 서버로 전송
scp -r deploy/stg/v2/ user@<V2_IP>:~/private-chain/
scp deploy/private-node.tar.gz user@<V2_IP>:~/private-chain/

# V3, V4 동일하게 반복
```

### Phase 3: 각 서버에서 실행 (서버별 동일)

```bash
cd ~/private-chain

# 1. 이미지 로드
docker load < private-node.tar.gz

# 2. IP 설정 (★ 유일하게 수정하는 부분)
cd v1/   # 각 서버는 해당 번호 디렉토리
cp .env.example .env
vi .env
# → V1_IP, V2_IP, V3_IP, V4_IP 4개만 입력

# 3. 기동
docker compose up -d

# 4. 정상 기동 확인
docker compose logs -f
```

### Phase 4: 연결 상태 확인

4개 노드 모두 기동 후 각 노드에서:

```bash
# Peer 연결 수 (3이면 정상 - 나머지 3개 노드와 연결)
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"net_peerCount","params":[],"id":1}'

# 블록 생성 확인 (0x0이 아니면 정상)
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'

# 동기화 상태 확인
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_syncing","params":[],"id":1}'
```

---

## 방화벽 포트 오픈 (서버별 동일)

각 서버의 방화벽/보안그룹에서 **인바운드** 허용 필요:

| 포트 | 프로토콜 | 용도 | 허용 대상 |
|------|----------|------|-----------|
| `8545` | TCP | JSON-RPC | 클라이언트 IP (또는 전체) |
| `8000` | TCP | Consensus P2P | 다른 3개 노드 IP |
| `30303` | TCP | ETH P2P | 다른 3개 노드 IP |
| `30303` | UDP | ETH P2P (discovery) | 다른 3개 노드 IP |

> P2P 포트(8000, 30303)는 노드 간 통신만 필요하므로 다른 노드 IP만 허용 권장.

---

## 노드별 고정 식별값 요약

### Consensus P2P 공개키 (ed25519)

| 노드 | Pubkey |
|------|--------|
| V1 | `0x1b040599fa8420315da0b6ba4fa079171e898c71cbcf7dc3d443a9859b83cfee` |
| V2 | `0xf9bae1f8b538d0959878a79265c8aad301073919081e031f3b8458a9c1ea6f9b` |
| V3 | `0x291b5fc290d027f7db1143874b4a55001ee148fcc831a1a321ebba3fccd40a38` |
| V4 | `0xa5fda96a0acbccacc7eb6cbe8c7daab95d2fdb07ee7a76214d1ed3fa349a6984` |

### ETH P2P Node ID (secp256k1)

| 노드 | Node ID |
|------|---------|
| V1 | `b3ef3c2b01d44b93523fd72548f26bdedfa81a4a562954642b2df47d4dc909d6fbf3d6ba5be8129a5bca2dbb6e58b2f056c50826c1c9f5920909eb32354c8cd7` |
| V2 | `7133da97e86b56c085ace3af157906abad1a63dd7d46eff88547d1b45f8ebe9e5e91c04e578416cfa48997bf713d9679587a9eb7eee6107984d9fa0bd80e75d9` |
| V3 | `75f30b1daf2495f48e383eb968e412d006c7b6d0ebdff44edb6ecd448f015b2636a7bb9c1537d994828a18b18df704885201f5e8b69e4e0e1e686c5d243ed6d2` |
| V4 | `e2345300377d13deef2b391b0a2838d7801de9e1b8e718e58231bca78f8fdc64e8ca60a80275c8f11891ca34e3cdf5afe800f430fe9ef0a71d61948b354579a7` |

---

## 트러블슈팅

### 노드가 블록을 생성하지 않는 경우

```bash
# 로그에서 consensus 상태 확인
docker compose logs | grep -i "consensus\|proposal\|finali"

# peer 연결 확인
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"net_peerCount","params":[],"id":1}'
```

→ peerCount가 0이면 방화벽 확인 또는 IP 설정 오류.

### ETH P2P peers가 연결되지 않는 경우

```bash
# admin_peers로 연결 상태 확인
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"admin_peers","params":[],"id":1}' | python3 -m json.tool
```

→ `--p2p.secret-key /keys/nodekey`가 정상 적용되었는지 확인.
→ 30303 포트 방화벽 확인.

### 재시작 후 합의가 멈추는 경우

데이터를 유지한 재시작은 정상 동작합니다:
```bash
docker compose down && docker compose up -d
```

데이터를 초기화해야 하는 경우:
```bash
docker compose down -v   # 볼륨 삭제
docker compose up -d     # 제네시스부터 재시작
```
