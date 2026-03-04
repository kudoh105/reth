// ============================================================================
// 파일: alloy-evm-0.27.2/src/eth/mod.rs (핵심 부분)
// 역할: Evm + EvmFactory의 이더리움 구현체
//
// 구조:
//   EthEvmFactory  = EvmFactory 구현체 (EthEvm를 만드는 팩토리)
//   EthEvmBuilder  = EthEvm를 만들기 위한 빌더 패턴 구조체
//   EthEvm         = Evm 구현체 (revm::RevmEvm의 래퍼)
//
// [revm과의 관계]
//   EthEvm은 revm::Evm(RevmEvm)을 직접 감싸는(wrapping) rust 구조체
//   모든 실제 EVM 계산은 revm에서 담당
//   EthEvm = thin wrapper around revm that implements alloy-evm's Evm trait
// ============================================================================

// ─────────────────────────────────────────────────────────────
// 타입 별칭
// ─────────────────────────────────────────────────────────────

/// 이더리움 EVM의 컨텍스트 타입
/// revm의 Context에 이더리움 전용 타입 파라미터를 고정
///
/// Context<BlockEnv, TxEnv, CfgEnv, DB>:
///   BlockEnv: 블록 환경 (번호, 타임스탬프, basefee...)
///   TxEnv:    트랜잭션 환경 (sender, value, data, gas...)
///   CfgEnv:   설정 환경 (chain_id, spec, 제한값들...)
///   DB:       데이터베이스 (상태 조회/수정용)
pub type EthEvmContext<DB> = Context<BlockEnv, TxEnv, CfgEnv, DB>;

// ─────────────────────────────────────────────────────────────
// EthEvm - revm::RevmEvm를 감싸는 이더리움 EVM 구현체
// ─────────────────────────────────────────────────────────────

/// 이더리움 EVM 구현체
///
/// 내부적으로 revm::Evm(RevmEvm)을 가지고,
/// alloy-evm의 Evm trait을 구현함
///
/// 제네릭 파라미터:
///   DB: 데이터베이스 타입 (= &mut State<...>)
///   I:  Inspector 타입 (기본: NoOpInspector, 트레이싱 시: TracingInspector)
///   PRECOMPILE: 프리컴파일 컨테이너 타입 (기본: EthPrecompiles)
pub struct EthEvm<DB: Database, I, PRECOMPILE = EthPrecompiles> {
    /// 내부 revm EVM 인스턴스
    ///
    /// RevmEvm<Context, Inspector, Instructions, Precompiles, Frame>:
    ///   Context: EthEvmContext<DB> - DB + BlockEnv + TxEnv + CfgEnv
    ///   Inspector: I - 트레이서 (기본: NoOpInspector)
    ///   Instructions: EthInstructions - EVM 오피코드 구현체
    ///                 ADD, SUB, PUSH, POP, SLOAD, SSTORE, CALL, CREATE 등
    ///   Precompiles: PRECOMPILE - 내장 컨트랙트들
    ///   Frame: EthFrame - 콜 프레임 (스택 프레임과 유사)
    inner: RevmEvm<
        EthEvmContext<DB>,
        I,
        EthInstructions<EthInterpreter, EthEvmContext<DB>>,  // EVM 명령어 구현
        PRECOMPILE,   // 프리컴파일
        EthFrame,     // 실행 프레임 (CALL, DELEGATECALL 등 재귀 실행 관리)
    >,

    /// 인스펙터를 실제로 호출할 것인지 여부
    /// true: 각 opcode 실행 시 inspector 콜백 호출 (디버깅/트레이싱용)
    /// false: NoOpInspector처럼 동작 (일반 실행, 성능 최적화)
    inspect: bool,
}

impl<DB: Database, I, PRECOMPILE> EthEvm<DB, I, PRECOMPILE> {
    /// 새 EthEvm 생성
    /// inspect=false이면 Inspector가 있어도 호출되지 않음 → 성능 이점
    pub const fn new(
        evm: RevmEvm<...>,
        inspect: bool,  // true = 트레이싱 모드, false = 일반 실행 모드
    ) -> Self {
        Self { inner: evm, inspect }
    }

    /// 내부 revm Evm에 직접 접근 (필요한 경우)
    pub fn into_inner(self) -> RevmEvm<...> {
        self.inner
    }

    /// revm Context에 대한 참조 반환
    pub const fn ctx(&self) -> &EthEvmContext<DB> {
        &self.inner.ctx
    }
}

// Deref: EthEvm → EthEvmContext 자동 역참조
// Java의 "is-a" 관계처럼 EthEvm이 EthEvmContext인 것처럼 사용 가능
// 예: eth_evm.block (EthEvmContext의 필드인 BlockEnv에 직접 접근)
impl<DB: Database, I, PRECOMPILE> Deref for EthEvm<DB, I, PRECOMPILE> {
    type Target = EthEvmContext<DB>;
    fn deref(&self) -> &Self::Target { self.ctx() }
}

