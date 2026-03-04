# Reth 핵심 데이터 타입 (Primitives) 분석

> 분석일: 2026-02-19  
> 분석 근거: `crates/primitives-traits/src/`, `crates/primitives/src/`, `crates/ethereum/primitives/src/`

---

## 1. Primitives 전체 구조

### 1.1 3계층 타입 아키텍처

Reth의 데이터 타입은 **3계층**으로 나뉘어 있습니다. 이 구조가 reth의 "체인 독립적 설계"의 핵심입니다.

```
┌────────────────────────────────────────────────────────┐
│  Layer 3: crates/ethereum/primitives/                  │
│  이더리움 전용 구체 타입 (Concrete Types)               │
│  - Block, TransactionSigned, Receipt, EthPrimitives    │
│  Java 비유: @Entity 클래스 (실제 DB 매핑 객체)           │
├────────────────────────────────────────────────────────┤
│  Layer 2: crates/primitives/                           │
│  re-export + 편의 래퍼                                  │
│  - 대부분 Layer 1/3에서 re-export                       │
│  Java 비유: 파사드 패키지 (한 곳에서 import)             │
├────────────────────────────────────────────────────────┤
│  Layer 1: crates/primitives-traits/                    │
│  추상 인터페이스 (Trait 정의)                            │
│  - Block, BlockBody, SignedTransaction, Receipt trait   │
│  - NodePrimitives trait (전체 타입 조합 정의)            │
│  Java 비유: interface/abstract class 모음               │
└────────────────────────────────────────────────────────┘
```

> **Java 개발자를 위한 핵심 포인트**: 이것은 Java의 `interface → ServiceImpl` 패턴과 동일합니다.
> `primitives-traits`가 interface를 정의하고, `ethereum/primitives`가 이더리움용 구현체를 제공합니다.
> 다른 EVM 체인(예: Polygon)은 같은 trait을 다른 방식으로 구현하면 됩니다.

### 1.2 관련 소스 파일 맵

```
crates/primitives-traits/src/
├── lib.rs                 ← 모든 trait re-export
├── node.rs                ← ⭐ NodePrimitives trait (타입 시스템의 루트)
├── block/
│   ├── mod.rs             ← ⭐ Block trait
│   ├── body.rs            ← ⭐ BlockBody trait
│   ├── sealed.rs          ← SealedBlock (해시 포함 블록)
│   ├── recovered.rs       ← RecoveredBlock (서명자 복구된 블록)
│   ├── header.rs          ← BlockHeader 관련
│   └── error.rs           ← 블록 에러 타입
├── header/
│   ├── mod.rs             ← BlockHeader trait
│   ├── sealed.rs          ← SealedHeader
│   └── header_mut.rs      ← 헤더 수정 trait
├── transaction/
│   ├── mod.rs             ← Transaction 관련 trait
│   ├── signed.rs          ← ⭐ SignedTransaction trait
│   ├── recover.rs         ← 서명 복구
│   ├── signature.rs       ← 서명 관련
│   ├── error.rs           ← 트랜잭션 에러
│   ├── access_list.rs     ← 접근 리스트
│   └── execute.rs         ← 실행 관련
├── account.rs             ← ⭐ Account struct
├── receipt.rs             ← ⭐ Receipt trait
├── storage.rs             ← StorageEntry struct
├── size.rs                ← InMemorySize trait
├── proofs.rs              ← 머클 루트 계산
└── withdrawal.rs          ← Withdrawal 타입

crates/ethereum/primitives/src/
├── lib.rs                 ← ⭐ EthPrimitives (이더리움 NodePrimitives 구현)
├── receipt.rs             ← ⭐ EthereumReceipt 구체 타입
└── transaction.rs         ← TransactionSigned 호환 테스트
```

---

## 2. NodePrimitives — 타입 시스템의 루트

> **소스**: `crates/primitives-traits/src/node.rs`

### 2.1 정의

```rust
// Java로 치면: 모든 도메인 타입을 하나로 묶는 최상위 interface
pub trait NodePrimitives:
    Send + Sync + Unpin + Clone + Default + fmt::Debug + PartialEq + Eq + 'static
{
    /// 블록 타입 (= Java의 Block.class)
    type Block: FullBlock<Header = Self::BlockHeader, Body = Self::BlockBody>
        + MaybeSerdeBincodeCompat;
    /// 블록 헤더 타입
    type BlockHeader: FullBlockHeader;
    /// 블록 바디 타입
    type BlockBody: FullBlockBody<Transaction = Self::SignedTx, OmmerHeader = Self::BlockHeader>;
    /// 서명된 트랜잭션 타입
    type SignedTx: FullSignedTx;
    /// 영수증 타입
    type Receipt: FullReceipt;
}
```

