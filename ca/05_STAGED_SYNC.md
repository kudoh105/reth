# Staged Sync (동기화 파이프라인) 분석

> 분석일: 2026-02-19  
> 소스 경로: `crates/stages/api/`, `crates/stages/stages/`, `crates/stages/types/`

---

## 1. 개념 개요

### Staged Sync란?

블록체인 동기화를 **여러 단계(Stage)**로 나누어 **순차적으로** 실행하는 아키텍처입니다. Erigon(Go 이더리움 클라이언트)에서 처음 도입했고, reth가 이를 Rust로 구현했습니다.

### Java 비유: Spring Batch 파이프라인

```
Spring Batch                          reth Staged Sync
─────────────────                     ─────────────────
Job                          ═══      Pipeline
Step                         ═══      Stage
JobExecution                 ═══      run_loop()
StepExecution                ═══      execute()
ItemReader/Writer/Processor  ═══      Stage::execute() 내부 로직
rollback / compensate        ═══      Stage::unwind()
JobRepository (체크포인트)    ═══      StageCheckpoint (DB에 저장)
```

### 왜 단계별로 나누는가?

1. **관심사 분리**: 각 Stage가 하나의 작업만 담당 (헤더 다운로드, 바디 다운로드, 실행 등)
2. **체크포인팅**: 각 Stage 완료 후 DB에 진행상황 저장 → 중단 시 이어서 재개 가능
3. **되감기(Unwind)**: 잘못된 블록 발견 시 **역순으로** 각 Stage를 되감는 것이 가능
4. **디버깅 용이**: 어느 Stage에서 문제가 발생했는지 명확히 파악 가능

---

## 2. 전체 파이프라인 실행 흐름

```
┌─────────────────────────────────────────────────────────────┐
│                     Pipeline.run_loop()                      │
│                                                              │
│  ┌──────────────────────────────────────────────────────┐    │
│  │  1. move_to_static_files()                            │    │
│  │     → DB 데이터를 Static File로 이동                   │    │
│  └──┬───────────────────────────────────────────────────┘    │
│     │                                                        │
│     ▼                                                        │
│  ┌──────────────────────────────────────────────────────┐    │
│  │  2. for each stage in stages:                         │    │
│  │     execute_stage_to_completion(stage)                 │    │
│  │                                                        │    │
│  │     ┌─────────────────────────────────────────────┐   │    │
│  │     │  a. poll_execute_ready()  ← 비동기 준비 대기 │   │    │
│  │     │  b. execute()             ← 실제 Stage 실행  │   │    │
│  │     │  c. save_stage_checkpoint ← 체크포인트 저장   │   │    │
│  │     │  d. commit()              ← DB 커밋          │   │    │
│  │     │  e. post_execute_commit() ← 후처리 훅        │   │    │
│  │     │                                              │   │    │
│  │     │  done == false → 다시 b로 루프               │   │    │
│  │     │  done == true  → 다음 Stage로                │   │    │
│  │     └─────────────────────────────────────────────┘   │    │
│  └──┬───────────────────────────────────────────────────┘    │
│     │                                                        │
│     ▼  에러 발생 시                                          │
│  ┌──────────────────────────────────────────────────────┐    │
│  │  3. unwind(target_block)                              │    │
│  │     → 모든 Stage를 **역순으로** 되감기                 │    │
│  └──────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

> **소스**: `crates/stages/api/src/pipeline/mod.rs` — `run_loop()` (L223~L259)

---

## 3. 15개 Stage 실행 순서 상세

DefaultStages 구성 (소스: `crates/stages/stages/src/sets.rs`):

```
실행 순서   StageId                  역할                           카테고리
────────   ─────────────────────   ──────────────────────────────  ────────
 1         Era                     ERA1 히스토리 파일 임포트 (선택)  Online
 2         Headers                 블록 헤더 다운로드               Online
 3         Bodies                  블록 바디(TX) 다운로드            Online
 ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ 
 4         SenderRecovery          TX 서명 → 보낸 주소 복구         Execution
 5         Execution               블록 실행 (EVM)                 Execution
 ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ 
 6         PruneSenderRecovery     보낸 주소 정리 (설정 시)         Pruning
 ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ 
 7         MerkleUnwind            머클 트라이 되감기 (unwind 전용)  Hashing
 8         AccountHashing          계정 상태 해싱                   Hashing
 9         StorageHashing          스토리지 상태 해싱               Hashing