// ─────────────────────────────────────────────────────────────
// Evm trait 구현 - EthEvm이 이더리움 EVM임을 선언
// ─────────────────────────────────────────────────────────────

impl<DB, I, PRECOMPILE> Evm for EthEvm<DB, I, PRECOMPILE>
where
    DB: Database,
    I: Inspector<EthEvmContext<DB>>,  // I가 EthEvmContext에서 동작하는 Inspector여야 함
    PRECOMPILE: PrecompileProvider<EthEvmContext<DB>, Output = InterpreterResult>,
{
    type DB = DB;
    type Tx = TxEnv;                // revm의 TxEnv
    type Error = EVMError<DB::Error>; // revm의 에러 타입
    type HaltReason = HaltReason;   // revm의 HaltReason (OutOfGas, StackOverflow 등)
    type Spec = SpecId;             // 이더리움 하드포크 스펙 ID
    type BlockEnv = BlockEnv;       // revm의 BlockEnv
    type Precompiles = PRECOMPILE;
    type Inspector = I;

    fn block(&self) -> &BlockEnv {
        &self.block  // Deref를 통해 EthEvmContext.block에 접근
    }

    fn chain_id(&self) -> u64 {
        self.cfg.chain_id  // Deref를 통해 EthEvmContext.cfg.chain_id에 접근
    }

    /// [핵심] 트랜잭션 실행
    ///
    /// inspect 플래그에 따라 두 경로로 분기:
    ///   true: inspect_tx(tx) → 각 opcode마다 inspector 콜백 호출
    ///   false: transact(tx)  → 순수 실행 (Inspector 없음, 기본값)
    ///
    /// 반환: ResultAndState {
    ///   result: Success|Revert|Halt
    ///   state: HashMap<Address, Account 변경사항>
    /// }
    fn transact_raw(&mut self, tx: TxEnv) -> Result<ResultAndState<HaltReason>, EVMError<DB::Error>> {
        if self.inspect {
            self.inner.inspect_tx(tx)  // 트레이싱 모드: Inspector 콜백 포함
        } else {
            self.inner.transact(tx)    // 일반 모드: 순수 EVM 실행
        }
    }

    /// 시스템 콜 실행
    ///
    /// system_call_with_caller(): revm이 제공하는 특수 실행 메서드
    ///   - gas_limit = 30_000_000 (고정, 블록 가스 한도와 무관)
    ///   - gas_price = 0 (시스템은 가스비 안 냄)
    ///   - nonce 검사 없음
    ///   - beneficiary 계정이 state에 미리 로드되지 않음 (일반 tx와 다른 점)
    fn transact_system_call(
        &mut self,
        caller: Address,    // SYSTEM_ADDRESS = 0xffffffff...fffffffe
        contract: Address,
        data: Bytes,
    ) -> Result<ResultAndState<HaltReason>, EVMError<DB::Error>> {
        self.inner.system_call_with_caller(caller, contract, data)
    }

    /// EVM 소비 + (DB, EvmEnv) 반환
    ///
    /// 블록 실행 완료 후 State<DB>(DB)를 꺼내올 때 사용
    /// Context를 분해해서 DB(State)와 EvmEnv를 추출
    fn finish(self) -> (Self::DB, EvmEnv<SpecId>) {
        // revm Context 구조체를 분해(destructure)
        let Context { block: block_env, cfg: cfg_env, journaled_state, .. } = self.inner.ctx;
        //                                                    ^^^^^^^^^^^^
        //                 journaled_state.database = State<DB> (여기에 DB가 들어있음!)

        (journaled_state.database, EvmEnv { block_env, cfg_env })
    }

    fn set_inspector_enabled(&mut self, enabled: bool) {
        self.inspect = enabled;
    }

    fn db(&self) -> &Self::DB {
        &self.inner.ctx.journaled_state.database
    }

    fn db_mut(&mut self) -> &mut Self::DB {
        &mut self.inner.ctx.journaled_state.database
    }
}

// ─────────────────────────────────────────────────────────────
// EthEvmBuilder - EthEvm 생성을 위한 빌더 패턴
// ─────────────────────────────────────────────────────────────

/// EthEvm을 단계적으로 구성하는 빌더
///
/// [Java 대응]
/// EthEvmBuilder ≈ Lombok @Builder 또는 직접 구현한 Builder 클래스
#[derive(Debug)]
pub struct EthEvmBuilder<DB: Database, I = NoOpInspector> {
    db: DB,                           // 데이터베이스
    block_env: BlockEnv,              // 블록 환경
    cfg_env: CfgEnv,                  // 설정 환경
    inspector: I,                     // Inspector (기본: NoOpInspector)
    inspect: bool,                    // Inspector 활성화 여부
    precompiles: Option<PrecompilesMap>, // 커스텀 프리컴파일 (None이면 스펙에서 자동 결정)
}

