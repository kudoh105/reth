# 네트워킹 P2P 분석

> 분석일: 2026-02-19  
> 소스 경로: `crates/net/` (14개 하위 crate)

---

## 1. 개념 개요

### 이더리움 P2P 네트워크란?

이더리움 노드들은 **devp2p** 프로토콜을 통해 서로 통신합니다. 중앙 서버 없이 노드들이 직접 연결하여 블록, 트랜잭션 등을 교환합니다.

### Java 비유

```
Java 기술                              reth P2P
─────────────────                      ─────────
Netty Server + EventLoop      ═══      NetworkManager (tokio 기반 이벤트 루프)
ChannelHandler                ═══      SessionManager (RLPx 세션 관리)
ServiceDiscovery (Eureka 등)  ═══      Discovery (discv4/discv5/DNS)
ConnectionPool                ═══      PeersManager (피어 풀 + 평판 관리)
MessageRouter                 ═══      Swarm (메시지 라우팅)
```

---

## 2. 전체 아키텍처

```
                                    ┌─────────────────┐
                                    │  NetworkHandle   │ ← 외부 API (클론 가능)
                                    └────────┬────────┘
                                             │ command channel
                                             ▼
┌──────────────────────────────────────────────────────────────────┐
│                        NetworkManager                            │
│  ┌───────────────────────────────────────────────────────┐       │
│  │                     Swarm                              │       │
│  │  ┌─────────────┐ ┌──────────────┐ ┌──────────────┐   │       │
│  │  │ Connection  │ │   Session    │ │  Network     │   │       │
│  │  │  Listener   │ │  Manager     │ │   State      │   │       │
│  │  │ (TCP 수신)  │ │ (RLPx 세션)  │ │(피어+상태)    │   │       │
│  │  └─────────────┘ └──────────────┘ └──────────────┘   │       │
│  └───────────────────────────────────────────────────────┘       │
│                                                                   │
│  ┌──────────────────────────────────────────────────────────────┐ │
│  │  별도 Task들 (tokio::spawn)                                  │ │
│  │                                                              │ │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐       │ │
│  │  │ Discovery    │  │ Transactions │  │ ETH Request  │       │ │
│  │  │  Task        │  │  Manager     │  │  Handler     │       │ │
│  │  │ (피어 발견)   │  │ (TX 전파)    │  │ (요청 응답)   │       │ │
│  │  └──────────────┘  └──────────────┘  └──────────────┘       │ │
│  └──────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────┘
```

### 핵심 구성요소 요약

| 구성요소 | 소스 위치 | 역할 |
|---------|----------|------|
| **NetworkManager** | `crates/net/network/src/manager.rs` | 네트워크 전체 상태를 관리하는 메인 이벤트 루프 |
| **Swarm** | `crates/net/network/src/swarm.rs` | 연결/세션/상태를 묶어 폴링하는 내부 컨테이너 |
| **NetworkHandle** | `crates/net/network/src/network.rs` | 외부에서 네트워크와 상호작용하는 핸들 (클론 가능) |
| **SessionManager** | `crates/net/network/src/session/` | RLPx 암호화 세션 생명주기 관리 |
| **PeersManager** | `crates/net/network/src/peers.rs` | 피어 풀 + 평판(Reputation) 시스템 |
| **Discovery** | `crates/net/network/src/discovery.rs` | discv4/v5/DNS 피어 발견 통합 |
| **TransactionsManager** | `crates/net/network/src/transactions/` | 트랜잭션 가십, 요청, 브로드캐스트 |
| **EthRequestHandler** | `crates/net/network/src/eth_requests.rs` | 원격 피어의 헤더/바디 요청 처리 |

---

## 3. 네트워크 관련 Crate 구조

> 소스: `crates/net/` (14개 하위 crate)

```
crates/net/
├── network/         ★ 핵심: NetworkManager, Swarm, Session, Peers
├── network-api/     네트워크 인터페이스 (trait 정의)
├── network-types/   네트워크 공통 타입 (PeersConfig, SessionsConfig)
├── p2p/             P2P 추상화 (HeaderDownloader, BodyDownloader trait)
├── eth-wire/        ETH 프로토콜 메시지 인코딩/디코딩 (RLPx)
├── eth-wire-types/  ETH 와이어 프로토콜 타입 정의
├── ecies/           ECIES 암호화 (RLPx 핸드셰이크)
├── discv4/          Node Discovery v4 (Kademlia DHT)
├── discv5/          Node Discovery v5 (ENR 기반)
├── dns/             DNS 기반 피어 발견 (EIP-1459)
├── downloaders/     헤더/바디 다운로더 구현체
├── peers/           피어 관련 유틸리티 (NodeRecord, PeerId)
├── banlist/         밴 리스트 관리
└── nat/             NAT 통과 (UPnP, STUN 등)
```

