# 트랜잭션 풀 (Transaction Pool) 분석

> 분석일: 2026-02-19  
> 소스 경로: `crates/transaction-pool/` (단일 crate, 내부 모듈 다수)

---

## 1. 개념 개요

### 트랜잭션 풀(Mempool)이란?

아직 블록에 포함되지 않은 **대기 중인 트랜잭션**들을 관리하는 메모리 내 저장소입니다. 네트워크에서 수신하거나 RPC로 제출된 트랜잭션을 검증하고 보관합니다. 블록 빌더(Payload Builder)가 여기서 트랜잭션을 선택합니다.

### Java 비유

```
Java 기술                              reth Transaction Pool
─────────────────                      ─────────
PriorityBlockingQueue         ═══      Pending 서브풀 (수수료 기준 정렬)
ConcurrentHashMap             ═══      Pool 내부 (해시 기반 조회)
Validator / @Valid             ═══      TransactionValidator (TX 검증)
Comparator                    ═══      TransactionOrdering (TX 정렬)
ScheduledTask (정리)           ═══      maintain_transaction_pool (유지보수)
```

---

## 2. 전체 아키텍처

```
입력 경로                              Pool 내부
──────────                             ─────────
P2P 네트워크                           ┌──────────────────────────┐
(TransactionsManager)                  │  TransactionValidator    │
  │                                    │  (검증: 서명, nonce,     │
  │  add_external_transactions()       │   잔액, 가스)            │
  └────────────────────────────────→   └────────────┬─────────────┘
                                                    │
RPC                                                 ▼
(eth_sendRawTransaction)               ┌──────────────────────────┐
  │                                    │    Pool (내부 저장소)      │
  │  add_transaction()                 │                          │
  └────────────────────────────────→   │  ┌────────┐ ┌────────┐  │
                                       │  │Pending │ │BaseFee │  │
                                       │  │(Ready) │ │        │  │
                                       │  └───┬────┘ └────────┘  │
                                       │  ┌───┴────┐ ┌────────┐  │
                                       │  │Queued  │ │ Blob   │  │
                                       │  │(Future)│ │(EIP4844)│  │
                                       │  └────────┘ └────────┘  │
                                       └──────────────────────────┘
                                                    │
출력 경로                                           │
──────────                                          ▼
Payload Builder  ←── best_transactions()    수수료 높은 순서로
P2P 전파         ←── pending_transactions() 다른 노드에 전파
```

---

## 3. 4개 서브풀 (SubPool)

| 서브풀 | 상태 | 진입 조건 | 역할 |
|--------|------|----------|------|
| **Pending** | ★ 즉시 실행 가능 | nonce 연속, 잔액 충분, 수수료 ≥ base fee | 블록 빌더가 여기서 TX 선택 |
| **BaseFee** | 거의 준비됨 | nonce 연속, 잔액 충분, 수수료 < base fee | base fee 하락 시 Pending 이동 |
| **Queued** | 미래 TX | nonce 갭 있음 또는 잔액 부족 | 선행 TX 도착 시 이동 |
| **Blob** | EIP-4844 전용 | Blob 트랜잭션, 별도 관리 | Blob 데이터의 특수 처리 |

### 서브풀 간 이동

```
새 TX 도착
    │
    ▼
검증 → 실패: 거부
    │
    ▼ 성공
    ├── nonce 갭 있음? ──→ Queued
    ├── 잔액 부족?     ──→ Queued
    ├── 수수료 < base fee? ──→ BaseFee
    ├── Blob TX?       ──→ Blob
    └── 모두 충족       ──→ Pending ★

새 블록 채굴 시:
    ├── base fee 변경 → BaseFee ↔ Pending 재분류
    ├── nonce 업데이트 → Queued → Pending 이동 가능
    └── 채굴된 TX → Pool에서 제거
```

---

## 4. 핵심 타입 분석

### 4.1 Pool — 메인 풀

> 소스: `crates/transaction-pool/src/lib.rs` L349~L352

```
Pool<V, T, S> {
    // V: TransactionValidator (검증기)
    // T: TransactionOrdering (정렬 기준)
    // S: BlobStore (Blob 저장소)
    inner: Arc<PoolInner<V, T, S>>,  // 실제 내부 구현
}
```

`Pool`은 **`Arc`로 감싸진 공유 참조** → 여러 곳에서 안전하게 공유 가능 (Java의 `ConcurrentHashMap`과 유사)

### 4.2 TransactionValidator — TX 검증

> 소스: `crates/transaction-pool/src/traits.rs`

검증 단계:

**1) 무상태(Stateless) 검증:**
- 서명 유효성
- 체인 ID 일치
- TX 크기 한도
- 가스 한도 (intrinsic gas)
- Blob 유효성 (EIP-4844)

