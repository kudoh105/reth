# Reth 패키지 구조 분석

> 분석일: 2026-02-18  
> 분석 근거: `Cargo.toml`, `docs/repo/layout.md`, 각 crate 디렉토리 구조

---

## 1. 전체 프로젝트 구조 요약

### Java 프로젝트와의 비교

```
reth (Workspace = Maven Multi-Module Project)
├── bin/                  ← 실행 바이너리 (= Java의 main 모듈)
├── crates/               ← 핵심 라이브러리들 (= Java의 서브 모듈들)
├── examples/             ← 사용 예제 (= Java의 examples 모듈)
├── testing/              ← 테스트 유틸리티
├── docs/                 ← 기여자 문서
├── Cargo.toml            ← 워크스페이스 루트 (= parent pom.xml)
└── Cargo.lock            ← 의존성 잠금 파일 (= 특정 버전 고정)
```

### 규모 파악

| 구분 | 수량 |
|------|------|
| 전체 workspace 멤버 수 | **약 120개** |
| crates/ 하위 도메인 디렉토리 | **35개** |
| bin/ 바이너리 | **3개** |
| examples/ | **28개** |
| Rust 에디션 | 2024 |
| MSRV | 1.88 |

---

## 2. 아키텍처 개요도

아래 다이어그램은 reth의 전체 아키텍처를 **레이어 기반**으로 보여줍니다.

```
┌──────────────────────────────────────────────────────────┐
│                    bin/reth (진입점)                       │
│            Java로 치면 Application.main()                  │
├──────────────────────────────────────────────────────────┤
│                     CLI Layer                             │
│              crates/cli/* (명령어 파싱)                     │
├──────────────────────────────────────────────────────────┤
│                   Node Builder                            │
│         crates/node/* (노드 조립 & 시작)                    │
├────────────┬──────────────┬──────────────┬───────────────┤
│  RPC Layer │  Engine API  │  Sync Layer  │ Payload Layer │
│ crates/rpc │  crates/rpc/ │ crates/stages│ crates/payload│
│            │  engine-api  │              │               │
├────────────┴──────────────┴──────────────┴───────────────┤
│                  Execution Layer (EVM)                     │
│           crates/evm/* + crates/revm                      │
├──────────────────────────────────────────────────────────┤
│                  Consensus Layer                          │
│              crates/consensus/*                           │
├──────────────────────────────────────────────────────────┤
│                Transaction Pool                           │
│            crates/transaction-pool                         │
├────────────────────┬─────────────────────────────────────┤
│   Storage Layer    │         Networking Layer             │
│  crates/storage/*  │          crates/net/*                │
│  (MDBX, Provider)  │    (P2P, Discovery, Wire)           │
├────────────────────┴─────────────────────────────────────┤
│                    Primitives                              │
│        crates/primitives, primitives-traits                │
│        crates/chainspec, crates/trie/*                    │
└──────────────────────────────────────────────────────────┘
```

---

## 3. bin/ — 바이너리 (실행 파일)

> **Java 비유**: Spring Boot의 `@SpringBootApplication` main 클래스

| 디렉토리 | 역할 |
|----------|------|
| `bin/reth/` | **메인 실행 바이너리**. 이더리움 풀 노드 기동 |
| `bin/reth-bench/` | 벤치마킹 도구 |
| `bin/reth-bench-compare/` | 벤치마크 결과 비교 도구 |

- `Cargo.toml`의 `default-members = ["bin/reth"]` 이므로, 단순히 `cargo build`하면 `reth` 바이너리만 빌드됩니다.
- `bin/reth/`가 **프로그램의 진입점**입니다. 여기서 CLI 파싱 → 노드 빌더 → 각 레이어 초기화가 시작됩니다.

---

## 4. crates/ — 핵심 라이브러리 도메인별 분석

### 4.1 Primitives (기본 타입) — `crates/primitives*`, `crates/chainspec`

> **Java 비유**: 도메인 모델 패키지 (`model/`, `entity/`)

