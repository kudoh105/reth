// ============================================================================
// 파일: alloy-evm-0.27.2/src/block/mod.rs (핵심 부분)
// 역할: 블록 하나를 실행하는 핵심 인터페이스 정의
//
// [Java 대응]
// BlockExecutor ≈ Service Interface (블록 실행 비즈니스 로직)
// BlockExecutorFactory ≈ Factory Interface (BlockExecutor를 생성하는 팩토리)
//
// [3단계 실행 프로세스]
// 1. apply_pre_execution_changes() - 전처리 (시스템 콜)
// 2. execute_transaction() - 트랜잭션 실행 (여러 번 반복)
// 3. finish() - 후처리 (블록 보상 등)
// ============================================================================

/// 블록 하나를 실행하는 핵심 trait
///
/// [Java 대응] interface BlockExecutor { ... }
pub trait BlockExecutor {
    // ─────────────────────────────────────────────────────
    // 연관 타입 정의
    // ─────────────────────────────────────────────────────

    /// 블록 내 트랜잭션의 합의 레이어 타입
    ///
    /// 예: EthereumTxEnvelope (Legacy, EIP-2930, EIP-1559 등 모두 포함)
    ///
    /// 트랜잭션 흐름:
    ///   Self::Transaction (합의 tx)
    ///   → Recovered<Self::Transaction> (서명에서 sender 복구 후)
    ///   → TxEnv (revm용 환경 타입으로 변환)
    ///   → EVM 실행 → ExecutionResult
    ///   → ExecutionResult + Transaction → Receipt(영수증) 생성
    type Transaction;

    /// 트랜잭션 실행 결과로 만들어지는 영수증 타입
    /// 예: reth_ethereum_primitives::Receipt
    type Receipt;

    /// 이 블록 실행기 내부에서 사용하는 EVM 인스턴스 타입
    /// EVM이 직접 개별 트랜잭션을 실행함
    type Evm: Evm<Tx: FromRecoveredTx<Self::Transaction> + FromTxWithEncoded<Self::Transaction>>;

    /// 단일 트랜잭션 실행 결과 타입
    /// ResultAndState<HaltReason> 같은 것을 래핑한 타입
    type Result: TxResult<HaltReason = <Self::Evm as Evm>::HaltReason>;

    // ─────────────────────────────────────────────────────
    // 핵심 메서드 (구현 필수)
    // ─────────────────────────────────────────────────────

    /// [단계 1] 블록 트랜잭션 실행 전 시스템 수준 변경사항 적용
    ///
    /// 내용:
    ///   - EIP-2935: 블록해시 히스토리 컨트랙트 호출 (Prague 이후)
    ///   - EIP-4788: 비콘 블록 루트 컨트랙트 호출 (Cancun 이후)
    ///   - DAO fork: 특수 상태 처리 (이더리움 역사적 사건)
    ///   - Spurious Dragon: state_clear_flag 설정
    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError>;

