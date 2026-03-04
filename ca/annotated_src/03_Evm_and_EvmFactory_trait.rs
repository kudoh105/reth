// ============================================================================
// 파일: alloy-evm-0.27.2/src/evm.rs (핵심 부분)
// 역할: 개별 트랜잭션 실행 인터페이스 (Evm trait + EvmFactory trait)
//
// [Java 대응]
// Evm          ≈ EntityManager (실제 "실행"을 담당하는 가장 낮은 레벨)
// EvmFactory   ≈ EntityManagerFactory (Evm 인스턴스를 생성하는 팩토리)
//
// [계층 구조에서의 위치]
// ConfigureEvm (최상위)
//   └─ BlockExecutorFactory
//       └─ EvmFactory ← 바로 여기!
//           └─ Evm ← 그리고 여기!
// ============================================================================

// ============================================================================
// Evm trait - 트랜잭션 하나를 실행하는 가장 낮은 레벨 인터페이스
// ============================================================================
pub trait Evm {

    // ─────────────────────────────────────────────────────
    // 연관 타입들 - 구현체가 정해주는 구체적인 타입
    // ─────────────────────────────────────────────────────

    /// 내부 데이터베이스 타입
    /// 실제로는 &mut State<StateProviderDatabase<SP>> 형태
    /// State = revm의 캐시+번들 상태 관리 구조체
    type DB;

    /// 트랜잭션 환경 타입 (revm에서 사용)
    /// 실제로는 revm::context::TxEnv
    /// TxEnv에는 caller, value, data, gas_limit 등 트랜잭션 정보가 담김
    type Tx: IntoTxEnv<Self::Tx>;  // 자기 자신으로 변환 가능 (항등 변환)

    /// EVM 실행 에러 타입
    /// 실제로는 EVMError<DB::Error>
    type Error: EvmError;

    /// 실행 중단(halt) 이유 타입
    /// 예: OutOfGas, StackOverflow, InvalidJumpDest, etc.
    type HaltReason: HaltReasonTr + Send + Sync + 'static;

    /// 하드포크 스펙 ID 타입
    /// 예: SpecId::CANCUN, SpecId::PRAGUE
    type Spec: Debug + Copy + Hash + Eq + Send + Sync + Default + 'static;

    /// 블록 환경 타입 (BlockEnv)
    /// BlockEnv에는 블록번호, 타임스탬프, beneficiary, basefee 등이 담김
    type BlockEnv: BlockEnvironment;

    /// 프리컴파일 컨테이너 타입
    /// 이더리움의 내장 컨트랙트들 (예: ECRECOVER, SHA256, RIPEMD160...)
    type Precompiles;

    /// 인스펙터(트레이서) 타입
    /// 트랜잭션 실행 중 각 opcode 실행마다 훅을 받을 수 있음
    /// eth_traceTransaction 같은 API에서 사용
    type Inspector;

    // ─────────────────────────────────────────────────────
    // 핵심 실행 메서드
    // ─────────────────────────────────────────────────────

    /// [핵심] 트랜잭션을 실행하고 결과(EVM 실행 결과 + 상태 변경)를 반환
    ///
    /// transact와의 차이: 이 메서드는 이미 TxEnv로 변환된 트랜잭션을 받음
    ///
    /// 반환: ResultAndState {
    ///   result: ExecutionResult { Success|Revert|Halt, gas_used, logs },
    ///   state:  EvmState (변경된 계정들의 HashMap)
    /// }
    fn transact_raw(&mut self, tx: Self::Tx) -> Result<ResultAndState<Self::HaltReason>, Self::Error>;