### 2.2 이더리움 구현

```rust
// 소스: crates/ethereum/primitives/src/lib.rs

/// 이더리움의 NodePrimitives 구현
pub struct EthPrimitives;

impl NodePrimitives for EthPrimitives {
    type Block = crate::Block;               // alloy_consensus::Block<TransactionSigned>
    type BlockHeader = alloy_consensus::Header;
    type BlockBody = crate::BlockBody;        // alloy_consensus::BlockBody<TransactionSigned>
    type SignedTx = crate::TransactionSigned; // EthereumTxEnvelope<TxEip4844>
    type Receipt = crate::Receipt;            // EthereumReceipt<TxType>
}
```

### 2.3 Java 비유: 제네릭 타입 레지스트리

```java
// Java로 표현하면 이런 느낌입니다:
public interface NodePrimitives<
    BLK extends Block,
    HDR extends BlockHeader,
    BODY extends BlockBody,
    TX extends SignedTransaction,
    RCP extends Receipt
> { }

// 이더리움 구현
public class EthPrimitives implements NodePrimitives<
    EthBlock, EthHeader, EthBlockBody, EthTransactionSigned, EthReceipt
> { }
```

> **왜 이런 구조인가?**: reth가 이더리움 뿐만 아니라 Optimism, Polygon 등 다른 EVM 체인도 같은 코드베이스로 지원하기 위해서입니다. 각 체인은 자신만의 `NodePrimitives` 구현을 만들면 됩니다.

### 2.4 타입 별칭 (Type Alias) 헬퍼

```rust
// 복잡한 associated type 접근을 단순화하는 헬퍼
pub type HeaderTy<N> = <N as NodePrimitives>::BlockHeader;
pub type BodyTy<N>   = <N as NodePrimitives>::BlockBody;
pub type BlockTy<N>  = <N as NodePrimitives>::Block;
pub type ReceiptTy<N>= <N as NodePrimitives>::Receipt;
pub type TxTy<N>     = <N as NodePrimitives>::SignedTx;
```

Java에서 `NodePrimitives.getBlockType()` 메서드를 호출하는 것과 비슷합니다.

---

## 3. Block — 블록 타입

> **소스**: `crates/primitives-traits/src/block/mod.rs`

### 3.1 Block Trait (인터페이스)

```rust
// 핵심만 추출한 Block trait
pub trait Block: Send + Sync + Unpin + Clone + fmt::Debug + PartialEq + Eq
    + Encodable + Decodable + ... 
{
    type Header: BlockHeader;
    type Body: BlockBody;

    // 블록 생성
    fn new(header: Self::Header, body: Self::Body) -> Self;
    
    // 블록 접근
    fn header(&self) -> &Self::Header;
    fn body(&self) -> &Self::Body;
    
    // 블록 분해
    fn split(self) -> (Self::Header, Self::Body);
    
    // 봉인(Sealing) — 블록 해시 계산 후 변경 불가능하게 만듬
    fn seal_slow(self) -> SealedBlock<Self>;
    fn seal_unchecked(self, hash: B256) -> SealedBlock<Self>;
    
    // 서명자 복구 — 서명에서 보낸이 주소 복원
    fn recover_signers(&self) -> Result<Vec<Address>, RecoveryError>;
    fn try_into_recovered(self) -> Result<RecoveredBlock<Self>, BlockRecoveryError<Self>>;
}
```

### 3.2 블록 상태 변환 흐름

```
Block (기본 블록)
  │
  │ seal_slow() — 해시 계산
  ▼
SealedBlock (해시 포함, 변경 불가)
  │
  │ try_recover() — 트랜잭션 서명자 복구
  ▼
RecoveredBlock (서명자 정보 포함)
  │
  │ into_sealed_block() — 서명자 정보 제거
  ▼
SealedBlock (다시 봉인 상태로)
```

**Java 비유**:
```java
// Block → SealedBlock: @Immutable 처리
// SealedBlock → RecoveredBlock: 서명 검증 후 보낸이 주소 캐싱
// 이는 Java에서 DTO → 검증된 DTO → 보강된 DTO 변환 패턴과 유사
```