impl<DB: Database> EthEvmBuilder<DB, NoOpInspector> {
    /// EvmEnv와 DB로 빌더 초기화
    pub fn new(db: DB, env: EvmEnv) -> Self {
        Self {
            db,
            block_env: env.block_env,  // EvmEnv를 분해해서 각 환경으로
            cfg_env: env.cfg_env,
            inspector: NoOpInspector {},  // 기본: Inspector 없음
            inspect: false,               // 기본: 인스펙터 비활성
            precompiles: None,            // 기본: SpecId에서 자동 결정
        }
    }
}

impl<DB: Database, I> EthEvmBuilder<DB, I> {
    /// 커스텀 Inspector 설정
    pub fn inspector<J>(self, inspector: J) -> EthEvmBuilder<DB, J> {
        EthEvmBuilder { inspector, ..self_as_j }  // Inspector 타입이 바뀌므로 새 빌더 반환
    }

    /// Inspector + 활성화 함께 설정
    pub fn activate_inspector<J>(self, inspector: J) -> EthEvmBuilder<DB, J> {
        self.inspector(inspector).inspect()  // inspector 설정 + inspect=true
    }

    /// inspect 플래그 활성화
    pub const fn inspect(self) -> Self {
        self.set_inspect(true)
    }

    /// 커스텀 프리컴파일 설정
    /// 이 메서드로 추가 프리컴파일을 등록할 수 있음 (예: custom-evm 예제)
    pub fn precompiles(mut self, precompiles: PrecompilesMap) -> Self {
        self.precompiles = Some(precompiles);
        self
    }

    /// [최종] EthEvm 인스턴스 빌드
    pub fn build(self) -> EthEvm<DB, I, PrecompilesMap>
    where
        I: Inspector<EthEvmContext<DB>>,
    {
        // 프리컴파일 결정:
        //   커스텀이 있으면 그것을 사용
        //   없으면 cfg_env의 spec(SpecId)에서 표준 프리컴파일 로드
        let precompiles = match self.precompiles {
            Some(p) => p,
            None => PrecompilesMap::from_static(
                Precompiles::new(PrecompileSpecId::from_spec_id(self.cfg_env.spec))
                //             ▲ SpecId(예: CANCUN)에 맞는 프리컴파일 세트 로드
                //               CANCUN이면 ECRECOVER, SHA256, ..., KZG_POINT_EVAL 포함
            ),
        };

        // revm Context 구성 (빌더 패턴)
        let inner = Context::mainnet()          // 이더리움 mainnet 컨텍스트 초기화
            .with_block(self.block_env)          // 블록 환경 설정
            .with_cfg(self.cfg_env)              // 설정 환경 (chain_id, spec 등)
            .with_db(self.db)                    // 데이터베이스 연결
            .build_mainnet_with_inspector(self.inspector)  // Inspector 등록 + 빌드
            .with_precompiles(precompiles);      // 프리컴파일 등록

        EthEvm { inner, inspect: self.inspect }  // EthEvm으로 래핑
    }
}

// ─────────────────────────────────────────────────────────────
// EthEvmFactory - EvmFactory 구현체
// ─────────────────────────────────────────────────────────────

/// 이더리움 EVM을 만드는 팩토리
///
/// 기본 구현체. 커스텀 프리컴파일이 필요하면 이 팩토리 대신
/// 커스텀 EvmFactory를 구현하여 EthEvmConfig::new_with_evm_factory()에 주입.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]  // 추후 필드 추가 가능성을 열어둠
pub struct EthEvmFactory;

impl EvmFactory for EthEvmFactory {
    // 이 팩토리가 만드는 타입들 고정
    type Evm<DB: Database, I: Inspector<EthEvmContext<DB>>> = EthEvm<DB, I, Self::Precompiles>;
    type Context<DB: Database> = Context<BlockEnv, TxEnv, CfgEnv, DB>;
    type Tx = TxEnv;
    type Error<DBError: core::error::Error + Send + Sync + 'static> = EVMError<DBError>;
    type HaltReason = HaltReason;
    type Spec = SpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;  // 동적 프리컴파일 맵

    /// Inspector 없이 EthEvm 생성 (일반 블록 실행용)
    fn create_evm<DB: Database>(&self, db: DB, input: EvmEnv) -> Self::Evm<DB, NoOpInspector> {
        EthEvmBuilder::new(db, input).build()
        // = Context::mainnet()으로 revm 초기화
        // + SpecId에 맞는 표준 프리컴파일 로드
        // + EthEvm으로 래핑
    }

    /// Inspector 포함 EthEvm 생성 (트레이싱용)
    fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>>>(
        &self,
        db: DB,
        input: EvmEnv,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        EthEvmBuilder::new(db, input)
            .activate_inspector(inspector)  // Inspector 설정 + inspect=true
            .build()
    }
}
