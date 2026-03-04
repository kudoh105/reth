# 11. ERC20 Transfer 트랜잭션 & 블록 라이프사이클 — reth 소스코드 완전 추적

> **목표**: `transfer(to, amount)` 트랜잭션 1건이 reth 노드에 진입하여 블록에 포함되고,  
> 최종적으로 Static File에 영구 저장되기까지의 **전체 데이터 흐름**을 소스코드 파일·함수 단위로 추적한다.

---

## 전체 라이프사이클 한눈에 보기

```
┌──────────────────────────────────────────────────────────────────────────────────────┐
│                         ERC20 transfer(to, amount) 라이프사이클                        │
│                                                                                      │
│  ① TX 제출          ② 검증              ③ 풀 진입          ④ 블록 빌드                │
│  (RPC / P2P)  ───► (Validator)  ───►  (TxPool)   ───►  (Payload Builder)            │
│                                                              │                       │
│                                                              ▼                       │
│  ⑦ Static File      ⑥ State 기록       ⑤ EVM 실행          EVM 실행 (블록 빌드 중)    │
│  저장   ◄─────  DB Write  ◄──────  (EthBlockExecutor)  ◄──  CALL → SSTORE → LOG      │
│                                                                                      │
│  ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─  │
│  [동기화 경로] 다른 노드의 블록을 받을 때                                                 │
│  P2P 수신  ─►  Bodies Stage  ─►  SenderRecovery Stage  ─►  Execution Stage  ─►  DB   │
└──────────────────────────────────────────────────────────────────────────────────────┘
```

---

## ① TX 진입: RPC 경로 vs P2P 경로

### 경로 1-A: eth_sendRawTransaction (RPC)

```
클라이언트 (MetaMask 등)
  │  HTTP POST /  {"method":"eth_sendRawTransaction","params":["0x02..."]}
  ▼
┌─────────────────────────────────────────────────────────────────────┐
│  crates/rpc/rpc-eth-api/src/core.rs                                  │
│  trait EthApiServer                                                   │
│    async fn send_raw_transaction(&self, tx: Bytes) -> RpcResult<B256> │
│      └─► EthTransactions::send_raw_transaction()     [L79]           │
└─────────────────────────────────────────────────────────────────────┘
  │
  │  1. RLP 디코딩: Bytes → TransactionSigned
  │  2. ECDSA 서명 복구: recover_signer() → Recovered<TransactionSigned>
  │  3. Pool에 제출
  ▼
┌─────────────────────────────────────────────────────────────────────┐
│  crates/rpc/rpc-eth-api/src/helpers/transaction.rs  [L100]          │
│  fn send_raw_transaction_sync()                                       │
│    └─► pool.add_transaction(                                          │
│          TransactionOrigin::Local,   ← RPC 제출 = Local              │
│          transaction                                                  │
│        ).await                                                        │
└─────────────────────────────────────────────────────────────────────┘
```

### 경로 1-B: P2P 네트워크 (gossip)

```
피어 노드
  │  ETH/68 프로토콜 NewPooledTransactionHashes / Transactions
  ▼
┌─────────────────────────────────────────────────────────────────────┐
│  crates/net/network/src/transactions/mod.rs                          │
│  TransactionsManager::on_network_tx_event()                          │
│    └─► pool.add_external_transaction(transaction)                    │
│          ↓                                                            │
│        TransactionOrigin::External  ← P2P 수신 = External            │
└─────────────────────────────────────────────────────────────────────┘
```

**origin의 의미**:
| Origin | 의미 | propagate 여부 |
|--------|------|----------------|
| `Local` | RPC로 내가 제출 | 설정에 따라 다름 |
| `External` | P2P로 수신 | ✅ True (다시 전파) |
| `Private` | 내부 생성 | ❌ False |

---