### 3.3 BlockBody Trait

```rust
// 소스: crates/primitives-traits/src/block/body.rs
pub trait BlockBody: Send + Sync + Unpin + Clone + fmt::Debug + ... {
    /// 트랜잭션 타입
    type Transaction: SignedTransaction;
    /// 옴머 헤더 타입 (PoW 시절의 잔재, PoS에서는 비어있음)
    type OmmerHeader: BlockHeader;

    // 트랜잭션 관련
    fn transactions(&self) -> &[Self::Transaction];
    fn transactions_iter(&self) -> impl Iterator<Item = &Self::Transaction>;
    fn transaction_count(&self) -> usize;
    fn transaction_by_hash(&self, hash: &B256) -> Option<&Self::Transaction>;
    
    // 출금 관련 (Shanghai 하드포크 이후)
    fn withdrawals(&self) -> Option<&Withdrawals>;
    
    // 머클 루트 계산
    fn calculate_tx_root(&self) -> B256;
    fn calculate_withdrawals_root(&self) -> Option<B256>;
    
    // 서명자 복구
    fn recover_signers(&self) -> Result<Vec<Address>, RecoveryError>;
    
    // Blob 가스 (EIP-4844)
    fn blob_gas_used(&self) -> u64;
}
```

### 3.4 이더리움 Block 구체 타입

```rust
// 소스: crates/ethereum/primitives/src/lib.rs

/// 이더리움 블록 = alloy의 Block<TransactionSigned>
pub type Block = alloy_consensus::Block<TransactionSigned>;

/// 이더리움 블록 바디
pub type BlockBody = alloy_consensus::BlockBody<TransactionSigned>;
```

> **핵심 발견**: reth는 블록과 헤더의 구체 타입으로 **alloy_consensus** 라이브러리의 타입을 그대로 사용합니다. reth가 직접 정의하는 것은 trait(인터페이스)이고, 실제 데이터 구조는 alloy에 위임합니다.

### 3.5 블록 구조 다이어그램

```
Block (alloy_consensus::Block<TransactionSigned>)
├── header: Header (alloy_consensus::Header)
│   ├── parent_hash: B256           ← 부모 블록 해시
│   ├── ommers_hash: B256           ← 옴머 해시 (PoS에서는 빈값)
│   ├── beneficiary: Address        ← 블록 생성자 주소
│   ├── state_root: B256            ← 상태 트라이 루트
│   ├── transactions_root: B256     ← 트랜잭션 트라이 루트
│   ├── receipts_root: B256         ← 영수증 트라이 루트
│   ├── logs_bloom: Bloom           ← 로그 블룸 필터
│   ├── difficulty: U256            ← 난이도 (PoS에서는 0)
│   ├── number: u64                 ← 블록 번호
│   ├── gas_limit: u64              ← 가스 한도
│   ├── gas_used: u64               ← 사용된 가스
│   ├── timestamp: u64              ← 타임스탬프 (Unix)
│   ├── extra_data: Bytes           ← 추가 데이터
│   ├── mix_hash: B256              ← 믹스 해시 (PoS에서는 RANDAO)
│   ├── nonce: B64                  ← 논스 (PoS에서는 0)
│   ├── base_fee_per_gas: Option<u64>     ← EIP-1559 기본 수수료
│   ├── withdrawals_root: Option<B256>    ← Shanghai 출금 루트
│   ├── blob_gas_used: Option<u64>        ← EIP-4844 Blob 가스
│   ├── excess_blob_gas: Option<u64>      ← 초과 Blob 가스
│   └── parent_beacon_block_root: Option<B256> ← 비콘 체인 루트
│
└── body: BlockBody (alloy_consensus::BlockBody<TransactionSigned>)
    ├── transactions: Vec<TransactionSigned>  ← 트랜잭션 목록
    ├── ommers: Vec<Header>                   ← 옴머 블록 (보통 비어있음)
    └── withdrawals: Option<Withdrawals>      ← 출금 목록 (Shanghai 이후)
```

---

## 4. Transaction — 트랜잭션 타입

### 4.1 SignedTransaction Trait (인터페이스)

> **소스**: `crates/primitives-traits/src/transaction/signed.rs`

