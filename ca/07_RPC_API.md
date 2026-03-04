# RPC API 분석

> 분석일: 2026-02-19  
> 소스 경로: `crates/rpc/` (12개 하위 crate)

---

## 1. 개념 개요

### RPC란?

JSON-RPC 2.0 프로토콜을 통해 외부 클라이언트(지갑, DApp, 모니터링 도구)가 이더리움 노드와 통신하는 인터페이스입니다. HTTP, WebSocket, IPC 세 가지 전송 프로토콜을 지원합니다.

### Java 비유

```
Java 기술                              reth RPC
─────────────────                      ─────────
Spring Web @RestController    ═══      RPC Server (jsonrpsee)
@RequestMapping("/api/v1")    ═══      RethRpcModule::Eth / Debug / ...
DTO (Request/Response)        ═══      alloy-rpc-types
Service Layer                 ═══      EthApi, DebugApi, TraceApi
Repository Layer              ═══      Provider (DB 접근)
Spring WebFlux                ═══      async RPC handlers (tokio)
```

---

## 2. 전체 아키텍처

```
외부 클라이언트 (MetaMask, ethers.js, curl)
    │
    │  JSON-RPC 2.0 (HTTP / WebSocket / IPC)
    ▼
┌──────────────────────────────────────────────┐
│            RPC Server (jsonrpsee)             │
│                                              │
│  ┌─────────────────────────────────────────┐ │
│  │         RpcModuleBuilder                 │ │
│  │                                         │ │
│  │  ┌──────┐ ┌──────┐ ┌──────┐ ┌──────┐  │ │
│  │  │ eth_ │ │debug_│ │trace_│ │admin_│  │ │
│  │  └──┬───┘ └──┬───┘ └──┬───┘ └──┬───┘  │ │
│  │     │        │        │        │       │ │
│  │  ┌──┴────────┴────────┴────────┴────┐  │ │
│  │  │         공통 의존성               │  │ │
│  │  │  Provider | Pool | Network      │  │ │
│  │  │  EvmConfig | Consensus          │  │ │
│  │  └─────────────────────────────────┘  │ │
│  └─────────────────────────────────────────┘ │
└──────────────────────────────────────────────┘
```

---

## 3. RPC Crate 구조

> 소스: `crates/rpc/` (12개 하위 crate)

```
crates/rpc/
├── rpc-api/          ★ RPC trait 정의 (서버/클라이언트 인터페이스)
├── rpc/              ★ RPC trait 구현체 (실제 로직)
├── rpc-builder/      ★ RPC 서버 빌더 (모듈 조립 + 서버 시작)
├── rpc-eth-api/      eth_ 네임스페이스 API trait
├── rpc-eth-types/    eth_ 관련 타입 + 에러
├── rpc-convert/      블록체인 타입 ↔ RPC 타입 변환
├── rpc-engine-api/   Engine API (CL ↔ EL 통신)
├── rpc-server-types/ RPC 서버 공통 타입
├── rpc-layer/        미들웨어 레이어 (인증 등)
├── rpc-testing-util/ RPC 테스트 유틸리티
├── rpc-e2e-tests/    엔드투엔드 테스트
└── ipc/              IPC 전송 구현
```

---

## 4. RPC 네임스페이스 (15개)

### 4.1 표준 이더리움 네임스페이스

| 네임스페이스 | RPC 예시 | 설명 |
|-------------|---------|------|
| **eth_** | `eth_getBalance`, `eth_sendTransaction`, `eth_call` | 핵심 이더리움 API. 가장 빈번히 호출 |
| **net_** | `net_version`, `net_peerCount` | 네트워크 정보 |
| **web3_** | `web3_clientVersion`, `web3_sha3` | 기본 유틸리티 |
| **debug_** | `debug_traceTransaction`, `debug_traceBlockByNumber` | 트랜잭션 디버깅/추적 |
| **trace_** | `trace_transaction`, `trace_block` | OpenEthereum 호환 트레이싱 |
| **txpool_** | `txpool_content`, `txpool_status` | 트랜잭션 풀 조회 |
| **admin_** | `admin_addPeer`, `admin_nodeInfo` | 노드 관리 |
| **miner_** | `miner_setGasPrice` (PoS에서는 제한적) | 마이너 제어 (레거시) |