## ② TX 검증: EthTransactionValidator

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  crates/transaction-pool/src/validate/eth.rs                                  │
│  EthTransactionValidator<Client, Tx, Evm>                                     │
│                                                                               │
│  ┌─────────────────────────────────────────────────────────────────────────┐  │
│  │  STEP 1: validate_stateless()  [L279]                                   │  │
│  │  (DB 접근 없음, 빠른 거부 필터)                                              │  │
│  │                                                                          │  │
│  │  ✓ TX 타입 지원 확인      (EIP-1559 Type2 = ERC20 transfer에 일반적)        │  │
│  │  ✓ nonce != u64::MAX     (EIP-2681)                                     │  │
│  │  ✓ encoded_length ≤ 128KB  (DOS 방지)                                   │  │
│  │  ✓ gas_limit ≤ block_gas_limit                                          │  │
│  │  ✓ max_priority_fee ≤ max_fee_per_gas                                   │  │
│  │  ✓ chain_id 일치                                                         │  │
│  │  ✓ intrinsic_gas 검사   (21000 + calldata 비용)                          │  │
│  └─────────────────────────────────────────────────────────────────────────┘  │
│                    │ Ok(tx)                                                    │
│                    ▼                                                           │
│  ┌─────────────────────────────────────────────────────────────────────────┐  │
│  │  STEP 2: validate_stateful()  [L540]                                    │  │
│  │  (DB 조회 필요, 발신자 계정 상태 확인)                                        │  │
│  │                                                                          │  │
│  │  client.latest() → StateProvider  ← 최신 블록 상태 DB에서 조회             │  │
│  │                                                                          │  │
│  │  ✓ validate_sender_bytecode()  [L602]                                   │  │
│  │    sender가 EOA인지 확인 (컨트랙트는 TX 발신 불가, EIP-7702 제외)            │  │
│  │                                                                          │  │
│  │  ✓ validate_sender_nonce()  [L637]                                      │  │
│  │    tx.nonce >= account.nonce  (과거 nonce 거부)                          │  │
│  │    tx.nonce == account.nonce → Pending                                  │  │
│  │    tx.nonce >  account.nonce → Queued (nonce gap)                       │  │
│  │                                                                          │  │
│  │  ✓ validate_sender_balance()  [L655]                                    │  │
│  │    account.balance >= tx.gas_limit * max_fee + tx.value                 │  │
│  │    (ERC20 transfer는 value=0, 가스비만 검사)                               │  │
│  │                                                                          │  │
│  │  → TransactionValidationOutcome::Valid { balance, state_nonce, ... }    │  │
│  └─────────────────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────────────────┘
```

> **Java 비유**: `validate_stateless()`는 Bean Validation `@NotNull, @Size`,  
> `validate_stateful()`은 DB 조회가 필요한 `@Service` 레이어 비즈니스 검증.

---

## ③ TX 풀 진입: 서브풀 분류

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  crates/transaction-pool/src/pool/mod.rs                                      │
│  PoolInner::add_transactions()                                                │
│                                                                               │
│                      ValidPoolTransaction<T>                                  │
│                             │                                                 │
│              ┌──────────────┼───────────────────┐                            │
│              ▼              ▼                   ▼                            │
│        nonce 연속?    base_fee 충족?       blob tx?                           │
│         (gap 없음)    (max_fee >= base)                                       │
│              │              │                   │                            │
│         ┌────┴───┐    ┌─────┴────┐         ┌───┴────┐                       │
│         │ Yes   No    │ Yes    No           │  (EIP-4844만)                  │
│         │       │     │        │            └───┬────┘                       │
│         ▼       ▼     ▼        ▼                ▼                            │
│    ┌────────┐ ┌─────┐ ┌────────┐ ┌──────┐ ┌─────────┐                      │
│    │Pending │ │Queue│ │Pending │ │Base  │ │ Blob    │                       │
│    │ Pool   │ │  d  │ │ Pool   │ │ Fee  │ │ Pool    │                       │
│    │(실행   │ │Pool │ │        │ │ Pool │ │(사이드카)│                       │
│    │ 대기)  │ │(nonce│ │       │ │      │ │         │                       │
│    └────────┘ │ gap)│ └────────┘ └──────┘ └─────────┘                      │
│               └─────┘                                                        │
│                                                                               │
│  ★ ERC20 transfer (EIP-1559, nonce 연속, base_fee 충족 가정)                   │
│    → Pending Pool에 진입                                                       │
│                                                                               │
│  pending_transactions_listener() 채널로 hash 브로드캐스트                       │
│    → P2P TransactionsManager가 수신 → 피어들에게 전파                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

**서브풀 상태 전이**:
```
[Queued] ──nonce gap 해소──► [Pending] ──block 빌드 시──► 선택됨
[BaseFee] ──base_fee 하락──► [Pending]
[Pending] ──base_fee 상승──► [BaseFee]
```

---

## ④ 블록 빌딩: Payload Builder (CL이 블록 요청 시)

```
합의 레이어 (Beacon Node)
  │  engine_forkchoiceUpdatedV3(forkchoiceState, payloadAttributes)
  ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│  crates/engine/engine-api/src/engine_api.rs                                   │