```rust
pub trait SignedTransaction:
    Send + Sync + Unpin + Clone + fmt::Debug + PartialEq + Eq + Hash
    + Encodable + Decodable              // RLP 인코딩/디코딩
    + Encodable2718 + Decodable2718      // EIP-2718 인코딩/디코딩
    + alloy_consensus::Transaction       // 트랜잭션 공통 인터페이스
    + MaybeSerde                         // 선택적 serde
    + InMemorySize                       // 메모리 크기 추정
    + SignerRecoverable                  // 서명자 복구
    + TxHashRef                          // 해시 참조
    + IsTyped2718                        // 타입된 트랜잭션 여부
{
    /// 시스템 트랜잭션 여부 (L2에서 사용, 이더리움 메인넷에서는 false)
    fn is_system_tx(&self) -> bool { false }
    
    /// 풀 트랜잭션으로 브로드캐스트 가능 여부
    /// (EIP-4844 blob TX는 해시로만 전파 가능)
    fn is_broadcastable_in_full(&self) -> bool {
        !self.is_eip4844()
    }
    
    /// 서명에서 보낸이 주소 복구
    fn try_recover(&self) -> Result<Address, RecoveryError>;
    
    /// 서명 검증 + Recovered 래퍼 반환
    fn try_into_recovered(self) -> Result<Recovered<Self>, Self>;
    
    /// 보낸이를 알고 있을 때 Recovered로 감싸기 (검증 skip)
    fn with_signer(self, signer: Address) -> Recovered<Self>;
}
```

### 4.2 이더리움 트랜잭션 타입 계층

```rust
// 소스: crates/ethereum/primitives/src/lib.rs

/// 서명된 트랜잭션 (이더리움) = EthereumTxEnvelope<TxEip4844>
pub type TransactionSigned = alloy_consensus::EthereumTxEnvelope<TxEip4844>;
```

**EthereumTxEnvelope**은 alloy_consensus에서 제공하는 enum으로, 모든 트랜잭션 타입을 포함합니다:

```
TransactionSigned (EthereumTxEnvelope<TxEip4844>)
├── Legacy(Signed<TxLegacy>)         ← type 0x0: 기본 트랜잭션
├── Eip2930(Signed<TxEip2930>)       ← type 0x1: AccessList 포함
├── Eip1559(Signed<TxEip1559>)       ← type 0x2: 동적 수수료 (현재 주류)
├── Eip4844(Signed<TxEip4844>)       ← type 0x3: Blob 트랜잭션 (L2 데이터)
└── Eip7702(Signed<TxEip7702>)       ← type 0x4: EOA 코드 설정
```

### 4.3 alloy_consensus::Transaction Trait (공통 트랜잭션 인터페이스)

모든 트랜잭션 타입이 구현하는 공통 인터페이스:

```rust
pub trait Transaction {
    fn chain_id(&self) -> Option<ChainId>;        // 체인 ID
    fn nonce(&self) -> u64;                       // 논스 (순서 보장)
    fn gas_limit(&self) -> u64;                   // 가스 한도
    fn gas_price(&self) -> Option<u128>;          // 가스 가격 (Legacy)
    fn max_fee_per_gas(&self) -> u128;            // 최대 가스비 (EIP-1559)
    fn max_priority_fee_per_gas(&self) -> Option<u128>; // 우선순위 수수료
    fn max_fee_per_blob_gas(&self) -> Option<u128>;     // Blob 가스비
    fn value(&self) -> U256;                      // 전송 금액
    fn input(&self) -> &Bytes;                    // 호출 데이터 (calldata)
    fn kind(&self) -> TxKind;                     // Call vs Create
    fn is_create(&self) -> bool;                  // 컨트랙트 생성 여부
    fn access_list(&self) -> Option<&AccessList>; // 접근 목록
    fn blob_versioned_hashes(&self) -> Option<&[B256]>; // Blob 해시
    fn authorization_list(&self) -> Option<&[SignedAuthorization]>; // EIP-7702
}
```

### 4.4 트랜잭션 타입별 비교표

