// ============================================================================
// 파일: reth/crates/evm/evm/src/lib.rs (핵심 부분)
// 역할: EVM 전체 설정의 최상위 인터페이스 (ConfigureEvm trait)
//
// [Java 대응]
// ConfigureEvm ≈ @Configuration 클래스 + ApplicationService
// 이 trait을 구현한 EthEvmConfig가 실제로 주입되어 사용됨
//
// [아키텍처에서의 위치]
// ConfigureEvm (최상위)
//   └─ BlockExecutorFactory (블록 실행기 팩토리)
//       └─ EvmFactory (EVM 인스턴스 팩토리)
//           └─ Evm (개별 트랜잭션 실행)
// ============================================================================

// "auto_impl"은 &T나 Arc<T> 타입에 대해 trait 구현을 자동으로 생성해주는 매크로
// Java로 치면 Decorator 패턴을 자동으로 만들어주는 것
#[auto_impl::auto_impl(&, Arc)]
pub trait ConfigureEvm: Clone + Debug + Send + Sync + Unpin {
    // ─────────────────────────────────────────────────────
    // 연관 타입 정의 (Associated Types)
    // Java의 제네릭과 비슷하지만, 각 구현체마다 한 번만 정해짐
    // ─────────────────────────────────────────────────────

    /// 이 EVM이 다루는 기본 타입들의 묶음 (Java의 type-parameter처럼 생각)
    /// 예: EthPrimitives = { Block = EthBlock, Tx = TransactionSigned, Receipt = Receipt }
    type Primitives: NodePrimitives;

    /// evm_env(), next_evm_env() 같은 메서드가 실패할 때 반환하는 에러 타입
    /// EthEvmConfig는 Infallible(실패 불가)을 사용 → 항상 성공
    type Error: Error + Send + Sync + 'static;

    /// 다음 블록을 만들 때 필요한 추가 정보 (CL에서 전달받는 값들)
    /// EthEvmConfig에서는 NextBlockEnvAttributes { timestamp, fee_recipient, prev_randao, ... }
    type NextBlockEnvCtx: Debug + Clone;

    /// 블록 실행기(BlockExecutor)를 만드는 팩토리 타입
    /// EthEvmConfig에서는 EthBlockExecutorFactory<RethReceiptBuilder, Arc<ChainSpec>, EthEvmFactory>
    type BlockExecutorFactory: for<'a> BlockExecutorFactory<
        Transaction = TxTy<Self::Primitives>,  // 트랜잭션 타입
        Receipt = ReceiptTy<Self::Primitives>, // 영수증 타입
        ExecutionCtx<'a>: Debug + Send,        // 실행 컨텍스트 타입
        EvmFactory: EvmFactory<
            Tx: TransactionEnv
                    + FromRecoveredTx<TxTy<Self::Primitives>>
                    // 서명 복구된 tx로부터 변환 가능
                    + FromTxWithEncoded<TxTy<Self::Primitives>>, // 인코딩된 tx로부터 변환 가능
            Precompiles = PrecompilesMap, // 프리컴파일 맵
            Spec: Into<SpecId>,           // 하드포크 스펙 ID로 변환 가능
        >,
    >;

    /// 블록 조립기(BlockAssembler) 타입
    /// 실행 결과를 받아서 완성된 Block을 만듦
    /// EthEvmConfig에서는 EthBlockAssembler<ChainSpec>
    type BlockAssembler: BlockAssembler<
        Self::BlockExecutorFactory,
        Block = BlockTy<Self::Primitives>,
    >;

    // ─────────────────────────────────────────────────────
    // 핵심 getter 메서드 (구현 필수)
    // ─────────────────────────────────────────────────────

    /// 블록 실행기 팩토리를 반환 (= executor_factory 필드)
    fn block_executor_factory(&self) -> &Self::BlockExecutorFactory;