│  EngineApiHandler::fork_choice_updated_v3()                                   │
│    └─► payload_builder_handle.new_payload(payload_attributes)                 │
└──────────────────────────────────────────────────────────────────────────────┘
  │
  ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│  crates/payload/builder/src/service.rs                                        │
│  PayloadBuilderService::on_new_payload()                                      │
│    └─► generator.new_payload_job(config)                                      │
│          ↓                                                                    │
│  crates/payload/basic/src/lib.rs                                              │
│  BasicPayloadJobGenerator::new_payload_job()                                  │
│    → BasicPayloadJob 생성 (비동기 Future로 블록 빌드 시작)                       │
└──────────────────────────────────────────────────────────────────────────────┘
  │
  ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│  BasicPayloadJob::poll() — Tokio에 의해 반복 호출                               │
│                                                                               │
│  ┌─────────────────────────────────────────────────────────────────────────┐  │
│  │  1. pool.best_transactions_with_attributes(BestTransactionsAttributes { │  │
│  │       base_fee,                                                          │  │
│  │       blob_fee,                                                          │  │
│  │     })                                                                   │  │
│  │     ↓                                                                    │  │
│  │  crates/transaction-pool/src/traits.rs  [L406]                          │  │
│  │  PendingPool을 fee-priority 순으로 순회하는 Iterator 반환                  │  │
│  │  (CoinbaseTipOrdering: gas_tip = min(max_priority_fee, max_fee-base_fee))│  │
│  └─────────────────────────────────────────────────────────────────────────┘  │
│                    │ Iterator<ValidPoolTransaction>                            │
│                    ▼                                                           │
│  ┌─────────────────────────────────────────────────────────────────────────┐  │
│  │  2. CachedReads로 부모 블록 상태 캐싱                                      │  │
│  │  crates/revm/src/cached.rs                                              │  │
│  │  CachedReads::as_db_mut(parent_state_db)                                │  │
│  │    → 계정 조회 시 캐시 히트 → DB 접근 없음                                   │  │
│  └─────────────────────────────────────────────────────────────────────────┘  │
│                    │                                                           │
│                    ▼                                                           │
│  ┌─────────────────────────────────────────────────────────────────────────┐  │
│  │  3. EVM 실행 (각 트랜잭션마다 반복)                                         │  │
│  │  ConfigureEvm::executor_for_block(&mut state_db, &block)                │  │
│  │    └─► EthBlockExecutor::execute_one(block)                             │  │
│  │         ↓ 내부: 아래 ⑤번 참조                                             │  │
│  └─────────────────────────────────────────────────────────────────────────┘  │
│                    │                                                           │
│                    ▼                                                           │
│  ┌─────────────────────────────────────────────────────────────────────────┐  │
│  │  4. 블록 크기/가스 초과 시 TX 건너뜀                                        │  │
│  │     done → BuiltPayload 반환                                             │  │
│  │     CL이 engine_getPayloadV3() 호출 시 최상의 payload 반환                 │  │
│  └─────────────────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

