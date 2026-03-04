# 03. 스토리지 레이어 분석

> **최종 수정일**: 2026-02-18
> **분석 대상**: `crates/storage/` 하위 전체 크레이트

---

## 개요

Reth의 스토리지 레이어는 이더리움 블록체인 데이터를 효율적으로 저장·조회·관리하기 위한 계층입니다. Java로 비유하면, JPA/Hibernate가 DB를 추상화하듯이 Reth도 **trait 기반 추상화 → MDBX 구현** 패턴으로 스토리지를 관리합니다.

### 스토리지 서브크레이트 구조

```
crates/storage/
├── db-api/          # DB 추상화 trait (= JPA 인터페이스)
├── db/              # MDBX 구현체 (= Hibernate/JPA 구현)
├── db-common/       # DB 공통 유틸리티
├── db-models/       # DB 테이블의 Value 타입 (= JPA Entity)
├── codecs/          # Compact 직렬화 코덱 (= 커스텀 Serializer)
├── libmdbx-rs/      # MDBX C 라이브러리 바인딩 (= JDBC 드라이버)
├── nippy-jar/       # 불변 데이터 저장 형식 (= 정적 파일 스토리지)
├── storage-api/     # 고수준 스토리지 trait (= Service 인터페이스)
├── provider/        # storage-api 구현체 (= Service 구현)
├── rpc-provider/    # RPC용 Provider
├── errors/          # 스토리지 에러 타입
└── zstd-compressors/# Zstd 압축 사전
```

---

## 1. 데이터베이스 추상화 (db-api)

> **소스**: `crates/storage/db-api/src/`
> **Java 비유**: JPA의 `EntityManager`, `Repository` 인터페이스

### 1.1 핵심 trait 계층

```
Database            ← 데이터베이스 자체 (EntityManagerFactory)
├── tx() → DbTx    ← 읽기 전용 트랜잭션 (ReadOnly EntityManager)
└── tx_mut() → DbTxMut  ← 읽기/쓰기 트랜잭션 (Read-Write EntityManager)
     ├── put()          ← INSERT/UPDATE
     ├── delete()       ← DELETE
     └── clear()        ← TRUNCATE

DbTx (읽기)
├── get()              ← SELECT by PK
├── commit()           ← COMMIT
├── cursor_read()      ← 순회용 Cursor (ResultSet)
└── cursor_dup_read()  ← DUPSORT 테이블용 Cursor

DbTxMut (쓰기, DbTx 상속)
├── put()              ← INSERT/UPDATE
├── delete()           ← DELETE
├── cursor_write()     ← 쓰기 Cursor
└── cursor_dup_write() ← DUPSORT 쓰기 Cursor
```

### 1.2 Database trait

```rust
// 소스: crates/storage/db-api/src/database.rs
pub trait Database: Send + Sync + Debug {
    type TX: DbTx + Send + Sync + Debug + 'static;
    type TXMut: DbTxMut + DbTx + TableImporter + Send + Sync + Debug + 'static;

    fn tx(&self) -> Result<Self::TX, DatabaseError>;
    fn tx_mut(&self) -> Result<Self::TXMut, DatabaseError>;

    // 편의 메서드: 트랜잭션 생명주기 자동 관리
    fn view<T, F>(&self, f: F) -> Result<T, DatabaseError>
    where F: FnOnce(&Self::TX) -> T { ... }

    fn update<T, F>(&self, f: F) -> Result<T, DatabaseError>
    where F: FnOnce(&Self::TXMut) -> T { ... }
}
```

**Java 비유**:
```java
// Java로 대응하면 이런 느낌
interface Database {
    ReadOnlyTransaction beginReadTransaction();
    ReadWriteTransaction beginReadWriteTransaction();

    default <T> T view(Function<ReadOnlyTransaction, T> action) {
        var tx = beginReadTransaction();
        try { return action.apply(tx); }
        finally { tx.commit(); }
    }
}
```

### 1.3 테이블 추상화 (Table trait)

```rust
// 소스: crates/storage/db-api/src/table.rs
pub trait Table: Send + Sync + Debug + 'static {
    const NAME: &'static str;        // 테이블 이름
    const DUPSORT: bool;             // 하나의 Key에 여러 Value 허용 여부
    type Key: Key;                   // PK 타입 (Encode + Decode + Ord)
    type Value: Value;               // Value 타입 (Compress + Decompress)
}
```

**핵심 개념**: MDBX의 DUPSORT는 RDBMS의 복합키와 유사합니다. 하나의 `Key`에 여러 `Value`가 붙을 수 있으며, 이 경우 Value 타입을 `DupSort` trait으로 구분합니다.

### 1.4 데이터 직렬화 파이프라인

