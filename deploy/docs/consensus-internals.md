# Commonware Consensus — 소스코드 레벨 내부 동작 설명

> 대상 경로: `crates/commonware-consensus/src/`
> 작성 기준: 실제 소스코드 함수명·라인번호 기준

---

## 목차

1. [전체 아키텍처 개요](#1-전체-아키텍처-개요)
2. [Actor 구조 및 채널 맵](#2-actor-구조-및-채널-맵)
3. [Actor별 소스 분석](#3-actor별-소스-분석)
4. [핵심 플로우: Block Proposal](#4-핵심-플로우-block-proposal)
5. [핵심 플로우: Block Verification](#5-핵심-플로우-block-verification)
6. [핵심 플로우: Epoch Transition](#6-핵심-플로우-epoch-transition)
7. [핵심 플로우: Block Finalization](#7-핵심-플로우-block-finalization)
8. [P2P 채널 구성 (config.rs)](#8-p2p-채널-구성-configrs)
9. [Block 타입 구조 (block.rs)](#9-block-타입-구조-blockrs)
10. [코드 품질 리뷰 — 알려진 이슈](#10-코드-품질-리뷰--알려진-이슈)

---

## 1. 전체 아키텍처 개요

### 스레드 분리

```
┌─────────────────────────────────────────────────────────────┐
│  Thread 1: Reth (tokio)                                      │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐  │
│  │ EthereumNode │  │  JSON-RPC    │  │  MDBX Database   │  │
│  │ (block exec) │  │  (8545 port) │  │  (chain state)   │  │
│  └──────┬───────┘  └──────────────┘  └──────────────────┘  │
│         │ PrivateNodeHandle (MPSC, µs 지연)                   │
└─────────┼───────────────────────────────────────────────────┘
          │
┌─────────┼───────────────────────────────────────────────────┐
│  Thread 2: Commonware (commonware_runtime::tokio::Runner)    │
│         │                                                     │
│  ┌──────▼──────────────────────────────────────────────┐    │
│  │                  Engine (engine.rs)                   │    │
│  │  ┌────────────┐  ┌────────────┐  ┌───────────────┐  │    │
│  │  │application │  │  marshal   │  │    executor   │  │    │
│  │  │  actor     │  │   actor    │  │     actor     │  │    │
│  │  └────────────┘  └────────────┘  └───────────────┘  │    │
│  │  ┌────────────┐  ┌────────────┐  ┌───────────────┐  │    │
│  │  │epoch_mngr  │  │ dkg_manager│  │  peer_manager │  │    │
│  │  │  actor     │  │   actor    │  │     actor     │  │    │
│  │  └────────────┘  └────────────┘  └───────────────┘  │    │
│  │  ┌────────────┐                                       │    │
│  │  │ broadcast  │                                       │    │
│  │  │  engine    │                                       │    │
│  │  └────────────┘                                       │    │
│  └─────────────────────────────────────────────────────┘    │
│                                                               │
│  P2P Network (5개 채널: votes/certs/resolver/broadcast/marshal) │
└─────────────────────────────────────────────────────────────┘
```

### 초기화 순서 (`lib.rs::run_consensus_stack()`)

```
1. PrivateKey (Ed25519 서명키) 로드
2. BLS12-381 Share (임계값 서명 지분) 로드 (옵션)
3. Commonware P2P 네트워크 초기화
4. genesis validators에서 known-peers 파싱
5. 5개 P2P 채널 등록 (votes/certs/resolver/broadcast/marshal)
6. Engine::Builder::try_init() 호출 → 7개 Actor 초기화
7. network.start() + engine.start() 동시 실행
8. 어느 쪽이든 종료되면 전체 종료
```

---

## 2. Actor 구조 및 채널 맵

### 내부 MPSC 채널 맵

```
                    ┌─────────────────────────────────────┐
                    │         Simplex BFT Engine           │
                    │    (commonware-simplex 크레이트)      │
                    └──┬──────────────┬──────────────┬────┘
                       │ propose()    │ verify()     │ broadcast()
                       │ (oneshot)    │ (oneshot)    │
                       ▼              ▼              ▼
           ┌───────────────────────────────────────────────┐
           │         application::Actor (bounded MPSC)      │
           │  Messages: Genesis | Propose | Verify |        │
           │           Broadcast                            │
           └──────┬────────────────────┬───────────────────┘
                  │                    │
          marshal.subscribe()   executor.canonicalize_head()
          marshal.proposed()    execution_node.beacon_engine()
          marshal.verified()    execution_node.payload_builder()
                  │
                  ▼
     ┌────────────────────────────────┐
     │    marshal::Actor (internal)   │
     │  - 블록 저장/배포/구독 관리     │
     │  - P2P로 블록 브로드캐스트      │
     └──────────┬─────────────────────┘
                │ Reporter chain (finalized 통지)
         ┌──────▼──────┐
         │  executor   │──── canonicalize_head → EL FCU
         │  ::Actor    │──── finalize → EL new_payload
         └──────┬──────┘
                │ Reporter chain
         ┌──────▼──────────────┐
         │  dkg_manager::Actor │──── epoch_manager.enter/exit
         └──────┬──────────────┘
                │ Reporter chain
         ┌──────▼──────────────┐
         │ epoch_manager::Actor│──── simplex::Engine 생성/종료
         └──────┬──────────────┘     per-epoch BLS Scheme 관리
                │ Reporter chain
         ┌──────▼──────────────┐
         │  peer_manager::Actor│──── oracle.track/overwrite
         └─────────────────────┘     (사설망: no-op)
```

### 채널 타입 요약

| Actor | 채널 타입 | 버퍼 크기 | 비고 |
|-------|-----------|-----------|------|
| `application` | bounded MPSC | `mailbox_size` (설정값) | 역압 적용 |
| `epoch_manager` | unbounded MPSC | 무제한 | 에폭 전환 메시지 |
| `dkg_manager` | unbounded MPSC | 무제한 | 확정 블록 통지 |
| `peer_manager` | unbounded MPSC | 무제한 | 피어 관리 |
| `executor` | unbounded MPSC | 무제한 | FCU/Finalize 명령 |
| Oneshot | 단발성 응답 | 1 | Propose/Verify/Canonicalize 응답 |

---

## 3. Actor별 소스 분석

### 3-1. `consensus/engine.rs` — Engine Builder & Orchestrator

**역할**: 모든 Actor를 초기화·시작·연결하는 최상위 조율자

#### Builder 구조체 (lines 67-92)

```rust
pub struct Builder<TBlocker, TPeerManager> {
    fee_recipient: Address,          // 블록 보상 수취 주소
    execution_node: Option<PrivateNodeHandle>, // EL 핸들 (MPSC 기반)
    blocker: TBlocker,               // P2P 오라클 (Epoch 차단 제어)
    peer_manager: TPeerManager,      // P2P 피어 관리 오라클
    partition_prefix: String,        // DB 파티션 접두사 ("private-chain")
    signer: PrivateKey,              // Ed25519 노드 서명키
    share: Option<Share>,            // BLS12-381 임계값 서명 지분
    mailbox_size: usize,             // application 채널 버퍼
    deque_size: usize,               // simplex 내부 큐 크기
    // 타이밍 파라미터들
    time_for_peer_response: Duration,
    time_to_propose: Duration,       // = time-to-build-proposal
    time_to_collect_notarizations: Duration,
    ...
}
```

#### `try_init()` 초기화 순서 (lines 104-367)

```
1. genesis에서 epoch_length 읽기 (line 124)
2. peer_manager Actor 생성 (lines 134-137)
3. buffered broadcast engine 생성 (lines 139-148)
4. 2개 immutable archive 초기화 (lines 150-206)
   - finalizations_by_height: 높이 → 확정 정보
   - finalized_blocks: Digest → 확정 블록 본문
5. marshal Actor 생성 + genesis 블록 사전 등록 (lines 248-283)
6. executor Actor 생성 (lines 285-295)
7. application Actor 생성 (lines 296-306)
8. epoch_manager Actor 생성 (lines 307-330)
9. dkg_manager Actor 생성 (lines 331-365)
10. Engine 구조체 반환
```

#### `run()` 실행 (lines 451-508)

```rust
// Reporter 체인 구성 (확정 통지 전파 순서)
marshal.start(
    Reporters::from((
        executor_mailbox,        // 1순위: 실행 레이어 FCU
        Reporters::from((
            dkg_manager_mailbox, // 2순위: 에폭 경계 감지
            Reporters::from((
                epoch_manager_mailbox, // 3순위: 에폭 Manager
                Reporters::from((peer_manager_mailbox, executor_mailbox))
            ))
        ))
    )),
    broadcast_mailbox,
    resolver,
)

// 모든 Actor 병렬 실행 + 오류 전파
try_join_all(vec![
    application, broadcast, executor, marshal,
    peer_manager, epoch_manager, dkg_manager
])
```

---

### 3-2. `consensus/application/actor.rs` — Proposal & Verification

**역할**: Simplex BFT의 `Automaton` 트레이트 구현체. 블록 제안·검증·제네시스 처리

#### State 구조체 (lines 34-39)

```rust
struct State {
    execution_node: PrivateNodeHandle, // EL 연결 핸들
    executor: executor::Mailbox,       // FCU 명령 채널
    marshal: marshal::Mailbox,         // 블록 배포 채널
    scheme_provider: SchemeProvider,   // BLS Scheme 조회
}
```

#### `run()` — 메인 메시지 루프 (lines 90-119)

```rust
loop {
    match mailbox.recv().await? {
        Message::Genesis { epoch, response } => {
            let digest = handle_genesis(epoch).await;
            response.send(digest);       // oneshot 응답
        }
        Message::Propose { parent, response, round } => {
            match handle_propose(parent, round).await {
                Ok(digest) => response.send(Ok(digest)),
                Err(e)     => response.send(Err(e)),
            }
        }
        Message::Verify { parent, payload, proposer, response, round } => {
            handle_verify(parent, payload, proposer, response, round).await;
            // ⚠️ 응답은 handle_verify() 내부에서 직접 전송
        }
        Message::Broadcast { payload } => {
            handle_broadcast(payload).await; // 현재 no-op
        }
    }
}
```

#### `handle_genesis()` (lines 121-158)

```
Epoch 0 : EL genesis 블록 해시 반환
           → execution_node.genesis_block_hash()

Epoch N>0: 에폭 경계 블록 해시 반환
           → marshal archive에서 읽기
           → 높이 = epoch_strategy.first(epoch)
              fallback = epoch.get() * epoch_length
           → 없으면 B256::ZERO 반환 (경고 로그)
```

#### `handle_propose()` (lines 160-271) ← **핵심 함수**

```
1. marshal.subscribe(round, parent_digest) 호출
   → 부모 블록 객체 수신 (P2P 대기 포함)

2. ForkchoiceUpdated 전송 (lines 181-213)
   - head_block_hash = parent_hash
   - safe_block_hash  = parent_hash
   - finalized_block_hash = parent_hash
   - PayloadAttributes {
       timestamp: 현재시각,
       prev_randao: B256::ZERO,   // VRF 미구현
       suggested_fee_recipient: fee_recipient,
     }
   → payload_id 수신

3. time_to_propose (= --consensus.time-to-build-proposal) 만큼 대기
   → EL 트랜잭션 수집·실행 시간

4. payload_builder.resolve_kind(payload_id, PayloadKind::WaitForPending)
   → EthBuiltPayload 수신

5. SealedBlock 추출 → Block::from_execution_block() 래핑

6. execution_node.beacon_engine().new_payload(block) 전송
   → EL에 새 블록 등록 (P2P 배포 전 사전 등록)

7. marshal.proposed(round, block) 호출
   → marshal이 블록을 P2P 브로드캐스트 + 로컬 저장

8. block.digest() 반환 → Simplex에 제안 완료 알림
```

#### `handle_verify()` (lines 273-348) ← **검증 함수**

```
1. marshal.subscribe(None, block_digest) 호출
   → None = 공증(notarization) 없이 P2P 전파본 수락
   → 블록 본문 수신 대기

2. marshal.subscribe(None, parent_digest) 호출
   → 부모 블록 본문 수신

3. executor.canonicalize_head(parent_height, parent_digest) 호출
   → EL의 canonical head 갱신 (FCU Safe Head 업데이트)
   ⚠️ 현재 응답(Ok/Err) 확인 안 함 — 아래 이슈 참조

4. execution_node.beacon_engine().new_payload(block) 전송
   → EL에 블록 실행 요청 (트랜잭션 실행 + 상태 전이)

5. payload_status.is_valid() 확인
   - true  → marshal.verified(round, block)
   - false → 경고 로그만 (거부)

6. response.send(is_valid) 호출
   → Simplex에 검증 결과 반환
   ⚠️ 반환값 무시 — 아래 이슈 참조
```

---

### 3-3. `epoch/manager/actor.rs` — Epoch 생명주기 관리

**역할**: 에폭별 Simplex BFT 인스턴스 생성·종료, BLS Scheme 등록·삭제

#### `run()` — Muxer 초기화 (lines 115-195)

```rust
// 3개 P2P 채널을 에폭별로 멀티플렉싱
let vote_mux = Muxer::new(votes_channel);
let certificates_mux = Muxer::new(certificates_channel);
let resolver_mux = Muxer::new(resolver_channel);

loop {
    match mailbox.recv().await {
        Content::Enter(transition) => enter(transition, &muxes).await,
        Content::Exit(exit)       => exit_epoch(exit).await,
        Content::Update(update)   => { /* 확정 블록 수신 확인 */ }
    }
}
```

#### `enter()` — 새 에폭 진입 (lines 197-283)

```
1. 에폭 순서 확인 (재진입 방지)
   epoch > latest_active → continue
   epoch <= latest_active → 경고 후 무시

2. BLS Scheme 생성 (lines 216-233)
   - share 있음: Scheme::signer(public, participants, share, ...)
     → 임계값 서명 참여자 (블록 서명 가능)
   - share 없음: Scheme::verifier(participants, ...)
     → 서명 검증만 가능

3. Simplex BFT 엔진 생성 (lines 235-263)
   - DB 파티션: "{prefix}_consensus_epoch_{epoch}"
   - 타이밍: leader_timeout, notarization_timeout, nullify_timeout, fetch_timeout
   - Automaton: application mailbox
   - Reporter: marshal, peer_manager

4. P2P 채널 등록 (lines 265-267)
   ⚠️ vote_mux.register(epoch).await.unwrap()   ← 패닉 위험
   ⚠️ certificates_mux.register(epoch).await.unwrap()
   ⚠️ resolver_mux.register(epoch).await.unwrap()

5. active_epochs에 엔진 삽입
6. 메트릭 업데이트 (active_epochs++, latest_epoch, signer/verifier 카운터)
```

#### `exit_epoch()` — 에폭 종료 (lines 285-304)

```
1. active_epochs.remove(epoch) → Handle 반환
2. handle.abort() → Simplex BFT 인스턴스 강제 종료
3. scheme_provider.delete(epoch) → BLS Scheme 메모리 해제
4. 메트릭 업데이트 (active_epochs--)
5. 에폭 없으면 경고 로그
```

---

### 3-4. `dkg/manager/mod.rs` — 정적 DKG + 에폭 전환 감지

**역할**: 결정론적 DKG로 BLS 키 생성. 확정 블록 높이 기준 에폭 경계 감지 후 epoch_manager에 Enter/Exit 지시

#### `run()` — 초기화 (lines 113-306)

```
1. genesis validators 파싱 (Ed25519 공개키 리스트)
   → 없으면 즉시 반환

2. 결정론적 DKG 실행 (lines 140-155)
   - Seed = hash(NAMESPACE) ++ hash("PRIVATE_CHAIN_DKG_SEED")
     ⚠️ DefaultHasher 사용 (비암호학적 해시) — 이슈 참조
   - dkg::deal::<MinSig, _, N3f1>(rng, participants)
     → (polynomial, shares) 반환
   - 모든 노드가 동일한 seed → 동일한 shares 생성
     → 각 노드는 자신의 인덱스에 해당하는 share를 사용

3. 시작 에폭 결정 (lines 177-242)
   - 최초 기동: epoch 0 시작
   - 재시작 (finalized blocks 있음):
     last_finalized_height에 해당하는 에폭 계산
     → 불필요한 epoch 0~N 재전환 방지

4. epoch_manager.enter(starting_epoch, polynomial, share, participants)
   → 첫 에폭 진입

5. TODO: peer_manager.track() 연결 미구현 (lines 265-267)

6. 메인 루프: 확정 블록 감시 (lines 269-306)
   Update::Tip(_, height, _):
     block_epoch = epoch_strategy.epoch(height)
     if block_epoch > current_epoch:
       epoch_manager.enter(next_epoch, ...)
       epoch_manager.exit(current_epoch)
       current_epoch = next_epoch
   Update::Block(_, ack):
     ack.ack() // 수신 확인
```

---

### 3-5. `consensus/application/executor/actor.rs` — EL FCU 관리

**역할**: Canonical Head 추적. Forkchoice 상태를 EL에 주기적으로 전송

#### 핵심 동작

```
시작 시 backfill:
  last_consensus_finalized_height (marshal archive에서 읽기)
  last_execution_finalized_height (EL에서 읽기)
  gap만큼 new_payload 재전송 → EL 동기화

canonicalize():
  - FCU 상태가 변경된 경우만 EL에 전송 (중복 방지)
  - head, safe, finalized 해시 모두 업데이트

finalize():
  - 확정된 블록을 EL에 new_payload로 전송
  - EL의 finalized_block_hash 갱신

FCU Heartbeat:
  - 주기적 재전송 (EL이 reorg를 방지하기 위해 유지)
```

---

### 3-6. `consensus/peer_manager/actor.rs` — 피어 집합 관리 (사설망)

**역할**: 공개망 대비 사설망은 정적 validator set → 사실상 no-op

```rust
fn handle_finalized(&mut self, update: Update<Block>) {
    // 사설망: validator set이 genesis에서 고정됨
    // 동적 변경 불필요
    update.ack(); // 수신 확인만
}
```

---

## 4. 핵심 플로우: Block Proposal

```
Simplex BFT
 (propose 호출)
     │
     │ Message::Propose { parent: (epoch, view, digest), round }
     ▼
application::Actor::handle_propose()
     │
     ├─1─► marshal.subscribe(round, parent_digest)
     │      └─ 부모 Block 객체 대기 (로컬 or P2P 수신)
     │
     ├─2─► execution_node.beacon_engine()
     │      .fork_choice_updated(
     │          head=parent_hash, safe=parent_hash, fin=parent_hash,
     │          PayloadAttributes {
     │              timestamp: SystemTime::now(),
     │              prev_randao: B256::ZERO,
     │              fee_recipient: self.fee_recipient,
     │          }
     │      )
     │      └─ payload_id 수신
     │
     ├─3─► tokio::time::sleep(self.new_payload_wait_time)
     │      (= --consensus.time-to-build-proposal, 기본 1000ms)
     │      [EL이 이 시간 동안 txpool에서 트랜잭션 수집·실행]
     │
     ├─4─► execution_node.payload_builder()
     │      .resolve_kind(payload_id, PayloadKind::WaitForPending)
     │      └─ EthBuiltPayload 수신
     │      └─ sealed_block = built_payload.block().clone()
     │      └─ block = Block::from_execution_block(sealed_block)
     │
     ├─5─► execution_node.beacon_engine()
     │      .new_payload(block.clone())
     │      [P2P 배포 전 EL에 블록 사전 등록]
     │
     ├─6─► marshal.proposed(round, block)
     │      └─ marshal이 블록을 P2P 브로드캐스트
     │      └─ 다른 노드가 구독 가능한 상태로 저장
     │
     └─7─► return block.digest()
            └─ Simplex가 이 digest를 제안(Proposal)으로 사용

                   ← 총 소요: ~time-to-build-proposal + ~50ms 오버헤드
```

---

## 5. 핵심 플로우: Block Verification

```
Simplex BFT
 (다른 노드 제안 수신)
     │
     │ Message::Verify { parent, payload: block_digest, proposer, round }
     ▼
application::Actor::handle_verify()
     │
     ├─1─► marshal.subscribe(None, block_digest)
     │      └─ None = notarization 없이 P2P 전파본 직접 수락
     │      └─ 제안 블록 본문 대기 (P2P에서 수신)
     │      └─ 블록이 도착할 때까지 블로킹
     │
     ├─2─► marshal.subscribe(None, parent_digest)
     │      └─ 부모 블록 본문 대기
     │
     ├─3─► executor.canonicalize_head(parent_height, parent_digest)
     │      └─ EL canonical head → 부모 블록으로 갱신
     │      ⚠️ 응답 확인 안 함 (무시)
     │
     ├─4─► execution_node.beacon_engine()
     │      .new_payload(block)
     │      └─ EL이 블록의 트랜잭션 실행 + 상태 전이 수행
     │      └─ payload_status 반환
     │
     ├─5─► is_valid = payload_status.is_valid()
     │      is_valid=true  → marshal.verified(round, block)
     │      is_valid=false → warn! 로그 (거부)
     │
     └─6─► response.send(is_valid)
            └─ Simplex에 검증 결과 반환
            ⚠️ 반환값 무시 (send 실패 감지 안 됨)

                   ← 총 소요: ~P2P 전파 시간 + EL 실행 시간
```

---

## 6. 핵심 플로우: Epoch Transition

```
dkg_manager::Actor::run() — 확정 블록 감시 루프
     │
     │ [새 확정 블록 수신: Update::Tip(_, height, _)]
     │
     ├─1─► block_epoch = epoch_strategy.epoch(height)
     │      (기본 100블록마다 epoch 증가)
     │
     ├─2─► if block_epoch > current_epoch:
     │      │
     │      ├─A─► epoch_manager.enter(
     │      │          next_epoch = current_epoch + 1,
     │      │          polynomial,   // BLS 공개키 다항식
     │      │          my_share,     // 내 BLS 서명 지분
     │      │          participants, // 전체 검증자 목록
     │      │     )
     │      │
     │      └─B─► epoch_manager.exit(current_epoch)
     │
     ▼
epoch_manager::Actor::enter()
     │
     ├─1─► BLS Scheme 생성
     │      share 있음: Scheme::signer(public, participants, share)
     │      share 없음: Scheme::verifier(participants)
     │
     ├─2─► Simplex BFT 엔진 생성
     │      - DB 파티션: "private-chain_consensus_epoch_{N}"
     │      - Automaton = application mailbox
     │      - 타이밍 파라미터 주입
     │
     ├─3─► P2P 채널 멀티플렉서에 에폭 등록
     │      vote_mux.register(epoch)       // ⚠️ .unwrap()
     │      certificates_mux.register(epoch) // ⚠️ .unwrap()
     │      resolver_mux.register(epoch)   // ⚠️ .unwrap()
     │
     └─4─► active_epochs.insert(epoch, engine_handle)
            [새 Simplex 인스턴스 시작, 이 에폭의 투표 시작]

epoch_manager::Actor::exit_epoch()
     │
     ├─1─► active_epochs.remove(old_epoch)
     ├─2─► handle.abort() → Simplex 강제 종료
     └─3─► scheme_provider.delete(old_epoch) → BLS 메모리 해제

                   ← 에폭 전환 총 소요: <10ms (P2P 대기 없음)
```

---

## 7. 핵심 플로우: Block Finalization

```
Simplex BFT
 (2/3+ notarization 달성)
     │
     ▼
marshal::Actor (finalization 처리)
     │
     │ [확정 블록 → Reporter 체인으로 전파]
     │
     ├─1─► executor_mailbox.report(Update::Block(block, ack))
     │
     ▼
executor::Actor::forward_finalized()
     │
     ├─A─► canonicalize(block_height, block_digest, block_digest)
     │      → EL FCU (head=final, safe=final, finalized=final)
     │
     ├─B─► execution_node.beacon_engine()
     │      .new_payload(block)
     │      → EL에 확정 블록 재전송 (캐시 미스 대비)
     │
     └─C─► ack.ack() → marshal에 처리 완료 알림
     │
     ├─2─► dkg_manager_mailbox.report(Update::Tip(height, digest))
     │      → 에폭 경계 감지 (위 6번 플로우)
     │
     ├─3─► epoch_manager_mailbox.report(Update::Block(block, ack))
     │      → active epochs에 확정 정보 전달
     │
     └─4─► peer_manager_mailbox.report(Update::Block(block, ack))
            → 사설망: no-op (ack만 처리)
```

---

## 8. P2P 채널 구성 (`config.rs`)

### 채널 식별자 및 속도 제한

```rust
// config.rs
pub const VOTES_CHANNEL_IDENT:        Channel = 0;  // 투표 메시지
pub const CERTIFICATES_CHANNEL_IDENT: Channel = 1;  // 인증서 메시지
pub const RESOLVER_CHANNEL_IDENT:     Channel = 2;  // 블록 해결(fetch)
pub const BROADCASTER_CHANNEL_IDENT:  Channel = 3;  // 범용 브로드캐스트
pub const MARSHAL_CHANNEL_IDENT:      Channel = 4;  // 블록 배포
pub const DKG_CHANNEL_IDENT:          Channel = 5;  // DKG (미사용)

// 속도 제한 (Quota = 초당 메시지 수)
pub const BROADCASTER_LIMIT: Quota = 8;     // 낮음 — 전체 브로드캐스트
pub const MARSHAL_LIMIT:      Quota = 8;    // 낮음 — 블록 배포
pub const VOTES_LIMIT:        Quota = 128;  // 높음 — 투표 트래픽
pub const CERTIFICATES_LIMIT: Quota = 128;  // 높음 — 인증서 트래픽
pub const RESOLVER_LIMIT:     Quota = 128;  // 높음 — 블록 fetch 트래픽
pub const DKG_LIMIT:          Quota = 128;  // 높음 (미사용)
```

### Muxer 동작 원리

```
P2P 채널 (공유, Epoch 구분 없음)
         │
         ▼
    ┌─────────┐
    │  Muxer  │  ← epoch_manager가 생성
    └────┬────┘
         │ .register(epoch) 호출 시
         │ 해당 epoch 전용 (Sender, Receiver) 반환
         ▼
┌───────────────────────────────────┐
│  Epoch 1 Simplex │ Epoch 2 Simplex│
│  votes channel   │ votes channel  │
└───────────────────────────────────┘
         ↑ 에폭 번호로 라우팅
```

---

## 9. Block 타입 구조 (`block.rs`)

```rust
// block.rs
#[repr(transparent)]
pub struct Block(SealedBlock<EthPrimitives::Block>);
//                └─ reth-primitives의 Ethereum 블록 (헤더+바디+해시)

// 구현된 commonware 트레이트:
// - Digestible: digest() = block_hash (B256)
// - Committable: commitment() = digest()
// - Heightable: height() = block_number (u64)
// - Block: parent() = parent_digest()
// - Write/Read: RLP 직렬화/역직렬화
// - EncodeSize: RLP 크기 계산

impl Block {
    pub fn from_execution_block(block: SealedBlock<...>) -> Self
    pub fn into_inner(self) -> SealedBlock<...>
    pub fn block_hash(&self) -> B256
    pub fn digest(&self) -> Digest      // B256 래퍼
    pub fn parent_digest(&self) -> Digest
    pub fn timestamp(&self) -> u64
}
```

---

## 10. 코드 품질 리뷰 — 알려진 이슈

### 🔴 Critical (패닉 위험)

#### [C-1] Mux 채널 등록 시 `.unwrap()` 패닉
```
파일: crates/commonware-consensus/src/consensus/epoch/manager/actor.rs
라인: 273-275

현재:
    vote_mux.register(epoch.get()).await.unwrap();
    certificates_mux.register(epoch.get()).await.unwrap();
    resolver_mux.register(epoch.get()).await.unwrap();

문제: P2P 채널이 닫혀있으면 즉시 패닉 → 프로세스 종료
개선: .ok_or_eyre("mux channel closed")? 로 변경하여 graceful 종료
```

#### [C-2] `canonicalize_head` 응답 무시
```
파일: crates/commonware-consensus/src/consensus/application/actor.rs
라인: 303-311

현재:
    let response = self.state.executor.canonicalize_head(
        parent_height, parent_digest
    );
    // response 사용 안 함 (무시)

문제: EL FCU 실패 시 감지 불가
     → 잘못된 canonical head로 블록 검증 계속 진행
개선: response.await?.wrap_err("canonicalize_head failed")?
```

#### [C-3] `response.send()` 반환값 무시 (verify)
```
파일: crates/commonware-consensus/src/consensus/application/actor.rs
라인: 344-345

현재:
    response.send(is_valid);   // Result<(), _> 무시

문제: Simplex BFT가 수신 대기 중 타임아웃으로 채널 닫으면
     검증 결과가 전달되지 않음 (무음 실패)
개선:
    if response.send(is_valid).is_err() {
        warn!("verify response channel closed — simplex timed out?");
    }
```

#### [C-4] Epoch Manager Scheme 생성 `.expect()` 패닉
```
파일: crates/commonware-consensus/src/consensus/epoch/manager/actor.rs
라인: 233

현재:
    .expect("our private share must match our slice of the public key")

문제: DKG share와 participants 불일치 시 패닉
상황: 노드 재시작 후 설정 불일치, 또는 향후 동적 validator set 변경 시
개선: match 문으로 에러 처리 후 해당 에폭 skip
```

---

### 🟡 High (동작 오류 가능)

#### [H-1] DKG Seed에 비암호학적 해시 사용
```
파일: crates/commonware-consensus/src/consensus/dkg/manager/mod.rs
라인: 140-145

현재:
    use std::collections::hash_map::DefaultHasher; // ← 비암호학적!
    let seed = DefaultHasher::new()
        .chain(NAMESPACE)
        .chain("PRIVATE_CHAIN_DKG_SEED")
        .finish();

문제:
- DefaultHasher는 Rust 버전에 따라 다른 결과 반환 가능
- 암호학적 보안 보장 없음 (구조 예측 가능)
- 프로덕션 환경에서 키 예측 공격 위험

개선: SHA-256이나 BLAKE3 등 암호학적 해시 사용
    let seed = blake3::hash(
        &[NAMESPACE, b"PRIVATE_CHAIN_DKG_SEED"].concat()
    );
```

#### [H-2] Peer Manager DKG 연동 미구현 (TODO)
```
파일: crates/commonware-consensus/src/consensus/dkg/manager/mod.rs
라인: 265-267

현재:
    // TODO: Inform the peer manager about the participants
    // once DKG is fully integrated.

영향:
- 에폭 전환 시 peer_manager가 새 validator set 모름
- 사설망은 정적 validator set이라 현재는 무해
- 향후 동적 validator set 지원 시 반드시 구현 필요
```

#### [H-3] Backfill 기간 중 Race Condition 가능성
```
파일: executor/actor.rs (backfill 로직)

상황:
  재시작 시 executor가 EL finalized 높이와
  consensus finalized 높이 간 gap을 채우기 위해
  new_payload 순차 전송

위험:
  backfill 완료 전에 새 블록 finalization이 도착하면
  순서 보장이 깨질 수 있음

현재 완화: 재시작 시 marshal이 이전 메시지 큐 보유 가능
개선: backfill 완료 확인 후 new finalization 처리
```

---

### 🟠 Medium (코드 개선 필요)

#### [M-1] `application/actor.rs` — `prev_randao: B256::ZERO`
```
라인: 195 (PayloadAttributes 생성)

현재: prev_randao: B256::ZERO
이유: VRF 리더 선출 결과를 prev_randao에 반영하는 구현 미완성
영향: MEV 저항성 약화 (공격자가 블록 내용 예측 가능)
     현재 사설망에서는 허용 수준
```

#### [M-2] `handle_genesis()` — epoch 경계 블록 없을 때 B256::ZERO 반환
```
라인: 148

현재:
    warn!("boundary block not found for epoch {epoch}");
    return Ok(B256::ZERO);

위험: Simplex가 제로 해시를 genesis로 인식하면
      체인 분기 유발 가능
개선: Err 반환하여 해당 에폭 초기화 실패로 처리
```

#### [M-3] `peer_manager` `execution_node` 필드 미사용
```
파일: peer_manager/actor.rs
라인: 17

#[allow(dead_code)]
execution_node: PrivateNodeHandle,

상태: 동적 validator set 기능 구현 전까지 미사용
     사설망에서는 무해
```

---

### 🟢 Low (개선 사항)

#### [L-1] 메트릭 라벨 부재
```
actor.rs 각 곳에 prometheus 메트릭 등록 있으나
일부는 epoch 라벨 없이 집계 → 디버깅 어려움
개선: epoch 라벨 추가
```

#### [L-2] `handle_broadcast()` no-op
```
application/actor.rs::handle_broadcast()
현재 아무 동작 없음 (빈 함수)
향후 실시간 블록 구독자 알림에 활용 가능
```

#### [L-3] 로그 레벨 일관성
```
일부 정상 경로에 warn! 사용, 일부 오류 경로에 debug!
레벨 정책 정비 필요
```

---

## 부록: 주요 설정값과 소스 매핑

| CLI 파라미터 | 소스 변수 | 사용 위치 |
|---|---|---|
| `--consensus.time-to-build-proposal` | `Builder::time_to_propose` | `handle_propose()` sleep 시간 |
| `--consensus.wait-for-proposal` | Simplex leader timeout | `epoch_manager::enter()` → simplex config |
| `--consensus.known-peers` | `parse_peer_entry()` | `lib.rs::run_consensus_stack()` |
| `--consensus.epoch-length` | genesis `epoch_length` | `dkg_manager::run()`, `epoch_strategy` |
| `--builder.gaslimit` | EL 설정 | `PayloadAttributes` 통해 EL로 전달 |

---

## 부록: Actor 생존 시간 다이어그램

```
프로세스 시작
     │
     ├── peer_manager::Actor     ─────────────────────────── 프로세스 종료까지
     ├── broadcast::Engine       ─────────────────────────── 프로세스 종료까지
     ├── executor::Actor         ─────────────────────────── 프로세스 종료까지
     ├── marshal::Actor          ─────────────────────────── 프로세스 종료까지
     ├── dkg_manager::Actor      ─────────────────────────── 프로세스 종료까지
     ├── epoch_manager::Actor    ─────────────────────────── 프로세스 종료까지
     ├── application::Actor      ─────────────────────────── 프로세스 종료까지
     │
     │   [에폭별 Simplex 인스턴스]
     ├── simplex::Engine(epoch=0)  ──────┤
     ├── simplex::Engine(epoch=1)        ──────┤
     ├── simplex::Engine(epoch=2)              ──────┤
     │   (각 100블록 = 1 에폭, abort()로 종료)
     │
프로세스 종료
```
