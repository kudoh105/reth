# 커스터마이징 포인트 식별

> 분석일: 2026-02-19  
> 소스 경로: `crates/node/`, `crates/exex/`, `examples/` (28개 예제)

---

## 1. 개념 개요

reth는 **"모듈러 바이 디자인"**을 표방하며, 거의 모든 핵심 컴포넌트를 trait(인터페이스)로 추상화하여 교체 가능하게 설계되었습니다. 이 문서는 **금융 프로젝트에서 활용할 수 있는 커스터마이징 포인트**를 종합합니다.

### Java 비유

```
Java 기술                              reth 커스터마이징
─────────────────                      ─────────
Spring의 @Bean 교체             ═══      Node Builder의 with_*() 메서드
Spring Plugin Architecture     ═══      ExEx (Execution Extension) 플러그인
Spring의 Profile/Configuration ═══      Feature Flags + Chain Spec
SPI (ServiceLoader)            ═══      trait 구현체 교체
```

---

## 2. 커스터마이징 계층도

```
커스터마이징 난이도 (쉬움 → 어려움)
═══════════════════════════════════════════════════════════

① 설정 변경          ② ExEx 플러그인       ③ 컴포넌트 교체        ④ 코어 수정
(가장 쉬움)           (플러그인 방식)        (trait 교체)          (포크 방식)
                                                               
toml 설정 파일       ExExContext 구독      PayloadBuilder 교체   블록 구조 변경
CLI 플래그           이벤트 처리            EVM 규칙 교체         합의 알고리즘 변경
RPC 모듈 선택        인덱서/브리지         TX 검증 규칙 교체      프리미티브 타입 변경
프루닝 설정          롤업 파생              RPC 네임스페이스 추가  
                                          P2P 서브프로토콜 추가   
```

---

## 3. 28개 공식 예제 분류

> 소스: `examples/`

### Level 1: 설정/관찰 (비침습적)

| 예제 | 설명 | 난이도 |
|------|------|--------|
| `db-access` | DB 직접 접근으로 데이터 조회 | ★☆☆ |
| `node-event-hooks` | 노드 이벤트 후크 등록 | ★☆☆ |
| `beacon-api-sse` | Beacon API SSE 이벤트 수신 | ★☆☆ |
| `network` | 네트워크 설정 및 피어 연결 | ★☆☆ |
| `full-contract-state` | 전체 컨트랙트 상태 조회 | ★☆☆ |

### Level 2: RPC/네트워크 확장

| 예제 | 설명 | 난이도 |
|------|------|--------|
| `node-custom-rpc` | 커스텀 RPC 네임스페이스 추가 | ★★☆ |
| `custom-rpc-middleware` | RPC 미들웨어 레이어 추가 | ★★☆ |
| `rpc-db` | RPC에서 DB 직접 접근 | ★★☆ |
| `custom-rlpx-subprotocol` | 커스텀 P2P 서브프로토콜 | ★★☆ |
| `manual-p2p` | 수동 P2P 연결 관리 | ★★☆ |
| `network-proxy` | 네트워크 프록시 | ★★☆ |
| `network-txpool` | 네트워크 TX 풀 연동 | ★★☆ |

### Level 3: 핵심 컴포넌트 교체

| 예제 | 설명 | 난이도 |
|------|------|--------|
| `custom-evm` | **커스텀 EVM 규칙** (프리컴파일 등) | ★★★ |
| `custom-payload-builder` | **커스텀 블록 빌더** | ★★★ |
| `custom-node-components` | **노드 컴포넌트 전체 교체** | ★★★ |
| `custom-engine-types` | **커스텀 엔진 타입** | ★★★ |
| `custom-dev-node` | 커스텀 개발 노드 | ★★☆ |
| `custom-inspector` | 커스텀 EVM 인스펙터 | ★★☆ |
| `custom-hardforks` | 커스텀 하드포크 정의 | ★★★ |
| `custom-beacon-withdrawals` | Beacon 출금 커스터마이징 | ★★★ |
| `precompile-cache` | 프리컴파일 캐시 | ★★☆ |

### Level 4: ExEx (Execution Extension) 플러그인

| 예제 | 설명 | 난이도 |
|------|------|--------|
| `exex-subscription` | ExEx 이벤트 구독 | ★★☆ |
| `exex-test` | ExEx 테스트 프레임워크 | ★★☆ |
| `txpool-tracing` | TX 풀 트레이싱 (ExEx 활용) | ★★☆ |

### Level 5: 타 체인 연동

| 예제 | 설명 | 난이도 |
|------|------|--------|
| `bsc-p2p` | BNB Smart Chain P2P 연동 | ★★★ |
| `polygon-p2p` | Polygon P2P 연동 | ★★★ |
| `node-builder-api` | Node Builder API 활용 | ★★★ |

---

## 4. 핵심 커스터마이징 포인트 상세

### 4.1 ExEx (Execution Extension) — 가장 강력한 플러그인 시스템

> 소스: `crates/exex/`

**코드 변경 없이** 노드에 새로운 기능을 추가하는 플러그인 시스템:

```
ExExContext {
    notifications: CanonStateNotifications, // ★ 모든 블록 실행 결과 수신
    components: FullNodeComponents,          // 노드 전체 컴포넌트 접근
}

사용 예:
node_builder
    .install_exex("my-indexer", |ctx: ExExContext<_>| async {
        // 블록 실행 알림을 받아서 인덱싱
        while let Some(notification) = ctx.notifications.next().await {
            process_blocks(notification);
            ctx.send_finished_height(tip);
        }
    });
```

