# 페이로드 빌더 (Payload Builder) 분석

> 분석일: 2026-02-19  
> 소스 경로: `crates/payload/` (6개 하위 crate)

---

## 1. 개념 개요

### Payload란?

PoS 이더리움에서 **Execution Layer(reth)가 빌드하는 블록의 실행 데이터**입니다. Consensus Layer(Beacon 노드)가 `engine_forkchoiceUpdatedV3`를 호출하여 블록 생성을 요청하면, reth는 Payload(트랜잭션을 포함한 블록)를 빌드합니다.

### Java 비유

```
Java 기술                              reth Payload Builder
─────────────────                      ─────────
ScheduledExecutorService      ═══      PayloadBuilderService (백그라운드 빌드)
Callable<BlockResult>         ═══      PayloadJob (점진적 개선 Future)
Factory Pattern               ═══      PayloadJobGenerator (Job 생성)
CompletionService             ═══      PayloadStore (결과 조회)
```

---

## 2. 전체 흐름

```
Beacon Node (CL)                              reth (EL)
     │                                           │
     │  engine_forkchoiceUpdatedV3               │
     │  (with payload_attributes)                │
     │ ─────────────────────────────────────────→ │
     │                                           │
     │                              PayloadBuilderService
     │                                    │
     │                          PayloadJobGenerator
     │                          .new_payload_job(attr)
     │                                    │
     │                              ┌─────▼──────┐
     │                              │ PayloadJob  │ (백그라운드 Task)
     │                              │             │
     │                              │  빈 블록 생성  ← 즉시 (Fall-back)
     │                              │  TX 추가 빌드  ← 반복 (점진적)
     │                              │  최적 블록 유지 ← 계속 갱신
     │                              └─────┬──────┘
     │                                    │
     │  engine_getPayloadV4               │
     │ ─────────────────────────────────→  │
     │                                    │
     │  ← best_payload 반환       resolve() 호출
     │    (가장 수익성 높은 블록)           │
```

---

## 3. Crate 구조

```
crates/payload/
├── builder/             ★ 핵심 trait + PayloadBuilderService
│   ├── traits.rs        PayloadJob, PayloadJobGenerator trait
│   └── service.rs       PayloadBuilderService, PayloadBuilderHandle
├── builder-primitives/  PayloadBuilderError 등 기본 타입
├── primitives/          BuiltPayload, PayloadBuilderAttributes trait
├── basic/               ★ BasicPayloadJobGenerator (기본 구현체)
├── util/                유틸리티
└── validator/           페이로드 검증
```

---

## 4. 핵심 타입 분석

### 4.1 PayloadJob — 점진적 블록 빌드

> 소스: `crates/payload/builder/src/traits.rs` L20~L82

```
trait PayloadJob: Future {
    type PayloadAttributes;       // 빌드 입력 파라미터
    type BuiltPayload;            // 빌드된 블록
    type ResolvePayloadFuture;    // 최종 해결 Future

    fn best_payload()  → BuiltPayload     // ★ 현재까지 최선의 블록
    fn payload_attributes() → Attributes  // 빌드 속성 조회
    fn resolve_kind(kind) → (Future, Keep) // ★ CL 요청 시 최종 블록 반환
}
```

**핵심 설계 원칙:**
1. `PayloadJob`은 **Future** — 백그라운드에서 계속 돌며 더 나은 블록을 빌드
2. `best_payload()`는 **항상** 유효한 블록 반환 가능해야 함 (빈 블록이라도)
3. CL 요청 시 `resolve_kind()`로 1초 이내에 응답해야 함
4. `KeepPayloadJobAlive::Yes/No`로 resolve 후에도 계속 빌드할지 결정

### 4.2 PayloadJobGenerator — Job 팩토리

> 소스: `crates/payload/builder/src/traits.rs` L95~L121

```
trait PayloadJobGenerator {
    type Job: PayloadJob;

    fn new_payload_job(attr) → Result<Job>    // ★ 새 빌드 작업 생성
    fn on_new_state(notification)              // 체인 상태 변경 시 캐시 갱신
}
```

### 4.3 PayloadBuilderService — 서비스 루프

> 소스: `crates/payload/builder/src/service.rs` L191~