**2) 상태 기반(Stateful) 검증:**
- 보낸 사람 계정이 EOA인지 확인
- nonce ≥ 현재 계정 nonce
- 잔액 ≥ value + (gas_limit × max_fee_per_gas)

### 4.3 TransactionOrdering — TX 정렬

> 소스: `crates/transaction-pool/src/ordering.rs`

블록 빌더가 트랜잭션을 선택할 때의 우선순위:

```
CoinbaseTipOrdering:
    우선순위 = max_priority_fee_per_gas (팁이 높을수록 우선)
    
    → 수수료 수익을 최대화하는 정렬
```

### 4.4 BlobStore — Blob 데이터 저장

> 소스: `crates/transaction-pool/src/blobstore/`

EIP-4844 Blob 트랜잭션의 대용량 데이터를 별도 저장:

| 구현체 | 저장 위치 | 용도 |
|--------|----------|------|
| `InMemoryBlobStore` | 메모리 | 테스트/개발 |
| `DiskFileBlobStore` | 디스크 | 프로덕션 (메모리 절약) |

---

## 5. 트랜잭션 생명주기

```
1. TX 수신 (P2P 또는 RPC)
   │
2. 무상태 검증 (서명, 크기 등)
   │  실패 → 거부 + 에러 반환
   │
3. 상태 기반 검증 (nonce, 잔액 등)
   │  실패 → 거부 + 에러 반환
   │
4. 서브풀 분류 → Pending / BaseFee / Queued / Blob
   │
5. 풀 크기 제한 확인
   │  초과 → 가장 낮은 수수료 TX 퇴출
   │
6. P2P 전파 (External TX인 경우)
   │
7. 블록에 포함 (Payload Builder가 선택)
   │  또는 퇴출/만료
   │
8. 블록 채굴 확인 → Pool에서 제거
```

---

## 6. 풀 유지보수 (Pool Maintenance)

> 소스: `crates/transaction-pool/src/maintain.rs` (40KB)

`maintain_transaction_pool_future` — 새 블록이 채굴될 때마다 풀을 업데이트하는 백그라운드 태스크:

| 이벤트 | 처리 |
|--------|------|
| **새 블록 (Commit)** | 채굴된 TX 제거, 변경된 계정 상태 반영, 서브풀 재분류 |
| **체인 재구성 (Reorg)** | 되감긴 블록의 TX를 풀에 재추가, 새 체인의 TX 제거 |
| **Base fee 변경** | BaseFee ↔ Pending 서브풀 간 TX 이동 |
| **Blob fee 변경** | Blob 서브풀의 TX 가격 재평가 |

---

## 7. TX 원본(Origin) 분류

| 원본 | 설명 | 전파 여부 | 특별 처리 |
|------|------|----------|----------|
| **External** | P2P 네트워크에서 수신 | ✅ 다른 노드에 전파 | 없음 |
| **Local** | 로컬 RPC로 제출 | ✅ (설정 가능) | 퇴출 우선순위 낮음 |
| **Private** | 프라이빗 RPC | ❌ 전파 안 함 | MEV 보호 등 |

---

## 8. 설정 (PoolConfig)

> 소스: `crates/transaction-pool/src/config.rs`

| 설정 | 기본값 | 설명 |
|------|--------|------|
| `pending_pool.max_count` | 10,000 | Pending 풀 최대 TX 수 |
| `pending_pool.max_size` | 20MB | Pending 풀 최대 메모리 |
| `basefee_pool.max_count` | 10,000 | BaseFee 풀 최대 TX 수 |
| `queued_pool.max_count` | 10,000 | Queued 풀 최대 TX 수 |
| `blob_pool.max_count` | 5,000 | Blob 풀 최대 TX 수 |
| `price_bump` | 10% | 교체 시 필요한 가격 인상률 |

---

## 9. 커스터마이징 포인트

| 포인트 | 방법 | 용도 |
|--------|------|------|
| **TX 검증 규칙** | `TransactionValidator` trait 구현 | 커스텀 검증 로직 (허용 목록, 제한 등) |
| **TX 정렬 기준** | `TransactionOrdering` trait 구현 | MEV 전략, 공정성 정렬 |
| **풀 크기 조정** | `PoolConfig` 파라미터 | 메모리/성능 튜닝 |
| **Blob 저장소** | `BlobStore` trait 구현 | 커스텀 Blob 저장 전략 |
| **TX 필터링** | 검증기에서 특정 TX 거부 | 스팸 방지, 허용 목록 |
| **가격 범프 정책** | `PriceBumpConfig` | TX 교체 가격 정책 변경 |
