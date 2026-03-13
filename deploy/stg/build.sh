#!/bin/bash
# =================================================================
# Private Chain 이미지 빌드 및 배포 준비 스크립트
# 실행 위치: reth 프로젝트 루트
#   bash deploy/stg/build.sh
# =================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RETH_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
STG_DIR="${SCRIPT_DIR}"

echo "=================================================="
echo " Private Node 빌드 시작"
echo " Root: ${RETH_ROOT}"
echo "=================================================="

# 1. Docker 이미지 빌드
echo ""
echo "[1/3] Docker 이미지 빌드 중... (약 10~20분 소요)"
cd "${RETH_ROOT}"
docker build -t private-node:latest -f deploy/Dockerfile .
echo "✓ 이미지 빌드 완료"

# 2. 이미지 tar.gz 저장
echo ""
echo "[2/3] 이미지 파일 저장 중..."
docker save private-node:latest | gzip > "${STG_DIR}/private-node.tar.gz"
SIZE=$(du -sh "${STG_DIR}/private-node.tar.gz" | cut -f1)
echo "✓ 저장 완료: stg/private-node.tar.gz (${SIZE})"

# 3. genesis.json을 각 패키지에 복사
echo ""
echo "[3/3] genesis.json 각 패키지에 복사 중..."
for node in v1 v2 v3 v4; do
  cp "${RETH_ROOT}/deploy/genesis.json" "${STG_DIR}/${node}/genesis.json"
  echo "  ✓ ${node}/genesis.json"
done

echo ""
echo "=================================================="
echo " 빌드 완료! 다음 단계:"
echo ""
echo " 각 서버로 패키지 전송:"
echo "   scp -r stg/v1/ user@<V1_IP>:~/private-chain/"
echo "   scp stg/private-node.tar.gz user@<V1_IP>:~/private-chain/"
echo ""
echo " 각 서버에서 실행:"
echo "   docker load < private-node.tar.gz"
echo "   cd v1/ && cp .env.example .env"
echo "   # .env에서 V1_IP ~ V4_IP 입력"
echo "   docker compose up -d"
echo "=================================================="