    /// 블록 조립기를 반환 (= block_assembler 필드)
    fn block_assembler(&self) -> &Self::BlockAssembler;

    /// [핵심] 주어진 블록 헤더로부터 EVM 환경(EvmEnv)을 구성
    ///
    /// EvmEnv는 두 부분으로 구성됨:
    ///   cfg_env: CfgEnv - chain_id, spec(하드포크), 가스 제한 등 설정
    ///   block_env: BlockEnv - 블록번호, 타임스탬프, basefee, beneficiary 등
    ///
    /// 예를 들어 헤더의 timestamp를 보고 어떤 하드포크 스펙을 사용할지 결정
    fn evm_env(&self, header: &HeaderTy<Self::Primitives>) -> Result<EvmEnvFor<Self>, Self::Error>;

    /// [핵심] 아직 존재하지 않는 "다음 블록"의 EVM 환경을 구성
    ///
    /// Payload Building(새 블록 생성)에 사용됨.
    /// 블록이 아직 없으므로 CL(합의 레이어)에서 제공하는 속성값으로 환경을 구성:
    ///   - timestamp: 다음 블록의 타임스탬프 (이걸로 하드포크 결정)
    ///   - prev_randao: PoS의 랜덤 값
    ///   - suggested_fee_recipient: 수수료 수신자 주소
    ///   - gas_limit: 블록 가스 한도
    fn next_evm_env(
        &self,
        parent: &HeaderTy<Self::Primitives>, // 부모 블록 헤더 (이전 블록)
        attributes: &Self::NextBlockEnvCtx,  // CL이 제공하는 다음 블록 속성
    ) -> Result<EvmEnvFor<Self>, Self::Error>;

