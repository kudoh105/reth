# EVM 실행 흐름 - 소스코드 읽기 가이드

> **이 폴더의 파일들은 원본 소스코드에 한국어 주석을 상세히 달아놓은 학습용 파일입니다.**
> 원본 소스코드를 수정한 것이 아니라, 학습 목적의 주석이 달린 사본입니다.

---

## 📚 읽는 순서

### Phase 1: 진입점 이해 (가장 먼저 읽어야 함)
```
01_ConfigureEvm_trait.rs
    → "EVM 전체 설정의 최상위 인터페이스"
    → Java의 @Configuration + ApplicationService 역할
```

### Phase 2: 블록 실행 기계장치 이해
```
02_BlockExecutor_trait.rs
    → "블록 하나를 실행하는 핵심 인터페이스"
    → 3단계: 전처리 → 트랜잭션실행 → 후처리

03_Evm_trait.rs
    → "개별 트랜잭션 실행 인터페이스"
    → 가장 낮은 레벨, revm을 직접 호출
```

### Phase 3: 이더리움 구체 구현체 이해
```
04_EthEvmConfig.rs
    → ConfigureEvm을 이더리움에 맞게 구현
    → "이것이 실제 사용되는 구현체다"

05_EthEvm_and_EthEvmFactory.rs
    → Evm + EvmFactory를 이더리움에 맞게 구현
    → revm을 감싸는 래퍼

06_EthBlockExecutor.rs
    → BlockExecutor를 이더리움에 맞게 구현
    → 가장 핵심! 실제 실행 로직이 여기 있음

07_EthBlockAssembler.rs
    → 실행 결과로 완성된 블록 조립
```

### Phase 4: 보조 구성요소
```
08_SystemCaller.rs
    → 시스템 콜 (EIP-2935, EIP-4788 등) 담당

09_BasicBlockExecutor.rs
    → reth 수준의 래퍼 (Executor trait 구현)

10_StateManagement.rs
    → 상태 관리 (StateProviderDatabase, CachedReads)
```

---

## 🗺️ 전체 호출 지도 (한 눈에 보기)

```
[진입: Staged Sync / Payload Builder]
    │
    ▼
EthEvmConfig::executor(db)                          # 04번 파일
    │  = BasicBlockExecutor::new(self, db)
    ▼
BasicBlockExecutor::execute(&block)                 # 09번 파일
    │
    ├─ executor_for_block(&mut db, &block)
    │   ├─ evm_env(header)          → EvmEnv 구성
    │   ├─ evm_with_env(db, env)    → EthEvmFactory::create_evm()  # 05번 파일
    │   ├─ context_for_block(block) → EthBlockExecutionCtx
    │   └─ create_executor(evm,ctx) → EthBlockExecutor::new()       # 06번 파일
    │
    └─ executor.execute_block(transactions)          # 02번 파일
        │
        ├─ 1단계: apply_pre_execution_changes()     # 08번 파일 (시스템 콜)
        │   ├─ EIP-2935 blockhashes 콜
        │   └─ EIP-4788 beacon root 콜
        │
        ├─ 2단계: for tx: execute_transaction(tx)  # 06번 파일
        │   ├─ evm.transact(tx_env)                # 05번 파일 → revm 호출
        │   ├─ receipt_builder.build_receipt()
        │   └─ evm.db_mut().commit(state)
        │
        └─ 3단계: finish()                         # 06번 파일
            ├─ 블록 보상 계산
            ├─ Withdrawal 처리
            └─ EIP-6110/7002/7251 처리
```

---

## 💡 Rust 문법 빠른 참조 (처음 읽을 때 헷갈리는 것들)

```rust
// 1. impl SomeTrait for SomeStruct
//    = "SomeStruct가 SomeTrait 인터페이스를 구현함"
//    Java: class SomeStruct implements SomeTrait { ... }
impl BlockExecutor for EthBlockExecutor<...> { ... }

// 2. <T: SomeTrait>  또는  where T: SomeTrait
//    = "T는 SomeTrait를 구현하는 타입이어야 한다"
//    Java: <T extends SomeTrait>
fn execute<T: BlockExecutor>(executor: T) { ... }

// 3. &self vs &mut self vs self
//    &self      = 읽기 전용 참조 (Java의 일반 메서드)
//    &mut self  = 쓰기 가능 참조 (Java의 setXxx 메서드)
//    self       = 소유권 이전 (이 후 변수 사용 불가, Java에서는 없는 개념)

// 4. Result<T, E>
//    = "성공하면 T를 반환하고, 실패하면 E를 반환"
//    Java: try { return T; } catch (E e) { throw e; }

// 5. ? 연산자
//    = "Result가 Err이면 즉시 현재 함수에서 에러 반환"
//    Java: 체크 예외를 throws로 선언하는 것과 비슷
let result = some_function()?;  // Err면 즉시 return Err(...)

// 6. Option<T>
//    = "값이 있을 수도 없을 수도 있음"
//    Java: Optional<T>
let value: Option<u64> = Some(42);  // 값 있음
let nothing: Option<u64> = None;    // 값 없음

// 7. Arc<T>
//    = "여러 스레드가 공유하는 참조 카운팅 포인터"
//    Java: AtomicReference 또는 공유 bean 참조

// 8. 라이프타임 'a
//    &'a str = "이 참조는 'a 라이프타임 동안 유효하다"
//    Java에서는 없는 개념 (GC가 처리하므로)
//    그냥 "참조의 유효 범위를 컴파일러가 체크하는 것"으로 이해

// 9. where 절
//    함수/impl 블록 아래 긴 타입 제약을 별도로 작성하는 문법
impl<C> SomeTrait for SomeStruct<C>
where
    C: AnotherTrait + Clone + Send,  // C는 이 세 trait를 모두 구현해야 함
{
    // ...
}

// 10. type 별칭
//     복잡한 타입에 이름을 붙임
//     Java의 typedef 또는 제네릭 특화 클래스와 비슷
type EvmEnvFor<C> = EvmEnv<...>;  // 긴 타입을 짧게 줄임
```