---

## 4. 연결 수립 과정

### 4.1 피어 발견 (Discovery)

```
1. Discovery v4 (Kademlia DHT)
   ├── UDP 기반
   ├── PING/PONG → FINDNODE → NEIGHBOURS
   └── 가장 가까운 노드를 점진적으로 탐색

2. Discovery v5 (ENR 기반)
   ├── UDP 기반, EIP-778 ENR 레코드
   └── 토픽 기반 피어 발견 지원

3. DNS Discovery (EIP-1459)
   ├── DNS TXT 레코드에서 피어 목록 조회
   └── 방화벽 뒤의 노드도 피어 찾기 가능

→ 발견된 피어 → PeersManager에 등록
```

### 4.2 RLPx 세션 수립

```
피어 발견                  TCP 연결              RLPx 핸드셰이크          ETH 서브프로토콜
   │                         │                      │                       │
   ▼                         ▼                      ▼                       ▼
PeersManager       ConnectionListener     ECIES 암호화 핸드셰이크    Status 메시지 교환
"이 피어에         또는 dial_outbound()     (secp256k1 키 교환)        (chain ID, genesis,
 연결할까?"                                                           head block 등)
   │                         │                      │                       │
   └─────────────────────────┴──────────────────────┴───────────────────────┘
                                     │
                                     ▼
                          ActiveSession 생성 (양방향 통신 가능)
```

> **소스**: `crates/net/network/src/manager.rs` — `new()` (L237~L370)

### 4.3 세션 확립 후 이벤트 흐름

```
ActiveSession (피어별 독립 Task)
    │
    ├── 수신 메시지 → SessionEvent → Swarm → NetworkManager
    │                                           │
    │                                    ┌──────┴──────┐
    │                                    │             │
    │                              PeerMessage    ETH Request
    │                              (메시지 타입별 분기)
    │                                    │             │
    │                               ┌────┴────┐   EthRequestHandler
    │                               │         │   (GetHeaders, GetBodies)
    │                          NewBlock   Transactions
    │                          NewBlockHashes  PooledTransactions
    │                               │         │
    │                          BlockImport  TransactionsManager
    │
    └── 발신 메시지 ← SessionCommand ← NetworkHandle
```

---

## 5. 핵심 컴포넌트 상세

### 5.1 NetworkManager — 이벤트 루프

> 소스: `crates/net/network/src/manager.rs` L108~L151

`NetworkManager`는 **tokio Future**로 구현되어 무한 루프로 폴링됩니다:

```
poll() {
  1. from_handle_rx 폴링 → on_handle_message()  // NetworkHandle에서 온 명령
  2. swarm 폴링 → on_swarm_event()              // 내부 네트워크 이벤트
  3. block_import 폴링 → on_block_import_result() // 블록 임포트 결과
}
```

핸들 메시지 처리 (L666~L765):
- `AnnounceBlock`: 새 블록 전파
- `EthRequest`: 원격 피어에 요청 전송
- `SendTransaction`: 트랜잭션 전송
- `AddPeerAddress` / `RemovePeer`: 피어 추가/제거
- `DisconnectPeer`: 피어 연결 해제
- `SetNetworkState`: Active/Hibernate 전환

### 5.2 PeersManager — 피어 풀 + 평판 시스템

> 소스: `crates/net/network/src/peers.rs` (121KB — 가장 큰 파일)

**평판(Reputation) 기반 피어 관리** 시스템:

| 기능 | 설명 |
|------|------|
| **평판 점수** | 각 피어에 정수형 평판 점수 부여 (좋은 행동 ↑, 나쁜 행동 ↓) |
| **자동 밴** | 평판이 임계값 이하로 떨어지면 자동 밴 |
| **Backoff** | 연결 실패/나쁜 행동 시 일정 시간 재연결 차단 |
| **연결 제한** | 최대 인바운드/아웃바운드 연결 수 제한 |
| **Trusted Peers** | 신뢰 피어는 항상 연결 유지 시도 |

Java 비유: **커넥션 풀 + Circuit Breaker + 블랙리스트** 조합