    /// 기존 블록의 실행을 위한 "실행 컨텍스트" 구성
    ///
    /// EthBlockExecutionCtx {
    ///   parent_hash,           // EIP-2935 시스템 콜에 필요
    ///   parent_beacon_block_root, // EIP-4788 시스템 콜에 필요
    ///   ommers,                // 블록 보상 계산에 필요
    ///   withdrawals,           // Withdrawal 처리에 필요
    ///   extra_data,
    ///   tx_count_hint,         // receipts Vec 사전 할당용
    /// }
    fn context_for_block<'a>(
        &self,
        // 'a는 라이프타임: 반환값이 block 참조보다 오래 살 수 없다는 보장
        // Java에서는 GC가 처리하므로 없는 개념
        block: &'a SealedBlock<BlockTy<Self::Primitives>>,
    ) -> Result<ExecutionCtxFor<'a, Self>, Self::Error>;

    /// 다음 블록(아직 없는 블록)의 실행 컨텍스트 구성 (Payload Building용)
    fn context_for_next_block(
        &self,
        parent: &SealedHeader<HeaderTy<Self::Primitives>>,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<ExecutionCtxFor<'_, Self>, Self::Error>;

    // ─────────────────────────────────────────────────────
    // 기본 구현이 있는 편의 메서드들 (구현 선택적)
    // Java의 default method와 동일한 개념
    // ─────────────────────────────────────────────────────

    /// EvmFactory를 반환 (BlockExecutorFactory 내부의 팩토리 꺼내기)
    fn evm_factory(&self) -> &EvmFactoryFor<Self> {
        self.block_executor_factory().evm_factory()
    }

    /// 주어진 DB와 EvmEnv로 EVM 인스턴스 생성
    /// 내부적으로: evm_factory().create_evm(db, evm_env)
    fn evm_with_env<DB: Database>(&self, db: DB, evm_env: EvmEnvFor<Self>) -> EvmFor<Self, DB> {
        self.evm_factory().create_evm(db, evm_env)
    }

    /// 블록 헤더로부터 EVM 인스턴스를 바로 만드는 편의 메서드
    /// 내부: evm_env(header) + evm_with_env(db, evm_env)
    fn evm_for_block<DB: Database>(
        &self,
        db: DB,
        header: &HeaderTy<Self::Primitives>,
    ) -> Result<EvmFor<Self, DB>, Self::Error> {
        let evm_env = self.evm_env(header)?; // 헤더 → EvmEnv
        Ok(self.evm_with_env(db, evm_env)) // EvmEnv + DB → EVM 인스턴스
    }

    /// Inspector(트레이서)와 함께 EVM 인스턴스 생성
    /// 디버깅, 트랜잭션 트레이싱(eth_traceTransaction 등) 에 사용
    fn evm_with_env_and_inspector<DB, I>(
        &self,
        db: DB,
        evm_env: EvmEnvFor<Self>,
        inspector: I, // 예: TracingInspector for eth_debug_trace*
    ) -> EvmFor<Self, DB, I>
    where
        DB: Database,
        I: InspectorFor<Self, DB>,
    {
        self.evm_factory().create_evm_with_inspector(db, evm_env, inspector)
    }

    /// EVM + 실행컨텍스트 → BlockExecutor 생성
    /// 내부: block_executor_factory().create_executor(evm, ctx)
    fn create_executor<'a, DB, I>(
        &'a self,
        evm: EvmFor<Self, &'a mut State<DB>, I>,
        ctx: <Self::BlockExecutorFactory as BlockExecutorFactory>::ExecutionCtx<'a>,
    ) -> impl BlockExecutorFor<'a, Self::BlockExecutorFactory, DB, I>
    where
        DB: Database,
        I: InspectorFor<Self, &'a mut State<DB>> + 'a,
    {
        self.block_executor_factory().create_executor(evm, ctx)
    }

    /// [가장 자주 쓰이는 메서드] 주어진 블록에 대한 BlockExecutor를 한 번에 생성
    ///
    /// 내부 흐름:
    ///   1. evm_for_block(db, header)  → EVM 인스턴스 생성
    ///   2. context_for_block(block)   → 실행 컨텍스트 생성
    ///   3. create_executor(evm, ctx)  → BlockExecutor 생성
    fn executor_for_block<'a, DB: Database>(
        &'a self,
        db: &'a mut State<DB>,
        block: &'a SealedBlock<<Self::Primitives as NodePrimitives>::Block>,
    ) -> Result<impl BlockExecutorFor<'a, Self::BlockExecutorFactory, DB>, Self::Error> {
        let evm = self.evm_for_block(db, block.header())?; // EVM 만들기
        let ctx = self.context_for_block(block)?; // 컨텍스트 만들기
        Ok(self.create_executor(evm, ctx)) // BlockExecutor 반환
    }

    /// 블록 빌더(BlockBuilder) 생성
    /// Executor와 달리 Assembler도 포함 → 블록 자체를 만들 수 있음
    fn create_block_builder<'a, DB, I>(
        &'a self,
        evm: EvmFor<Self, &'a mut State<DB>, I>,
        parent: &'a SealedHeader<HeaderTy<Self::Primitives>>,
        ctx: <Self::BlockExecutorFactory as BlockExecutorFactory>::ExecutionCtx<'a>,
    ) -> impl BlockBuilder<
        Primitives = Self::Primitives,
        Executor: BlockExecutorFor<'a, Self::BlockExecutorFactory, DB, I>,
    >
    where
        DB: Database,
        I: InspectorFor<Self, &'a mut State<DB>> + 'a,
    {
        // BasicBlockBuilder = executor + ctx + assembler + parent 를 묶는 구조체
        BasicBlockBuilder {
            executor: self.create_executor(evm, ctx.clone()),
            ctx,
            assembler: self.block_assembler(),
            parent,
            transactions: Vec::new(), // 실행된 트랜잭션 수집용
        }
    }

    /// [Payload Building 진입점] 다음 블록을 위한 BlockBuilder 생성
    ///
    /// 이 메서드가 Payload Building의 시작점!
    ///
    /// 흐름:
    ///   1. next_evm_env(parent, &attributes)  → 다음 블록의 EvmEnv 구성
    ///   2. evm_with_env(db, evm_env)          → EVM 인스턴스 생성
    ///   3. context_for_next_block(parent, attributes) → 실행 컨텍스트
    ///   4. create_block_builder(evm, parent, ctx)     → BlockBuilder 반환
    fn builder_for_next_block<'a, DB: Database + 'a>(
        &'a self,
        db: &'a mut State<DB>,
        parent: &'a SealedHeader<<Self::Primitives as NodePrimitives>::BlockHeader>,
        attributes: Self::NextBlockEnvCtx, // CL에서 온 블록 속성값
    ) -> Result<
        impl BlockBuilder<
            Primitives = Self::Primitives,
            Executor: BlockExecutorFor<'a, Self::BlockExecutorFactory, DB>,
        >,
        Self::Error,
    > {
        let evm_env = self.next_evm_env(parent, &attributes)?;
        let evm = self.evm_with_env(db, evm_env);
        let ctx = self.context_for_next_block(parent, attributes)?;
        Ok(self.create_block_builder(evm, parent, ctx))
    }

    /// [블록 검증 진입점] 기존 블록을 실행하는 Executor 생성
    ///
    /// 사용 예:
    ///   let mut executor = evm_config.executor(state_db);
    ///   let output = executor.execute(&block)?;
    ///
    /// BasicBlockExecutor는 내부에 State<DB>를 갖고 있어서
    /// 여러 블록을 순서대로 실행하며 상태를 누적할 수 있음
    #[auto_impl(keep_default_for(&, Arc))]
    fn executor<DB: Database>(
        &self,
        db: DB,
    ) -> impl Executor<DB, Primitives = Self::Primitives, Error = BlockExecutionError> {
        BasicBlockExecutor::new(self, db)
    }

    /// 배치 실행기 (여러 블록을 순서대로 실행)
    /// 내부적으로는 executor()와 동일한 BasicBlockExecutor를 반환
    #[auto_impl(keep_default_for(&, Arc))]
    fn batch_executor<DB: Database>(
        &self,
        db: DB,
    ) -> impl Executor<DB, Primitives = Self::Primitives, Error = BlockExecutionError> {
        BasicBlockExecutor::new(self, db)
    }
}