데이터가 DB에 저장되기까지의 변환 과정:

```
[Rust 구조체]
    │ Compress trait: 구조체 → 압축 바이트    (Value 저장 시)
    │ Encode trait: 구조체 → 바이트 키         (Key 저장 시)
    ▼
[Raw Bytes] → MDBX에 저장

[MDBX에서 읽기]
    │ Decompress trait: 압축 바이트 → 구조체   (Value 읽기 시)
    │ Decode trait: 바이트 키 → 구조체          (Key 읽기 시)
    ▼
[Rust 구조체]
```

**Java 비유**: JPA의 `AttributeConverter`나 Hibernate의 `UserType`과 유사합니다. 다만 Reth는 JPA 어노테이션 대신 trait 구현으로 직렬화를 정의합니다.

### 1.5 커서 (Cursor) — 데이터 순회

```rust
// 소스: crates/storage/db-api/src/cursor.rs
pub trait DbCursorRO<T: Table> {
    fn first(&mut self) -> PairResult<T>;         // 첫 번째 레코드
    fn seek_exact(&mut self, key: T::Key) -> PairResult<T>;  // 정확한 키 검색
    fn seek(&mut self, key: T::Key) -> PairResult<T>;        // 이상(>=) 검색
    fn next(&mut self) -> PairResult<T>;          // 다음 레코드
    fn prev(&mut self) -> PairResult<T>;          // 이전 레코드
    fn last(&mut self) -> PairResult<T>;          // 마지막 레코드
    fn current(&mut self) -> PairResult<T>;       // 현재 레코드

    // 이터레이터 생성
    fn walk(&mut self, start: Option<T::Key>)
        -> Result<Walker<'_, T, Self>, DatabaseError>;
}
```

**Java 비유**: JDBC의 `ResultSet`과 유사하지만, 양방향 탐색과 범위 검색이 기본입니다.

---

## 2. 테이블 스키마 (tables)

> **소스**: `crates/storage/db-api/src/tables/mod.rs`

Reth는 `tables!` 매크로를 사용하여 모든 테이블을 선언합니다. 모든 테이블은 `Tables` enum으로 열거됩니다.

### 2.1 전체 테이블 목록

| 테이블명 | Key 타입 | Value 타입 | 설명 |
|----------|----------|------------|------|
| **CanonicalHeaders** | `BlockNumber` (u64) | `B256` (블록 해시) | 정규 체인의 블록번호→해시 매핑 |
| **HeaderTerminalDifficulties** | `BlockNumber` | `CompactU256` | 각 블록까지의 누적 난이도 (PoW) |
| **HeaderNumbers** | `B256` (블록 해시) | `BlockNumber` | 해시→블록번호 역매핑 |
| **Headers** | `BlockNumber` | `Header` | 블록 헤더 |
| **BlockBodyIndices** | `BlockNumber` | `StoredBlockBodyIndices` | 블록 바디의 트랜잭션 인덱스 |
| **BlockWithdrawals** | `BlockNumber` | `StoredBlockWithdrawals` | 출금 정보 (post-Shanghai) |
| **Transactions** | `TxNumber` (u64) | 서명된 트랜잭션 | 트랜잭션 (글로벌 순번) |
| **TransactionHashNumbers** | `TxHash` (B256) | `TxNumber` | 트랜잭션 해시→순번 매핑 |
| **TransactionSenders** | `TxNumber` | `Address` | 트랜잭션 발신자 주소 |
| **Receipts** | `TxNumber` | 트랜잭션 영수증 | 실행 결과 (가스 사용량, 로그 등) |
| **PlainAccountState** | `Address` | `Account` | 계정의 최신 상태 (잔액, nonce) |
| **PlainStorageState** | `Address` | `StorageEntry` | 스마트 컨트랙트 스토리지 (DUPSORT) |
| **AccountChangeSets** | `BlockNumber` | `AccountBeforeTx` | 계정 변경 이력 (DUPSORT) |
| **StorageChangeSets** | `BlockNumberAddress` | `StorageEntry` | 스토리지 변경 이력 (DUPSORT) |
| **AccountsHistory** | `ShardedKey<Address>` | `IntegerList` | 계정 변경이 발생한 블록 목록 |
| **StoragesHistory** | `StorageShardedKey` | `IntegerList` | 스토리지 슬롯 변경 블록 목록 |
| **HashedAccounts** | `B256` | `Account` | Keccak(주소)→계정 (트라이용) |
| **HashedStorages** | `B256` | `StorageEntry` | Keccak(주소)→스토리지 (트라이용, DUPSORT) |
| **AccountsTrie** | `StoredNibbles` | `BranchNodeCompact` | 계정 머클 트라이 노드 |
| **StoragesTrie** | `B256` | `StorageTrieEntry` | 스토리지 머클 트라이 노드 (DUPSORT) |
| **Bytecodes** | `B256` (코드 해시) | `Bytecode` | 스마트 컨트랙트 바이트코드 |
| **StageCheckpoints** | `StageId` (문자열) | `StageCheckpoint` | 동기화 스테이지 진행 상태 |
| **PruneCheckpoints** | `PruneSegment` | `PruneCheckpoint` | 데이터 정리(pruning) 진행 상태 |
| **VersionHistory** | `u64` (타임스탬프) | `ClientVersion` | 클라이언트 버전 기록 |