```
PayloadBuilderService {
    generator: Gen,                     // PayloadJobGenerator 인스턴스
    payload_jobs: Vec<(Job, PayloadId)>, // 활성 빌드 작업 목록
    service_tx/rx: mpsc channel,        // 명령 수신 채널
    payload_events: broadcast,          // 이벤트 브로드캐스트
}
```

**폴링 루프:**
```
poll() {
  1. 채널에서 명령 수신 (NewPayload, BestPayload, Resolve 등)
  2. 각 payload_job을 폴링 → 더 나은 블록이 나오면 업데이트
  3. 타임아웃된 job 정리
}
```

### 4.4 BasicPayloadJobGenerator — 기본 구현

> 소스: `crates/payload/basic/src/lib.rs` L51~L66

```
BasicPayloadJobGenerator {
    client: Client,          // 상태 DB 접근
    executor: Tasks,         // 태스크 실행기
    config: Config,          // 빌드 설정 (간격, 데드라인 등)
    builder: Builder,        // 실제 블록 빌드 로직 (PayloadBuilder trait)
    pre_cached: Option<...>, // 부모 블록 캐시
    payload_task_guard: ..., // 동시 빌드 제한 (Semaphore)
}
```

**빌드 설정 (BasicPayloadJobGeneratorConfig):**

| 설정 | 기본값 | 설명 |
|------|--------|------|
| `interval` | 1초 | 새 페이로드 빌드 주기 |
| `deadline` | 12초 | 최대 빌드 시간 |
| `max_payload_tasks` | 3 | 동시 빌드 Task 수 |

---

## 5. PayloadBuilderHandle — 외부 API

> 소스: `crates/payload/builder/src/service.rs` L112~L181

Engine API가 PayloadBuilderService와 통신하는 인터페이스:

| 메서드 | 호출 시점 | 동작 |
|--------|----------|------|
| `send_new_payload(attr)` | `engine_forkchoiceUpdated` | 새 빌드 작업 시작 → PayloadId 반환 |
| `best_payload(id)` | `engine_getPayload` (조회) | 현재까지 최선의 블록 반환 (job은 계속) |
| `resolve_kind(id, kind)` | `engine_getPayload` (최종) | 최종 블록 반환 (job 종료 가능) |
| `payload_timestamp(id)` | 내부 | 빌드 중인 페이로드 타임스탬프 |

---

## 6. 블록 빌드 과정 상세

```
1. CL → engine_forkchoiceUpdatedV3(attributes)
   │
2. PayloadBuilderService → generator.new_payload_job(attributes)
   │
3. BasicPayloadJob 생성
   │  ├─ 즉시: 빈 블록(또는 최소 블록) 빌드 → best_payload로 저장
   │  └─ 백그라운드 Task 시작
   │
4. 반복 빌드 (interval마다)
   │  ├─ TX Pool에서 수익성 높은 트랜잭션 선택
   │  ├─ EVM으로 트랜잭션 실행
   │  ├─ 가스 한도 내에서 최대한 포함
   │  ├─ 블록 수수료 수익 계산
   │  └─ 이전보다 나은 블록이면 best_payload 갱신
   │
5. CL → engine_getPayloadV4(id)
   │
6. resolve_kind(PayloadKind::Earliest)
   │  ├─ 최선의 블록 반환
   │  └─ KeepPayloadJobAlive::No → job 종료
```

---

## 7. 커스터마이징 포인트

| 포인트 | 방법 | 용도 |
|--------|------|------|
| **커스텀 블록 빌더** | `PayloadBuilder` trait 구현 | TX 선택 로직, 블록 구조 변경 |
| **커스텀 Job 생성기** | `PayloadJobGenerator` trait 구현 | 빌드 전략 전체 변경 |
| **빌드 설정 조정** | `BasicPayloadJobGeneratorConfig` | 빌드 간격, 데드라인, 동시 작업 수 |
| **TX 순서 전략** | `PayloadBuilder` 내부 | MEV 추출, 우선순위 전략 |
| **빈 블록 정책** | `PayloadJob::best_payload()` | 초기 블록의 내용 결정 |
| **Pre-caching** | `on_new_state()` | 부모 블록 상태 사전 캐시 |