// ============================================================================
// NextBlockEnvAttributes - 다음 블록을 위한 CL 제공 속성값
//
// CL(Consensus Layer, 예: Lighthouse)이 Engine API를 통해 EL(=reth)에게
// "이런 블록을 만들어라"고 알려주는 정보들
// ============================================================================
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextBlockEnvAttributes {
    /// 다음 블록의 타임스탬프 (Unix 시간, 초 단위)
    /// 이 값으로 어떤 하드포크가 활성화되었는지 결정
    pub timestamp: u64,

    /// 블록 보상 및 가스비를 받을 주소 (= coinbase/miner)
    /// PoW에서는 채굴자 주소, PoS에서는 validator 지정 주소
    pub suggested_fee_recipient: Address,

    /// PoS의 무작위성 값 (prevRandao)
    /// RANDAO 믹스값 → 블록 환경의 prevrandao 필드로 사용됨
    pub prev_randao: B256,

    /// 블록의 최대 가스 한도
    pub gas_limit: u64,

    /// EIP-4788: 비콘 블록 루트 (부모 블록의 beacon chain 루트)
    /// Cancun 이후 활성. None이면 EIP-4788 시스템 콜 미실행
    pub parent_beacon_block_root: Option<B256>,

    /// EIP-4895: Validator 출금 목록
    /// Shanghai 이후 활성. 각 Withdrawal = { validator_index, address, amount }
    pub withdrawals: Option<Withdrawals>,

    /// 블록 헤더의 extra_data 필드 (32바이트 이내 자유 데이터)
    pub extra_data: Bytes,
}
