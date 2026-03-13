# V2 노드 배포 패키지

이 디렉토리를 V2 서버에 통째로 복사하여 실행합니다.
전체 배포 절차는 `stg/SETUP_GUIDE.md`를 참고하세요.

## 이 노드 정보

| 항목 | 값 |
|------|-----|
| **노드 번호** | V2 |
| **Consensus 포트** | 8000 |
| **ETH P2P 포트** | 30303 |
| **RPC 포트** | 8545 |
| **Fee Recipient** | 0x0000...0002 |
| **Consensus PubKey** | `0xf9bae1f8b538d0959878a79265c8aad301073919081e031f3b8458a9c1ea6f9b` |
| **ETH Node ID** | `7133da97e86b56c0...` (keys/nodekey에서 고정) |

## 빠른 시작

```bash
# 1. IP 설정
cp .env.example .env
vi .env   # V1_IP ~ V4_IP 입력

# 2. 이미지 로드 (빌드 서버에서 전송받은 tar.gz)
docker load < private-node.tar.gz

# 3. genesis.json 복사 후 기동
docker compose up -d

# 4. 로그 확인
docker compose logs -f
```

## 운영 명령어

```bash
docker compose logs -f                    # 로그 스트리밍
docker compose ps                         # 상태 확인
docker compose down                       # 정지
docker compose down -v                    # 정지 + 데이터 초기화

# 블록 높이 확인
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'

# peer 연결 확인
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"net_peerCount","params":[],"id":1}'
```
