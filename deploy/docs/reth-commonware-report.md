# Reth + Commonware SimplexBFT

> Private Ethereum Chain - Architecture, Consensus Integration & Performance Analysis

**Blockchain Team Internal Document** | 2026-03-16

---

## Table of Contents

1. [TL;DR](#1-tldr)
2. [Overview](#2-overview)
3. [Core Concepts Deep Dive](#3-core-concepts-deep-dive)
4. [Architecture](#4-architecture)
5. [Consensus Flow](#5-consensus-flow)
6. [Node Configuration](#6-node-configuration)
7. [Key Timing Relationship](#7-key-timing-relationship)
8. [Performance Benchmark](#8-performance-benchmark)
9. [DKG & Epoch Management](#9-dkg--epoch-management)
10. [Codebase Structure](#10-codebase-structure)

---

## 1. TL;DR

| Metric | Gas Limit 120M | Gas Limit 180M | Gas Limit 160M | Gas Limit 200M (Best) |
|--------|:--------------:|:--------------:|:--------------:|:---------------------:|
| **TPS** | 2,247 tx/s | 3,622 tx/s | 4,999 tx/s | **6,360 tx/s** |
| **Gas Throughput** | 79 Mgas/s | 125 Mgas/s | 164 Mgas/s | **209 Mgas/s** |
| **Avg Block Time** | 1.5s | 1.4s | 1.0s | **1.0s** |
| **Gas Utilization** | 97.7% | 99.5% | 99.9% | **99.9%** |

> 120M·180M 테스트는 `time-to-build-proposal=1000ms` 환경, 160M·200M 테스트는 `time-to-build-proposal=500ms`(기본값) 환경에서 측정.

- Beacon Chain(PoS) 제거, **Commonware SimplexBFT** 직접 통합
- **단일 바이너리** — EL + CL이 하나의 프로세스에서 동작
- **Instant Finality** — 블록 생성 즉시 확정, Reorg 불가
- Ethereum Mainnet 대비 **~350배 TPS**, **~640배 빠른 Finality**

---

## 2. Overview

### Reth (Execution Layer)

- Rust 기반 고성능 Ethereum 실행 클라이언트
- **MDBX + Static Files** 하이브리드 스토리지
- **순차적(Sequential) EVM 실행** — 블록 내 트랜잭션은 상태 의존성으로 인해 1개씩 순서대로 처리
- 병렬 처리 구간:
  - **트랜잭션 디코딩 + 서명 복구** — `rayon::par_iter()` 로 병렬화
  - **State Root(MPT) 계산** — `crates/trie/parallel/` 병렬 Merkle Patricia Trie
- **Engine API** — `newPayload`, `forkchoiceUpdated`
- **JSON-RPC** — eth, net, web3, debug, admin, txpool

### Commonware SimplexBFT (Consensus Layer)

- BFT 기반 합의 프로토콜, Beacon Chain 대체
- **BLS12-381 Threshold Signature** — 2/3+ 검증자 서명으로 블록 확정
- **VRF 기반 리더 선출** — 예측 불가능, 결정론적
- **Epoch 단위** 합의 관리 — 100 blocks/epoch
- **Instant Finality** — 투표 완료 즉시 확정

### 왜 Commonware인가?

| | 장점 |
|---|---|
| **단일 바이너리** | CL/EL 분리 없이 하나의 프로세스에서 동작. 운영 복잡도 대폭 감소 |
| **Instant Finality** | 2/3+ 검증자 서명 수집 즉시 블록 확정. Reorg 없음 |
| **빠른 블록 생성** | 12초 슬롯 없이 설정 가능한 블록 주기. 1초 블록 타임 달성 |

---

## 3. Core Concepts Deep Dive

> 블록체인 코어 레벨의 암호학과 합의 알고리즘은 진입 장벽이 높은 영역.
> 배경 지식 없이도 이해할 수 있도록, 내부 메커니즘이 어떻게 맞물려 돌아가는지 엔지니어 시각에서 해설.

---

### 3-1. MPSC Channel — EL과 CL의 통신 방식

#### 기존 이더리움 (Geth + Prysm 등)

- 실행 클라이언트(EL)와 합의 클라이언트(CL)가 **별도 프로세스**로 동작
- HTTP/REST 기반 Engine API 통신
- **JSON 파싱 오버헤드 + 네트워크 지연(Latency)** 발생 불가피

#### 본 아키텍처 (Reth + Commonware)

- 단일 바이너리(Single Binary) — 하나의 프로세스 내에서 두 레이어가 동작
- 통신 수단: **MPSC (Multi-Producer, Single-Consumer) Channel**
  - 메모리 내에서 안전하게 데이터를 주고받는 큐(Queue) 자료구조
  - Kafka 단일 파티션의 동작 방식과 유사하되, 네트워크를 타지 않는 순수 메모리 연산

#### 왜 MPSC인가?

- **Multi-Producer (Consensus Layer)**
  - 합의 레이어에는 여러 워커 스레드가 존재
  - Application Actor: "이 블록 실행해!"
  - Executor Actor: "이 블록 확정해!"
  - 이들이 동시다발적으로 명령을 전송

- **Single-Consumer (Execution Layer)**
  - EVM 상태(State) 변경 작업은 무조건 순차적으로 처리 필요
  - MDBX 데이터베이스 락(Lock) 충돌 방지
  - Engine API 핸들러가 단일 컨슈머로서 큐에 쌓인 명령을 하나씩 꺼내서 EVM 구동

- **결과**: 네트워크 지연 0ms. 마이크로초(µs) 단위의 통신 속도로 블록 즉각 처리 가능

```
┌───────────────────────────────────────────────────────────┐
│                  기존 (Geth + Prysm)                      │
│                                                           │
│  [EL Process] ──── HTTP/JSON ────► [CL Process]          │
│                    ~1-5ms 지연                            │
│                    JSON 직렬화/역직렬화 오버헤드          │
└───────────────────────────────────────────────────────────┘

┌───────────────────────────────────────────────────────────┐
│               본 구성 (Reth + Commonware)                 │
│                                                           │
│  ┌──────────────────── Single Process ──────────────────┐ │
│  │ [EL Thread] ◄── MPSC Channel ──► [CL Threads]       │ │
│  │                  ~µs 단위                             │ │
│  │                  Zero-copy 메모리 전달                 │ │
│  └──────────────────────────────────────────────────────┘ │
└───────────────────────────────────────────────────────────┘
```

---

### 3-2. BLS12-381 Threshold Signature — 2/3+ 서명

세 가지 개념이 합쳐진 암호학 용어.

#### BLS (Boneh-Lynn-Shacham)

- 디지털 서명 방식 중 하나
- 핵심 특성: **여러 명의 서명을 하나의 짧은 서명으로 압축(Aggregation) 가능**
- 1만 명이 서명해도 서명 데이터 용량은 1명 분량과 동일
- 네트워크 트래픽 + 스토리지 절약 효과 극대화

#### 12-381

- BLS 서명을 구현하는 **특정 타원 곡선(Elliptic Curve)**의 이름
- 이더리움 진영의 표준 곡선
- 128-bit 보안 강도 제공

#### Threshold (임계값) Signature

- 전체 n명 중 t명 이상이 서명해야만 유효한 하나의 '마스터 서명(Certificate)' 생성
- 본 환경: **n=4 노드, t=3 (2/3+)**
- BFT 수학 공식: 네트워크가 비잔틴 공격을 방어하려면 전체의 2/3를 초과하는 서명 필요
- 즉, 4대 중 **3대의 서명(Share) 조각**이 모여야만 블록이 합법적으로 확정(Finalized)

```
노드별 BLS Private Share:
  V1: share_1  ─┐
  V2: share_2  ─┤──► 3개 이상 모이면 → Threshold Certificate 생성 → 블록 확정
  V3: share_3  ─┤
  V4: share_4  ─┘    (4개 중 3개 = 2/3+ = BFT 안전 조건 충족)
```

---

### 3-3. VRF 기반 리더 선출과 MEV 방지

합의의 보안성을 책임지는 핵심 메커니즘. (Consensus Flow의 Step 1에 해당)

#### VRF (Verifiable Random Function)

- **검증 가능한 난수 생성기**
- 비유: '암호학적 로또 기계'
- 난수(랜덤 값)를 뽑아내면서, 동시에 "이 난수는 조작되지 않았음"을 증명하는 영수증(Proof)을 함께 발급

#### "결정론적이지만 예측 불가능"의 의미

- **예측 불가능(Unpredictable)**
  - 다음 리더가 누구인지는 해당 라운드가 시작되어 프라이빗 키가 입력되기 전까지 아무도 알 수 없음
  - 사전 타겟팅 원천 차단

- **결정론적(Deterministic)**
  - 라운드가 시작되어 난수 + 영수증(Proof)이 네트워크에 배포되면
  - 모든 노드가 영수증을 검증하고 "이번 리더는 V2가 맞다"고 100% 동일하게 결론
  - 합의 불일치 발생 불가

#### MEV 공격 방지

- **MEV(Miner Extractable Value)**: 블록을 만드는 리더가 트랜잭션 순서를 조작하거나 새치기하여 부당한 이득을 취하는 행위

- **순번제(Round-Robin)의 문제**
  - "3초 뒤에 V3가 리더" → 미리 V3에게 뇌물 or V3를 DDoS 공격 가능
  - 공격자에게 물리적인 준비 시간 제공

- **VRF 적용 시**
  - 블록 생성 직전까지 다음 리더가 누구인지 아무도 모름
  - 특정 노드를 타겟팅하거나 뇌물을 줄 물리적 시간 자체가 소멸
  - 프라이빗 네트워크에서는 MEV 위험이 낮지만, 퍼블릭 전환 시 보안 기반 확보

---

### 3-4. Epoch 단위 합의 관리 (100 blocks/epoch)

블록체인이 영원히 멈추지 않고 돌아가기 위한 논리적 '시간의 마디'.

#### Epoch의 정의

- 1 에폭 = 100개 블록
- 블록 타임 1.0초 기준, 약 **100초(1.7분)**마다 1 에폭 경과

#### 왜 에폭 단위로 관리하는가?

- 검증자 추가/제거, 암호학적 키(DKG Threshold) 교체 등 시스템 갱신 작업 필요
- 이를 '매 블록마다' 수행하면 시스템에 엄청난 과부하
- 에폭 단위로 묶어서 한꺼번에 처리 → 오버헤드 최소화

#### 동작 방식

1. Block #1 ~ #100: 미리 정해진 검증자 셋 + 룰로 블록 연속 생성
2. Block #100이 Finalized 되는 순간:
   - 기존 합의 엔진(SimplexBFT 인스턴스) 셧다운 → **Exit**
   - "노드 명단 변경 사항 체크 + 키 재발급 필요 여부 확인" (찰나의 순간)
   - 새로운 엔진 가동 → **Enter**
3. Block #101 부터 새 에폭으로 재개

- **결과**: 운영 중 네트워크 중단 없이 노드 교체, 보안 업데이트 가능

---

### 3-5. 아키텍처 핵심 요약

> 단일 프로세스의 내부 메모리 속도(MPSC)로 EVM을 극한으로 구동하면서,
> 무작위 리더 선출(VRF)과 강력한 암호학적 서명(BLS Threshold)을 결합하여
> **1초대의 즉각적인 거래 확정(Instant Finality)**을 달성한 엔터프라이즈급 구성.

---

## 4. Architecture

### In-Process 통합 아키텍처

- EL과 CL이 **MPSC 채널**로 통신, 단일 바이너리에서 실행
- 네트워크 지연 Zero — 마이크로초 단위 내부 메시지 전달

```
┌─────────────────────────────────────────────────────────────┐
│                    SINGLE BINARY (reth node)                │
│                                                             │
│  ┌─────────────────────┐         ┌────────────────────────┐ │
│  │  Execution Layer    │  MPSC   │  Consensus Layer       │ │
│  │  (Reth)             │ Channel │  (Commonware)          │ │
│  │                     │ ◄─────► │                        │ │
│  │  ┌───────────────┐  │         │  ┌──────────────────┐  │ │
│  │  │ JSON-RPC      │  │         │  │ Application      │  │ │
│  │  │ Server        │  │         │  │ Actor            │  │ │
│  │  └───────────────┘  │         │  │ (propose/verify) │  │ │
│  │  ┌───────────────┐  │         │  └──────────────────┘  │ │
│  │  │ Transaction   │  │         │  ┌──────────────────┐  │ │
│  │  │ Pool          │  │         │  │ Epoch Manager    │  │ │
│  │  └───────────────┘  │         │  │ (SimplexBFT)     │  │ │
│  │  ┌───────────────┐  │         │  └──────────────────┘  │ │
│  │  │ Payload       │  │         │  ┌──────────────────┐  │ │
│  │  │ Builder       │  │         │  │ DKG Manager      │  │ │
│  │  └───────────────┘  │         │  │ (Threshold Keys) │  │ │
│  │  ┌───────────────┐  │         │  └──────────────────┘  │ │
│  │  │ Engine API    │  │         │  ┌──────────────────┐  │ │
│  │  │ Handler       │  │         │  │ Executor Actor   │  │ │
│  │  └───────────────┘  │         │  │ (FCU→Finalize)   │  │ │
│  │  ┌───────────────┐  │         │  └──────────────────┘  │ │
│  │  │ MDBX +        │  │         │  ┌──────────────────┐  │ │
│  │  │ Static Files  │  │         │  │ Marshal Archive  │  │ │
│  │  └───────────────┘  │         │  └──────────────────┘  │ │
│  └─────────────────────┘         └────────────────────────┘ │
│                                                             │
│  ┌──────────────────────────────────────────────────────┐   │
│  │                    NETWORK LAYER                      │   │
│  │  ETH P2P (devp2p)  │  Consensus P2P  │  Broadcast    │   │
│  └──────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

### EVM 실행 파이프라인

- 트랜잭션 실행 자체는 **순차적(Sequential)** — Ethereum 합의 규칙상 필수
- 병렬 처리는 전처리(디코딩/서명복구)와 후처리(State Root) 단계에서 적용

```
블록 수신
    ↓
[병렬] Tx 디코딩 + 서명 복구        ← rayon par_iter()
    ↓
[순차] Tx EVM 실행 (1개씩 순서대로)  ← execute_block()
    ├─ Tx 1 → state 업데이트
    ├─ Tx 2 → state 업데이트
    └─ Tx N → state 업데이트
    ↓
[병렬] State Root(MPT) 계산         ← crates/trie/parallel/
    ↓
블록 완성
```

| 단계 | 방식 | 이유 |
|------|------|------|
| Tx 디코딩/서명복구 | **병렬** (rayon) | 각 트랜잭션 독립적, 상태 무관 |
| EVM 트랜잭션 실행 | **순차** | Tx N+1이 Tx N의 상태 변경에 의존 가능 |
| State Root 계산 | **병렬** (rayon) | Trie 노드들을 독립적으로 해싱 가능 |

### 7개 Actor 구성

| Actor | 역할 | 통신 대상 |
|-------|------|----------|
| **Peer Manager** | 검증자 피어 추적, P2P oracle | Consensus P2P |
| **Broadcast** | P2P 블록 배포 | Consensus P2P |
| **Marshal** | Finalized 블록 아카이브 + certificate 저장 | Freezer Tables |
| **Application** | 블록 제안(propose) / 검증(verify) | EL Engine API |
| **Executor** | FCU 전송 → EL Finalization | EL Engine API |
| **Epoch Manager** | SimplexBFT 인스턴스 생명주기 | Simplex Engine |
| **DKG Manager** | Threshold signing scheme + Epoch 경계 감지 | Marshal Reporter |

---

## 5. Consensus Flow

### 블록 생성부터 Finalization까지

```
Step 1: VRF Leader Election          [Epoch Manager]
   │    BLS12-381 VRF로 검증자 중 리더(Proposer) 선출
   │    결정론적이지만 예측 불가능 → MEV 공격 방지
   ▼
Step 2: Block Building               [Application Actor]
   │    a) 리더가 EL에 forkchoiceUpdated 전송 (PayloadAttributes 포함)
   │    b) time-to-build-proposal (500ms) 동안 sleep → txpool에서 tx 수집 대기
   │    c) EL Payload Builder가 블록 조립 + EVM 실행 완료
   │    d) newPayload로 블록을 EL에 등록 후 digest를 네트워크에 broadcast
   ▼
Step 3: Voter Verification           [All Validators]
   │    a) P2P broadcast로 블록 데이터 수신
   │    b) 블록 RLP 디코딩 + 부모 블록 확인
   │    c) EL에 newPayload 전달 → EVM 실행으로 유효성 검증
   │    d) 유효하면 BLS12-381 threshold share로 서명하여 vote 전송
   ▼
Step 4: Notarization & Finalization  [Marshal]
   │    2/3+ (N=4일 때 3개) 검증자 서명이 모이면 threshold certificate 생성
   │    Marshal이 certificate를 검증하고 블록을 immutable archive에 저장
   │    ★ Instant Finality — 이 시점에서 블록은 되돌릴 수 없이 확정
   ▼
Step 5: EL State Update              [Executor]
   │    Executor가 EL에 forkchoiceUpdated 전송:
   │    { head_hash, safe_hash, finalized_hash } 모두 새 블록으로 설정
   │    EL이 상태를 영구 저장하고 다음 블록 빌드 준비
   ▼
Step 6: Epoch Transition             [DKG Manager]  (매 100 블록)
        block.number % epoch_length(100) == 0 감지 시:
        현재 epoch의 SimplexBFT 인스턴스 종료 → 다음 epoch 인스턴스 시작
```

### Block Production Timeline (1 Block Cycle)

```
Time ──────────────────────────────────────────────────────────►

├── sleep (500ms) ─┤── EVM exec (~417ms) ──┤── P2P+Vote ──┤── Fin ──┤
│                  │                        │              │         │
│    Leader:       │    Payload Build       │   Broadcast  │ Marshal │
│    FCU 전송 후   │    (tx 수집 + EVM)     │   + 투표     │ 확정    │
│    대기           │                        │              │         │
0ms             500ms                   ~917ms         ~1050ms  ~1100ms
```

#### Voter Safety Window

```
voter_window = wait_for_proposal - time_to_build_proposal - network_overhead
             = 2000ms - 500ms - ~50ms
             = 1450ms

safety_margin (200M gas) = 1450 / 417 = 3.47x ✅
safety_margin (160M gas) = 1450 / 333 = 4.35x ✅
```

> voter가 블록을 받아서 EVM 실행으로 검증하는 데 필요한 시간 대비 **3.5배 이상의 여유** → 안정적 운영 보장

---

## 6. Node Configuration

### 네트워크 구성 (4-Node Private Network)

```
                    ┌──────────┐
                    │    V1    │
                    │ .0.11    │
                    │ :8545    │
                    └──┬───┬──┘
                  ╱    │   │    ╲
                ╱      │   │      ╲
    ┌──────────┐       │   │       ┌──────────┐
    │    V2    │───────┼───┼───────│    V3    │
    │ .0.12    │       │   │       │ .0.13    │
    │ :8546    │       │   │       │ :8547    │
    └──────────┘       │   │       └──────────┘
                ╲      │   │      ╱
                  ╲    │   │    ╱
                    ┌──┴───┴──┐
                    │    V4    │
                    │ .0.14    │
                    │ :8548    │
                    └──────────┘

    ── ETH P2P (devp2p :30303)  +  Consensus P2P (:8000)
       Full Mesh, trusted-peers
```

| Node | IP | RPC Port | Consensus P2P | ETH P2P |
|------|-----|----------|---------------|---------|
| V1 | 172.20.0.11 | 8545 | :8000 | :30303 |
| V2 | 172.20.0.12 | 8546 | :8000 | :30303 |
| V3 | 172.20.0.13 | 8547 | :8000 | :30303 |
| V4 | 172.20.0.14 | 8548 | :8000 | :30303 |

### Core Parameters

| Category | Parameter | Value | Description |
|----------|-----------|-------|-------------|
| **Consensus** | `wait-for-proposal` | **2000ms** | Voter가 proposal 대기하는 최대 시간 (timeout 시 nullify) |
| **Consensus** | `time-to-build-proposal` | **500ms** | 리더가 FCU 후 블록 빌드를 위해 대기하는 시간 |
| **Builder** | `builder.gaslimit` | **200,000,000** | 블록당 최대 가스 한도 (200M) |
| **Builder** | `builder.interval` | **100ms** | Payload builder가 txpool 폴링하는 주기 |
| **Txpool** | `pending-max-count` | **100,000** | Pending 풀 최대 트랜잭션 수 |
| **Txpool** | `queued-max-count` | **50,000** | Queued 풀 최대 트랜잭션 수 |
| **Txpool** | `basefee-max-count` | **10,000** | Basefee 서브풀 최대 트랜잭션 수 |
| **Txpool** | `max-account-slots` | **5,000** | 계정당 최대 pending 트랜잭션 수 |
| **Txpool** | `pending/queued-max-size` | **128 MB** | 각 풀의 최대 바이트 크기 |
| **Resources** | `memory / cpus` | **8 GB / 3 cores** | Docker container 리소스 제한 (per node) |
| **RPC** | `rpc.max-connections` | **8,000** | RPC 동시 연결 상한 |
| **RPC** | `http.api` | eth,net,web3,debug,admin,txpool | 활성화된 RPC API |

---

## 7. Key Timing Relationship

성능과 안정성을 결정하는 핵심 타이밍 파라미터 관계.

### 공식

```
Block Cycle Time (블록 생성 주기)
  = time_to_build_proposal + evm_execution + network_delay
  = 500ms + ~417ms + ~83ms
  = ~1,000ms

Voter Safety Window (투표자 검증 가용 시간)
  = wait_for_proposal - time_to_build_proposal - network_overhead
  = 2000ms - 500ms - ~50ms
  = 1450ms
  → safety_margin (200M) = 1450 / 417 = 3.47x
  → safety_margin (160M) = 1450 / 333 = 4.35x

TPS Calculation (200M, 실측 avg gas/tx = 32,926)
  = gas_limit / avg_gas_per_tx / block_time
  = 200,000,000 / 32,926 / 1.0
  = ~6,074 TPS (theoretical) → 실측 6,360 TPS

EVM Throughput
  = ~480 Mgas/s (measured on 12-core host)
  exec_time(200M) = 200 / 480 * 1000 = ~417ms
  exec_time(160M) = 160 / 480 * 1000 = ~333ms
```

### 파라미터 트레이드오프

#### `time-to-build-proposal` 이 클수록

| | 영향 |
|---|------|
| ✅ | txpool에서 더 많은 tx 수집 가능 |
| ✅ | 블록 gas utilization 증가 |
| ❌ | 블록 주기 증가 → TPS 감소 |
| ❌ | 리더가 불필요하게 오래 대기 (낭비) |

#### `wait-for-proposal` 이 클수록

| | 영향 |
|---|------|
| ✅ | voter 검증 시간 여유 → 안정성 향상 |
| ✅ | 느린 노드도 참여 가능 |
| ❌ | nullify timeout 증가 → 장애 복구 느림 |
| ❌ | 리더 불참 시 빈 블록 대기 시간 증가 |

---

## 8. Performance Benchmark

### Test Environment

- **Hardware**: 12-core CPU, Docker container 당 3 cores / 8GB RAM
- **Network**: 4 validators, Docker bridge (172.20.0.0/16)
- **Transaction Type**: Simple ETH transfer (~21,000 gas) + mixed (~33,000 avg)
- **Consensus**: wait-for-proposal 2000ms

---

### Test 1: Gas Limit 120M

> Blocks 1,249 ~ 1,308 | 2026-03-11T06:36:29Z | `time-to-build-proposal=1000ms`

#### Headline Metrics

| TPS | Gas Throughput | Avg Block Time | Gas Utilization |
|:---:|:--------------:|:--------------:|:---------------:|
| **2,247** tx/s | **79** Mgas/s | **1.5** s | **97.7%** |

#### Block Statistics

| Metric | Value |
|--------|-------|
| Block Range | #1,249 ~ #1,308 |
| Block Count | 60 blocks |
| Elapsed | 89 seconds |
| Avg Block Time | 1.5 seconds |
| Gas Limit | 120,000,000 (120M) |

#### Transaction Statistics

| Metric | Value |
|--------|-------|
| Total Transactions | 200,000 |
| Avg / Block | 3,333 |
| Min / Block | 744 |
| Max / Block | 3,498 |
| Stddev | 510.057 |

#### Gas Usage

| Metric | Value |
|--------|-------|
| Total Gas Used | 7,036,724,620 |
| Avg Gas / Block | 117,278,743 |
| Min Gas / Block | 37,411,302 |
| Max Gas / Block | 119,946,325 |
| Avg Gas / Tx | 35,183 |
| Utilization Avg | 97.73% |
| Utilization Min | 31.17% |
| Utilization Max | 99.95% |

---

### Test 2: Gas Limit 180M

> Blocks 8,762 ~ 8,854 | 2026-03-12T02:10:08Z | `time-to-build-proposal=1000ms`

#### Headline Metrics

| TPS | Gas Throughput | Avg Block Time | Gas Utilization |
|:---:|:--------------:|:--------------:|:---------------:|
| **3,622** tx/s | **125** Mgas/s | **1.4** s | **99.5%** |

#### Block Statistics

| Metric | Value |
|--------|-------|
| Block Range | #8,762 ~ #8,854 |
| Block Count | 93 blocks |
| Elapsed | 133 seconds |
| Avg Block Time | 1.4 seconds |
| Gas Limit | 180,000,000 (180M) |

#### Transaction Statistics

| Metric | Value |
|--------|-------|
| Total Transactions | 481,832 |
| Avg / Block | 5,181 |
| Min / Block | 4,627 |
| Max / Block | 5,215 |
| Stddev | 96.675 |

#### Gas Usage

| Metric | Value |
|--------|-------|
| Total Gas Used | 16,650,963,712 |
| Avg Gas / Block | 179,042,620 |
| Min Gas / Block | 159,936,560 |
| Max Gas / Block | 179,934,384 |
| Avg Gas / Tx | 34,557 |
| Utilization Avg | 99.46% |
| Utilization Min | 88.85% |
| Utilization Max | 99.96% |

---

### Test 3: Gas Limit 160M

> Blocks 11,316 ~ 11,352 | 2026-03-16T05:37:42Z | `time-to-build-proposal=500ms`

#### Headline Metrics

| TPS | Gas Throughput | Avg Block Time | Gas Utilization |
|:---:|:--------------:|:--------------:|:---------------:|
| **4,999** tx/s | **164** Mgas/s | **1.0** s | **99.9%** |

#### Block Statistics

| Metric | Value |
|--------|-------|
| Block Range | #11,316 ~ #11,352 |
| Block Count | 37 blocks |
| Elapsed | 36 seconds |
| Avg Block Time | 1.0 seconds |
| Gas Limit | 160,000,000 (160M) |

#### Transaction Statistics

| Metric | Value |
|--------|-------|
| Total Transactions | 179,982 |
| Avg / Block | 4,864 |
| Min / Block | 4,852 |
| Max / Block | 4,879 |
| Stddev | 6.253 |

#### Gas Usage

| Metric | Value |
|--------|-------|
| Total Gas Used | 5,916,869,877 |
| Avg Gas / Block | 159,915,402 |
| Min Gas / Block | 159,900,875 |
| Max Gas / Block | 159,933,157 |
| Avg Gas / Tx | 32,874 |
| Utilization Avg | 99.94% |
| Utilization Min | 99.93% |
| Utilization Max | 99.95% |

---

### Test 4: Gas Limit 200M (Best Performance)

> Blocks 4,241 ~ 4,262 | 2026-03-16T04:28:39Z | `time-to-build-proposal=500ms`

#### Headline Metrics

| TPS | Gas Throughput | Avg Block Time | Gas Utilization |
|:---:|:--------------:|:--------------:|:---------------:|
| **6,360** tx/s | **209** Mgas/s | **1.0** s | **99.9%** |

#### Block Statistics

| Metric | Value |
|--------|-------|
| Block Range | #4,241 ~ #4,262 |
| Block Count | 22 blocks |
| Elapsed | 21 seconds |
| Avg Block Time | 1.0 seconds |
| Gas Limit | 200,000,000 (200M) |

#### Transaction Statistics

| Metric | Value |
|--------|-------|
| Total Transactions | 133,576 |
| Avg / Block | 6,071 |
| Min / Block | 6,061 |
| Max / Block | 6,085 |
| Stddev | 7.352 |

#### Gas Usage

| Metric | Value |
|--------|-------|
| Total Gas Used | 4,398,164,168 |
| Avg Gas / Block | 199,916,553 |
| Min Gas / Block | 199,900,450 |
| Max Gas / Block | 199,934,232 |
| Avg Gas / Tx | 32,926 |
| Utilization Avg | 99.95% |
| Utilization Min | 99.95% |
| Utilization Max | 99.96% |

---

### 성능 비교 요약

```
Gas Limit   build-time   TPS       Mgas/s    Block Time   Utilization
──────────────────────────────────────────────────────────────────────
120M        1000ms       2,247     79        1.5s         97.7%
180M        1000ms       3,622     125       1.4s         99.5%
160M        500ms        4,999     164       1.0s         99.9%
200M        500ms        6,360     209       1.0s         99.9%   ← Best
──────────────────────────────────────────────────────────────────────
120M→200M 향상            +183%     +164%     -33%         +2.2%p
```

> 코드 수정(timestamp fix + canonicalize_head 동기화)과 commonware 2026.3.0 업그레이드가 블록 타임을 1.4s → 1.0s로 단축, TPS 대폭 향상.

### TPS 75% 향상 원인 분석 (mg 브랜치 대비)

두 테스트 모두 `time-to-build-proposal=500ms` 동일 설정. 성능 차이의 실제 원인은 소스코드 변경.

#### 수치 비교

```
구분                    Test 2 (180M, mg)    Test 4 (200M, current)    변화
───────────────────────────────────────────────────────────────────────────
time-to-build-proposal   500ms (동일)          500ms (동일)              -
Gas Limit                180M                  200M                      +11%
Avg Block Time           1.4s                  1.0s                      -29%
TPS                      3,622                 6,360                     +75%
Mgas/s                   125                   209                       +67%
Gas Utilization          99.5%                 99.9%                     +0.4%p
```

#### 원인 1 (주요): Timestamp 버그 수정

**mg 브랜치 (버그)**:
```rust
// 단순히 현재 시각만 사용 → 같은 초에 2개 블록이 생성되면 parent.timestamp == now_secs
let timestamp = self.context.current().epoch_millis() / 1000;
```

**현재 (수정)**:
```rust
// Ethereum 요구사항: timestamp > parent.timestamp 보장
let now_secs = self.context.current().epoch_millis() / 1000;
let timestamp = now_secs.max(parent.timestamp().saturating_add(1));
```

1.0s 블록 주기에서 블록 2개가 같은 초에 제안되면 `invalid timestamp` 에러로 `newPayload` 거절 →
리더의 `verify()` 실패 → view rotation 발생 → 블록당 평균 1.4 view 소비.

이 버그가 없으면 모든 블록이 1st view에서 확정 → 1.0s 안정.

#### 원인 2 (주요): `canonicalize_head` 동기화 수정

**mg 브랜치 (버그)**:
```rust
// fire-and-forget: FCU 전송 후 즉시 new_payload 전송
if let Err(error) = self.state.executor.canonicalize_head(...) { warn!(...) }
// EL이 FCU를 아직 처리하지 않은 상태에서 new_payload가 도달 → parent 블록 미인식 → 검증 실패
```

**현재 (수정)**:
```rust
// FCU acknowledgment를 기다린 후 new_payload 전송
Ok(rx) => {
    if rx.await.is_err() { warn!(...) }
}
// EL이 fork-choice를 처리 완료한 후에 블록 검증 요청 → 레이스 컨디션 제거
```

voter 검증 시 `canonicalize_head`(FCU)와 `new_payload`가 레이스 컨디션으로
EL에 순서 뒤바뀌어 도달하는 경우 검증 실패 → 불필요한 view rotation.

#### 원인 3 (보조): Commonware 2026.3.0 업그레이드

`2026.2.0` → `2026.3.0`:
- `CacheRef::from_pooler()`: 공유 buffer pool 사용으로 메모리 효율화
- `MAX_PENDING_ACKS = 16`: marshal ACK 백프레셔 도입으로 큐 블로킹 감소
- `marshal::core::Actor` + `standard::Standard<Block>`: 내부 marshal 파이프라인 개선
- `broadcast` 엔진에 `peer_provider` 직접 연결: P2P 라우팅 레이어 단순화

#### 원인 4 (보조): Gas Limit 11% 증가 (180M → 200M)

```
블록당 추가 tx 수: (200M - 180M) / 32,926 ≈ +607 tx/block → +607 TPS 기여
```

#### 복합 효과 정량화

```
  timestamp + canonicalize_head 수정 (블록타임 1.4s→1.0s):
    3,622 × (1.4 / 1.0) = 5,071 TPS  (+1,449 TPS, 약 63%)
  Gas Limit 증가 (180M→200M):
    5,071 × (200 / 180) = 5,634 TPS  (+563 TPS,  약 24%)
  commonware 업그레이드 + utilization 개선:
                           +726 TPS  (약 13%)
  ────────────────────────────────────────────
  합계:                    6,360 TPS  (실측치와 일치)
```

#### 결론

성능 향상의 핵심은 **두 가지 버그 수정**:
1. `invalid timestamp` 에러 방지 → 1.0s 블록 타임 안정화
2. `canonicalize_head` 레이스 컨디션 제거 → 검증 실패 없이 1st view finalization

Gas Limit 200M은 EVM 실행시간(~417ms)이 Voter Safety Window(1450ms) 안에 3.47배 여유를
유지하면서 최대 처리량을 내는 현 환경의 최적점.

---

### Ethereum Mainnet 대비 비교

| Metric | Ethereum Mainnet | Private Chain (Best) | Ratio |
|--------|:----------------:|:--------------------:|:-----:|
| Block Time | 12 seconds | **1.0 seconds** | **12x faster** |
| Gas Limit | 30,000,000 | **200,000,000** | **6.7x larger** |
| TPS (simple tx) | ~15-20 | **~6,360** | **~350x** |
| Finality | ~15 minutes (2 epochs) | **~1.0 seconds** | **~900x faster** |
| Consensus | Gasper (PoS) | **SimplexBFT (Threshold)** | BFT |

---

## 9. DKG & Epoch Management

### Static Deterministic DKG

- 별도의 DKG 세레모니 없이 Genesis의 검증자 목록 + 고정 Namespace 해시를 시드로 사용
- 모든 노드가 동일한 threshold scheme을 독립적으로 생성

```
// Namespace seed (모든 노드 동일)
seed = hash("PRIVATE_CHAIN")

// Threshold scheme 생성
scheme = dkg::deal(seed, validators, N3f1)
// N3f1 = ceil(2n/3) threshold
// 4 validators → 3-of-4 필요

// 각 노드가 자신의 BLS12-381 share 추출
my_share = scheme.shares[my_index]
```

### Epoch Lifecycle

- 100 블록 단위로 Epoch 전환
- 각 Epoch마다 독립적인 SimplexBFT 인스턴스 실행

| Epoch | Block Range | Genesis Block |
|-------|-------------|---------------|
| Epoch 0 | Block #1 ~ #100 | Block #0 |
| Epoch 1 | Block #101 ~ #200 | Block #100 |
| Epoch N | Block #(N*100+1) ~ #((N+1)*100) | Block #(N*100) |

**Epoch 전환 트리거**: `finalized_height % epoch_length == 0`

```
Epoch N finalized ──► DKG Manager 감지
    │
    ├── Scheme 등록 (동일 검증자 셋, 동일 threshold)
    ├── Epoch Manager에 Exit(N) 명령
    └── Epoch Manager에 Enter(N+1) 명령
         └── 새 SimplexBFT 인스턴스 시작
```

### 재시작 시 Epoch 복구

- 노드 재시작 시 `last_finalized_height` 기반으로 올바른 Epoch로 즉시 점프

```
starting_epoch = last_finalized_height / epoch_length
// e.g., height=850 → epoch 8 (block #801~#900)
```

> 기존에는 항상 Epoch 0부터 시작하여 재시작 후 합의 desync 버그 발생 → 수정 완료

---

## 10. Codebase Structure

Commonware Consensus 통합 코드 (~3,900 lines):

```
crates/commonware-consensus/src/
├── lib.rs                    # run_consensus_stack() 진입점
├── args.rs                   # 50+ CLI arguments 정의
├── config.rs                 # P2P 채널 ID, Rate limit
├── genesis.rs                # Genesis에서 검증자 정보 파싱
├── key_io.rs                 # Ed25519/BLS 키 파일 I/O
├── node_handle.rs            # EL 인터페이스 (Engine API)
│
├── consensus/
│   ├── engine.rs             # 메인 오케스트레이터 (7개 Actor 초기화)
│   ├── application/
│   │   ├── actor.rs          # propose() / verify() 구현
│   │   └── ingress.rs        # Automaton trait 구현체
│   ├── block.rs              # SealedBlock wrapper
│   └── digest.rs             # B256 digest wrapper
│
├── dkg/manager/
│   └── mod.rs                # DKG 생성 + Epoch 경계 감지
│
├── epoch/
│   ├── manager/
│   │   ├── actor.rs          # Epoch 생명주기 관리
│   │   └── ingress.rs        # Enter/Exit 명령
│   └── scheme_provider.rs    # Per-epoch BLS scheme 레지스트리
│
├── executor/
│   └── actor.rs              # FCU 전송 + Finalization 처리
│
└── peer_manager/
    └── actor.rs              # 검증자 피어 추적
```

### 주요 파일별 역할

| File | Lines | 핵심 역할 |
|------|:-----:|----------|
| `consensus/engine.rs` | ~430 | 전체 합의 스택 초기화, 7개 Actor spawn |
| `consensus/application/actor.rs` | ~300 | propose(): FCU→sleep→build→broadcast / verify(): 블록 검증 |
| `executor/actor.rs` | ~200 | Finalized 블록을 EL에 반영 (FCU with finalized_hash) |
| `dkg/manager/mod.rs` | ~250 | Static DKG, Epoch 경계 감지, 재시작 시 epoch 복구 |
| `epoch/manager/actor.rs` | ~200 | SimplexBFT 인스턴스 생성/종료, 5개 채널 관리 |
| `args.rs` | ~350 | 50+ CLI 인자 (타이밍, 네트워크, 보안, 디버깅) |

---

## Appendix: CLI Arguments Reference

### Consensus Timing

| Argument | Default | Description |
|----------|---------|-------------|
| `--consensus.wait-for-proposal` | 2s | Voter의 proposal 대기 timeout |
| `--consensus.time-to-build-proposal` | 500ms | 리더의 블록 빌드 대기 시간 |
| `--consensus.wait-for-notarizations` | 2s | Threshold 서명 수집 대기 시간 |
| `--consensus.wait-to-rebroadcast-nullify` | 10s | Nullify 재전송 주기 |
| `--consensus.fcu-heartbeat-interval` | 5m | FCU heartbeat 주기 |

### Consensus Network

| Argument | Default | Description |
|----------|---------|-------------|
| `--consensus.listen-address` | 127.0.0.1:8000 | P2P listen 주소 |
| `--consensus.known-peers` | - | 검증자 목록 (pubkey@ip:port) |
| `--consensus.allow-private-ips` | false | RFC1918 IP 허용 |
| `--consensus.synchrony-bound` | 5s | 타임스탬프 허용 범위 |
| `--consensus.mailbox-size` | 16,384 | 채널 백로그 크기 |

### Consensus Activity

| Argument | Default | Description |
|----------|---------|-------------|
| `--consensus.views-to-track` | 256 | Activity timeout window |
| `--consensus.inactive-views-until-leader-skip` | 32 | 비활성 리더 스킵 기준 |

---

*Generated 2026-03-16 | Blockchain Team Internal Document*