### 2.2 핵심 데이터 모델

#### StoredBlockBodyIndices — 블록 바디 인덱스

```rust
// 소스: crates/storage/db-models/src/blocks.rs
pub struct StoredBlockBodyIndices {
    pub first_tx_num: TxNumber,    // 블록의 첫 트랜잭션 글로벌 순번
    pub tx_count: u64,             // 블록 내 트랜잭션 수
}
// tx_num_range() → first_tx_num..first_tx_num+tx_count
```

**핵심 설계**: 트랜잭션은 글로벌 순번(`TxNumber`)으로 관리됩니다. 블록은 `first_tx_num`과 `tx_count`를 통해 해당 블록의 트랜잭션 범위를 알 수 있습니다.

```
Block 0: first_tx_num=0,  tx_count=3  → tx 0, 1, 2
Block 1: first_tx_num=3,  tx_count=5  → tx 3, 4, 5, 6, 7
Block 2: first_tx_num=8,  tx_count=2  → tx 8, 9
```

#### ShardedKey — 히스토리 샤딩

```rust
// 소스: crates/storage/db-api/src/models/sharded_key.rs
pub struct ShardedKey<T> {
    pub key: T,                        // 원본 키 (예: Address)
    pub highest_block_number: BlockNumber,  // 이 샤드의 최고 블록 번호
}
```

히스토리 데이터가 너무 커질 수 있으므로, 블록 번호 기준으로 샤드(분할)합니다:
```
(Address(0xAA) | 200)  → 블록 0~200 사이의 변경 블록 목록
(Address(0xAA) | 500)  → 블록 201~500 사이의 변경 블록 목록
(Address(0xAA) | MAX)  → 블록 501~ 현재까지의 변경 블록 목록
```

**Java 비유**: 히스토리 테이블을 블록 단위로 파티셔닝하는 것과 유사합니다.

#### AccountBeforeTx — 변경 전 상태

```rust
// 소스: crates/storage/db-models/src/accounts.rs
pub struct AccountBeforeTx {
    pub address: Address,
    pub info: Option<Account>,  // None이면 생성 전(존재하지 않던 계정)
}
```

`AccountChangeSets` 테이블에 저장되며, 블록 되돌리기(revert)를 위해 **변경 전** 상태를 기록합니다.

### 2.3 데이터 접근 패턴

```
[블록 해시로 헤더 조회]
HeaderNumbers(hash) → block_number
Headers(block_number) → Header

[블록의 트랜잭션 조회]
BlockBodyIndices(block_number) → {first_tx_num, tx_count}
Transactions(first_tx_num..first_tx_num+tx_count) → Vec<Tx>

[과거 계정 상태 조회 (블록 N 시점)]
AccountsHistory(ShardedKey(addr, N 이상의 샤드)) → IntegerList(변경 블록 목록)
→ N 직전의 변경 블록 찾기
AccountChangeSets(해당 블록) → AccountBeforeTx → 그 시점의 Account

[현재 계정 상태 조회]
PlainAccountState(address) → Account {nonce, balance, bytecode_hash}
```

---

## 3. Compact 코덱 (codecs)

> **소스**: `crates/storage/codecs/src/lib.rs`
> **Java 비유**: 커스텀 `Serializer` + `Deserializer`. Protobuf/Avro의 역할을 합니다.

### 3.1 Compact trait

```rust
pub trait Compact: Sized {
    fn to_compact<B>(&self, buf: &mut B) -> usize
    where B: BufMut + AsMut<[u8]>;

    fn from_compact(buf: &[u8], len: usize) -> (Self, &[u8]);
}
```

`Compact`는 reth의 **공간 최적화 직렬화** 형식입니다. 주요 특징:

- **가변 길이 정수**: 선행 0바이트를 제거합니다. `u64(2)` → 1바이트 (0xFF… → 8바이트)
- **StructFlags 비트필드**: `bool`, `Option` 등의 메타 정보를 비트 단위로 구조체 앞에 배치
- **고정 크기 타입은 그대로**: `Address`(20바이트), `B256`(32바이트)는 압축하지 않음
- **varuint 인코딩**: 가변 길이 데이터(Vec, Option의 내부 값)의 길이를 varuint으로 저장