## ⑤ EVM 실행: ERC20 transfer의 내부 동작

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  ERC20 transfer(address to, uint256 amount) 실행 상세                          │
│                                                                               │
│  Input data (ABI encoded):                                                    │
│  0xa9059cbb                    ← transfer() 함수 셀렉터 (keccak256 앞 4바이트)  │
│  000...000<to_address>         ← to 주소 (32바이트 패딩)                       │
│  000...000<amount>             ← 금액 (32바이트)                               │
│                                                                               │
┌──────────────────────────────────────────────────────────────────────────────┤
│  crates/evm-eth-block-executor/src/execute.rs (또는 evm-ethereum)            │
│  EthBlockExecutor::execute_one(block)                                         │
│                                                                               │
│  for tx in block.transactions():                                              │
│    ┌──────────────────────────────────────────────────────────────────────┐   │
│    │  STEP A: 가스비 선납 (Pre-execution)                                   │   │
│    │  sender.balance -= gas_limit * max_fee_per_gas                        │   │
│    │  sender.nonce += 1                                                    │   │
│    └──────────────────────────────────────────────────────────────────────┘   │
│                │                                                               │
│                ▼                                                               │
│    ┌──────────────────────────────────────────────────────────────────────┐   │
│    │  STEP B: EVM 실행 (revm 라이브러리)                                    │   │
│    │                                                                      │   │
│    │  21,000 가스 (intrinsic) 차감                                         │   │
│    │  calldata 비용 차감 (non-zero byte: 16gas, zero: 4gas)                │   │
│    │                                                                      │   │
│    │  CALL to ERC20 contract address:                                     │   │
│    │    → EVM이 ERC20 컨트랙트 바이트코드 로드                               │   │
│    │       (DB에서 code_hash로 조회, CachedReads에 캐싱)                    │   │
│    │                                                                      │   │
│    │  ERC20 bytecode 실행:                                                 │   │
│    │    CALLDATALOAD  ← function selector 읽기                             │   │
│    │    JUMPI         ← transfer() 분기로 이동                              │   │
│    │                                                                      │   │
│    │    [잔액 확인]                                                         │   │
│    │    SLOAD slot(keccak256(sender, 0))   ← sender 잔액                  │   │
│    │      (State.storage() 호출 → CachedReads or DB)                      │   │
│    │    SUB                                ← 잔액 차감                     │   │
│    │    SSTORE slot(sender)               ← sender 잔액 갱신               │   │
│    │                                                                      │   │
│    │    SLOAD slot(keccak256(to, 0))       ← to 잔액                      │   │
│    │    ADD                                ← 잔액 추가                     │   │
│    │    SSTORE slot(to)                    ← to 잔액 갱신                  │   │
│    │                                                                      │   │
│    │    [이벤트 발행]                                                        │   │
│    │    LOG3 (Transfer event)                                             │   │
│    │      topic0: keccak256("Transfer(address,address,uint256)")          │   │
│    │      topic1: from_address                                            │   │
│    │      topic2: to_address                                              │   │
│    │      data:   amount                                                  │   │
│    │                                                                      │   │
│    │    RETURN 1 (success)                                                │   │
│    └──────────────────────────────────────────────────────────────────────┘   │
│                │                                                               │
│                ▼                                                               │
│    ┌──────────────────────────────────────────────────────────────────────┐   │
│    │  STEP C: 가스비 정산 (Post-execution)                                  │   │
│    │  gas_used 계산                                                        │   │
│    │  남은 가스 환불: sender.balance += (gas_limit - gas_used) * gas_price  │   │
│    │  coinbase(블록 제안자) 수수료 지급:                                      │   │
│    │    coinbase.balance += gas_used * effective_tip                       │   │
│    └──────────────────────────────────────────────────────────────────────┘   │
│                │                                                               │
│                ▼                                                               │
│    ┌──────────────────────────────────────────────────────────────────────┐   │
│    │  STEP D: Receipt 생성                                                 │   │
│    │  Receipt {                                                           │   │
│    │    status: true (success),                                           │   │
│    │    cumulative_gas_used: N,                                           │   │
│    │    logs: [Transfer(from, to, amount)],   ← LOG3 결과                 │   │
│    │    bloom: BloomFilter(logs),                                         │   │
│    │  }                                                                   │   │
│    └──────────────────────────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