    /// [자주 쓰이는 버전] 다양한 타입을 자동으로 TxEnv로 변환 후 실행
    ///
    /// IntoTxEnv trait: "TxEnv로 변환될 수 있는 것"을 나타내는 trait
    ///   예: Recovered<TransactionSigned>.into_tx_env() → TxEnv
    ///
    /// 구현:
    fn transact(&mut self, tx: impl IntoTxEnv<Self::Tx>) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        self.transact_raw(tx.into_tx_env())
    }

    /// [시스템 콜 전용] 시스템 주소에서 컨트랙트를 호출하는 특수 실행
    ///
    /// 일반 트랜잭션과 다른 점:
    ///   - caller가 SYSTEM_ADDRESS (0xfffffffffffffffffffffffffffffffffffffffe)
    ///   - gas_limit이 무제한 (30,000,000 고정)
    ///   - 잔액 검사 없음 (시스템 콜은 가스비 없음)
    ///   - beneficiary 계정이 state에 미리 로드되지 않음
    ///
    /// 사용처:
    ///   - EIP-2935: HISTORY_STORAGE_ADDRESS에 부모 해시 저장
    ///   - EIP-4788: BEACON_ROOTS_ADDRESS에 비콘 루트 저장
    ///   - EIP-7002: 출금 요청 컨트랙트 호출
    ///   - EIP-7251: 통합 요청 컨트랙트 호출
    fn transact_system_call(
        &mut self,
        caller: Address,    // 항상 SYSTEM_ADDRESS
        contract: Address,  // 호출할 컨트랙트 주소
        data: Bytes,        // calldata
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error>;

    /// 현재 블록 환경에 대한 참조 반환 (BlockEnv)
    fn block(&self) -> &Self::BlockEnv;

    /// Chain ID 반환 (예: 1 = Ethereum mainnet)
    fn chain_id(&self) -> u64;

    /// 데이터베이스 immutable 참조
    fn db(&self) -> &Self::DB;

    /// 데이터베이스 mutable 참조
    /// 상태를 직접 수정할 때 사용
    /// 예: evm.db_mut().commit(state) → 트랜잭션 실행 결과를 캐시에 커밋
    fn db_mut(&mut self) -> &mut Self::DB;

    /// 트랜잭션 실행 + 즉시 DB에 커밋하는 편의 메서드
    ///
    /// EVM 내부 State가 DatabaseCommit을 구현할 때만 사용 가능
    /// (= State<DB>가 DatabaseCommit를 구현함)
    fn transact_commit(
        &mut self,
        tx: impl IntoTxEnv<Self::Tx>,
    ) -> Result<ExecutionResult<Self::HaltReason>, Self::Error>
    where
        Self::DB: DatabaseCommit,  // DB가 commit 기능을 지원해야 함
    {
        let ResultAndState { result, state } = self.transact(tx)?;
        self.db_mut().commit(state);  // 상태 즉시 커밋
        Ok(result)
    }

    /// EVM 인스턴스를 소비(consume)하고 (DB, EvmEnv) 튜플 반환
    ///
    /// 블록 실행 완료 후 DB(State)를 꺼내올 때 사용
    /// finish() 후에는 이 EVM 인스턴스를 더 이상 쓸 수 없음 (소유권 이전)
    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec, Self::BlockEnv>)
    where
        Self: Sized;

    /// EVM을 소비하고 DB만 반환
    fn into_db(self) -> Self::DB
    where
        Self: Sized,
    {
        self.finish().0
    }

    /// EVM을 소비하고 환경(EvmEnv)만 반환
    fn into_env(self) -> EvmEnv<Self::Spec, Self::BlockEnv>
    where
        Self: Sized,
    {
        self.finish().1
    }

    /// Inspector를 활성화/비활성화
    /// true: 트레이싱 모드 (각 opcode마다 inspector 호출)
    /// false: 일반 모드 (inspector 호출 없음, 성능 더 좋음)
    fn set_inspector_enabled(&mut self, enabled: bool);

    /// 프리컴파일 컨테이너 참조 반환
    fn precompiles(&self) -> &Self::Precompiles;
    fn precompiles_mut(&mut self) -> &mut Self::Precompiles;

    /// 인스펙터 참조 반환
    fn inspector(&self) -> &Self::Inspector;
    fn inspector_mut(&mut self) -> &mut Self::Inspector;

    /// DB, Inspector, Precompiles의 mutable 참조를 한 번에 반환
    /// Rust 차용 규칙 때문에 따로따로 mutable 참조를 못 받을 때 사용
    fn components_mut(&mut self) -> (&mut Self::DB, &mut Self::Inspector, &mut Self::Precompiles);
}