### 3.2 Compress/Decompress와 Compact의 관계

```
┌──────────────────────────────────────────────┐
│ Table Value 저장 파이프라인                    │
│                                              │
│ Rust Struct                                  │
│   │ Compact::to_compact() → 압축된 바이트     │
│   │   (Compress trait이 내부적으로 호출)       │
│   ▼                                          │
│ [Compact Bytes] → MDBX에 저장                │
│                                              │
│ MDBX에서 로드                                │
│   │ Decompress::decompress() → 구조체         │
│   │   (Compact::from_compact() 내부 호출)     │
│   ▼                                          │
│ Rust Struct                                  │
└──────────────────────────────────────────────┘
```

### 3.3 Zstd 압축 (선택적)

일부 타입(예: `EthereumReceipt`, `TransactionSigned`)은 `Compact` 위에 Zstd 사전 압축을 추가로 적용합니다:

```
// crates/storage/zstd-compressors/ — 미리 학습된 Zstd 사전 파일
Transaction (Zstd) → 약 30~50% 추가 압축
Receipt (Zstd)     → 약 40~60% 추가 압축
```

---

## 4. MDBX 구현 (db)

> **소스**: `crates/storage/db/src/implementation/mdbx/mod.rs`
> **Java 비유**: Hibernate의 `SessionFactory` 구현

### 4.1 DatabaseEnv — MDBX 환경

```rust
// 소스: crates/storage/db/src/implementation/mdbx/mod.rs
pub struct DatabaseEnv {
    inner: Environment,                              // MDBX 환경
    dbis: Arc<HashMap<&'static str, ffi::MDBX_dbi>>, // 미리 열어둔 테이블 핸들
    metrics: Option<Arc<DatabaseEnvMetrics>>,         // 성능 메트릭
    _lock_file: Option<StorageLock>,                 // 파일 잠금 (RW시)
}

impl Database for DatabaseEnv {
    type TX = tx::Tx<RO>;         // 읽기 전용 트랜잭션
    type TXMut = tx::Tx<RW>;     // 읽기/쓰기 트랜잭션

    fn tx(&self) -> Result<Self::TX, DatabaseError> {
        Tx::new(self.inner.begin_ro_txn()?, self.dbis.clone(), self.metrics.clone())
    }
    fn tx_mut(&self) -> Result<Self::TXMut, DatabaseError> {
        Tx::new(self.inner.begin_rw_txn()?, self.dbis.clone(), self.metrics.clone())
    }
}
```

### 4.2 MDBX 설정 (DatabaseArguments)

```rust
pub struct DatabaseArguments {
    client_version: ClientVersion,
    geometry: Geometry<Range<usize>>,          // DB 크기 설정
    log_level: Option<LogLevel>,
    max_read_transaction_duration: Option<MaxReadTransactionDuration>,
    exclusive: Option<bool>,                   // 배타적 모드
    max_readers: Option<u64>,                  // 최대 리더 수
    sync_mode: SyncMode,                       // 동기화 모드
}
```

**주요 기본값**:

| 설정 | 기본값 | 설명 |
|------|--------|------|
| 최대 DB 크기 | 8 TB | `geometry.size = 0..(8 * TERABYTE)` |
| 성장 단위 | 4 GB | `growth_step = 4 * GIGABYTE` |
| 최대 리더 수 | 32,000 | MDBX 한계 32,767 중 |
| 동기화 모드 | Durable | 모든 트랜잭션 디스크 플러시 |
| Readahead | 비활성 | 랜덤 접근 최적화 |
| 최대 테이블 수 | 256 | 커스텀 테이블 여유분 포함 |
| Coalesce | 활성 | 인접 페이지 병합 |

### 4.3 테이블 생성 및 관리

```rust
// 테이블 생성 (시작 시 1회)
pub fn create_tables(&mut self) -> Result<(), DatabaseError> {
    self.create_and_track_tables_for::<Tables>()
}

// 더 이상 사용하지 않는 테이블 삭제
pub fn drop_orphan_table(&self, name: &str) -> Result<bool, DatabaseError> { ... }

// 클라이언트 버전 기록
pub fn record_client_version(&self, version: ClientVersion) -> Result<(), DatabaseError> { ... }
```

### 4.4 읽기/쓰기 패턴 예시

```rust
// 데이터 쓰기
let tx = env.tx_mut()?;
tx.put::<Headers>(block_number, header)?;
tx.commit()?;

// 데이터 읽기
let tx = env.tx()?;
let header = tx.get::<Headers>(block_number)?;
tx.commit()?;

// 커서를 이용한 순회
let tx = env.tx()?;
let mut cursor = tx.cursor_read::<PlainAccountState>()?;
let mut walker = cursor.walk(None)?;
while let Some((address, account)) = walker.next().transpose()? {
    // 모든 계정 순회
}
```