## ⑥ State 변경 커밋 및 DB 기록

### 블록 빌드 완료 후 (자체 블록 빌드 경로)

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  revm::db::State (BundleState)                                                │
│                                                                               │
│  모든 TX 실행 후 변경사항이 BundleState에 누적:                                   │
│  {                                                                            │
│    sender_address: { balance: new_balance, nonce: new_nonce },               │
│    erc20_contract: { storage: { slot_sender: new_bal, slot_to: new_bal } },  │
│    coinbase_address: { balance: += tips },                                    │
│  }                                                                            │
│                                                                               │
│  executor.into_state().take_bundle() → BundleState                           │
│                                                                               │
│  ExecutionOutcome { bundle_state, receipts, first_block }                     │
└──────────────────────────────────────────────────────────────────────────────┘
  │
  ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│  DB 기록 (Execution Stage 또는 Payload 확정 후)                                │
│                                                                               │
│  provider_rw.write_state_changes(bundle_state)                               │
│    ├─ Tables::PlainAccountState    : nonce, balance 기록                       │
│    ├─ Tables::PlainStorageState    : ERC20 storage slot 기록                  │
│    ├─ Tables::Bytecodes            : 컨트랙트 코드 (변경 없으면 skip)             │
│    └─ Tables::BlockBodyIndices     : 블록 내 TX 범위 기록                       │
│                                                                               │
│  provider_rw.write_receipts(block_num, receipts)                              │
│    └─ Tables::Receipts (또는 Static File)                                     │
│                                                                               │
│  provider_rw.commit()  ← MDBX 트랜잭션 커밋 (Java의 @Transactional commit)   │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

## ⑦ Static File 이전: 영구 보관

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  crates/static-file/static-file/src/producer.rs                               │
│  StaticFileProducer::copy_to_static_files()                                   │
│                                                                               │
│  확정된(finalized) 블록 데이터를 MDBX에서 Static File로 이전:                    │
│                                                                               │
│  Headers   → headers/  (ZStd 압축, sequential read 최적화)                    │
│  Transactions → transactions/  (TX 원본 데이터)                               │
│  Receipts  → receipts/   ← ERC20 Transfer 이벤트 로그 포함                    │
│                                                                               │
│  이전 후 MDBX에서 해당 데이터 삭제 (pruner 실행)                                 │
│  → MDBX 크기 감소, compaction 부담 감소                                         │
└──────────────────────────────────────────────────────────────────────────────┘
  │
  ▼
[최종 상태]
  ├─ MDBX: 최근 블록의 Account/Storage 상태 (random access 최적화)
  ├─ Static Files (compressed): 모든 블록 Header/TX/Receipt (sequential read)
  └─ 메모리: revm State 캐시 (다음 블록 빌드를 위한 CachedReads)