| 필드 | Legacy (0x0) | EIP-2930 (0x1) | EIP-1559 (0x2) | EIP-4844 (0x3) | EIP-7702 (0x4) |
|------|:---:|:---:|:---:|:---:|:---:|
| `nonce` | ✅ | ✅ | ✅ | ✅ | ✅ |
| `gas_limit` | ✅ | ✅ | ✅ | ✅ | ✅ |
| `to` | ✅ | ✅ | ✅ | ✅ (필수) | ✅ |
| `value` | ✅ | ✅ | ✅ | ✅ | ✅ |
| `input` | ✅ | ✅ | ✅ | ✅ | ✅ |
| `gas_price` | ✅ | ✅ | ❌ | ❌ | ❌ |
| `max_fee_per_gas` | ❌ | ❌ | ✅ | ✅ | ✅ |
| `max_priority_fee_per_gas` | ❌ | ❌ | ✅ | ✅ | ✅ |
| `access_list` | ❌ | ✅ | ✅ | ✅ | ✅ |
| `max_fee_per_blob_gas` | ❌ | ❌ | ❌ | ✅ | ❌ |
| `blob_versioned_hashes` | ❌ | ❌ | ❌ | ✅ | ❌ |
| `authorization_list` | ❌ | ❌ | ❌ | ❌ | ✅ |

### 4.5 Java 비교

```java
// Java로 표현하면:
public sealed interface Transaction 
    permits TxLegacy, TxEip2930, TxEip1559, TxEip4844, TxEip7702 {
    
    Optional<Long> chainId();
    long nonce();
    long gasLimit();
    BigInteger value();
    byte[] input();
    // ...
}

// Signed 버전은 서명 정보를 포함하는 래퍼
public record SignedTransaction(Transaction tx, Signature sig, Hash txHash) {
    public Address recoverSender() { ... }
}
```

---

## 5. Account — 계정 타입

> **소스**: `crates/primitives-traits/src/account.rs`

### 5.1 Account Struct

```rust
pub struct Account {
    /// 계정 논스 (트랜잭션 순번, 컨트랙트 생성 횟수)
    pub nonce: u64,
    /// 계정 잔액 (Wei 단위, 1 ETH = 10^18 Wei)
    pub balance: U256,
    /// 바이트코드 해시 (스마트 컨트랙트인 경우)
    /// None이면 EOA (외부 소유 계정)
    pub bytecode_hash: Option<B256>,
}
```

### 5.2 핵심 메서드

```rust
impl Account {
    /// 바이트코드 보유 여부 (= 스마트 컨트랙트 여부)
    pub const fn has_bytecode(&self) -> bool {
        self.bytecode_hash.is_some()
    }

    /// 빈 계정 판별 (SpuriousDragon 하드포크 이후)
    /// nonce == 0 && balance == 0 && bytecode == None
    pub fn is_empty(&self) -> bool {
        self.nonce == 0 &&
            self.balance.is_zero() &&
            self.bytecode_hash.is_none_or(|hash| hash == KECCAK_EMPTY)
    }

    /// revm Account에서 reth Account로 변환
    pub fn from_revm_account(revm_account: &revm_state::Account) -> Self { ... }

    /// 트라이 계정으로 변환 (스토리지 루트 포함)
    pub fn into_trie_account(self, storage_root: B256) -> TrieAccount { ... }
}
```

### 5.3 Java 비교

```java
// Java로 표현하면:
public class Account {
    private long nonce;             // 트랜잭션 순번
    private BigInteger balance;     // 잔액 (Wei)
    private byte[] bytecodeHash;    // 컨트랙트 코드 해시 (nullable)
    
    public boolean isContract() { return bytecodeHash != null; }
    public boolean isEmpty() { 
        return nonce == 0 && balance.equals(BigInteger.ZERO) && bytecodeHash == null; 
    }
}
```

### 5.4 계정 타입 구분

```
이더리움 계정
├── EOA (Externally Owned Account) — 외부 소유 계정
│   ├── bytecode_hash = None
│   ├── 개인 키로 트랜잭션 서명 가능
│   └── 예: 일반 사용자 지갑
│
└── Contract Account — 컨트랙트 계정
    ├── bytecode_hash = Some(keccak256(bytecode))
    ├── 자체적으로 트랜잭션 실행 불가 (호출만 받음)
    └── 예: ERC-20 토큰 컨트랙트, DEX 컨트랙트
```

### 5.5 Bytecode 타입

```rust
// 소스: crates/primitives-traits/src/account.rs
pub struct Bytecode(pub RevmBytecode);

// RevmBytecode는 revm의 바이트코드 타입:
// - LegacyAnalyzed: 일반 바이트코드 (점프 테이블 포함)
// - Eip7702: EIP-7702 위임 바이트코드
```

