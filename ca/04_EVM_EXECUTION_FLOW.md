# 04. EVM 실행 흐름 분석

> **소스코드 경로 기준으로 100% 정확하게 작성된 분석 문서입니다.**
> 외부 문서나 추측이 아닌, 실제 소스코드만을 기반으로 합니다.

---

## 목차

1. [전체 아키텍처 개요](#1-전체-아키텍처-개요)
2. [레이어별 Trait 구조](#2-레이어별-trait-구조)
3. [이더리움 구체 구현체 매핑](#3-이더리움-구체-구현체-매핑)
4. [블록 실행 흐름 (기존 블록 검증)](#4-블록-실행-흐름-기존-블록-검증)
5. [블록 빌딩 흐름 (새 블록 생성)](#5-블록-빌딩-흐름-새-블록-생성)
6. [트랜잭션 실행 상세](#6-트랜잭션-실행-상세)
7. [시스템 콜 상세](#7-시스템-콜-상세)
8. [상태 관리 구조](#8-상태-관리-구조)
9. [커스터마이징 포인트](#9-커스터마이징-포인트)
10. [핵심 타입 별칭 정리](#10-핵심-타입-별칭-정리)

---

## 1. 전체 아키텍처 개요

### EVM 실행의 3-Layer 아키텍처

Reth의 EVM 실행은 **3개의 레이어**로 구성됩니다. 이 설계는 관심사 분리(Separation of Concerns)를 철저히 따릅니다.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  Layer 3: ConfigureEvm  (reth 전용 = Java의 ApplicationService)              │
│                                                                              │
│  역할: EVM 전체 설정의 최상위 통합점                                            │
│  - evm_env(), next_evm_env(): 블록 헤더에서 EVM 환경 구성                      │
│  - executor(): 블록 실행기 생성                                               │
│  - builder_for_next_block(): 블록 빌더 생성                                   │
│  구현: EthEvmConfig (reth/crates/ethereum/evm/src/lib.rs)                    │
├──────────────────────────────────────────────────────────────────────────────┤
│  Layer 2: BlockExecutor / BlockExecutorFactory  (alloy-evm = Java의 Service) │
│                                                                              │
│  역할: 블록 단위 실행 관리                                                     │
│  - apply_pre_execution_changes(): 시스템 콜 적용 (EIP-2935, EIP-4788)         │
│  - execute_transaction(): 트랜잭션 실행 + 커밋                                │
│  - finish(): 후처리 + 블록 결과 반환                                           │
│  구현: EthBlockExecutor (alloy-evm/src/eth/block.rs)                         │
├──────────────────────────────────────────────────────────────────────────────┤
│  Layer 1: Evm / EvmFactory  (alloy-evm = Java의 Repository/DAO)              │
│                                                                              │
│  역할: 개별 트랜잭션 실행                                                      │
│  - transact(): 트랜잭션 실행 → ResultAndState 반환                            │
│  - transact_system_call(): 시스템 콜 실행                                     │
│  구현: EthEvm (alloy-evm/src/eth/mod.rs) ← revm 래핑                        │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Java 대응 비유

```
ConfigureEvm       ≈ @Configuration + ApplicationService (전체 설정 + 오케스트레이션)
BlockExecutorFactory ≈ Factory Pattern (BlockExecutor를 생성하는 팩토리)
BlockExecutor        ≈ Service Layer (블록 단위 비즈니스 로직)
Evm                  ≈ Repository/DAO (개별 트랜잭션 실행 = DB 쿼리 실행)
EvmFactory           ≈ DataSource Factory (DB 커넥션 풀에서 커넥션 꺼내듯 EVM 인스턴스 생성)
```

---

## 2. 레이어별 Trait 구조

### 2.1 Layer 1: `Evm` trait (개별 트랜잭션 실행)

> 📁 `alloy-evm-0.27.2/src/evm.rs`

```rust
/// EVM 인스턴스. 하나의 블록 실행 동안 유지되며, 트랜잭션을 하나씩 실행합니다.
pub trait Evm {
    type DB;            // 데이터베이스 (= &mut State<DB>)
    type Tx;            // 트랜잭션 환경 (= TxEnv)
    type Error;         // 에러 타입 (= EVMError<DB::Error>)
    type HaltReason;    // 실행 중단 이유 (= HaltReason)
    type Spec;          // 스펙 ID (= SpecId, 예: CANCUN, PRAGUE)
    type BlockEnv;      // 블록 환경 (= BlockEnv)
    type Precompiles;   // 프리컴파일 맵
    type Inspector;     // 인스펙터 (트레이싱 등)

    /// 트랜잭션 실행의 핵심 메서드: TxEnv를 받아 ResultAndState를 반환
    fn transact_raw(&mut self, tx: Self::Tx) -> Result<ResultAndState<Self::HaltReason>, Self::Error>;

    /// 유연한 입력을 받는 래퍼 (IntoTxEnv를 통한 자동 변환)
    fn transact(&mut self, tx: impl IntoTxEnv<Self::Tx>) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        self.transact_raw(tx.into_tx_env())
    }

    /// 시스템 콜 전용 실행 (EIP-2935, EIP-4788 등)
    /// - beneficiary가 state에 로드되는 것을 방지하는 특수 처리
    fn transact_system_call(
        &mut self, caller: Address, contract: Address, data: Bytes,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error>;

    fn block(&self) -> &Self::BlockEnv;
    fn db_mut(&mut self) -> &mut Self::DB;
    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec, Self::BlockEnv>);
}
```

**Java 비유**: `Evm`은 JDBC의 `PreparedStatement`와 비슷합니다.
- `transact()` = `statement.execute()` (트랜잭션 하나 실행)
- `transact_system_call()` = 특수 쿼리 실행 (시스템 전용)
- `finish()` = 커넥션 반환 + 결과 받기

### 2.2 Layer 1: `EvmFactory` trait (EVM 인스턴스 생성)

> 📁 `alloy-evm-0.27.2/src/evm.rs`

```rust
/// EVM 인스턴스를 생성하는 팩토리
pub trait EvmFactory {
    type Evm<DB: Database, I: Inspector<Self::Context<DB>>>: Evm<...>;
    type Context<DB: Database>;
    type Tx;
    type Error<DBError>;
    type HaltReason;
    type Spec;
    type BlockEnv;
    type Precompiles;

    /// Inspector 없이 EVM 생성 (일반 실행용)
    fn create_evm<DB: Database>(&self, db: DB, evm_env: EvmEnv<...>) -> Self::Evm<DB, NoOpInspector>;

    /// Inspector 포함 EVM 생성 (트레이싱/디버깅용)
    fn create_evm_with_inspector<DB, I>(&self, db: DB, input: EvmEnv<...>, inspector: I) -> Self::Evm<DB, I>;
}
```

**Java 비유**: `EvmFactory`는 `DataSourceFactory` 또는 `EntityManagerFactory`와 같습니다.
- `create_evm()` = `entityManagerFactory.createEntityManager()` (EVM 인스턴스 생성)

### 2.3 Layer 2: `BlockExecutor` trait (블록 단위 실행)

> 📁 `alloy-evm-0.27.2/src/block/mod.rs`

```rust
/// 블록 하나를 실행하는 핵심 trait
pub trait BlockExecutor {
    type Transaction;      // 합의 레이어 트랜잭션 타입
    type Receipt;          // 영수증 타입
    type Evm: Evm<...>;   // 내부 EVM 인스턴스
    type Result: TxResult; // 트랜잭션 실행 결과 타입

    // ─── 3단계 실행 프로세스 ───

    /// 1단계: 전처리 (시스템 콜)
    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError>;

    /// 2단계: 트랜잭션 실행 (커밋 없이)
    fn execute_transaction_without_commit(&mut self, tx: impl ExecutableTx<Self>) -> Result<Self::Result, BlockExecutionError>;

    /// 2단계+: 트랜잭션 결과 커밋
    fn commit_transaction(&mut self, output: Self::Result) -> Result<u64, BlockExecutionError>;

    /// 3단계: 후처리 + 결과 반환
    fn finish(self) -> Result<(Self::Evm, BlockExecutionResult<Self::Receipt>), BlockExecutionError>;

    // ─── 편의 메서드 (기본 구현) ───

    /// 2단계 통합: 실행 + 커밋 (가장 일반적인 사용법)
    fn execute_transaction(&mut self, tx: impl ExecutableTx<Self>) -> Result<u64, BlockExecutionError> {
        self.execute_transaction_with_result_closure(tx, |_| ())
    }

    /// 전체 블록 실행: 1단계 → 2단계(모든 tx) → 3단계
    fn execute_block(
        mut self,
        transactions: impl IntoIterator<Item = impl ExecutableTx<Self>>,
    ) -> Result<BlockExecutionResult<Self::Receipt>, BlockExecutionError> {
        self.apply_pre_execution_changes()?;
        for tx in transactions { self.execute_transaction(tx)?; }
        self.apply_post_execution_changes()
    }
}
```

### 2.4 Layer 2: `BlockExecutorFactory` trait (BlockExecutor 생성)

> 📁 `alloy-evm-0.27.2/src/block/mod.rs`

```rust
pub trait BlockExecutorFactory: 'static {
    type EvmFactory: EvmFactory;
    type ExecutionCtx<'a>: Clone;  // 블록 실행 컨텍스트 (parent_hash, withdrawals 등)
    type Transaction;
    type Receipt;

    fn evm_factory(&self) -> &Self::EvmFactory;

    /// EVM 인스턴스 + 실행 컨텍스트를 받아 BlockExecutor 생성
    fn create_executor<'a, DB, I>(
        &'a self,
        evm: <Self::EvmFactory as EvmFactory>::Evm<&'a mut State<DB>, I>,
        ctx: Self::ExecutionCtx<'a>,
    ) -> impl BlockExecutorFor<'a, Self, DB, I>;
}
```

### 2.5 Layer 3: `ConfigureEvm` trait (최상위 통합)

> 📁 `reth/crates/evm/evm/src/lib.rs`

```rust
pub trait ConfigureEvm: Clone + Debug + Send + Sync + Unpin {
    type Primitives: NodePrimitives;     // 노드 프리미티브 (Block, Tx, Receipt)
    type Error;
    type NextBlockEnvCtx;                // 다음 블록 구성에 필요한 컨텍스트
    type BlockExecutorFactory: BlockExecutorFactory<...>;
    type BlockAssembler: BlockAssembler<...>;

    // ─── 핵심 메서드 ───

    /// 기존 블록 헤더로부터 EvmEnv 구성
    fn evm_env(&self, header: &Header) -> Result<EvmEnvFor<Self>, Self::Error>;

    /// 다음 블록 환경 구성 (payload building용)
    fn next_evm_env(&self, parent: &Header, attributes: &Self::NextBlockEnvCtx) -> Result<EvmEnvFor<Self>, Self::Error>;

    /// 기존 블록의 실행 컨텍스트 구성
    fn context_for_block<'a>(&self, block: &'a SealedBlock<Block>) -> Result<ExecutionCtxFor<'a, Self>, Self::Error>;

    /// 다음 블록의 실행 컨텍스트 구성
    fn context_for_next_block(&self, parent: &SealedHeader, attributes: Self::NextBlockEnvCtx) -> Result<ExecutionCtxFor<'_, Self>, Self::Error>;

    // ─── 상위 편의 메서드 (기본 구현) ───

    /// 블록 실행기 생성 (가장 많이 쓰이는 메서드)
    fn executor<DB: Database>(&self, db: DB) -> impl Executor<DB, ...> {
        BasicBlockExecutor::new(self, db)
    }

    /// 블록 빌더 생성 (payload building용)
    fn builder_for_next_block<'a, DB>(&'a self, db: &'a mut State<DB>, parent: &'a SealedHeader, attributes: Self::NextBlockEnvCtx
    ) -> Result<impl BlockBuilder<...>, Self::Error> {
        let evm_env = self.next_evm_env(parent, &attributes)?;
        let evm = self.evm_with_env(db, evm_env);
        let ctx = self.context_for_next_block(parent, attributes)?;
        Ok(self.create_block_builder(evm, parent, ctx))
    }
}
```

---

## 3. 이더리움 구체 구현체 매핑

### Trait → 구현체 매핑 테이블

| Trait (추상) | 이더리움 구현체 | 소스 위치 |
|---|---|---|
| `ConfigureEvm` | `EthEvmConfig` | `reth/crates/ethereum/evm/src/lib.rs` |
| `BlockExecutorFactory` | `EthBlockExecutorFactory<RethReceiptBuilder, Arc<ChainSpec>, EthEvmFactory>` | `alloy-evm/src/eth/block.rs` |
| `BlockExecutor` | `EthBlockExecutor` | `alloy-evm/src/eth/block.rs` |
| `BlockAssembler` | `EthBlockAssembler` | `reth/crates/ethereum/evm/src/build.rs` |
| `EvmFactory` | `EthEvmFactory` | `alloy-evm/src/eth/mod.rs` |
| `Evm` | `EthEvm<DB, I, PrecompilesMap>` | `alloy-evm/src/eth/mod.rs` |
| `ReceiptBuilder` | `RethReceiptBuilder` | `reth/crates/ethereum/evm/src/receipt.rs` |
| `EthExecutorSpec` | `EthSpec` (or `Arc<ChainSpec>` via blanket impl) | `alloy-evm/src/eth/spec.rs` |
| `Executor` (reth) | `BasicBlockExecutor<ConfigureEvm, DB>` | `reth/crates/evm/evm/src/execute.rs` |
| `BlockBuilder` (reth) | `BasicBlockBuilder<F, Executor, Builder, N>` | `reth/crates/evm/evm/src/execute.rs` |

### `EthEvmConfig` 내부 구조

> 📁 `reth/crates/ethereum/evm/src/lib.rs`

```rust
pub struct EthEvmConfig<C = ChainSpec, EvmFactory = EthEvmFactory> {
    /// 블록 실행 팩토리: EthBlockExecutorFactory<RethReceiptBuilder, Arc<C>, EvmFactory>
    pub executor_factory: EthBlockExecutorFactory<RethReceiptBuilder, Arc<C>, EvmFactory>,
    /// 블록 조립기
    pub block_assembler: EthBlockAssembler<C>,
}
```

**Java 비유**: `EthEvmConfig`는 `@Configuration` 클래스처럼,
- `executor_factory` = `@Bean public BlockExecutorFactory executorFactory() { ... }`
- `block_assembler` = `@Bean public BlockAssembler assembler() { ... }`

---

## 4. 블록 실행 흐름 (기존 블록 검증)

> 동기화(Sync) 시 외부에서 받은 블록을 검증하는 흐름입니다.

### 4.1 진입점: `executor.execute(&block)`

> 📁 `reth/crates/evm/evm/src/execute.rs`

```
호출 코드:
    let output = evm_config.executor(state_db).execute(&block)?;

내부 흐름:
    Executor::execute()
    └─ self.execute_one(block)
    │   └─ self.strategy_factory.executor_for_block(&mut self.db, block)
    │   │   └─ ConfigureEvm::executor_for_block()
    │   │       ├─ evm = self.evm_for_block(db, block.header())
    │   │       │   ├─ evm_env = self.evm_env(header)         // EvmEnv 구성
    │   │       │   └─ self.evm_with_env(db, evm_env)         // EVM 인스턴스 생성
    │   │       ├─ ctx = self.context_for_block(block)         // 실행 컨텍스트 구성
    │   │       └─ self.create_executor(evm, ctx)              // BlockExecutor 생성
    │   │           └─ EthBlockExecutorFactory::create_executor()
    │   │               └─ EthBlockExecutor::new(evm, ctx, &self.spec, &self.receipt_builder)
    │   │
    │   └─ executor.execute_block(block.transactions_recovered())
    │       ├─ (1) apply_pre_execution_changes()     ← 전처리
    │       ├─ (2) for tx: execute_transaction(tx)   ← 트랜잭션 실행
    │       └─ (3) apply_post_execution_changes()    ← 후처리
    │
    └─ self.db.merge_transitions(BundleRetention::Reverts)
    └─ BlockExecutionOutput { state: state.take_bundle(), result }
```

### 4.2 단계 1: `apply_pre_execution_changes()`

> 📁 `alloy-evm/src/eth/block.rs` — `EthBlockExecutor`의 구현

```rust
fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
    // 1. Spurious Dragon 이후면 state clear flag 설정
    let state_clear_flag = self.spec.is_spurious_dragon_active_at_block(
        self.evm.block().number().saturating_to()
    );
    self.evm.db_mut().set_state_clear_flag(state_clear_flag);

    // 2. EIP-2935: 블록해시 히스토리 컨트랙트 호출 (Prague 이후)
    self.system_caller.apply_blockhashes_contract_call(self.ctx.parent_hash, &mut self.evm)?;

    // 3. EIP-4788: 비콘 루트 컨트랙트 호출 (Cancun 이후)
    self.system_caller.apply_beacon_root_contract_call(
        self.ctx.parent_beacon_block_root, &mut self.evm
    )?;

    Ok(())
}
```

### 4.3 단계 2: 트랜잭션 실행

> 📁 `alloy-evm/src/eth/block.rs` — `EthBlockExecutor`의 구현

```
execute_transaction(tx)
└─ execute_transaction_with_result_closure(tx, |_| ())
    └─ execute_transaction_with_commit_condition(tx, |res| { CommitChanges::Yes })
        ├─ execute_transaction_without_commit(tx)
        │   ├─ tx.into_parts() → (tx_env, recovered_tx)
        │   ├─ 가스 한도 검증: tx.gas_limit() > block_available_gas → Error
        │   ├─ self.evm.transact(tx_env) → ResultAndState { result, state }
        │   └─ EthTxResult { result, blob_gas_used, tx_type }
        │
        └─ commit_transaction(output)
            ├─ self.system_caller.on_state(StateChangeSource::Transaction(n), &state)
            ├─ self.gas_used += result.gas_used()
            ├─ self.blob_gas_used += blob_gas_used (Cancun 이후)
            ├─ self.receipts.push(receipt_builder.build_receipt(...))
            ├─ self.evm.db_mut().commit(state)   ← 상태 변경 커밋
            └─ Ok(gas_used)
```

### 4.4 단계 3: `finish()` (후처리)

> 📁 `alloy-evm/src/eth/block.rs` — `EthBlockExecutor`의 구현

```rust
fn finish(mut self) -> Result<(Self::Evm, BlockExecutionResult<R::Receipt>), BlockExecutionError> {
    // 1. Prague 이후: EIP-6110 디포짓 수집 + EIP-7002/7251 시스템 콜
    let requests = if self.spec.is_prague_active_at_timestamp(...) {
        let deposit_requests = eip6110::parse_deposits_from_receipts(&self.spec, &self.receipts)?;
        let mut requests = Requests::default();
        if !deposit_requests.is_empty() {
            requests.push_request_with_type(DEPOSIT_REQUEST_TYPE, deposit_requests);
        }
        self.system_caller.append_post_execution_changes(&mut self.evm, &mut requests)?;
        requests
    } else {
        Requests::default()
    };

    // 2. 블록 보상 계산 (PoW 시절) + Withdrawal 처리 (Shanghai 이후)
    let mut balance_increments = post_block_balance_increments(
        &self.spec, self.evm.block(), self.ctx.ommers, self.ctx.withdrawals.as_deref(),
    );

    // 3. DAO 포크 특별 처리 (DAO 해킹 사건 대응)
    if self.spec.ethereum_fork_activation(EthereumHardfork::Dao)
        .transitions_at_block(self.evm.block().number().saturating_to()) {
        let drained_balance: u128 = self.evm.db_mut()
            .drain_balances(dao_fork::DAO_HARDFORK_ACCOUNTS)?.into_iter().sum();
        *balance_increments.entry(dao_fork::DAO_HARDFORK_BENEFICIARY).or_default() += drained_balance;
    }

    // 4. 잔액 증가 적용
    self.evm.db_mut().increment_balances(balance_increments.clone())?;

    // 5. state hook 호출 (모니터링/트레이싱용)
    self.system_caller.try_on_state_with(|| { ... })?;

    // 6. 결과 반환
    Ok((self.evm, BlockExecutionResult {
        receipts: self.receipts,
        requests,
        gas_used: self.gas_used,
        blob_gas_used: self.blob_gas_used,
    }))
}
```

### 4.5 전체 흐름 다이어그램

```
                    ┌──────────────────────────────┐
                    │ evm_config.executor(db)       │
                    │   = BasicBlockExecutor::new() │
                    └──────────────┬───────────────┘
                                   │
                    ┌──────────────▼───────────────┐
                    │     executor.execute(&block)  │
                    └──────────────┬───────────────┘
                                   │
            ┌──────────────────────▼──────────────────────┐
            │  executor_for_block(&mut db, &block)        │
            │  ┌─ evm_env = evm_env(header)               │
            │  │    └─ EvmEnv { cfg_env, block_env }      │
            │  ├─ evm = evm_with_env(db, evm_env)         │
            │  │    └─ EthEvmFactory::create_evm()        │
            │  │        └─ EthEvmBuilder::new(...).build() │
            │  │            └─ revm::Context + Inspector   │
            │  ├─ ctx = context_for_block(block)           │
            │  │    └─ EthBlockExecutionCtx { ... }       │
            │  └─ create_executor(evm, ctx)                │
            │       └─ EthBlockExecutor::new()            │
            └──────────────────────┬──────────────────────┘
                                   │
            ┌──────────────────────▼───────────────────────┐
            │  executor.execute_block(transactions)        │
            │                                              │
            │  ┌─ ① apply_pre_execution_changes()         │
            │  │     ├─ set_state_clear_flag()             │
            │  │     ├─ EIP-2935: blockhashes contract     │
            │  │     └─ EIP-4788: beacon root contract     │
            │  │                                           │
            │  ├─ ② for each tx in transactions:          │
            │  │     ├─ 가스 한도 검증                       │
            │  │     ├─ evm.transact(tx_env) → ResultAndState│
            │  │     ├─ receipt = receipt_builder.build()   │
            │  │     ├─ gas_used += tx_gas                 │
            │  │     └─ evm.db_mut().commit(state)         │
            │  │                                           │
            │  └─ ③ finish()                              │
            │       ├─ EIP-6110: parse deposit requests    │
            │       ├─ EIP-7002: withdrawal requests       │
            │       ├─ EIP-7251: consolidation requests    │
            │       ├─ 블록 보상 계산 + 적용                 │
            │       ├─ Withdrawal 처리                      │
            │       └─ DAO fork 처리 (해당 시)              │
            └──────────────────────┬──────────────────────┘
                                   │
            ┌──────────────────────▼──────────────────────┐
            │  db.merge_transitions(BundleRetention::Reverts)│
            │  BlockExecutionOutput {                      │
            │      state: db.take_bundle(),  ← BundleState │
            │      result: BlockExecutionResult { ... }    │
            │  }                                           │
            └──────────────────────────────────────────────┘
```

---

## 5. 블록 빌딩 흐름 (새 블록 생성)

> Payload Building 시 새로운 블록을 만드는 흐름입니다.

### 5.1 진입점: `builder_for_next_block()`

> 📁 `reth/crates/evm/evm/src/lib.rs`

```rust
fn builder_for_next_block<'a, DB>(&'a self, db: &'a mut State<DB>, parent: &'a SealedHeader, attributes: Self::NextBlockEnvCtx)
    -> Result<impl BlockBuilder<...>, Self::Error>
{
    // 1. 다음 블록의 EVM 환경 구성
    let evm_env = self.next_evm_env(parent, &attributes)?;
    // 2. EVM 인스턴스 생성
    let evm = self.evm_with_env(db, evm_env);
    // 3. 다음 블록의 실행 컨텍스트 구성
    let ctx = self.context_for_next_block(parent, attributes)?;
    // 4. 블록 빌더 생성
    Ok(self.create_block_builder(evm, parent, ctx))
}
```

### 5.2 `NextBlockEnvAttributes` 구조

> 📁 `reth/crates/evm/evm/src/lib.rs`

```rust
/// CL(Consensus Layer)에서 전달받는 다음 블록 속성
pub struct NextBlockEnvAttributes {
    pub timestamp: u64,                       // 블록 타임스탬프
    pub suggested_fee_recipient: Address,      // 수수료 수신자
    pub prev_randao: B256,                     // 랜덤 값 (PoS)
    pub gas_limit: u64,                        // 가스 한도
    pub parent_beacon_block_root: Option<B256>, // 비콘 블록 루트
    pub withdrawals: Option<Withdrawals>,      // 출금 목록
    pub extra_data: Bytes,                     // 추가 데이터
}
```

### 5.3 블록 빌더 사용 흐름

```
호출 코드:
    let mut builder = evm_config.builder_for_next_block(&mut state, &parent, attributes)?;
    builder.apply_pre_execution_changes()?;
    for tx in pending_transactions {
        builder.execute_transaction(tx)?;
    }
    let outcome = builder.finish(state_provider)?;
    // outcome.block = 완성된 블록

내부 흐름:
    BasicBlockBuilder::finish()
    ├─ executor.finish()               → (evm, BlockExecutionResult)
    ├─ evm.finish()                    → (db, EvmEnv)
    ├─ db.merge_transitions(BundleRetention::Reverts)
    ├─ hashed_state = state.hashed_post_state(&db.bundle_state)
    ├─ (state_root, trie_updates) = state.state_root_with_updates(hashed_state)
    ├─ assembler.assemble_block(BlockAssemblerInput { ... })
    │   └─ EthBlockAssembler::assemble_block()
    │       ├─ transactions_root = proofs::calculate_transaction_root()
    │       ├─ receipts_root = calculate_receipt_root()
    │       ├─ logs_bloom = logs_bloom(receipts)
    │       ├─ withdrawals_root (Shanghai 이후)
    │       ├─ requests_hash (Prague 이후)
    │       ├─ excess_blob_gas (Cancun 이후)
    │       └─ Header + BlockBody → Block
    └─ BlockBuilderOutcome { execution_result, hashed_state, trie_updates, block }
```

### 5.4 `EthBlockAssembler::assemble_block()` 상세

> 📁 `reth/crates/ethereum/evm/src/build.rs`

```rust
fn assemble_block(&self, input: BlockAssemblerInput<'_, '_, F>) -> Result<Self::Block, BlockExecutionError> {
    // 실행 결과 디스트럭처링
    let BlockAssemblerInput {
        evm_env, execution_ctx: ctx, parent, transactions,
        output: BlockExecutionResult { receipts, requests, gas_used, blob_gas_used },
        state_root, ..
    } = input;

    // 각종 루트/블룸 계산
    let transactions_root = proofs::calculate_transaction_root(&transactions);
    let receipts_root = calculate_receipt_root(...);
    let logs_bloom = logs_bloom(receipts.iter().flat_map(|r| r.logs()));

    // 포크별 조건부 필드
    let withdrawals = (Shanghai 활성화).then(|| ctx.withdrawals...);
    let withdrawals_root = withdrawals.as_deref().map(|w| calculate_withdrawals_root(w));
    let requests_hash = (Prague 활성화).then(|| requests.requests_hash());
    let excess_blob_gas = (Cancun 활성화).then(|| ...);

    // 최종 블록 조립
    let header = Header {
        parent_hash: ctx.parent_hash,
        beneficiary: evm_env.block_env.beneficiary(),
        state_root,
        transactions_root,
        receipts_root,
        logs_bloom,
        timestamp,
        number: evm_env.block_env.number(),
        gas_limit: evm_env.block_env.gas_limit(),
        gas_used: *gas_used,
        base_fee_per_gas: Some(evm_env.block_env.basefee()),
        // ... 기타 필드
    };

    Ok(Block { header, body: BlockBody { transactions, ommers: Default::default(), withdrawals } })
}
```

---

## 6. 트랜잭션 실행 상세

### 6.1 트랜잭션 → TxEnv 변환 흐름

```
합의 레이어 트랜잭션
    Recovered<TransactionSigned>
        │
        ▼ (FromRecoveredTx trait)
    revm::TxEnv {
        caller: Address,      // 서명에서 복구된 sender
        gas_limit: u64,
        value: U256,
        data: Bytes,          // calldata
        to: TxKind,           // CREATE or CALL
        nonce: u64,
        chain_id: Option<u64>,
        access_list: AccessList,
        max_fee_per_gas: u64,
        max_priority_fee_per_gas: u64,
        blob_hashes: Vec<B256>,
        max_fee_per_blob_gas: u64,
        // ...
    }
```

### 6.2 EVM 내부 실행 (`EthEvm::transact_raw`)

> 📁 `alloy-evm/src/eth/mod.rs`

```rust
fn transact_raw(&mut self, tx: Self::Tx) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
    if self.inspect {
        self.inner.inspect_tx(tx)   // Inspector가 활성화된 경우 (트레이싱)
    } else {
        self.inner.transact(tx)     // 일반 실행
    }
}
```

여기서 `self.inner`는 `revm::RevmEvm`이며, 실제 EVM 바이트코드 인터프리터를 호출합니다.

### 6.3 실행 결과 구조

```rust
ResultAndState<HaltReason> {
    result: ExecutionResult<HaltReason> {
        enum {
            Success { output, gas_used, logs, ... },
            Revert { output, gas_used },
            Halt { reason, gas_used },
        }
    },
    state: EvmState  // HashMap<Address, Account { info, storage_changes, status }>
                     //   각 주소별로 변경된 계정 정보와 스토리지 변경사항
}
```

### 6.4 Receipt 생성 (`RethReceiptBuilder`)

> 📁 `reth/crates/ethereum/evm/src/receipt.rs`

```rust
impl ReceiptBuilder for RethReceiptBuilder {
    type Transaction = TransactionSigned;
    type Receipt = Receipt;

    fn build_receipt<E: Evm>(&self, ctx: ReceiptBuilderCtx<'_, TxType, E>) -> Self::Receipt {
        let ReceiptBuilderCtx { tx_type, result, cumulative_gas_used, .. } = ctx;
        Receipt {
            tx_type,                              // Legacy, EIP-2930, EIP-1559 등
            success: result.is_success(),          // EIP-658 상태 코드
            cumulative_gas_used,                   // 블록 내 누적 가스
            logs: result.into_logs(),              // 이벤트 로그
        }
    }
}
```

---

## 7. 시스템 콜 상세

### 7.1 SystemCaller 구조

> 📁 `alloy-evm/src/block/system_calls/mod.rs`

```rust
pub struct SystemCaller<Spec> {
    spec: Spec,                          // 체인 스펙 (하드포크 정보)
    hook: Option<Box<dyn OnStateHook>>,   // 상태 변경 모니터링 훅
}
```

### 7.2 전처리 시스템 콜 (Pre-Block)

| EIP | 활성 조건 | 호출 대상 | 설명 |
|-----|----------|-----------|------|
| **EIP-2935** | Prague 이후 | `HISTORY_STORAGE_ADDRESS` | parent_hash를 블록해시 히스토리 컨트랙트에 저장 |
| **EIP-4788** | Cancun 이후 | `BEACON_ROOTS_ADDRESS` | parent_beacon_block_root를 비콘 루트 컨트랙트에 저장 |

#### EIP-2935 상세 흐름

> 📁 `alloy-evm/src/block/system_calls/eip2935.rs`

```rust
pub fn transact_blockhashes_contract_call<Halt>(
    spec: impl EthereumHardforks,
    parent_block_hash: B256,
    evm: &mut impl Evm<HaltReason = Halt>,
) -> Result<Option<ResultAndState<Halt>>, BlockExecutionError> {
    // Prague가 활성화되지 않은 경우 → 아무것도 안 함
    if !spec.is_prague_active_at_timestamp(...) { return Ok(None); }
    // 제네시스 블록이면 → 아무것도 안 함
    if evm.block().number().is_zero() { return Ok(None); }

    // SYSTEM_ADDRESS(0xfffffffe)가 HISTORY_STORAGE_ADDRESS를 호출
    // calldata = parent_block_hash
    let res = evm.transact_system_call(
        SYSTEM_ADDRESS,            // caller: 0xff...fe
        HISTORY_STORAGE_ADDRESS,   // to: EIP-2935 컨트랙트
        parent_block_hash.0.into(), // data: 부모 블록 해시
    )?;
    Ok(Some(res))
}
```

#### EIP-4788 상세 흐름 (유사 패턴)

> 📁 `alloy-evm/src/block/system_calls/eip4788.rs`

- Cancun 활성화 시에만 동작
- `SYSTEM_ADDRESS` → `BEACON_ROOTS_ADDRESS` 호출
- calldata = `parent_beacon_block_root`

### 7.3 후처리 시스템 콜 (Post-Block)

| EIP | 활성 조건 | 호출 대상 | 설명 |
|-----|----------|-----------|------|
| **EIP-6110** | Prague 이후 | Receipt 파싱 | 디포짓 이벤트를 영수증에서 추출 |
| **EIP-7002** | Prague 이후 | 출금 요청 컨트랙트 | validator withdrawal 요청 수집 |
| **EIP-7251** | Prague 이후 | 통합 요청 컨트랙트 | validator consolidation 요청 수집 |

### 7.4 블록 보상 계산

> 📁 `alloy-evm/src/block/state_changes.rs`

```rust
pub fn post_block_balance_increments<H>(
    spec: impl EthereumHardforks,
    block_env: impl Block,
    ommers: &[H],
    withdrawals: Option<&Withdrawals>,
) -> AddressMap<u128> {
    let mut balance_increments = AddressMap::new();

    // 1. PoW 블록 보상 (Merge 이전에만)
    if let Some(base_block_reward) = calc::base_block_reward(&spec, block_number) {
        // 엉클 보상
        for ommer in ommers {
            *balance_increments.entry(ommer.beneficiary()).or_default()
                += calc::ommer_reward(base_block_reward, block_number, ommer.number());
        }
        // 전체 블록 보상
        *balance_increments.entry(block_env.beneficiary()).or_default()
            += calc::block_reward(base_block_reward, ommers.len());
    }

    // 2. Shanghai 이후: Withdrawal 처리
    if spec.is_shanghai_active_at_timestamp(...) {
        for withdrawal in withdrawals {
            if withdrawal.amount > 0 {
                *balance_increments.entry(withdrawal.address).or_default()
                    += withdrawal.amount_wei();
            }
        }
    }

    balance_increments
}
```

---

## 8. 상태 관리 구조

### 8.1 데이터베이스 레이어 스택

```
┌──────────────────────────────────────────────────────────────┐
│  EVM (EthEvm)                                                │
│   ├─ DB: &mut State<StateProviderDatabase<SP>>               │
│   │   ├─ State (revm)                                        │
│   │   │   ├─ cache: CacheState { accounts, contracts }       │
│   │   │   ├─ transition_state: TransitionState                │
│   │   │   │   └─ 각 트랜잭션의 상태 변경을 추적               │
│   │   │   ├─ bundle_state: BundleState                        │
│   │   │   │   └─ 모든 변경사항의 요약 (pre/post 값)           │
│   │   │   └─ database: StateProviderDatabase<SP>              │
│   │   │       └─ SP: StateProvider (실제 DB 접근)             │
│   │   │                                                      │
│   │   └─ 읽기 흐름: cache → database(원본 DB)               │
│   │      쓰기 흐름: cache → transition_state → bundle_state  │
│   └─ 각 tx 후: evm.db_mut().commit(state)                   │
│       → 캐시에 상태 변경 기록 (아직 원본 DB에는 안 씀)        │
└──────────────────────────────────────────────────────────────┘
```

### 8.2 `StateProviderDatabase` — reth의 DB 어댑터

> 📁 `reth/crates/revm/src/database.rs`

```rust
/// EvmStateProvider(= StateProvider)를 revm의 Database로 변환하는 어댑터
pub struct StateProviderDatabase<DB>(pub DB);

impl<DB: EvmStateProvider> Database for StateProviderDatabase<DB> {
    type Error = ProviderError;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.basic_ref(address)  // → EvmStateProvider::basic_account()
    }
    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.code_by_hash_ref(code_hash)  // → EvmStateProvider::bytecode_by_hash()
    }
    fn storage(&mut self, address: Address, index: U256) -> Result<U256, Self::Error> {
        self.storage_ref(address, index)  // → EvmStateProvider::storage()
    }
    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.block_hash_ref(number)  // → EvmStateProvider::block_hash()
    }
}
```

**Java 비유**: `StateProviderDatabase`는 JPA의 `EntityManager` 위에 `DataSource` 어댑터를 씌운 것과 같습니다.
- `AccountReader` → `basic_account()` = `findById(address)`
- `BytecodeReader` → `bytecode_by_hash()` = 바이트코드 조회
- `StateProvider` → `storage()` = 스토리지 슬롯 조회

### 8.3 `CachedReads` — 페이로드 빌딩용 캐시

> 📁 `reth/crates/revm/src/cached.rs`

```rust
/// 반복적 페이로드 빌딩 시 동일 데이터의 재조회를 방지하는 캐시
pub struct CachedReads {
    pub accounts: AddressMap<CachedAccount>,   // 계정 정보 캐시
    pub contracts: B256Map<Bytecode>,          // 컨트랙트 코드 캐시
    pub block_hashes: HashMap<u64, B256>,      // 블록 해시 캐시
}
```

**사용 패턴**:
```rust
let mut cached_reads = CachedReads::default();
let db = cached_reads.as_db_mut(state_provider);  // 캐시 레이어 래핑
let state = State::builder().with_database(db).build();
// 이후 state를 통해 DB를 조회하면 캐시 → 원본 DB 순으로 조회됨
// 반복적인 payload building에서 큰 성능 이점
```

### 8.4 상태 변경 커밋 흐름

```
(트랜잭션 실행)
    evm.transact(tx_env)
    → ResultAndState { result, state: EvmState(HashMap<Address, changes>) }

(트랜잭션 커밋 - 아직 State 내부에만)
    evm.db_mut().commit(state)
    → State의 cache에 변경사항 반영
    → transition_state에 변경 기록 추가

(블록 실행 완료 후 - Bundle 생성)
    state.merge_transitions(BundleRetention::Reverts)
    → 모든 transition을 bundle_state로 병합
    → bundle_state = { 변경된 계정들의 (원본정보, 현재정보, 스토리지변경, revert정보) }

(최종 결과)
    state.take_bundle()
    → BundleState 반환 (별도 처리는 상위 레이어의 책임)
```

---

## 9. 커스터마이징 포인트

### 9.1 커스텀 EVM 프리컴파일 추가

> 📁 `reth/examples/custom-evm/src/main.rs`

```rust
// 1. EvmFactory를 구현하여 커스텀 프리컴파일 추가
pub struct MyEvmFactory;

impl EvmFactory for MyEvmFactory {
    // ... 타입 정의 생략 ...

    fn create_evm<DB>(&self, db: DB, input: EvmEnv) -> Self::Evm<DB, NoOpInspector> {
        let spec = input.cfg_env.spec;
        let mut evm = Context::mainnet()
            .with_db(db).with_cfg(input.cfg_env).with_block(input.block_env)
            .build_mainnet_with_inspector(NoOpInspector {})
            .with_precompiles(PrecompilesMap::from_static(EthPrecompiles::default().precompiles));

        // Prague 스펙에서 커스텀 프리컴파일 추가
        if spec == SpecId::PRAGUE {
            evm = evm.with_precompiles(PrecompilesMap::from_static(prague_custom()));
        }

        EthEvm::new(evm, false)
    }
}

// 2. ExecutorBuilder로 노드 빌더에 등록
pub struct MyExecutorBuilder;
impl<Node> ExecutorBuilder<Node> for MyExecutorBuilder {
    type EVM = EthEvmConfig<ChainSpec, MyEvmFactory>;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        Ok(EthEvmConfig::new_with_evm_factory(ctx.chain_spec(), MyEvmFactory::default()))
    }
}

// 3. 노드에 적용
NodeBuilder::new(node_config)
    .with_types::<EthereumNode>()
    .with_components(EthereumNode::components().executor(MyExecutorBuilder::default()))
    .launch().await;
```

### 9.2 커스터마이징 가능 계층 요약

| 계층 | 커스터마이징 방법 | 효과 |
|------|-----------------|------|
| `EvmFactory` | 새 struct + `impl EvmFactory` | 프리컴파일 추가/변경, EVM 동작 변경 |
| `BlockExecutor` | 새 struct + `impl BlockExecutor` | 블록 전/후 처리 로직 변경 |
| `BlockAssembler` | 새 struct + `impl BlockAssembler` | 블록 헤더 조립 방식 변경 |
| `ReceiptBuilder` | 새 struct + `impl ReceiptBuilder` | 영수증 생성 방식 변경 |
| `ConfigureEvm` | 새 struct + `impl ConfigureEvm` | 위 모든 것의 통합 변경 |
| `BlockExecutorFactory` | 새 struct + `impl BlockExecutorFactory` | 블록 실행기 팩토리 로직 변경 |

---

## 10. 핵심 타입 별칭 정리

> 📁 `reth/crates/evm/evm/src/aliases.rs`

```rust
// ConfigureEvm의 팩토리에서 꺼낸 EvmFactory 타입
type EvmFactoryFor<C> = <<C as ConfigureEvm>::BlockExecutorFactory as BlockExecutorFactory>::EvmFactory;

// ConfigureEvm에서 만들어지는 EVM 인스턴스 타입
type EvmFor<C, DB, I> = <EvmFactoryFor<C> as EvmFactory>::Evm<DB, I>;

// ConfigureEvm에서 사용하는 EvmEnv 타입
type EvmEnvFor<C> = EvmEnv<<EvmFactoryFor<C> as EvmFactory>::Spec,
                           <EvmFactoryFor<C> as EvmFactory>::BlockEnv>;

// ConfigureEvm에서 사용하는 TxEnv 타입
type TxEnvFor<C> = <EvmFactoryFor<C> as EvmFactory>::Tx;

// ConfigureEvm에서 사용하는 ExecutionCtx 타입
type ExecutionCtxFor<'a, C> = <<C as ConfigureEvm>::BlockExecutorFactory as BlockExecutorFactory>::ExecutionCtx<'a>;

// Inspector 타입
type InspectorFor<C, DB> = Inspector<<EvmFactoryFor<C> as EvmFactory>::Context<DB>>;
```

### EvmEnv 구조

> 📁 `alloy-evm/src/env.rs`

```rust
pub struct EvmEnv<Spec = SpecId, BlockEnv = revm::context::BlockEnv> {
    pub cfg_env: CfgEnv<Spec>,  // chain_id, spec_id, 각종 제한값 등
    pub block_env: BlockEnv,    // number, timestamp, beneficiary, basefee, gas_limit 등
}
```

### EthBlockExecutionCtx 구조

> 📁 `alloy-evm/src/eth/block.rs`

```rust
pub struct EthBlockExecutionCtx<'a> {
    pub parent_hash: B256,                          // 부모 블록 해시
    pub parent_beacon_block_root: Option<B256>,     // 비콘 블록 루트
    pub ommers: &'a [Header],                       // 엉클 블록들
    pub withdrawals: Option<Cow<'a, Withdrawals>>,  // 출금 목록
    pub extra_data: Bytes,                          // 추가 데이터
    pub tx_count_hint: Option<usize>,               // 트랜잭션 수 힌트 (receipts 벡터 사전 할당)
}
```

### 실행 결과 타입 체인

```
트랜잭션 실행:
    evm.transact(tx_env)
    → ResultAndState<HaltReason>    // revm 수준 결과
    → EthTxResult { result, blob_gas_used, tx_type }  // Eth 트랜잭션 결과 래핑

블록 실행:
    executor.finish()
    → (Evm, BlockExecutionResult<Receipt>)  // 블록 수준 결과
    │   └─ { receipts, requests, gas_used, blob_gas_used }

전체 실행:
    executor.execute(&block)  (reth Executor trait)
    → BlockExecutionOutput<Receipt>  // 상태 변경 포함
    │   └─ { result: BlockExecutionResult, state: BundleState }

배치 실행:
    executor.execute_batch(&blocks)  (reth Executor trait)
    → ExecutionOutcome<Receipt>  // 여러 블록 결과 집합
        └─ { bundle: BundleState, receipts: Vec<Vec<Receipt>>,
             first_block, requests: Vec<Requests> }
```

---

## 부록: 파일별 핵심 역할 요약

| 파일 | 핵심 역할 |
|------|----------|
| `reth/crates/evm/evm/src/lib.rs` | `ConfigureEvm` trait, `NextBlockEnvAttributes`, `TransactionEnv` |
| `reth/crates/evm/evm/src/execute.rs` | `Executor`, `BlockBuilder`, `BlockAssembler` trait, `BasicBlockExecutor`, `BasicBlockBuilder` |
| `reth/crates/ethereum/evm/src/lib.rs` | `EthEvmConfig` — ConfigureEvm 이더리움 구현 |
| `reth/crates/ethereum/evm/src/build.rs` | `EthBlockAssembler` — 블록 조립 이더리움 구현 |
| `reth/crates/ethereum/evm/src/receipt.rs` | `RethReceiptBuilder` — 영수증 빌더 |
| `alloy-evm/src/evm.rs` | `Evm`, `EvmFactory` trait (트랜잭션 실행 추상화) |
| `alloy-evm/src/eth/mod.rs` | `EthEvm`, `EthEvmFactory` — Evm 이더리움 구현 |
| `alloy-evm/src/eth/block.rs` | `EthBlockExecutor`, `EthBlockExecutorFactory` — 블록 실행 이더리움 구현 |
| `alloy-evm/src/block/mod.rs` | `BlockExecutor`, `BlockExecutorFactory` trait |
| `alloy-evm/src/block/system_calls/` | `SystemCaller`, EIP-2935/4788/7002/7251 시스템 콜 |
| `alloy-evm/src/block/state_changes.rs` | 블록 보상, Withdrawal 처리, 잔액 변경 |
| `alloy-evm/src/eth/spec.rs` | `EthExecutorSpec`, `EthSpec` — 체인 스펙 |
| `alloy-evm/src/env.rs` | `EvmEnv`, `BlockEnvironment` — EVM 환경 설정 |
| `reth/crates/revm/src/database.rs` | `StateProviderDatabase` — reth DB → revm Database 어댑터 |
| `reth/crates/revm/src/cached.rs` | `CachedReads` — 페이로드 빌딩용 읽기 캐시 |
| `reth/crates/evm/execution-types/src/execute.rs` | `BlockExecutionOutput` — 상태 포함 블록 결과 |
| `reth/crates/evm/execution-types/src/execution_outcome.rs` | `ExecutionOutcome` — 다수 블록 실행 결과 |