    /// [단계 2-A] 트랜잭션을 실행하되 상태에 커밋하지 않음
    ///
    /// 반환값: 실행 결과 + 변경될 상태 정보 (아직 커밋 안 됨)
    ///
    /// 용도:
    ///   - 트랜잭션 시뮬레이션 (커밋 없이 결과만 확인)
    ///   - 커밋 조건을 커스터마이징 하고 싶을 때
    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>, // ExecutableTx = 실행 가능한 트랜잭션 (TxEnv로 변환 가능)
    ) -> Result<Self::Result, BlockExecutionError>;

    /// [단계 2-B] 이전에 실행된 트랜잭션 결과를 상태에 커밋
    ///
    /// 이 메서드에서:
    ///   - 상태 변경을 DB에 커밋 (캐시 레벨)
    ///   - 가스 사용량 누적
    ///   - 영수증 생성 및 저장
    ///
    /// 반환값: 이 트랜잭션이 사용한 가스량
    fn commit_transaction(&mut self, output: Self::Result) -> Result<u64, BlockExecutionError>;

    /// [단계 3] 블록 후처리 + 실행 결과 반환
    ///
    /// 내용:
    ///   - Prague 이후: EIP-6110(디포짓), EIP-7002(출금요청), EIP-7251(통합요청) 처리
    ///   - 블록 보상 계산 (PoW 시절)
    ///   - Withdrawal 처리 (Shanghai 이후)
    ///   - DAO fork 특수 처리
    ///
    /// 반환: (EVM 인스턴스, 블록 실행 결과)
    ///   - EVM을 반환하는 이유: 내부 DB(State)를 꺼내기 위해
    fn finish(
        self,
    ) -> Result<(Self::Evm, BlockExecutionResult<Self::Receipt>), BlockExecutionError>;

    // ─────────────────────────────────────────────────────
    // 기본 구현이 있는 편의 메서드들
    // ─────────────────────────────────────────────────────

    /// [가장 일반적] 트랜잭션 실행 + 즉시 커밋
    ///
    /// 내부: execute_transaction_with_result_closure(tx, |_| ())
    ///
    /// 반환값: 가스 사용량
    fn execute_transaction(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<u64, BlockExecutionError> {
        self.execute_transaction_with_result_closure(tx, |_| ())
    }

    /// 트랜잭션 실행 + 클로저로 결과 처리 + 커밋
    ///
    /// f: 실행 결과를 받아서 처리하는 클로저 (콜백 함수)
    /// 예: 실행 결과를 로깅하거나 메트릭을 수집할 때 사용
    ///
    /// [Java 대응] 거의 없음. 람다를 콜백으로 받는 패턴
    fn execute_transaction_with_result_closure(
        &mut self,
        tx: impl ExecutableTx<Self>,
        f: impl FnOnce(&ExecutionResult<<Self::Evm as Evm>::HaltReason>),
    ) -> Result<u64, BlockExecutionError> {
        // CommitChanges::Yes = "실행 결과를 반드시 커밋하라"
        self.execute_transaction_with_commit_condition(tx, |res| {
            f(res);
            CommitChanges::Yes
        })
        .map(Option::unwrap_or_default)
    }

    /// 가장 세밀한 제어: 실행 후 커밋 여부를 클로저로 결정
    ///
    /// f가 CommitChanges::No를 반환하면 상태 변경이 취소됨 (롤백)
    /// f가 CommitChanges::Yes를 반환하면 상태 변경 확정
    ///
    /// 사용 예: 가스비가 일정량 이상인 트랜잭션만 포함하는 커스텀 블록 빌더
    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutableTx<Self>,
        f: impl FnOnce(&ExecutionResult<<Self::Evm as Evm>::HaltReason>) -> CommitChanges,
    ) -> Result<Option<u64>, BlockExecutionError> {
        // 1. 실행 (커밋 없음)
        let output = self.execute_transaction_without_commit(tx)?;

        // 2. f 클로저를 통해 커밋 여부 결정
        if !f(&output.result().result).should_commit() {
            return Ok(None); // 커밋 안 함 → 상태 변경 없음
        }

        // 3. 커밋
        let gas_used = self.commit_transaction(output)?;
        Ok(Some(gas_used))
    }

    /// [가장 편리한 메서드] 블록 전체를 실행하는 통합 메서드
    ///
    /// 내부 흐름:
    ///   1. apply_pre_execution_changes()  → 전처리
    ///   2. for tx in transactions: execute_transaction(tx) → 각 트랜잭션 실행
    ///   3. apply_post_execution_changes() → 후처리
    ///
    /// BasicBlockExecutor::execute_one()에서 이 메서드를 호출함
    fn execute_block(
        mut self,
        transactions: impl IntoIterator<Item = impl ExecutableTx<Self>>,
        //             ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
        //             Rust의 제네릭 이터레이터 (Java: Iterable<ExecutableTx>)
    ) -> Result<BlockExecutionResult<Self::Receipt>, BlockExecutionError>
    where
        Self: Sized, // "self를 이동(move)할 수 있어야 함" → finish()에서 self를 소비하기 때문
    {
        self.apply_pre_execution_changes()?; // ? = 에러면 즉시 반환

        for tx in transactions {
            self.execute_transaction(tx)?; // 각 트랜잭션 실행
        }

        // apply_post_execution_changes() = finish()의 축약 버전
        // finish()를 호출하고 EVM은 버리고 결과만 가져옴
        self.apply_post_execution_changes()
    }

    /// EVM에 대한 mutable 참조를 반환
    fn evm_mut(&mut self) -> &mut Self::Evm;

    /// EVM에 대한 immutable 참조를 반환
    fn evm(&self) -> &Self::Evm;

    /// 지금까지 실행한 모든 영수증의 슬라이스를 반환
    fn receipts(&self) -> &[Self::Receipt];

    /// 상태 변경 훅 설정 (모니터링/디버깅용)
    /// 각 상태 변경이 일어날 때마다 훅이 호출됨
    fn set_state_hook(&mut self, hook: Option<Box<dyn OnStateHook>>);
}

// ============================================================================
// BlockExecutionResult - 블록 실행 결과 타입
// ============================================================================