### 4.2 Engine API (Consensus Layer 연동)

| 네임스페이스 | RPC 예시 | 설명 |
|-------------|---------|------|
| **engine_** | `engine_newPayloadV4`, `engine_forkchoiceUpdatedV3` | **CL(Beacon)↔EL(reth) 통신** |

> Engine API는 PoS 이더리움에서 가장 중요한 RPC 인터페이스입니다.
> Consensus Layer(Beacon 노드)가 이 API를 호출하여 블록 생성/검증을 지시합니다.

### 4.3 확장 네임스페이스

| 네임스페이스 | 설명 |
|-------------|------|
| **reth_** | reth 고유 API (내부 상태 조회 등) |
| **otterscan_** | Otterscan 블록 탐색기 호환 API |
| **rpc_** | RPC 모듈 관리 (활성화된 모듈 조회 등) |
| **mev_** | MEV (Maximal Extractable Value) 관련 API |
| **testing_** | 테스트용 API |
| **validation_** | 블록 제출 검증 API |

---

## 5. 핵심 컴포넌트 상세

### 5.1 RPC trait 패턴 (인터페이스 ↔ 구현 분리)

```
crates/rpc/rpc-api/          crates/rpc/rpc/
(trait 정의)                  (구현체)
─────────────                 ─────────
EthApiServer (trait)    ←──   EthApi (struct)
DebugApiServer (trait)  ←──   DebugApi (struct)
TraceApiServer (trait)  ←──   TraceApi (struct)
AdminApiServer (trait)  ←──   AdminApi (struct)
...                           ...
```

**Java로 비유하면:**
```java
// rpc-api: 인터페이스 정의
public interface EthApiServer {
    String eth_getBalance(String address, String blockNumber);
    String eth_call(CallRequest request, String blockNumber);
}

// rpc: 구현
@Service
public class EthApi implements EthApiServer {
    @Override
    public String eth_getBalance(String address, String blockNumber) {
        return provider.getBalance(address, blockNumber);
    }
}
```

### 5.2 RpcModuleBuilder — 빌더 패턴

> 소스: `crates/rpc/rpc-builder/src/lib.rs` L118~L133

```rust
RpcModuleBuilder {
    provider: Provider,      // DB 접근 (블록, 상태 조회)
    pool: Pool,              // 트랜잭션 풀
    network: Network,        // 네트워크 정보
    executor: TaskSpawner,   // 비동기 태스크 실행기
    evm_config: EvmConfig,   // EVM 설정
    consensus: Consensus,    // 합의 엔진
}
```

**사용 예:**
```
let rpc = RpcModuleBuilder::new(provider, pool, network, executor, evm, consensus)
    .module(RethRpcModule::Eth)       // eth_ 네임스페이스 활성화
    .module(RethRpcModule::Debug)     // debug_ 네임스페이스 활성화
    .module(RethRpcModule::Trace)     // trace_ 네임스페이스 활성화
    .build();
```

### 5.3 EthApi — 가장 핵심적인 RPC 구현

> 소스: `crates/rpc/rpc/src/eth/`

`EthApi`는 `eth_` 네임스페이스의 모든 메서드를 구현하며, 가장 복잡한 RPC 모듈입니다:

| 기능 그룹 | 메서드 예시 | 내부 동작 |
|----------|-----------|----------|
| **상태 조회** | `eth_getBalance`, `eth_getCode` | Provider에서 최신/과거 상태 조회 |
| **TX 전송** | `eth_sendRawTransaction` | TX Pool에 트랜잭션 추가 |
| **TX 조회** | `eth_getTransactionByHash` | Pool + DB에서 TX 검색 |
| **블록 조회** | `eth_getBlockByNumber` | Provider에서 블록 조회 |
| **가스 추정** | `eth_estimateGas`, `eth_createAccessList` | EVM 시뮬레이션 실행 |
| **호출 실행** | `eth_call` | EVM에서 읽기 전용 실행 |
| **필터/구독** | `eth_newFilter`, `eth_subscribe` | 이벤트 스트림 + WebSocket |
| **수수료** | `eth_gasPrice`, `eth_feeHistory` | 가스 가격 오라클 |