---

## 5. 정적 파일 시스템 (NippyJar + Static Files)

> **소스**: `crates/storage/nippy-jar/src/lib.rs`, `crates/static-file/types/src/segment.rs`
> **Java 비유**: 과거 데이터를 읽기 전용으로 압축하여 저장하는 **아카이브 시스템**. Java의 Parquet/ORC 같은 컬럼형 저장 형식.

### 5.1 NippyJar 개요

`NippyJar`는 **불변 데이터**를 위한 특화된 저장 형식입니다:

```rust
pub struct NippyJar<H = ()> {
    version: usize,              // 형식 버전
    user_header: H,              // 사용자 정의 헤더 (SegmentHeader)
    columns: usize,              // 컬럼 수
    path: PathBuf,               // 데이터 파일 경로
    compressor: Option<Compressors>,  // 압축기 (Zstd, Lz4)
    rows: usize,                 // 행 수
    max_row_size: usize,         // 최대 행 크기
}
```

**핵심 특성**:
- **컬럼 기반 저장**: 데이터를 컬럼별로 분리하여 저장 → 같은 타입의 데이터가 연속 → 높은 압축률
- **오프셋 기반 접근**: `offsets` 파일을 참조하여 O(1)로 특정 행에 접근
- **mmap 기반 읽기**: 메모리 매핑을 통한 빠른 읽기
- **불변성**: 한번 쓰면 변경 불가 (append만 가능)

**파일 구조**:
```
static_file_headers_0_499999             # 데이터 파일
static_file_headers_0_499999.idx         # 인덱스 파일
static_file_headers_0_499999.off         # 오프셋 파일
static_file_headers_0_499999.conf        # 설정 파일 (NippyJar 메타데이터)
static_file_headers_0_499999.csoff       # 체인지셋 오프셋 (체인지셋 세그먼트만)
```

### 5.2 Static File 세그먼트

```rust
// 소스: crates/static-file/types/src/segment.rs
pub enum StaticFileSegment {
    Headers,              // 3 columns: CanonicalHeaders, Headers, HeaderTerminalDifficulties
    Transactions,         // 1 column: 트랜잭션 데이터
    Receipts,             // 1 column: 영수증 데이터
    TransactionSenders,   // 1 column: 트랜잭션 발신자 주소
    AccountChangeSets,    // 1 column: 계정 변경 이력
    StorageChangeSets,    // 1 column: 스토리지 변경 이력
}
```

### 5.3 SegmentHeader

```rust
pub struct SegmentHeader {
    expected_block_range: SegmentRangeInclusive,  // 파일이 커버하는 블록 범위
    block_range: Option<SegmentRangeInclusive>,   // 실제 데이터가 있는 블록 범위
    tx_range: Option<SegmentRangeInclusive>,      // 트랜잭션 범위
    segment: StaticFileSegment,                   // 세그먼트 종류
    changeset_offsets_len: u64,                   // 체인지셋 오프셋 수
}
```

### 5.4 데이터 흐름: MDBX → Static File

```
[새 블록 도착]
  │ 실행 후 MDBX에 저장 (hot storage)
  ▼
[충분한 블록 축적]  (예: 500,000 블록 단위)
  │ Static File Producer가 MDBX에서 데이터를 읽어
  │ NippyJar 파일로 기록 (Lz4 압축)
  ▼
[Static File 생성 완료]
  │ MDBX에서 이동된 데이터 제거 가능 (pruning)
  ▼
[조회 시] StaticFileProvider가 세그먼트 범위를 확인하여
  ├── 범위 내: NippyJar에서 직접 읽기 (mmap, 빠름)
  └── 범위 외: MDBX에서 읽기

파일명 예시:
  static_file_headers_0_499999           → 블록 0~499,999 헤더
  static_file_transactions_500000_999999 → 블록 500,000~999,999 트랜잭션
```

### 5.5 압축 전략

| 세그먼트 | 기본 압축 | 컬럼 수 | 비고 |
|----------|----------|---------|------|
| Headers | Lz4 | 3 | 해시, 헤더, 누적난이도 |
| Transactions | Lz4 | 1 | |
| Receipts | Lz4 | 1 | |
| TransactionSenders | Lz4 | 1 | |
| AccountChangeSets | Lz4 | 1 | 블록별 offset 사이드카 파일 포함 |
| StorageChangeSets | Lz4 | 1 | 블록별 offset 사이드카 파일 포함 |

---

## 6. Provider 아키텍처