**금융 프로젝트 활용:**
- 거래 이력 인덱서
- 잔액 변동 모니터링
- 규제 준수 감사 로그
- 브리지/롤업 파생

### 4.2 커스텀 EVM — 실행 규칙 변경

> 소스: `examples/custom-evm/`

`ConfigureEvm` trait을 구현하여 EVM 동작을 변경:

```
커스텀 가능 항목:
├── 프리컴파일(Precompile) 추가/교체
│   → 예: 커스텀 서명 검증, 오라클 연동
├── 가스 비용 조정
├── 옵코드 동작 변경
└── EVM 인스펙터(Inspector) 주입
    → 예: 트랜잭션 트레이싱, 디버깅
```

### 4.3 커스텀 TX 검증 — 트랜잭션 필터링

`TransactionValidator` trait 구현으로 TX 풀 진입 규칙 변경:

```
커스텀 가능 항목:
├── 화이트리스트/블랙리스트
├── 최대 가스 제한 커스텀
├── 특정 TX 타입 차단
├── 수수료 최소값 정책
└── 커스텀 서명 방식 지원
```

### 4.4 커스텀 블록 빌더 — 블록 구성 전략

`PayloadBuilder` trait 구현으로 블록 빌드 전략 변경:

```
커스텀 가능 항목:
├── TX 선택 전략 (수수료 극대화, 공정성 등)
├── 블록 크기 정책
├── MEV 전략 (번들 포함, 백러닝 방지 등)
├── 블록 내 TX 순서 결정
└── 커스텀 블록 필드 추가
```

### 4.5 커스텀 RPC — API 확장

커스텀 RPC 네임스페이스 추가:

```
node_builder
    .rpc(|ctx| {
        ctx.modules.merge_configured(
            MyCustomRpc::new(ctx.registry).into_rpc()
        );
    });
```

### 4.6 커스텀 P2P 서브프로토콜

`IntoRlpxSubProtocol` trait으로 ETH 외 추가 프로토콜 지원:

```
커스텀 가능 항목:
├── 프라이빗 메시지 교환
├── 커스텀 데이터 동기화
└── 사이드체인 통신
```

### 4.7 커스텀 하드포크 — 프로토콜 규칙 변경

> 소스: `examples/custom-hardforks/`

`ChainSpec`을 커스터마이징하여 자체 하드포크 규칙 정의:

```
커스텀 가능 항목:
├── 블록 보상 변경
├── 가스 정책 변경
├── 새로운 프리컴파일 활성화 시점
└── 옵코드 동작 변경 시점
```

---

## 5. Node Builder API — 통합 진입점

> 소스: `crates/node/builder/`

모든 커스터마이징을 하나의 빌더 API로 통합:

```
NodeBuilder::new(config)
    .with_types::<MyNode>()           // 노드 타입 정의
    .with_components(|builder| {
        builder
            .evm(MyEvmConfig::new())          // ① EVM 교체
            .payload(MyPayloadBuilder::new())  // ② 블록 빌더 교체
            .pool(MyTxPool::new())            // ③ TX 풀 교체
            .network(MyNetworkConfig::new())  // ④ 네트워크 설정
    })
    .extend_rpc_modules(|ctx| {               // ⑤ RPC 확장
        ctx.modules.merge_configured(MyRpc.into_rpc());
    })
    .install_exex("indexer", my_indexer)       // ⑥ ExEx 플러그인
    .launch()
    .await;
```

---

## 6. 금융 프로젝트 활용 시나리오

### 시나리오 1: 프라이빗 이더리움 네트워크 (허가형)

| 커스터마이징 | 구현 방법 |
|-------------|----------|
| 허가된 노드만 참여 | `PeersConfig` + 신뢰 피어 고정 |
| TX 제출자 화이트리스트 | `TransactionValidator` 커스텀 |
| 커스텀 합의 | `Consensus` trait 구현 |
| KYC 데이터 인덱싱 | ExEx 플러그인 |

### 시나리오 2: 토큰 거래 플랫폼

| 커스터마이징 | 구현 방법 |
|-------------|----------|
| 거래 모니터링 API | 커스텀 RPC 네임스페이스 |
| 실시간 잔액 추적 | ExEx + WebSocket 이벤트 |
| 프론트러닝 방지 | 커스텀 `TransactionOrdering` |
| 커스텀 프리컴파일 | `ConfigureEvm` 교체 |

### 시나리오 3: 규제 준수 노드

| 커스터마이징 | 구현 방법 |
|-------------|----------|
| 감사 로그 | ExEx로 모든 상태 변경 기록 |
| 제재 주소 차단 | `TransactionValidator`에서 차단 |
| 보고서 생성 API | 커스텀 RPC |
| 데이터 보존 정책 | `PruneModes` 설정 |

---

## 7. 커스터마이징 결정 트리

```
커스터마이징 목표
    │
    ├── 상태 변경 모니터링? ──→ ExEx 플러그인 (코드 변경 최소)
    │
    ├── 새로운 API 추가? ──→ 커스텀 RPC 네임스페이스
    │
    ├── TX 필터링/정렬? ──→ TransactionValidator / TransactionOrdering
    │
    ├── 블록 빌드 전략? ──→ PayloadBuilder trait 구현
    │
    ├── EVM 실행 규칙? ──→ ConfigureEvm trait 구현
    │
    ├── 네트워크 프로토콜? ──→ RLPx 서브프로토콜
    │
    └── 체인 규칙 전체? ──→ ChainSpec + Hardfork 정의 (가장 복잡)
```