// ============================================================================
// EvmEnv - EVM 실행 환경 설정
//
// 하나의 블록을 실행하는 데 필요한 환경 정보의 묶음
// ============================================================================

/// EVM 환경 설정 컨테이너
/// 블록 헤더와 체인 스펙으로부터 계산되는 값들을 담음
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvmEnv<Spec = SpecId, BlockEnv = revm::context::BlockEnv> {
    /// 설정 환경 (체인 수준 설정들)
    /// 포함 내용:
    ///   chain_id: u64              → 네트워크 식별자 (1=mainnet, 5=goerli...)
    ///   spec: Spec                 → 하드포크 스펙 (CANCUN, PRAGUE...)
    ///   limit_contract_code_size   → EIP-170: 컨트랙트 코드 크기 제한
    ///   limit_contract_initcode_size → EIP-3860: 배포 코드 크기 제한
    ///   tx_gas_limit_cap           → EIP-7825: tx 가스 한도 상한 (Osaka~)
    pub cfg_env: CfgEnv<Spec>,

    /// 블록 수준 환경
    /// 포함 내용:
    ///   number: U256               → 블록 번호
    ///   beneficiary: Address       → 수수료 받는 주소 (miner/validator)
    ///   timestamp: U256            → Unix 타임스탬프
    ///   difficulty: U256            → PoW 난이도 (PoS면 0)
    ///   prevrandao: Option<B256>   → PoS 랜덤값 (PoW면 None)
    ///   gas_limit: u64             → 블록 최대 가스
    ///   basefee: u64               → EIP-1559 기본 수수료 (wei 단위 아님)
    ///   blob_excess_gas_and_price  → EIP-4844 blob 가스 (Cancun~)
    pub block_env: BlockEnv,
}

// EvmEnv의 이더리움 전용 생성자들 (alloy-evm/src/eth/env.rs에 정의)
impl EvmEnv<SpecId> {
    /// 기존 블록 헤더로부터 EvmEnv 생성
    ///
    /// 처리 내용:
    ///   1. 타임스탬프와 블록번호로 SpecId(하드포크) 결정
    ///   2. Osaka 이후: tx_gas_limit_cap 설정 (EIP-7825)
    ///   3. Cancun 이후: blob_excess_gas_and_price 계산
    ///   4. Paris(Merge) 이후: difficulty=0, prevrandao=mix_hash 설정
    pub fn for_eth_block(
        header: impl BlockHeader,      // 블록 헤더
        chain_spec: impl EthereumHardforks, // 체인 스펙 (하드포크 정보)
        chain_id: u64,
        blob_params: Option<BlobParams>,  // EIP-4844 blob 파라미터
    ) -> Self {
        // 내부적으로: spec_by_timestamp_and_block_number() 호출
        // → timestamp와 block_number를 보고 적절한 SpecId 반환
        // 예: timestamp > cancun_timestamp → SpecId::CANCUN
        let spec = spec_by_timestamp_and_block_number(&chain_spec, header.timestamp(), header.number());

        let cfg_env = CfgEnv::new_with_spec(spec).with_chain_id(chain_id);

        let block_env = BlockEnv {
            number: U256::from(header.number()),
            beneficiary: header.beneficiary(),   // coinbase 주소
            timestamp: U256::from(header.timestamp()),
            // Merge(Paris) 이후: difficulty=0, prevrandao=mix_hash(헤더 필드)
            difficulty: if is_merge_active { U256::ZERO } else { header.difficulty() },
            prevrandao: if is_merge_active { header.mix_hash() } else { None },
            gas_limit: header.gas_limit(),
            basefee: header.base_fee_per_gas().unwrap_or_default(),
            blob_excess_gas_and_price: /* EIP-4844 계산 */,
        };

        Self::new(cfg_env, block_env)
    }