/// 블록 하나를 실행한 결과
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockExecutionResult<T> {
    /// 블록 내 모든 트랜잭션의 영수증 목록 (순서대로)
    pub receipts: Vec<T>,

    /// EIP-7685: 블록 내 모든 합의 레이어 요청 (Prague 이후)
    /// 디포짓 요청, 출금 요청, 통합 요청 등이 여기에 담김
    pub requests: Requests,

    /// 블록 전체의 가스 사용량 (wei 단위 아님, 가스 단위)
    pub gas_used: u64,

    /// 블록 내 blob 트랜잭션들의 총 blob 가스 사용량 (Cancun 이후)
    pub blob_gas_used: u64,
}

// ============================================================================
// CommitChanges - 트랜잭션 커밋 여부 결정 타입
// ============================================================================

/// execute_transaction_with_commit_condition에서 사용하는 열거형
/// Java enum과 거의 동일한 개념
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use] // 반환값을 무시하면 경고
pub enum CommitChanges {
    /// 상태 변경을 커밋함
    Yes,
    /// 상태 변경을 커밋하지 않음 (롤백 효과)
    No,
}

// ============================================================================
// ExecutableTx - 실행 가능한 트랜잭션 타입 제약
//
// ExecutableTx<E>를 구현하는 타입들:
//   - Recovered<Transaction>         : 서명에서 sender를 복구한 트랜잭션
//   - &Recovered<Transaction>        : 위의 참조형
//   - WithEncoded<Recovered<Tx>>     : 인코딩된 bytes와 함께 있는 트랜잭션
// ============================================================================

/// BlockExecutor에서 실행 가능한 트랜잭션임을 나타내는 trait
/// 이 trait을 구현하면 execute_transaction()에 넘길 수 있음
pub trait ExecutableTx<E: BlockExecutor + ?Sized>:
    ExecutableTxParts<<E::Evm as Evm>::Tx, E::Transaction>
{
}

// ============================================================================
// BlockExecutorFactory - BlockExecutor 생성 팩토리
//
// [Java 대응]
// interface BlockExecutorFactory {
//     BlockExecutor createExecutor(Evm evm, ExecutionCtx ctx);
// }
// ============================================================================

#[auto_impl::auto_impl(Arc)]
pub trait BlockExecutorFactory: 'static {
    /// 내부에서 사용하는 EVM 팩토리 타입
    /// EVM 인스턴스를 만드는 역할
    type EvmFactory: EvmFactory;

    /// 블록 실행에 필요한 추가 컨텍스트 타입
    ///
    /// EVM 자체는 트랜잭션 레벨 정보(sender, value 등)와
    /// 블록 레벨 기본 정보(번호, 타임스탬프, basefee)를 갖고 있으나,
    /// ExecutionCtx는 그 외에 필요한 것들을 담음:
    ///
    /// EthBlockExecutionCtx에는:
    ///   - parent_hash: EIP-2935 시스템 콜에 필요한 부모 블록 해시
    ///   - parent_beacon_block_root: EIP-4788에 필요한 비콘 루트
    ///   - ommers: Uncle 블록들 (PoW 보상 계산에 필요)
    ///   - withdrawals: Validator 출금 목록
    type ExecutionCtx<'a>: Clone;

    /// 이 팩토리가 만드는 BlockExecutor가 처리하는 트랜잭션 타입
    type Transaction;

    /// 이 팩토리가 만드는 BlockExecutor가 생성하는 영수증 타입
    type Receipt;

    /// 내부 EvmFactory에 대한 참조 반환
    fn evm_factory(&self) -> &Self::EvmFactory;

    /// [핵심] EVM 인스턴스 + 실행 컨텍스트 → BlockExecutor 생성
    ///
    /// 파라미터:
    ///   evm: 이미 블록 환경이 구성된 EVM 인스턴스 (= DB와 BlockEnv가 세팅된 상태)
    ///   ctx: 블록 실행에 필요한 추가 정보 (parent_hash, withdrawals 등)
    ///
    /// EthBlockExecutorFactory::create_executor()에서:
    ///   → EthBlockExecutor::new(evm, ctx, &spec, &receipt_builder)
    fn create_executor<'a, DB, I>(
        &'a self,
        evm: <Self::EvmFactory as EvmFactory>::Evm<&'a mut State<DB>, I>,
        ctx: Self::ExecutionCtx<'a>,
    ) -> impl BlockExecutorFor<'a, Self, DB, I>
    where
        DB: Database + 'a,
        I: Inspector<<Self::EvmFactory as EvmFactory>::Context<&'a mut State<DB>>> + 'a;
}