| 크레이트 | 역할 | Java 비유 |
|---------|------|-----------|
| `crates/primitives/` | Block, Transaction, Receipt 등 핵심 타입 | `domain/` 패키지 |
| `crates/primitives-traits/` | 핵심 타입의 추상 인터페이스 (trait) | `domain/` 패키지의 interface들 |
| `crates/chainspec/` | 체인 설정 (체인 ID, 하드포크 등) | `config/ChainConfig.java` |
| `crates/errors/` | 공통 에러 타입 | `exception/` 패키지 |

**왜 중요한가**: 모든 상위 레이어가 이 타입들에 의존합니다. 커스터마이징 시 가장 먼저 이해해야 할 영역입니다.

### 4.2 Storage (저장소) — `crates/storage/*`

> **Java 비유**: JPA/MyBatis Repository 계층 + 데이터베이스 드라이버

| 크레이트 | 역할 | Java 비유 |
|---------|------|-----------|
| `storage/db/` | MDBX 데이터베이스 추상화 (TX, Cursor, Table) | `JdbcTemplate` / `EntityManager` |
| `storage/db-api/` | 고수준 DB 접근 trait | `Repository` interface |
| `storage/db-common/` | 공유 DB 헬퍼 유틸리티 | `DatabaseUtils.java` |
| `storage/db-models/` | 디스크에 저장되는 테이블 모델 | `@Entity` 클래스들 |
| `storage/storage-api/` | 스토리지 API (상위 컴포넌트용) | `StorageService` interface |
| `storage/provider/` | 고수준 데이터 접근 (블록, TX, 상태) | `BlockRepository`, `TransactionRepository` |
| `storage/rpc-provider/` | RPC 접근 패턴에 특화된 provider | RPC 전용 DAO |
| `storage/codecs/` | 저장 코덱 (직렬화/역직렬화) | Jackson `ObjectMapper` 커스텀 직렬화 |
| `storage/codecs/derive/` | 코덱용 derive 매크로 | Lombok 같은 코드 생성 |
| `storage/libmdbx-rs/` | MDBX C 라이브러리의 Rust 바인딩 | JDBC 드라이버 |
| `storage/nippy-jar/` | 히스토리 데이터 압축 컬럼 저장 | 파일 기반 압축 저장소 |
| `storage/zstd-compressors/` | Zstandard 압축기 | GZIP 압축 유틸리티 |
| `storage/errors/` | 스토리지 에러 타입 | `DataAccessException` |

**핵심 DB**: **MDBX** (Lightning Memory-Mapped Database의 후속). 초고속 키-값 저장소로, Java의 RocksDB/LevelDB에 해당합니다.

### 4.3 Networking (P2P 네트워크) — `crates/net/*`

> **Java 비유**: Netty 기반 P2P 네트워킹 모듈

| 서브 카테고리 | 크레이트 | 역할 |
|-------------|---------|------|
| **코어** | `net/network/` | P2P 네트워크 매니저 (세션, 메시지 송수신) |
| **코어** | `net/network-api/` | 네트워크 컴포넌트 interface (trait) |
| **코어** | `net/network-types/` | 네트워크 공통 타입 |
| **코어** | `net/p2p/` | 고수준 P2P 헬퍼 |
| **코어** | `net/peers/` | 피어 관리, 평점 시스템 |
| **코어** | `net/banlist/` | 피어 차단 목록 |
| **코어** | `net/nat/` | 외부 IP 탐지 (UPnP 등) |
| **디스커버리** | `net/discv4/` | 노드 디스커버리 v4 프로토콜 |
| **디스커버리** | `net/discv5/` | 노드 디스커버리 v5 프로토콜 |
| **디스커버리** | `net/dns/` | DNS 기반 노드 디스커버리 (EIP-1459) |
| **프로토콜** | `net/eth-wire/` | eth 와이어 프로토콜 + RLPx 스택 |
| **프로토콜** | `net/eth-wire-types/` | eth 와이어 공통 타입 |
| **프로토콜** | `net/ecies/` | ECIES 암호화 (RLPx 핸드셰이크) |
| **다운로더** | `net/downloaders/` | 블록 헤더/바디 다운로더 |

**구조 패턴**: API trait → 구현체 → 타입 정의를 분리하는 패턴이 일관되게 사용됩니다. Java의 `interface → ServiceImpl → DTO` 패턴과 유사합니다.

### 4.4 Consensus (합의) — `crates/consensus/*`