### 5.3 SessionManager — RLPx 세션

> 소스: `crates/net/network/src/session/`

각 피어 연결은 독립적인 **RLPx 세션**으로 관리됩니다:

- **PendingSession**: 핸드셰이크 진행 중
- **ActiveSession**: 핸드셰이크 완료, 양방향 통신 가능
- 각 ActiveSession은 별도 tokio Task에서 실행
- ECIES 기반 암호화 통신

### 5.4 ETH 프로토콜 메시지

> 소스: `crates/net/eth-wire-types/`

주요 ETH 프로토콜 메시지:

| 메시지 | 방향 | 용도 |
|--------|------|------|
| `Status` | 양방향 | 세션 수립 시 체인 정보 교환 |
| `NewBlockHashes` | 브로드캐스트 | 새 블록 해시 알림 |
| `NewBlock` | 브로드캐스트 | 새 블록 전체 전파 |
| `GetBlockHeaders` | 요청 | 블록 헤더 요청 |
| `BlockHeaders` | 응답 | 블록 헤더 응답 |
| `GetBlockBodies` | 요청 | 블록 바디 요청 |
| `BlockBodies` | 응답 | 블록 바디 응답 |
| `Transactions` | 브로드캐스트 | 전체 트랜잭션 전파 |
| `NewPooledTransactionHashes` | 브로드캐스트 | 트랜잭션 해시 알림 |
| `GetPooledTransactions` | 요청 | 트랜잭션 요청 |
| `PooledTransactions` | 응답 | 트랜잭션 응답 |

### 5.5 TransactionsManager

> 소스: `crates/net/network/src/transactions/`

트랜잭션의 네트워크 전파를 담당:

- **수신**: 원격 피어에서 온 트랜잭션을 로컬 TX Pool에 추가
- **발신**: 로컬 TX Pool의 새 트랜잭션을 피어들에게 브로드캐스트
- **해시 기반 전파**: 먼저 해시만 전파 → 없는 피어가 전체 TX 요청
- **중복 방지**: 이미 본 트랜잭션은 재전파하지 않음

### 5.6 FetchClient — 다운로더용 클라이언트

> 소스: `crates/net/network/src/fetch/`

Staged Sync의 `HeaderDownloader`/`BodyDownloader`가 네트워크에 데이터를 요청하는 인터페이스:

```
HeaderStage → HeaderDownloader → FetchClient → SessionManager → 원격 피어
```

요청은 우선순위 큐로 관리되며, 가장 적합한 피어를 선택하여 전송합니다.

---

## 6. 네트워크 생명주기

```
1. 설정 로드
   NetworkConfig::builder(secret_key)
     .boot_nodes(mainnet_nodes())
     .build(provider)

2. 네트워크 생성
   NetworkManager::new(config).await

3. 빌더 패턴으로 구성
   NetworkManager::builder(config)
     .transactions(pool, tx_config)     // TX Manager 연결
     .request_handler(client)            // ETH 요청 핸들러 연결
     .split_with_handle()                // handle + tasks 분리

4. 실행
   tokio::task::spawn(network);          // 메인 이벤트 루프
   tokio::task::spawn(transactions);     // TX 관리 Task
   tokio::task::spawn(request_handler);  // ETH 요청 처리 Task

5. 외부 상호작용
   handle.add_peer(peer_id, addr);       // 피어 추가
   handle.fetch_client();                // 다운로더 클라이언트 획득
```

---

## 7. 커스터마이징 포인트

| 포인트 | 방법 | 용도 |
|--------|------|------|
| **커스텀 서브프로토콜** | `add_rlpx_sub_protocol()` + `IntoRlpxSubProtocol` trait | ETH 외 추가 프로토콜 지원 |
| **피어 관리 정책** | `PeersConfig` 파라미터 조정 | 연결 수, 평판 임계값, 신뢰 피어 |
| **트랜잭션 전파 규칙** | `TransactionsManagerConfig` | TX 브로드캐스트 동작 커스터마이징 |
| **네트워크 프리미티브** | `NetworkPrimitives` trait 구현 | 커스텀 블록/TX 타입 지원 |
| **블록 임포트 로직** | `BlockImport` trait 구현 | 수신 블록 검증/처리 커스터마이징 |
| **피어 필터** | `RequiredBlockFilter` | 특정 블록을 가진 피어만 연결 |
| **네트워크 모드** | `NetworkConnectionState` | Active/Hibernate 전환 |