```

---

## 동기화 경로: 다른 노드의 블록 수신 시

> 위 ①~⑦은 "내가 블록을 만드는" 경우. 다른 노드가 만든 블록을 동기화할 때는 Staged Sync 경로.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                        Staged Sync 경로 (동기화)                               │
│                                                                               │
│  P2P 수신 (ETH/68 프로토콜)                                                    │
│    Headers 응답 → HeaderStage::execute()                                      │
│    Bodies  응답 → BodyStage::execute()                                        │
│         │                                                                    │
│         ▼                                                                    │
│  ┌──────────────────────────────────────────────────────────────┐            │
│  │  SenderRecoveryStage::execute()                              │            │
│  │  crates/stages/stages/src/stages/sender_recovery.rs  [L86]  │            │
│  │                                                              │            │
│  │  블록 내 모든 TX의 발신자 주소 복구:                              │            │
│  │  setup_range_recovery() → OS Thread 스폰                     │            │
│  │    rayon::spawn(|| recover_sender(tx, rlp_buf)) × N         │            │
│  │    (100개 TX씩 rayon 워커에 병렬 처리)                          │            │
│  │                                                              │            │
│  │  recover_sender():                                           │            │
│  │    tx.recover_unchecked_with_buf(rlp_buf)                   │            │
│  │    → secp256k1 서명 복구 → sender Address                    │            │
│  │    → TransactionSenders 테이블에 저장                          │            │
│  └──────────────────────────────────────────────────────────────┘            │
│         │                                                                    │
│         ▼                                                                    │
│  ┌──────────────────────────────────────────────────────────────┐            │
│  │  ExecutionStage::execute()                                   │            │
│  │  crates/stages/stages/src/stages/execution.rs  [L288]       │            │
│  │                                                              │            │
│  │  for block_number in start..=max:                            │            │
│  │    block = provider.recovered_block(block_number)            │            │
│  │    ↓                                                         │            │
│  │    StateProviderDatabase(LatestStateProviderRef::new(provider))           │
│  │    executor = evm_config.batch_executor(db)  [L300]         │            │
│  │    ↓                                                         │            │
│  │    result = executor.execute_one(&block)  [L348]            │            │
│  │      ← ⑤번 EVM 실행과 동일 (ERC20 SSTORE, LOG 포함)          │            │
│  │    ↓                                                         │            │
│  │    consensus.validate_block_post_execution(&block, &result)  │            │
│  │      ← Receipt 루트, 가스 합계 검증                             │            │
│  │    ↓                                                         │            │
│  │    thresholds.is_end_of_batch() → 임계값 도달 시 break        │            │
│  │      (가스 합계, 메모리 크기, 경과시간 기준)                       │            │
│  └──────────────────────────────────────────────────────────────┘            │
│         │                                                                    │
│         ▼                                                                    │
│  ExecutionOutcome::from_blocks(start_block, bundle_state, receipts)          │
│  → DB 기록 (⑥번과 동일)                                                       │
│  → ExEx 알림 (ExExecutionExtension이 있으면 Chain 이벤트 전송)                  │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

## 전체 소스코드 파일 인덱스

| 단계 | 파일 | 핵심 함수 | 역할 |
|------|------|-----------|------|
| **① TX 진입 (RPC)** | `rpc/rpc-eth-api/src/core.rs` | `send_raw_transaction()` [L846] | HTTP → Pool |
| **① TX 진입 (P2P)** | `net/network/src/transactions/mod.rs` | `on_network_tx_event()` | ETH/68 → Pool |
| **② 검증 (무상태)** | `transaction-pool/src/validate/eth.rs` | `validate_stateless()` [L279] | 타입/크기/가스 검사 |
| **② 검증 (유상태)** | `transaction-pool/src/validate/eth.rs` | `validate_stateful()` [L540] | nonce/잔액 검사 |
| **③ 풀 분류** | `transaction-pool/src/pool/mod.rs` | `PoolInner::add_transactions()` | 서브풀 라우팅 |
| **③ Pending 관리** | `transaction-pool/src/pool/pending.rs` | `PendingPool::add()` | fee 정렬 삽입 |
| **④ 블록 빌드** | `payload/basic/src/lib.rs` | `BasicPayloadJob::poll()` | TX 선택 & 실행 |
| **④ TX 선택** | `transaction-pool/src/traits.rs` | `best_transactions_with_attributes()` [L406] | fee 순 Iterator |
| **④ 캐싱** | `revm/src/cached.rs` | `CachedReads::as_db_mut()` [L50] | DB 읽기 캐싱 |
| **⑤ EVM 설정** | `evm/evm/src/lib.rs` | `ConfigureEvm::executor_for_block()` | EVM 인스턴스 생성 |
| **⑤ 블록 실행** | `evm/evm/src/execute.rs` | `BlockExecutor::execute_one()` | TX 루프 실행 |
| **⑥ DB 기록** | `stages/stages/src/stages/execution.rs` | `ExecutionStage::execute()` [L288] | State → MDBX |
| **⑦ Static File** | `static-file/static-file/src/producer.rs` | `copy_to_static_files()` | MDBX → 압축파일 |
| **동기화 서명복구** | `stages/stages/src/stages/sender_recovery.rs` | `recover_range()` [L223] | rayon 병렬 복구 |
| **동기화 실행** | `stages/stages/src/stages/execution.rs` | `ExecutionStage::execute()` [L288] | 블록 재실행 |

---

## 데이터 변환 추적: bytes → 상태변경

```
[클라이언트가 보낸 것]
0x02f8...  (RLP encoded EIP-1559 TX)

                    │ decode
                    ▼