### 5.4 Engine API — CL ↔ EL 통신

> 소스: `crates/rpc/rpc-engine-api/`

PoS 이더리움에서 **Consensus Layer(Beacon 노드)**가 **Execution Layer(reth)**에게 지시하는 API:

```
Beacon Node (CL)                    reth (EL)
     │                                │
     │  engine_forkchoiceUpdatedV3    │
     │ ──────────────────────────────→│  "이 체인이 canonical입니다"
     │                                │  + "새 블록을 만들어주세요" (선택)
     │                                │
     │  engine_newPayloadV4           │
     │ ──────────────────────────────→│  "이 블록을 검증해주세요"
     │                                │
     │  engine_getPayloadV4           │
     │ ──────────────────────────────→│  "만들어진 블록을 주세요"
     │                                │
```

---

## 6. 전송 프로토콜 (Transport)

| 전송 | 포트 (기본) | 특징 | 용도 |
|------|-----------|------|------|
| **HTTP** | 8545 | 요청-응답, 상태 없음 | 일반 API 호출 |
| **WebSocket** | 8546 | 양방향, 구독 지원 | `eth_subscribe` 실시간 이벤트 |
| **IPC** | reth.ipc | 유닉스 소켓, 로컬 전용 | 로컬 관리, 높은 성능 |
| **Engine** | 8551 | HTTP + JWT 인증 | CL↔EL 통신 (보안) |

---

## 7. 요청 처리 흐름

```
클라이언트: eth_getBalance("0xabc...", "latest")
    │
    ▼
1. jsonrpsee 서버 → JSON 파싱 → 메서드 라우팅
    │
    ▼
2. EthApiServer::eth_getBalance() trait 메서드 호출
    │
    ▼
3. EthApi::balance() 구현체 실행
    │  - 블록 번호 해석 ("latest" → 현재 블록)
    │  - Provider에서 상태 조회
    │  - blocking I/O는 spawn_blocking으로 실행
    │
    ▼
4. 결과를 JSON-RPC response로 직렬화
    │
    ▼
클라이언트: {"result": "0x1234..."}
```

---

## 8. 비동기 처리 전략

> 소스: `crates/rpc/rpc/src/lib.rs` 문서 주석

reth RPC의 중요한 설계 원칙:

1. **모든 RPC 핸들러는 non-blocking이어야 함**
2. 디스크 I/O(DB 읽기)는 blocking → `spawn_blocking`으로 별도 스레드 풀에서 실행
3. `BlockingTaskGuard`로 동시 blocking 작업 수 제한
4. 캐시(`LruCache`)를 활용하여 반복 조회 최소화

```
async fn eth_getBalance() {
    // ✅ Good: blocking I/O를 별도 task로
    let result = task_spawner.spawn_blocking(|| {
        provider.get_account(address, block)
    }).await;

    // ❌ Bad: async 함수에서 직접 blocking I/O 
    // let result = provider.get_account(address, block);
}
```

---

## 9. 커스터마이징 포인트

| 포인트 | 방법 | 용도 |
|--------|------|------|
| **커스텀 RPC 메서드 추가** | `RpcModule::merge()` 또는 커스텀 네임스페이스 구현 | 프로젝트 전용 API 추가 |
| **RPC 네임스페이스 선택** | `RethRpcModule` enum으로 활성화할 모듈 선택 | 필요한 API만 노출 |
| **미들웨어 추가** | `RpcServiceBuilder` + Tower 레이어 | 인증, 로깅, 레이트 리밋 |
| **JWT 인증** | Engine API 기본 옵션 | CL↔EL 통신 보안 |
| **커스텀 EthApi** | `EthApiServer` trait 재구현 | eth_ 메서드 동작 변경 |
| **RPC 타입 커스텀** | `RpcTypes` / `EthApiTypes` trait | 커스텀 블록/TX 타입 반환 |
| **레이트 리밋** | `rate_limiter` 모듈 | API 호출 빈도 제한 |