    /// 다음 블록(아직 없는 블록)을 위한 EvmEnv 생성
    /// Payload Building 시 사용
    pub fn for_eth_next_block(
        parent: impl BlockHeader,         // 부모(현재 head) 블록 헤더
        attributes: NextEvmEnvAttributes, // CL이 제공하는 다음 블록 속성
        base_fee_per_gas: u64,            // 다음 블록의 basefee (EIP-1559 계산값)
        chain_spec: impl EthereumHardforks,
        chain_id: u64,
        blob_params: Option<BlobParams>,
    ) -> Self {
        // 다음 블록의 timestamp로 스펙 결정 (미래 블록이므로 새 하드포크일 수 있음)
        let spec = spec_by_timestamp_and_block_number(&chain_spec, attributes.timestamp, parent.number() + 1);

        let block_env = BlockEnv {
            number: U256::from(parent.number() + 1),  // 부모 + 1
            beneficiary: attributes.suggested_fee_recipient,
            timestamp: U256::from(attributes.timestamp),
            difficulty: U256::ZERO,           // PoS이므로 항상 0
            prevrandao: Some(attributes.prev_randao),  // CL이 제공
            gas_limit: attributes.gas_limit,
            basefee: base_fee_per_gas,        // 계산된 다음 블록의 base fee
            blob_excess_gas_and_price: /* blob gas 계산 */,
        };

        Self::new(cfg_env, block_env)
    }
}

// ============================================================================
// EvmFactory trait - EVM 인스턴스를 생성하는 팩토리
// ============================================================================

/// EVM 인스턴스를 만드는 팩토리
///
/// [Java 대응] interface EvmFactory { Evm createEvm(DB db, EvmEnv env); }
pub trait EvmFactory {

    /// 이 팩토리가 만드는 EVM 인스턴스 타입
    /// DB와 Inspector를 제네릭 파라미터로 받음
    type Evm<DB: Database, I: Inspector<Self::Context<DB>>>: Evm<
        DB = DB,
        Tx = Self::Tx,
        HaltReason = Self::HaltReason,
        Error = Self::Error<DB::Error>,
        Spec = Self::Spec,
        BlockEnv = Self::BlockEnv,
        Precompiles = Self::Precompiles,
        Inspector = I,
    >;

    /// EVM 내부 Context 타입 (revm의 Context 구조체)
    /// DB, Block, Tx, Cfg 정보를 모두 담는 컨테이너
    type Context<DB: Database>: ContextTr<Db = DB, Journal: JournalExt>;

    /// 트랜잭션 타입 (= TxEnv)
    type Tx: IntoTxEnv<Self::Tx>;

    /// 에러 타입 (DB 에러를 제네릭 파라미터로 받음)
    type Error<DBError: Error + Send + Sync + 'static>: EvmError;

    type HaltReason: HaltReasonTr + Send + Sync + 'static;

    /// 하드포크 스펙 타입 (= SpecId)
    type Spec: Debug + Copy + Hash + Eq + Send + Sync + Default + 'static;

    type BlockEnv: BlockEnvironment;

    /// 프리컴파일 컨테이너 타입
    type Precompiles;

    /// [핵심] Inspector 없이 EVM 생성 (일반 블록 실행용)
    ///
    /// 파라미터:
    ///   db: 상태 데이터베이스 (= &mut State<StateProviderDatabase<...>>)
    ///   evm_env: 블록 환경 설정 (chain_id, spec, block info 등)
    ///
    /// EthEvmFactory 구현:
    ///   EthEvmBuilder::new(db, input).build()
    ///   → revm::Context를 구성하고 EthEvm으로 감쌈
    fn create_evm<DB: Database>(&self, db: DB, evm_env: EvmEnv<Self::Spec, Self::BlockEnv>) -> Self::Evm<DB, NoOpInspector>;

    /// [Inspector 포함] 트레이싱/디버깅용 EVM 생성
    ///
    /// Inspector 예:
    ///   - TracingInspector: eth_debug_trace* API용 풀 트레이서
    ///   - NoOpInspector: 아무것도 안 함 (기본값)
    fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>>>(
        &self,
        db: DB,
        input: EvmEnv<Self::Spec, Self::BlockEnv>,
        inspector: I,
    ) -> Self::Evm<DB, I>;
}