TransactionSigned { nonce:5, to:ERC20, data:0xa9059cbb..., sig:{v,r,s} }

                    │ recover_signer()
                    ▼
Recovered<TransactionSigned> { tx, sender: 0xAlice... }

                    │ validate → add_to_pool
                    ▼
ValidPoolTransaction { tx, subpool: Pending, priority: fee_tip }

                    │ executor.execute_one()
                    ▼
ExecutionResult {
  gas_used: 46,000,    ← 21000 + ERC20 SSTORE×2 + LOG3
  logs: [Transfer(Alice, Bob, 100e18)],
  output: Bytes([1])   ← return true
}

                    │ take_bundle()
                    ▼
BundleState {
  Alice:    { balance: -gas_cost,   nonce: 6 },
  Bob:      { (no change, only storage) },
  ERC20:    { storage: { slot_Alice: -100e18, slot_Bob: +100e18 } },
  Coinbase: { balance: +gas_tip },
}

                    │ write_to_db + commit
                    ▼
MDBX:
  PlainAccountState[Alice] = { nonce: 6, balance: new_balance }
  PlainStorageState[ERC20][slot_Alice] = new_value
  PlainStorageState[ERC20][slot_Bob]   = new_value
  Receipts[tx_hash] = { status:1, gas_used:46000, logs:[...] }
```

---

## 핵심 개념 요약 (Java 개발자 관점)

| reth 개념 | Java 비유 |
|-----------|-----------|
| `TransactionPool` | `Queue<Transaction>` + 우선순위 정렬 |
| `EthTransactionValidator` | `@Service` 레이어의 비즈니스 검증 로직 |
| `validate_stateless()` | Bean Validation (`@NotNull`, `@Size`) |
| `validate_stateful()` | DB 조회 기반 검증 (`service.findById()`) |
| `ExecInput` | Batch Step의 `StepExecution` (어디까지 처리했는지) |
| `Stage::execute()` | Spring Batch `ItemProcessor.process()` |
| `provider_rw.commit()` | `@Transactional` Spring 트랜잭션 커밋 |
| `BundleState` | 트랜잭션 내 축적된 변경사항 (UoW 패턴) |
| `CachedReads` | Spring Cache `@Cacheable` (1차 캐시) |
| `rayon::spawn()` | `CompletableFuture.supplyAsync()` (워커 풀 제출) |
| `StaticFileProducer` | DB archiving + 압축 배치 작업 |

---

*다음: 12_ExecInput_block_range.rs — Pipeline Stage가 블록 범위를 계산하는 방법 (주석 소스코드)*