> **소스**: `crates/storage/storage-api/src/`, `crates/storage/provider/src/`
> **Java 비유**: Spring Service 계층. Repository에서 데이터를 가져와 비즈니스 로직에 전달.

### 6.1 고수준 스토리지 trait (storage-api)

```
HeaderProvider          ← 블록 헤더 조회
BlockReader             ← 전체 블록 조회 (헤더 + 바디 + 트랜잭션)
TransactionsProvider    ← 트랜잭션 조회
ReceiptProvider         ← 영수증 조회
StateProvider           ← 상태(계정, 스토리지, 바이트코드) 조회
AccountReader           ← 계정 정보 조회
BlockBodyIndicesProvider ← 블록 바디 인덱스 조회
StateProviderFactory    ← BlockId로 StateProvider 생성
```

#### StateProvider — 상태 조회의 핵심

```rust
// 소스: crates/storage/storage-api/src/state.rs
pub trait StateProvider: BlockHashReader + AccountReader + StorageRootProvider + ... {
    fn account_code(&self, addr: &Address) -> ProviderResult<Option<Bytecode>>;
    fn account_balance(&self, addr: &Address) -> ProviderResult<Option<U256>>;
    fn account_nonce(&self, addr: &Address) -> ProviderResult<Option<u64>>;
}
```

#### StateProviderFactory — 시점별 StateProvider 생성

```rust
pub trait StateProviderFactory {
    fn latest(&self) -> ProviderResult<StateProviderBox>;
    fn state_by_block_number_or_tag(&self, ...) -> ProviderResult<StateProviderBox>;
    fn history_by_block_number(&self, block: BlockNumber) -> ProviderResult<StateProviderBox>;
    fn history_by_block_hash(&self, hash: B256) -> ProviderResult<StateProviderBox>;
    fn state_by_block_id(&self, block_id: BlockId) -> ProviderResult<StateProviderBox>;
}
```

### 6.2 ProviderFactory — 최상위 팩토리

```rust
// 소스: crates/storage/provider/src/providers/database/mod.rs
pub struct ProviderFactory<N: NodeTypesWithDB> {
    db: N::DB,                                    // MDBX 데이터베이스
    chain_spec: Arc<N::ChainSpec>,               // 체인 스펙 (메인넷, 테스트넷 등)
    static_file_provider: StaticFileProvider,     // 정적 파일 제공자
    prune_modes: PruneModes,                     // 프루닝 설정
    storage: Arc<N::Storage>,                    // 노드 스토리지 핸들러
    storage_settings: Arc<RwLock<StorageSettings>>, // 스토리지 설정 캐시
    rocksdb_provider: RocksDBProvider,           // RocksDB 제공자
    changeset_cache: ChangesetCache,             // 체인지셋 캐시
    runtime: reth_tasks::Runtime,                // 비동기 런타임
}
```

**핵심 메서드**:
```rust
impl ProviderFactory {
    // 읽기 전용 Provider 생성
    fn provider(&self) -> ProviderResult<DatabaseProviderRO> { ... }

    // 읽기/쓰기 Provider 생성
    fn provider_rw(&self) -> ProviderResult<DatabaseProviderRW> { ... }

    // 최신 상태 조회
    fn latest(&self) -> ProviderResult<StateProviderBox> { ... }

    // 과거 상태 조회
    fn history_by_block_number(&self, block: u64) -> ProviderResult<StateProviderBox> { ... }
}
```

### 6.3 데이터 소스 라우팅

ProviderFactory가 데이터 요청을 적절한 소스로 라우팅하는 패턴:

```rust
// 헤더: 항상 Static File에서 조회
fn header_by_number(&self, num: BlockNumber) -> ProviderResult<Option<Self::Header>> {
    self.static_file_provider.header_by_number(num)
}

// 영수증: Static File 시도 → 실패 시 DB
fn receipt(&self, id: TxNumber) -> ProviderResult<Option<Self::Receipt>> {
    self.static_file_provider.get_with_static_file_or_database(
        StaticFileSegment::Receipts, id,
        |static_file| static_file.receipt(id),         // Static File 먼저
        || self.provider()?.receipt(id),                // 없으면 DB
    )
}

// 트랜잭션 해시로 조회: DB에서 TxNumber를 찾은 후 Static File에서 데이터 조회
fn transaction_by_hash(&self, hash: TxHash) -> ProviderResult<Option<Tx>> {
    self.provider()?.transaction_by_hash(hash)
    // 내부: TransactionHashNumbers(hash) → TxNumber → Transactions(TxNumber)
}
```

### 6.4 ConsistentProvider — 일관된 상태 뷰