10         MerkleExecute           머클 트라이 계산 (상태 루트)      Hashing
 ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ 
11         TransactionLookup       TX 해시 → 블록 번호 인덱스       Indexing
12         IndexStorageHistory     스토리지 변경 이력 인덱스         Indexing
13         IndexAccountHistory     계정 변경 이력 인덱스            Indexing
 ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ 
14         Prune                   설정에 따라 오래된 데이터 정리    Pruning
15         Finish                  파이프라인 완료 표시             Control
```

### Stage별 상세 설명

#### Stage 1~3: Online Stages (네트워크 필요)

| Stage | 하는 일 | 입력 | 출력 |
|-------|--------|------|------|
| **Era** | ERA1 아카이브 파일에서 히스토리 블록 임포트 (선택적) | ERA 파일 경로 | 블록 헤더+바디 Static File |
| **Headers** | P2P 네트워크에서 블록 헤더 다운로드 (역순) → ETL로 디스크에 수집 → Static File에 기록 | sync tip (해시) | Static File에 헤더 저장 |
| **Bodies** | 다운로드한 헤더에 대응하는 블록 바디(트랜잭션) 다운로드 | 헤더 범위 | Static File에 TX 저장 |

**HeaderStage 특징** (소스: `crates/stages/stages/src/stages/headers.rs`):
- `poll_execute_ready()`에서 비동기로 헤더를 **역순(새 블록→옛 블록)** 다운로드
- ETL `Collector`에 디스크 기반으로 수집 (메모리 절약)
- `execute()`에서 수집된 헤더를 **정순으로** Static File에 기록

#### Stage 4~5: Execution Stages (핵심 실행)

| Stage | 하는 일 | Java 비유 |
|-------|--------|-----------|
| **SenderRecovery** | 각 트랜잭션의 디지털 서명에서 **보낸 사람 주소**를 복구. `rayon`으로 병렬 처리 | 서명 검증 서비스 |
| **Execution** | 각 블록을 EVM으로 실행. 상태(계정 잔액, 스토리지 등) 변경사항 DB에 기록 | 핵심 비즈니스 로직 실행기 |

**ExecutionStage 특징** (소스: `crates/stages/stages/src/stages/execution.rs`):
- 블록 범위를 순회하며 `executor.execute_one(block)` 호출
- `consensus.validate_block_post_execution()` 으로 실행 후 검증
- 배치 크기(블록 수, 가스, 시간, 변경셋 크기)로 중간 커밋 제어
- ExEx(Execution Extension)에 알림 전송 → 플러그인 시스템과 연동

#### Stage 6: Pruning (선택적)

| Stage | 하는 일 |
|-------|--------|
| **PruneSenderRecovery** | sender recovery 프루닝 모드가 설정되어 있으면 보낸 주소 데이터 정리 |

#### Stage 7~10: Hashing Stages (상태 트라이 계산)

| Stage | 하는 일 |
|-------|--------|
| **MerkleUnwind** | unwind 시 머클 트라이 되감기 (execute 시에는 no-op) |
| **AccountHashing** | 평문(plain) 계정 상태를 해시하여 `HashedAccounts` 테이블에 저장 |
| **StorageHashing** | 평문 스토리지 상태를 해시하여 `HashedStorages` 테이블에 저장 |
| **MerkleExecute** | 해싱된 계정/스토리지에서 **머클 패트리시아 트라이**를 빌드 → **상태 루트(State Root)** 계산 |

`MerkleExecute`는 두 가지 모드로 동작합니다:
- **증분 모드**: 변경된 부분만 트라이 업데이트 (빠름)
- **재구축 모드**: 전체 트라이를 처음부터 빌드 (최초 동기화 시)

#### Stage 11~13: History Indexing Stages

| Stage | 하는 일 | 생성하는 인덱스 |
|-------|--------|----------------|
| **TransactionLookup** | TX 해시로 블록 번호를 조회할 수 있는 인덱스 생성 | `TransactionHashNumbers` |
| **IndexStorageHistory** | 어느 블록에서 어떤 스토리지가 변경되었는지 인덱스 | `StoragesHistory` |
| **IndexAccountHistory** | 어느 블록에서 어떤 계정이 변경되었는지 인덱스 | `AccountsHistory` |

#### Stage 14~15: 마무리

| Stage | 하는 일 |
|-------|--------|
| **Prune** | 설정에 따라 오래된 데이터(receipt, tx sender, 히스토리 등) 정리 |
| **Finish** | 파이프라인 한 사이클 완료 표시. 아무 작업도 하지 않음 |

---

## 4. 핵심 타입 분석

### 4.1 Stage trait (인터페이스)

> 소스: `crates/stages/api/src/stage.rs` L241~L308

```
trait Stage<Provider> {
    fn id()                  → StageId          // 고유 식별자
    fn poll_execute_ready()  → Poll<Result>     // 비동기 준비 (기본: 즉시 Ready)
    fn execute()             → Result<ExecOutput>  // ★ 실제 실행
    fn post_execute_commit() → Result<()>       // 커밋 후 훅 (기본: no-op)
    fn unwind()              → Result<UnwindOutput> // ★ 되감기
    fn post_unwind_commit()  → Result<()>       // 되감기 후 훅 (기본: no-op)
}
```

**Java로 표현하면:**
```java
interface Stage<Provider> {
    StageId id();
    ExecOutput execute(Provider provider, ExecInput input) throws StageError;
    UnwindOutput unwind(Provider provider, UnwindInput input) throws StageError;
    // + optional hooks
}
```

### 4.2 ExecInput / ExecOutput

```
ExecInput {
    target: Option<BlockNumber>,       // 목표 블록 번호
    checkpoint: Option<StageCheckpoint>, // 이전 체크포인트
}