> **Java 비유**: 비즈니스 로직 계층에서의 검증(Validator) 역할

| 크레이트 | 역할 |
|---------|------|
| `consensus/common/` | 공통 합의 함수 (수수료 계산 등) |
| `consensus/consensus/` | 합의 엔진 인터페이스 및 구현 |
| `consensus/debug-client/` | 디버깅/테스트용 합의 클라이언트 |

### 4.5 EVM 실행 (Execution) — `crates/evm/*`, `crates/revm/`

> **Java 비유**: 핵심 비즈니스 로직 수행 계층

| 크레이트 | 역할 | Java 비유 |
|---------|------|-----------|
| `evm/evm/` | EVM 설정 trait (체인별 커스텀 가능) | `EVMService` interface |
| `evm/execution-types/` | 실행 관련 타입 | 실행 결과 DTO |
| `evm/execution-errors/` | 실행 에러 타입 | `ExecutionException` |
| `revm/` | revm(Rust EVM) 통합 유틸리티 | EVM 엔진 어댑터 |

**revm**: 별도 프로젝트인 [revm](https://github.com/bluealloy/revm/)을 의존성으로 사용합니다. 실제 EVM 바이트코드 실행은 revm이 담당하고, reth는 이를 감싸서 블록 단위 실행을 관리합니다.

### 4.6 Sync (동기화) — `crates/stages/*`

> **Java 비유**: Spring Batch의 Step과 유사한 파이프라인

| 크레이트 | 역할 |
|---------|------|
| `stages/api/` | Staged Sync 공개 API |
| `stages/stages/` | 개별 Stage 구현 + 파이프라인 드라이버 |
| `stages/types/` | 공유 타입 |

**Staged Sync 아키텍처**: Erigon에서 차용한 패턴으로, 동기화를 여러 **단계(Stage)**로 나누어 순차적으로 실행합니다.
```
Stage 1: 헤더 다운로드 → Stage 2: 바디 다운로드 → Stage 3: 상태 실행 → ...
```
이는 Spring Batch의 `Step1 → Step2 → Step3` 체인과 개념적으로 동일합니다.

### 4.7 Engine (엔진) — `crates/engine/*`

> **Java 비유**: 이벤트 기반 비동기 처리 엔진

| 크레이트 | 역할 |
|---------|------|
| `engine/primitives/` | 엔진 API 기본 타입 |
| `engine/tree/` | 블록 트리 관리 (fork choice 처리) |
| `engine/service/` | 엔진 서비스 (CL과 통신) |
| `engine/local/` | 로컬 엔진 (CL 없이 독립 실행) |
| `engine/util/` | 엔진 유틸리티 |
| `engine/invalid-block-hooks/` | 잘못된 블록 처리 훅 |

**Engine API**: Consensus Layer(CL)와 통신하는 **표준 인터페이스**입니다. CL이 "이 블록을 실행해줘"라고 요청하면, Engine이 받아서 EVM에 전달합니다.

### 4.8 RPC — `crates/rpc/*`

> **Java 비유**: Spring MVC Controller + WebSocket Handler

| 크레이트 | 역할 | Java 비유 |
|---------|------|-----------|
| `rpc/rpc/` | RPC 구현체 (eth_, debug_, net_, trace_ 등) | `@RestController` 모음 |
| `rpc/rpc-api/` | RPC trait 정의 | Controller interface |
| `rpc/rpc-builder/` | RPC 서버 구성 빌더 | Server 설정 `@Configuration` |
| `rpc/rpc-engine-api/` | Engine API 구현 | Engine 전용 API endpoint |
| `rpc/rpc-eth-api/` | `eth_` 네임스페이스 API | `EthController` |
| `rpc/rpc-eth-types/` | `eth_` 네임스페이스 타입 | `EthDTO` |
| `rpc/rpc-server-types/` | RPC 서버 타입 & 상수 | Server 공통 타입 |
| `rpc/rpc-convert/` | reth 타입 ↔ RPC 타입 변환 | `ModelMapper` / `Converter` |
| `rpc/rpc-layer/` | RPC 미들웨어 (인증 등) | `Filter` / `Interceptor` |
| `rpc/rpc-testing-util/` | RPC 테스트 헬퍼 | `MockMvc` 유틸리티 |
| `rpc/ipc/` | IPC 전송 계층 | Unix Socket 통신 |
| `rpc/rpc-e2e-tests/` | E2E 테스트 | 통합 테스트 |

**지원 프로토콜**: JSON-RPC over HTTP, WebSocket, IPC (jsonrpsee 라이브러리 기반)  
**주요 네임스페이스**: `admin_`, `debug_`, `eth_`, `net_`, `trace_`, `txpool_`, `web3_`, `engine_`

### 4.9 Payload (페이로드/블록 빌더) — `crates/payload/*`

> **Java 비유**: 블록 생산 서비스

| 크레이트 | 역할 |
|---------|------|
| `payload/builder/` | 페이로드 빌더 추상화 + 서비스 |
| `payload/basic/` | 기본 페이로드 생성기 |
| `payload/builder-primitives/` | 빌더 공통 primitives |
| `payload/primitives/` | 페이로드 공유 타입 |
| `payload/validator/` | 페이로드 검증 |
| `payload/util/` | 빌더 유틸리티 |

### 4.10 Transaction Pool — `crates/transaction-pool/`

> **Java 비유**: 메시지 큐 / 인메모리 캐시

- 네트워크에서 받은 트랜잭션을 **메모리에 관리**하는 풀
- 블록 빌더가 블록에 포함할 트랜잭션을 이 풀에서 가져감
- Java의 `ConcurrentLinkedDeque` + 우선순위 큐처럼 작동

### 4.11 Ethereum 특화 — `crates/ethereum/*`

> **Java 비유**: 이더리움 메인넷 전용 구현체 (`impl` 모듈)

| 크레이트 | 역할 |
|---------|------|
| `ethereum/node/` | 이더리움 노드 조립 (= 모든 걸 연결) |
| `ethereum/evm/` | 이더리움 전용 EVM 설정 |
| `ethereum/consensus/` | 이더리움 합의 규칙 |
| `ethereum/payload/` | 이더리움 전용 블록 빌더 |
| `ethereum/engine-primitives/` | 이더리움 Engine API 타입 |
| `ethereum/hardforks/` | 하드포크 정의 (Frontier → Shanghai → ...) |
| `ethereum/primitives/` | 이더리움 전용 primitive 타입 |
| `ethereum/cli/` | 이더리움 전용 CLI 옵션 |
| `ethereum/reth/` | 이더리움용 reth 통합 크레이트 |

**핵심 포인트**: reth는 **체인 독립적 아키텍처**를 추구합니다. 공통 trait(인터페이스)은 `crates/` 루트에 있고, 이더리움 전용 구현은 `crates/ethereum/`에 분리합니다. 다른 EVM 체인(Optimism, Polygon 등)도 같은 패턴으로 구현 가능합니다.

### 4.12 Node (노드 조립) — `crates/node/*`

> **Java 비유**: Spring Boot `AutoConfiguration` / DI Container 설정

| 크레이트 | 역할 |
|---------|------|
| `node/builder/` | **노드 빌더** — 모든 컴포넌트를 조립 |
| `node/api/` | 노드 API trait |
| `node/core/` | 노드 핵심 설정 |
| `node/types/` | 노드 타입 정의 |
| `node/events/` | 노드 이벤트 (로그, 동기화 상태 등) |
| `node/metrics/` | 메트릭스 서버 구현 |
| `node/ethstats/` | ethstats 리포팅 |

**Node Builder 패턴**: reth의 **핵심 설계 패턴**입니다. 빌더 패턴을 사용하여 필요한 컴포넌트를 조합합니다.
```rust
// 개념적 예시 (실제 코드 아님)
NodeBuilder::new()
    .with_evm(EthereumEvm)
    .with_consensus(EthereumConsensus)
    .with_rpc(StandardRpc)
    .build()
    .run()
```
이것은 Java의 `@Configuration` + `@Bean`으로 서비스를 조립하는 것과 같은 개념입니다.

### 4.13 CLI — `crates/cli/*`

> **Java 비유**: Spring Boot CLI ArgumentResolver + Picocli

| 크레이트 | 역할 |
|---------|------|
| `cli/cli/` | CLI 프레임워크 |
| `cli/commands/` | 서브커맨드 구현 (node, db, stage 등) |
| `cli/runner/` | CLI 실행 런타임 |
| `cli/util/` | CLI 유틸리티 |

### 4.14 Trie (머클 트라이) — `crates/trie/*`

> **Java 비유**: 상태 증명을 위한 트리 자료구조

| 크레이트 | 역할 |
|---------|------|
| `trie/trie/` | Merkle Patricia Trie 구현 |
| `trie/common/` | 트라이 공통 타입 |
| `trie/db/` | 트라이 DB 통합 |
| `trie/parallel/` | 병렬 트라이 연산 |
| `trie/sparse/` | 희소(Sparse) 트라이 |

**용도**: 이더리움의 **상태 루트(State Root)**, **트랜잭션 루트**, **리시트 루트**를 계산하는 핵심 자료구조입니다.

### 4.15 ExEx (Execution Extensions) — `crates/exex/*`

> **Java 비유**: 이벤트 리스너 / Plugin 시스템

| 크레이트 | 역할 |
|---------|------|
| `exex/exex/` | 실행 확장 프레임워크 |
| `exex/types/` | ExEx 타입 |
| `exex/test-utils/` | ExEx 테스트 유틸 |

**ExEx**: reth의 **플러그인 시스템**입니다. 블록 실행 후 커스텀 로직(인덱싱, 이벤트 처리 등)을 삽입할 수 있습니다. 금융 프로젝트 커스터마이징에서 매우 유용할 수 있는 확장 포인트입니다.

### 4.16 Prune (가지치기) — `crates/prune/*`

| 크레이트 | 역할 |
|---------|------|
| `prune/prune/` | 오래된 데이터 정리 |
| `prune/types/` | 프루닝 타입 |
| `prune/db/` | 프루닝 DB 연산 |

### 4.17 Static File — `crates/static-file/*`

| 크레이트 | 역할 |
|---------|------|
| `static-file/static-file/` | 정적 파일 관리 유틸리티 |
| `static-file/types/` | 정적 파일 타입 |

### 4.18 ERA (역사 데이터) — `crates/era*`

| 크레이트 | 역할 |
|---------|------|
| `era/` | ERA1 형식 지원 (히스토리 데이터 아카이브) |
| `era-downloader/` | ERA 파일 다운로더 |
| `era-utils/` | ERA 유틸리티 |

### 4.19 기타 유틸리티

| 크레이트 | 역할 | Java 비유 |
|---------|------|-----------|
| `tasks/` | 비동기 태스크 관리 (패닉 안전) | `ExecutorService` |
| `metrics/` | 메트릭스 수집 | Micrometer |
| `tracing/` | 로깅 (tracing 기반) | SLF4J + Logback |
| `tracing-otlp/` | OpenTelemetry 연동 | OpenTelemetry Java Agent |
| `tokio-util/` | Tokio 비동기 유틸리티 | Reactor Netty 유틸 |
| `config/` | 설정 파일 관리 | `application.yml` 관리 |
| `etl/` | ETL(Extract-Transform-Load) 파이프라인 | Spring Batch ETL |
| `chain-state/` | 체인 상태 관리 | 체인 상태 캐시 |
| `fs-util/` | 파일시스템 유틸리티 | `FileUtils` |
| `stateless/` | Stateless 검증 | 상태 없는 블록 검증 |

---

## 5. examples/ — 예제 프로젝트

> **Java 비유**: `src/main/java/examples/` 또는 별도 예제 모듈

커스터마이징 시 **참고할 핵심 예제들**:

| 예제 | 설명 | 관련도 |
|------|------|--------|
| `custom-evm/` | **EVM 커스터마이징** | ⭐⭐⭐ |
| `custom-dev-node/` | **커스텀 개발 노드** | ⭐⭐⭐ |
| `custom-node-components/` | 노드 컴포넌트 교체 | ⭐⭐⭐ |
| `custom-engine-types/` | 엔진 타입 커스터마이징 | ⭐⭐ |
| `custom-payload-builder/` | 블록 빌더 커스터마이징 | ⭐⭐ |
| `custom-hardforks/` | 커스텀 하드포크 정의 | ⭐⭐ |
| `custom-inspector/` | EVM 인스펙터 커스터마이징 | ⭐⭐ |
| `node-custom-rpc/` | **커스텀 RPC 엔드포인트** | ⭐⭐⭐ |
| `node-builder-api/` | 노드 빌더 API 사용법 | ⭐⭐ |
| `custom-rlpx-subprotocol/` | 커스텀 P2P 서브프로토콜 | ⭐ |
| `custom-rpc-middleware/` | RPC 미들웨어 | ⭐⭐ |
| `custom-beacon-withdrawals/` | 비콘 출금 커스터마이징 | ⭐ |
| `db-access/` | 직접 DB 접근 | ⭐⭐ |
| `exex-*` | ExEx 확장 예제 | ⭐⭐ |
| `precompile-cache/` | 프리컴파일 캐시 | ⭐ |

---

## 6. 의존성 관계 흐름

```
bin/reth
  └── crates/node/builder (노드 조립)
        ├── crates/cli/* (CLI)
        ├── crates/rpc/* (RPC 서버)
        │     └── crates/rpc/rpc-eth-api → crates/storage/provider
        ├── crates/engine/* (Engine API)
        │     └── crates/evm/* + crates/revm (EVM 실행)
        ├── crates/stages/* (동기화)
        │     └── crates/net/* (네트워크에서 블록 다운로드)
        ├── crates/payload/* (블록 빌더)
        │     └── crates/transaction-pool (트랜잭션 풀)
        ├── crates/consensus/* (합의 검증)
        └── crates/storage/* (데이터 저장)
              └── crates/primitives* (기본 타입)
```

---

## 7. 핵심 설계 패턴 요약

### 7.1 Trait 기반 추상화 (= Java Interface 패턴)
```
crates/evm/evm/          ← trait 정의 (ConfigureEvm)
crates/ethereum/evm/     ← 이더리움 구현 (impl ConfigureEvm for EthereumEvm)
```
모든 핵심 컴포넌트가 이 패턴을 따릅니다. 커스터마이징 시 trait을 구현하면 됩니다.

### 7.2 Workspace 의존성 관리
`Cargo.toml`의 `[workspace.dependencies]`에서 모든 의존성 버전을 **한 곳에서 관리**합니다. Java의 parent pom에서 `<dependencyManagement>`를 사용하는 것과 동일합니다.

### 7.3 Feature Flag 기반 조건부 컴파일
많은 크레이트가 `default-features = false`를 사용합니다. Java의 Maven Profile과 유사하게, 필요한 기능만 선택적으로 활성화할 수 있습니다.

### 7.4 체인 독립적 설계
- 코어: `crates/evm/`, `crates/consensus/`, `crates/stages/` (체인 무관)
- 이더리움: `crates/ethereum/*` (이더리움 전용 구현)
- 다른 체인: 같은 패턴으로 `crates/mychain/*` 구현 가능

---

## 8. 금융 프로젝트 커스터마이징 관련 주요 지점

| 커스터마이징 영역 | 관련 크레이트 | 우선순위 |
|-----------------|------------|---------|
| 트랜잭션/블록 구조 변경 | `primitives*`, `ethereum/primitives` | 높음 |
| 합의 알고리즘 변경 | `consensus/*`, `ethereum/consensus` | 높음 |
| EVM 실행 규칙 수정 | `evm/*`, `ethereum/evm`, `revm` | 높음 |
| RPC API 확장 | `rpc/*` | 중간 |
| 블록 생성 로직 변경 | `payload/*` | 중간 |
| 네트워크 프로토콜 변경 | `net/*` | 중간 |
| ExEx로 이벤트 핸들링 | `exex/*` | 중간 |
| 스토리지 최적화 | `storage/*` | 낮음 |

---

## 9. 다음 분석 단계

이 패키지 구조 분석을 기반으로, 다음 단계에서는:

1. **핵심 Primitives 분석** (`crates/primitives*`) — Block, Transaction, Receipt의 구조
2. **Storage 레이어 분석** (`crates/storage/*`) — 데이터가 어떻게 저장/조회되는지
3. **EVM 실행 흐름 분석** (`crates/evm/*` + `crates/revm/`) — 트랜잭션이 어떻게 실행되는지

순서로 깊이 있게 분석할 예정입니다.