```rust
// 소스: crates/storage/provider/src/providers/consistent.rs
// 인메모리 상태 + DB 상태를 통합하여 일관된 뷰 제공
```

ConsistentProvider는 **인메모리에 아직 커밋되지 않은 블록**(pending blocks)과 **DB에 저장된 블록**을 통합하여 하나의 일관된 뷰를 제공합니다.

```
조회 순서:
1. 인메모리 블록 (최근, 아직 DB에 기록되지 않은 블록)
2. 데이터베이스 (MDBX)
3. 정적 파일 (NippyJar)

예: block_by_number(N)
  ├── N이 인메모리 범위에 있으면 → 인메모리에서 반환
  ├── N이 DB 범위에 있으면 → MDBX에서 반환
  └── N이 Static File 범위에 있으면 → NippyJar에서 반환
```

### 6.5 StateProvider 종류

| StateProvider 타입 | 설명 | 생성 메서드 |
|-------------------|------|------------|
| **LatestStateProvider** | 최신 상태 (PlainAccountState 직접 조회) | `factory.latest()` |
| **HistoricalStateProvider** | 과거 블록 시점의 상태 (ChangeSets 통해 복원) | `factory.history_by_block_number(n)` |

---

## 7. 일관성 확인 (Consistency Check)

```rust
impl ProviderFactory {
    pub fn check_consistency(&self) -> ProviderResult<(Option<u64>, Option<u64>)> {
        // Step 1: 정적 파일 내부 일관성 검사
        self.static_file_provider().check_file_consistency(&provider_ro)?;

        // Step 2: RocksDB ↔ 정적 파일 일관성 검사
        let rocksdb_unwind = self.rocksdb_provider().check_consistency(&provider_ro)?;

        // Step 3: 정적 파일 ↔ 체크포인트 일관성 검사
        let static_file_unwind = self.static_file_provider().check_consistency(&provider_ro)?;

        Ok((rocksdb_unwind, static_file_unwind))
    }
}
```

시작 시 MDBX, RocksDB, Static File 사이의 일관성을 검증하고, 불일치 발견 시 `MustUnwind` 에러를 반환하여 되감기(unwind)를 요구합니다.

---

## 8. 전체 아키텍처 다이어그램

```
┌─────────────────────────────────────────────────────────────────┐
│                     Application Layer                           │
│  (RPC, Sync Pipeline, Block Builder, Transaction Pool)          │
└───────────────────────┬─────────────────────────────────────────┘
                        │ 호출
┌───────────────────────▼─────────────────────────────────────────┐
│              Provider Layer (storage-api / provider)            │
│                                                                 │
│  ProviderFactory ─────┬──── ConsistentProvider                  │
│       │               │                                         │
│  ┌────▼────┐   ┌──────▼──────┐   ┌────────────────────┐       │
│  │ Latest  │   │ Historical  │   │ Pending (in-memory) │       │
│  │ State   │   │ State       │   │ State               │       │
│  └────┬────┘   └──────┬──────┘   └─────────┬──────────┘       │
└───────┼───────────────┼─────────────────────┼──────────────────┘
        │               │                     │
┌───────▼───────────────▼─────────────────────▼──────────────────┐
│              Storage Backends                                   │
│                                                                 │
│  ┌──────────────┐  ┌─────────────────┐  ┌───────────────────┐  │
│  │   MDBX       │  │  Static Files   │  │  RocksDB          │  │
│  │  (db-api/db) │  │  (NippyJar)     │  │  (실험적)         │  │
│  │              │  │                 │  │                   │  │
│  │ ┌──────────┐ │  │ ┌─────────────┐ │  │                   │  │
│  │ │ Tables   │ │  │ │ Segments    │ │  │                   │  │
│  │ │ (db-api) │ │  │ │ (static-   │ │  │                   │  │
│  │ └──────────┘ │  │ │  file)     │ │  │                   │  │
│  │ ┌──────────┐ │  │ └─────────────┘ │  │                   │  │
│  │ │ Codecs   │ │  │ ┌─────────────┐ │  │                   │  │
│  │ │(Compact) │ │  │ │ Compressor  │ │  │                   │  │
│  │ └──────────┘ │  │ │ (Lz4/Zstd) │ │  │                   │  │
│  │              │  │ └─────────────┘ │  │                   │  │
│  └──────────────┘  └─────────────────┘  └───────────────────┘  │
└────────────────────────────────────────────────────────────────┘
```

---

## 9. 디자인 패턴 요약

### 9.1 Trait 기반 추상화 (Strategy Pattern)