---

## 6. Receipt — 트랜잭션 영수증

> **소스**: `crates/ethereum/primitives/src/receipt.rs`

### 6.1 Receipt Struct

```rust
pub struct EthereumReceipt<T = TxType, L = Log> {
    /// 트랜잭션 타입 (Legacy, EIP-2930, EIP-1559, EIP-4844, EIP-7702)
    pub tx_type: T,
    /// 트랜잭션 실행 성공 여부 (statusCode)
    pub success: bool,
    /// 누적 가스 사용량 (블록 내 이 TX까지의 총 가스)
    pub cumulative_gas_used: u64,
    /// 이벤트 로그 (컨트랙트에서 발생한 이벤트들)
    pub logs: Vec<L>,
}
```

### 6.2 Receipt Trait (인터페이스)

```rust
// 소스: crates/primitives-traits/src/receipt.rs
pub trait Receipt:
    Send + Sync + Unpin + Clone + fmt::Debug
    + TxReceipt<Log = alloy_primitives::Log>   // 영수증 공통 인터페이스
    + RlpEncodableReceipt                       // RLP 인코딩
    + RlpDecodableReceipt                       // RLP 디코딩
    + Encodable + Decodable                     // 직렬화
    + Eip2718EncodableReceipt                   // EIP-2718 인코딩
    + Typed2718                                 // 타입 정보
    + MaybeSerde                                // 선택적 serde
    + InMemorySize                              // 메모리 크기
    + MaybeSerdeBincodeCompat                   // bincode 호환
{ }
```

### 6.3 TxReceipt 인터페이스 (alloy 제공)

```rust
// 모든 Receipt이 구현해야 하는 핵심 메서드
impl TxReceipt for EthereumReceipt {
    fn status(&self) -> bool { self.success }
    fn cumulative_gas_used(&self) -> u64 { self.cumulative_gas_used }
    fn logs(&self) -> &[Log] { &self.logs }
    fn bloom(&self) -> Bloom { logs_bloom(self.logs.iter()) }
}
```

### 6.4 Java 비교

```java
// Java로 표현하면:
public class Receipt {
    private TxType txType;           // 트랜잭션 타입
    private boolean success;         // 실행 성공 여부
    private long cumulativeGasUsed;  // 누적 가스 사용량
    private List<Log> logs;          // 이벤트 로그
    
    // 게산 필드 (저장하지 않고 필요할 때 계산)
    public Bloom getLogsBloom() { return calculateBloom(logs); }
}
```

### 6.5 Receipt의 저장 최적화

```rust
// Compact 구현에서 zstd 압축을 사용
// 7바이트 이상일 때 자동으로 zstd 압축 적용
impl Compact for Receipt {
    fn to_compact(&self, buf: &mut B) -> usize {
        // ... flags(1바이트) + 데이터
        let zstd = buffer.len() > 7;
        if zstd {
            // zstd 압축 적용 → 디스크 공간 절약
            reth_zstd_compressors::with_receipt_compressor(|compressor| {
                buf.put(compressor.compress(&buffer));
            });
        }
        // ...
    }
}
```

---

## 7. StorageEntry — 스토리지 엔트리

> **소스**: `crates/primitives-traits/src/storage.rs`

```rust
pub struct StorageEntry {
    /// 스토리지 키 (32바이트 슬롯 번호)
    pub key: B256,
    /// 스토리지 값 (256비트 정수)
    pub value: U256,
}
```

**Java 비유**: 스마트 컨트랙트의 상태 변수를 저장하는 키-값 쌍입니다.
```java
// Solidity: mapping(uint256 => uint256) storage;
// Java: Map<byte[32], BigInteger> contractStorage;
```

---

## 8. Log — 이벤트 로그

```rust
// alloy_primitives에서 제공
pub struct Log<T = LogData> {
    /// 이벤트를 발생시킨 컨트랙트 주소
    pub address: Address,
    /// 로그 데이터
    pub data: T,
}

pub struct LogData {
    /// topics[0] = 이벤트 시그니처 해시 (Transfer, Approval 등)
    /// topics[1..] = indexed 파라미터
    topics: Vec<B256>,
    /// 비-indexed 파라미터의 ABI 인코딩 데이터
    data: Bytes,
}
```

