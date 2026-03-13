# V1 노드 배포 패키지

이 디렉토리를 V1 서버에 통째로 복사하여 실행합니다.

## 디렉토리 구조

```
v1/
├── docker-compose.yml    # ← 수정 불필요
├── .env.example          # ← .env로 복사 후 IP 입력 필요
├── genesis.json          # ← 빌드 서버에서 복사 필요 (하단 참고)
├── keys/
│   ├── ed25519.hex       # Consensus 서명 키 (고정, 수정 금지)
│   └── nodekey           # ETH P2P 키 (고정, 수정 금지)
└── README.md
```

## 사전 조건

- Docker 및 Docker Compose v2 이상 설치
- 방화벽 포트 오픈: `8545/tcp`, `8000/tcp`, `30303/tcp`, `30303/udp`

## 배포 순서

### Step 1 — 파일 준비

```bash
# 빌드 서버에서 이 디렉토리를 scp로 전송
scp -r v1/ user@<V1_서버_IP>:~/private-chain/

# 또는 rsync
rsync -avz v1/ user@<V1_서버_IP>:~/private-chain/v1/
```

### Step 2 — Docker 이미지 전송

빌드 서버(reth 소스가 있는 곳)에서:
```bash
# 프로젝트 루트에서 이미지 빌드
cd /path/to/reth
docker build -t private-node:latest -f deploy/Dockerfile .

# 이미지 파일로 저장
docker save private-node:latest | gzip > private-node.tar.gz

# V1 서버로 전송
scp private-node.tar.gz user@<V1_서버_IP>:~/private-chain/
```

V1 서버에서:
```bash
cd ~/private-chain
docker load < private-node.tar.gz
```

### Step 3 — genesis.json 복사

```bash
# 빌드 서버에서
scp deploy/genesis.json user@<V1_서버_IP>:~/private-chain/v1/genesis.json
```

### Step 4 — 환경 설정 (.env)

```bash
cd ~/private-chain/v1
cp .env.example .env
```

**.env 파일에서 아래 4개 IP만 입력** (나머지는 수정 금지):

```dotenv
V1_IP=<이 서버(V1)의 공인 IP>
V2_IP=<V2 서버의 공인 IP>
V3_IP=<V3 서버의 공인 IP>
V4_IP=<V4 서버의 공인 IP>
```

### Step 5 — 노드 기동

```bash
cd ~/private-chain/v1
docker compose up -d
```

## 운영 명령어

```bash
# 로그 확인
docker compose logs -f

# 상태 확인
docker compose ps

# 노드 정지
docker compose down

# 데이터 완전 초기화 (주의: 체인 데이터 삭제)
docker compose down -v

# RPC 상태 확인
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'

# 이 노드의 enode 주소 확인
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"admin_nodeInfo","params":[],"id":1}' | python3 -m json.tool
```

## 이 노드 정보

| 항목 | 값 |
|------|-----|
| **노드 번호** | V1 |
| **Consensus 포트** | 8000 |
| **ETH P2P 포트** | 30303 |
| **RPC 포트** | 8545 |
| **Fee Recipient** | 0x0000...0001 |
| **Consensus PubKey** | `0x1b040599fa8420315da0b6ba4fa079171e898c71cbcf7dc3d443a9859b83cfee` |
| **ETH Node ID** | `b3ef3c2b01d44b93...` (keys/nodekey에서 고정) |

## 주의사항

- `keys/` 디렉토리의 파일은 **절대 수정/교체 금지** — 4개 노드의 합의 키가 사전 매칭되어 있음
- `.env`의 `V*_CONSENSUS_PUBKEY`, `V*_ETH_NODEID` 값은 **수정 금지** — 키에서 사전 계산된 고정값
- IP 입력 시 공인(Public) IP 사용 — 사설 IP 입력 시 노드 간 연결 불가