| 계층 | Trait (인터페이스) | 구현 (클래스) |
|------|-------------------|-------------|
| DB 엔진 | `Database`, `DbTx` | `DatabaseEnv`, `Tx<RO>`, `Tx<RW>` |
| 데이터 직렬화 | `Compress`, `Decompress`, `Compact` | 각 데이터 타입별 구현 |
| 스토리지 조회 | `HeaderProvider`, `BlockReader`, `StateProvider` | `DatabaseProvider`, `LatestStateProvider`, `HistoricalStateProvider` |
| Static File | `NippyJarHeader` | `SegmentHeader` |

### 9.2 팩토리 패턴

`ProviderFactory`가 다양한 Provider를 생성합니다:
- `provider()` → 읽기 전용 DatabaseProviderRO
- `provider_rw()` → 읽기/쓰기 DatabaseProviderRW
- `latest()` → LatestStateProvider
- `history_by_block_number()` → HistoricalStateProvider

### 9.3 데이터 티어링 (Hot/Cold Storage)

```
Hot Storage (MDBX)     ← 최근 블록, 빈번한 읽기/쓰기
    │
    │ Static File Producer (주기적)
    ▼
Cold Storage (NippyJar) ← 과거 블록, 읽기 전용, 높은 압축률
```

### 9.4 DUPSORT — 1:N 관계 최적화

MDBX의 DUPSORT를 활용하여 하나의 Key에 여러 Value를 효율적으로 저장합니다:
```
PlainStorageState (DUPSORT):
  Key: Address(0xAA) → Value1: StorageEntry{slot=0x01, value=100}
                     → Value2: StorageEntry{slot=0x02, value=200}
  Key: Address(0xBB) → Value1: StorageEntry{slot=0x01, value=300}
```

---

## 10. 커스터마이징 포인트 (금융 프로젝트 관점)

### 10.1 테이블 확장
- `Tables` enum에 새 테이블을 추가 가능 (최대 256개)
- `DatabaseEnv::create_tables_for::<CustomTableSet>()` 메서드로 커스텀 테이블 생성
- `TableSet` trait을 구현하는 새로운 enum을 정의

### 10.2 State Provider 커스터마이징
- `StateProvider` trait을 구현하는 새로운 Provider를 만들어 사이드 데이터를 통합 가능
- 예: 금융 특화 계정 메타데이터, 규제 정보 등을 추가 조회하는 Provider

### 10.3 Static File 세그먼트 확장
- `StaticFileSegment` enum에 새로운 세그먼트를 추가하여 금융 특화 데이터(예: 결제 이력, 감사 로그)를 NippyJar에 저장 가능

### 10.4 Compact 코덱 활용
- 금융 데이터 타입에 `#[derive(Compact)]`를 적용하여 공간 효율적인 저장
- `Compress`/`Decompress` trait을 구현하여 커스텀 압축 전략 적용

---

## 11. 분석 근거 소스 파일 목록

| 파일 경로 | 주요 내용 |
|----------|----------|
| `crates/storage/db-api/src/database.rs` | `Database` trait |
| `crates/storage/db-api/src/transaction.rs` | `DbTx`, `DbTxMut` trait |
| `crates/storage/db-api/src/cursor.rs` | 커서 trait (`DbCursorRO`, `DbCursorRW` 등) |
| `crates/storage/db-api/src/table.rs` | `Table`, `Key`, `Value`, `Compress`, `Encode` 등 trait |
| `crates/storage/db-api/src/tables/mod.rs` | 전체 테이블 정의 (`tables!` 매크로) |
| `crates/storage/db-api/src/models/sharded_key.rs` | `ShardedKey` 구조체 |
| `crates/storage/db-models/src/blocks.rs` | `StoredBlockBodyIndices`, `StoredBlockWithdrawals` |
| `crates/storage/db-models/src/accounts.rs` | `AccountBeforeTx` |
| `crates/storage/db/src/implementation/mdbx/mod.rs` | `DatabaseEnv`, `DatabaseArguments`, MDBX 구현 |
| `crates/storage/codecs/src/lib.rs` | `Compact` trait 및 기본 타입 구현 |
| `crates/storage/nippy-jar/src/lib.rs` | `NippyJar` 구조체 (불변 데이터 저장 형식) |
| `crates/static-file/types/src/segment.rs` | `StaticFileSegment`, `SegmentHeader` |
| `crates/storage/storage-api/src/state.rs` | `StateProvider`, `StateProviderFactory` trait |
| `crates/storage/storage-api/src/block.rs` | `BlockReader` trait |
| `crates/storage/storage-api/src/receipts.rs` | `ReceiptProvider` trait |
| `crates/storage/storage-api/src/transactions.rs` | `TransactionsProvider` trait |
| `crates/storage/provider/src/providers/database/mod.rs` | `ProviderFactory` |
| `crates/storage/provider/src/providers/consistent.rs` | `ConsistentProvider` |