**Java 비유**: 
```java
// Solidity event Transfer(address indexed from, address indexed to, uint256 value);
// Java에서는:
public class EventLog {
    Address contractAddress;        // 발생 컨트랙트
    byte[] eventSignatureHash;      // keccak256("Transfer(address,address,uint256)")
    List<byte[]> indexedParams;     // [from, to]
    byte[] nonIndexedData;          // abi.encode(value)
}
```

---

## 9. 핵심 설계 패턴 정리

### 9.1 Trait 계층 패턴 (Full* trait)

reth는 `Full*` 접두사 trait 패턴을 일관되게 사용합니다:

```
SignedTransaction (기본 요구사항)
  + MaybeCompact (DB 저장 능력)
  + MaybeSerdeBincodeCompat (바이너리 직렬화)
  = FullSignedTx (풀 노드 운영에 필요한 모든 기능)
```

```rust
// 패턴: 기본 trait + DB 저장 능력 = Full trait
pub trait FullSignedTx: SignedTransaction + MaybeCompact + MaybeSerdeBincodeCompat {}
pub trait FullReceipt: Receipt + MaybeCompact {}
pub trait FullBlock: Block<Header: FullBlockHeader, Body: FullBlockBody> {}
pub trait FullBlockBody: BlockBody<Transaction: FullSignedTx> + MaybeSerdeBincodeCompat {}
```

**Java 비유**:
```java
// 기본 인터페이스
interface SignedTransaction { ... }

// 풀 노드용 확장 인터페이스 (DB 저장, 직렬화 능력 추가)
interface FullSignedTx extends SignedTransaction, Serializable, CompactStorable { }
```

### 9.2 Blanket Implementation 패턴

```rust
// 조건을 만족하면 자동으로 FullSignedTx가 됨 (별도 구현 불필요)
impl<T> FullSignedTx for T where T: SignedTransaction + MaybeCompact + MaybeSerdeBincodeCompat {}
```

이것은 Java에서 불가능한 패턴입니다. Rust에서는 "특정 조건의 trait을 모두 구현하면 자동으로 다른 trait도 구현된다"는 규칙을 정의할 수 있습니다.

### 9.3 alloy 의존성 패턴

```
reth (노드 구현)
  │
  ├── 자체 정의: trait (인터페이스)
  │   - Block, BlockBody, SignedTransaction, Receipt
  │
  └── alloy에 위임: struct (데이터 구조)
      - alloy_consensus::Block, Header, TxLegacy, TxEip1559...
      - alloy_primitives::Address, B256, U256...
```

> **핵심**: reth는 "행동(trait)"을 정의하고, 데이터 구조는 alloy 생태계가 제공하는 것을 그대로 사용합니다. 이는 **관심사 분리**의 좋은 예시입니다.

---

## 10. 커스터마이징 시 주요 고려사항

### 10.1 새로운 트랜잭션 타입 추가

새로운 트랜잭션 타입(예: 금융 전용 메타 트랜잭션)을 추가하려면:

1. `SignedTransaction` trait을 구현하는 새 enum variant 추가
2. `NodePrimitives`에서 새 `SignedTx` 타입 지정
3. EVM 실행기에서 새 타입 처리 로직 추가
4. RLP/EIP-2718 인코딩/디코딩 구현

### 10.2 Receipt에 추가 정보 포함

금융 프로젝트에서 영수증에 추가 필드(예: 수수료 분배 정보)가 필요하면:

1. `Receipt` trait을 구현하는 커스텀 struct 정의
2. `NodePrimitives::Receipt` 타입으로 지정
3. 스토리지 레이어에서 Compact 인코딩 구현

### 10.3 주의점

- **alloy 타입 직접 수정 불가**: alloy는 외부 의존성이므로 Fork하지 않는 한 수정 불가
- **trait은 확장 가능**: reth의 trait 시스템을 활용하면 기존 코드 변경 없이 새 타입 추가 가능
- **Compact 인코딩**: DB 저장을 위해 반드시 `Compact` trait 구현 필요 (zstd 압축 포함)

---

## 11. 다음 분석 단계

이 Primitives 분석을 기반으로, 다음 단계에서는:

1. **스토리지 레이어 분석** (`crates/storage/*`) — 이 타입들이 어떻게 MDBX에 저장되는지
2. **EVM 실행 흐름 분석** (`crates/evm/*` + `crates/revm/`) — 트랜잭션이 어떻게 실행되고 Receipt가 생성되는지

순서로 분석할 예정입니다.