ExecOutput {
    checkpoint: StageCheckpoint,       // 현재 진행 위치
    done: bool,                        // true면 해당 Stage 완료
}
```

- `done: false` → Pipeline이 같은 Stage를 다시 실행 (대용량 데이터 분할 처리)
- `done: true` → 다음 Stage로 넘어감

### 4.3 UnwindInput / UnwindOutput

```
UnwindInput {
    checkpoint: StageCheckpoint,       // 현재 위치
    unwind_to: BlockNumber,            // 되감기 목표 블록
    bad_block: Option<BlockNumber>,    // 문제가 된 블록 (있는 경우)
}

UnwindOutput {
    checkpoint: StageCheckpoint,       // 되감기 후 위치
}
```

### 4.4 StageId (15개 Stage 식별자)

> 소스: `crates/stages/types/src/id.rs` L10~L36

```rust
enum StageId {
    Era, Headers, Bodies,           // Online
    SenderRecovery, Execution,      // Execution  
    PruneSenderRecovery,            // Pruning
    MerkleUnwind, AccountHashing,   // Hashing
    StorageHashing, MerkleExecute,  //
    TransactionLookup,              // Indexing
    IndexStorageHistory,            //
    IndexAccountHistory,            //
    Prune, Finish,                  // 마무리
    Other(&'static str),            // ★ 커스텀 Stage 지원
}
```

`Other(&'static str)`를 통해 **커스텀 Stage를 추가**할 수 있습니다.

### 4.5 Pipeline (파이프라인)

> 소스: `crates/stages/api/src/pipeline/mod.rs` L69~L95

```
Pipeline<N> {
    provider_factory: ProviderFactory<N>,     // DB 접근
    stages: Vec<BoxedStage<...>>,             // ★ Stage 목록 (순서대로)
    max_block: Option<BlockNumber>,           // 최대 블록 (설정 시)
    static_file_producer: ...,                // Static File 생산자
    event_sender: EventSender<PipelineEvent>, // 이벤트 발행
    tip_tx: Option<watch::Sender<B256>>,      // 동기화 타겟 블록 해시
    fail_on_unwind: bool,                     // unwind 시 실패 처리 여부
    ...
}
```

---

## 5. Unwind (되감기) 흐름

Pipeline에서 에러 발생 시 되감기 프로세스:

```
에러 발생 (예: 블록 5000에서 검증 실패)
    │
    ▼
Pipeline.unwind(target=4999)
    │
    ▼  Stage를 **실행 역순**으로 되감기
    │
    ├─ Finish.unwind()           ← 15번째 (마지막)
    ├─ Prune.unwind()
    ├─ IndexAccountHistory.unwind()
    ├─ IndexStorageHistory.unwind()
    ├─ TransactionLookup.unwind()
    ├─ MerkleExecute.unwind()
    ├─ StorageHashing.unwind()
    ├─ AccountHashing.unwind()
    ├─ MerkleUnwind.unwind()
    ├─ PruneSenderRecovery.unwind()
    ├─ Execution.unwind()        ← 변경된 계정/스토리지 상태 복원
    ├─ SenderRecovery.unwind()
    ├─ Bodies.unwind()
    ├─ Headers.unwind()
    └─ Era.unwind()              ← 1번째 (처음)
```

> **소스**: `crates/stages/api/src/pipeline/mod.rs` — `unwind()` (L296~L411)

각 Stage의 `unwind()`는:
1. 해당 Stage가 저장한 데이터를 `unwind_to` 블록까지 삭제
2. 체크포인트 업데이트
3. DB 커밋

---

## 6. StageSet 패턴 — Stage 조합

> 소스: `crates/stages/stages/src/sets.rs`

Stage들을 논리적 그룹으로 묶어서 관리합니다. **Java의 `@Configuration` 클래스에서 `@Bean`들을 그룹으로 등록하는 것**과 유사합니다.

```
DefaultStages = OnlineStages + OfflineStages + FinishStage
                     │              │
                     │              ├── ExecutionStages (SenderRecovery + Execution)
                     │              ├── PruneSenderRecovery (선택)
                     │              ├── HashingStages (Merkle + Hashing)
                     │              ├── HistoryIndexingStages (Lookup + History)
                     │              └── PruneStage
                     │
                     ├── EraStage (선택)
                     ├── HeaderStage
                     └── BodyStage
```

---

## 7. 에러 처리 전략

> 소스: `crates/stages/api/src/pipeline/mod.rs` — `on_stage_error()` (L531~L642)

| 에러 종류 | 처리 방식 |
|----------|----------|
| **DetachedHead** (분기 감지) | 일정 깊이만큼 unwind 후 재시도. 연속 실패 시 더 깊이 unwind |
| **Validation Error** | 이전 체크포인트로 unwind |
| **Execution Error** | 이전 체크포인트로 unwind |
| **MissingStaticFileData** | 해당 블록 -1로 unwind |
| **Fatal Error** | 파이프라인 중단 (복구 불가) |
| **기타 에러** | 재시도 가능으로 판단, 트랜잭션 버리고 Stage 재실행 |

---

## 8. Static File과의 연동

Pipeline은 매 `run_loop()` 시작 시 `move_to_static_files()`를 호출합니다:

```
1. static_file_producer.copy_to_static_files()
   → DB의 완료된 데이터를 Static File (NippyJar 압축)로 복사

2. PrunerBuilder.build() → pruner.run(prune_tip)
   → Static File로 이동 완료된 데이터를 DB에서 삭제
```

이 패턴은 **DB 크기를 작게** 유지하면서 히스토리 데이터를 **압축 파일로** 보관하는 전략입니다.

---

## 9. 커스터마이징 포인트

| 포인트 | 방법 | 용도 |
|--------|------|------|
| **커스텀 Stage 추가** | `StageId::Other("MyStage")` + `impl Stage` | 추가 인덱싱, 검증 로직 |
| **Stage 순서 변경** | `PipelineBuilder::add_stage()` 순서 조정 | Stage 실행 순서 제어 |
| **Stage 교체** | 기존 Stage 대신 커스텀 구현 등록 | EVM 실행 로직 변경 |
| **임계값 조정** | `ExecutionStageThresholds` (블록/가스/시간) | 배치 크기 튜닝 |
| **프루닝 설정** | `PruneModes` | 어떤 데이터를 얼마나 보관할지 |
