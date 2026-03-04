// ============================================================================
// 파일: alloy-evm-0.27.2/src/eth/block.rs (전체)
// 역할: 이더리움 블록 실행의 핵심 구현체
//
// 이 파일이 가장 중요! 실제 블록 실행 로직이 전부 여기 있음.
//
// 포함 내용:
//   1. EthBlockExecutionCtx  - 블록 실행 컨텍스트 (parent_hash, withdrawals 등)
//   2. EthBlockExecutorFactory - BlockExecutor를 만드는 팩토리
//   3. EthBlockExecutor      - 실제 블록 실행 로직 (BlockExecutor trait 구현)
//   4. EthTxResult           - 트랜잭션 실행 결과 래퍼
//
// [Java 대응]
// EthBlockExecutorFactory ≈ @Service Bean (싱글턴으로 관리)
// EthBlockExecutor        ≈ @Transactional Service Method 실행 컨텍스트
//                           (블록마다 새로 생성되는 임시 실행 객체)
// ============================================================================

// ─────────────────────────────────────────────────────────────
// EthBlockExecutionCtx - 블록 실행에 필요한 추가 컨텍스트
// ─────────────────────────────────────────────────────────────

/// 블록 실행에 필요하지만 EVM 환경(EvmEnv)에는 없는 추가 정보
///
/// EvmEnv: 블록번호, 타임스탬프, basefee, chain_id 등 EVM 계산에 필요한 것들
/// EthBlockExecutionCtx: 시스템 콜, 보상 계산, 출금 처리에 필요한 블록 메타데이터
///
/// 라이프타임 'a: 이 구조체가 참조하는 데이터(ommers, withdrawals)의
///               라이프타임과 묶임. 블록보다 오래 살 수 없음.
#[derive(Debug, Clone)]
pub struct EthBlockExecutionCtx<'a> {
    /// 부모(이전) 블록의 해시
    ///
    /// 사용처: EIP-2935 시스템 콜
    ///   → HISTORY_STORAGE_ADDRESS 컨트랙트에 parent_hash를 저장
    ///   → 이렇게 하면 스마트 컨트랙트에서 BLOCKHASH 오피코드로
    ///     최근 블록 해시를 조회할 수 있게 됨
    pub parent_hash: B256,

    /// 부모 블록의 비콘 체인 블록 루트 (Cancun 이후)
    ///
    /// 사용처: EIP-4788 시스템 콜
    ///   → BEACON_ROOTS_ADDRESS 컨트랙트에 저장
    ///   → 스마트 컨트랙트에서 비콘 블록 정보를 조회 가능하게 됨
    ///   → None이면 시스템 콜 미실행
    pub parent_beacon_block_root: Option<B256>,

    /// Uncle(Ommer) 블록 헤더 목록
    ///
    /// 사용처: PoW 블록 보상 계산
    ///   → Uncle 포함 시 Uncle 채굴자에게 보상, 메인 채굴자도 추가 보상
    ///   → PoS(Merge 이후)에서는 항상 빈 슬라이스 (&[])
    ///
    /// &'a [Header]: Header들의 슬라이스를 빌림 (복사 없이 참조)
    pub ommers: &'a [Header],

    /// Validator 출금 목록 (Shanghai 이후, EIP-4895)
    ///
    /// 사용처: 블록 후처리에서 각 validator의 잔액 증가 적용
    ///   각 Withdrawal = { validator_index, address, amount_in_gwei }
    ///
    /// Cow (Clone-on-Write):
    ///   Cow::Borrowed(&'a Withdrawals): 기존 블록의 withdrawals를 빌려옴 (복사 없음)
    ///   Cow::Owned(Withdrawals):       다음 블록 빌딩 시 CL에서 받은 데이터 소유
    pub withdrawals: Option<Cow<'a, Withdrawals>>,

    /// 블록 헤더의 extra_data 필드 (32바이트 이내 자유 데이터)
    /// 채굴자/validator가 넣는 임의 데이터 (예: 클라이언트 버전 표시)
    pub extra_data: Bytes,

    /// 이 블록에 담긴 예상 tx 수 (선택적 힌트)
    ///
    /// 성능 최적화: receipts Vec을 사전에 적절한 크기로 할당
    ///   Vec::with_capacity(tx_count_hint) → 재할당 횟수 감소
    pub tx_count_hint: Option<usize>,
}

// ─────────────────────────────────────────────────────────────
// EthBlockExecutorFactory - BlockExecutor를 만드는 팩토리
// ─────────────────────────────────────────────────────────────

/// 이더리움 블록 실행기 팩토리
///
/// 제네릭 파라미터 3개:
///   R = ReceiptBuilder 타입 (영수증을 어떻게 만들지)
///       EthEvmConfig에서는 RethReceiptBuilder 사용
///   C = ChainSpec 타입 (하드포크 정보)
///   EvmFactory = EVM 팩토리 (EthEvmFactory 기본값, 커스텀 가능)
///
/// 이 팩토리는 앱 시작 시 한 번 생성되어 Arc 등으로 공유됨 (싱글턴 패턴)
#[derive(Clone, Debug)]
pub struct EthBlockExecutorFactory<R, C, EvmFactory = EthEvmFactory> {
    /// 영수증 빌더 (트랜잭션 실행 결과 → 영수증 변환)
    receipt_builder: R,

    /// 체인 스펙 (Arc로 공유 = 복사 비용 없이 여러 곳에서 참조)
    ///
    /// Arc<C>: 스레드 안전한 레퍼런스 카운팅 포인터
    ///         여러 스레드가 동시에 접근 가능
    ///         C를 복사하지 않고 참조만 공유 (대형 ChainSpec 복사 방지)
    spec: C,

    /// EVM 팩토리 (EthEvm를 만드는 팩토리)
    evm_factory: EvmFactory,
}

impl<R, C, EvmFactory> EthBlockExecutorFactory<R, C, EvmFactory> {
    /// 팩토리 생성
    pub const fn new(receipt_builder: R, spec: C, evm_factory: EvmFactory) -> Self {
        Self { receipt_builder, spec, evm_factory }
    }

    /// 체인 스펙 참조 반환
    pub const fn spec(&self) -> &C {
        &self.spec
    }
}

impl<R, C, EvmF> BlockExecutorFactory for EthBlockExecutorFactory<R, C, EvmF>
where
    R: ReceiptBuilder<Transaction = TransactionSigned, Receipt: Debug + Send + Sync + 'static>,
    C: EthExecutorSpec + EthChainSpec<Header = Header> + Hardforks + Send + Sync + 'static,
    EvmF: EvmFactory<Tx = TxEnv, ...> + Send + Sync + Clone + 'static,
{
    type EvmFactory = EvmF;
    type ExecutionCtx<'a> = EthBlockExecutionCtx<'a>;
    type Transaction = TransactionSigned;
    type Receipt = R::Receipt;

    fn evm_factory(&self) -> &EvmF {
        &self.evm_factory
    }

    /// [핵심] EVM + 컨텍스트 → EthBlockExecutor 생성
    fn create_executor<'a, DB: Database + 'a, I: Inspector<...>>(
        &'a self,
        evm: EvmF::Evm<&'a mut State<DB>, I>,
        ctx: EthBlockExecutionCtx<'a>,
    ) -> impl BlockExecutorFor<'a, Self, DB, I> {
        EthBlockExecutor::new(evm, ctx, &self.spec, &self.receipt_builder)
    }
}

// ─────────────────────────────────────────────────────────────
// EthBlockExecutor - 블록 실행의 핵심!
// ─────────────────────────────────────────────────────────────

/// 이더리움 블록 실행기
///
/// 이 구조체의 인스턴스가 블록 하나를 실행하는 동안 살아있음.
/// BlockExecutor trait의 실제 구현체.
///
/// 제네릭 파라미터:
///   E: Evm - EVM 인스턴스 (EthEvm<DB, I>)
///   R: ReceiptBuilder - 영수증 빌더
///   C: EthExecutorSpec + Hardforks - 체인 스펙
pub struct EthBlockExecutor<'a, E, R, C> {
    /// EVM 인스턴스 (실제 opcode 실행 담당)
    /// 블록 실행 중 상태를 유지함
    evm: E,

    /// 현재 블록의 실행 컨텍스트 (parent_hash, ommers, withdrawals 등)
    ctx: EthBlockExecutionCtx<'a>,

    /// 현재까지 누적된 가스 사용량 (블록 내 모든 tx의 합)
    gas_used: u64,

    /// 현재까지 누적된 blob 가스 사용량 (Cancun 이후)
    blob_gas_used: u64,

    /// 지금까지 실행된 트랜잭션들의 영수증 목록 (순서 보장)
    /// receipts[0] = 첫 번째 tx의 영수증
    receipts: Vec<R::Receipt>,

    /// 시스템 콜 담당자 (EIP-2935, EIP-4788, EIP-7002 등)
    system_caller: SystemCaller<&'a C>,

    /// 영수증 빌더 참조
    receipt_builder: &'a R,
}

impl<'a, E: Evm, R: ReceiptBuilder<...>, C: EthExecutorSpec + Hardforks>
    EthBlockExecutor<'a, E, R, C>
{
    /// 새 블록 실행기 생성
    pub fn new(evm: E, ctx: EthBlockExecutionCtx<'a>, spec: &'a C, receipt_builder: &'a R) -> Self {
        let receipts_capacity = ctx.tx_count_hint.unwrap_or(0);
        Self {
            evm,
            ctx,
            gas_used: 0,
            blob_gas_used: 0,
            receipts: Vec::with_capacity(receipts_capacity),  // 사전 할당으로 성능 최적화
            system_caller: SystemCaller::new(spec),           // 시스템 콜러 초기화
            receipt_builder,
        }
    }
}

// ─────────────────────────────────────────────────────────────
// BlockExecutor trait 구현 - 핵심 실행 로직!
// ─────────────────────────────────────────────────────────────

impl<'a, E, R, C> BlockExecutor for EthBlockExecutor<'a, E, R, C>
where
    E: Evm<Tx = TxEnv, ...>,
    R: ReceiptBuilder<Transaction = TransactionSigned, ...>,
    C: EthExecutorSpec + Hardforks,
{
    type Transaction = TransactionSigned;
    type Receipt = R::Receipt;
    type Evm = E;
    type Result = EthTxResult<E::HaltReason>;  // 트랜잭션 실행 결과 타입

    // ─────────────────────────────────────────────────
    // 단계 1: 전처리 (apply_pre_execution_changes)
    // 모든 블록 트랜잭션 실행 전에 호출됨
    // ─────────────────────────────────────────────────
    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {

        // ① Spurious Dragon 이후: 빈 계정 정리 활성화
        //    EIP-161: 잔액=0, nonce=0, 코드=없는 "빈 계정"을 상태에서 삭제
        //    state_clear_flag = true이면 트랜잭션 실행 시 빈 계정 자동 정리
        let state_clear_flag = self.spec.ethereum_fork_activation(EthereumHardfork::SpuriousDragon)
            .active_at_block(self.evm.block().number().saturating_to());
        self.evm.db_mut().set_state_clear_flag(state_clear_flag);

        // ② EIP-2935: 블록해시 히스토리 컨트랙트 호출 (Prague 이후)
        //    SYSTEM_ADDRESS → HISTORY_STORAGE_ADDRESS 호출
        //    calldata = parent_block_hash
        //    → 컨트랙트가 블록해시를 저장 → BLOCKHASH 오피코드가 더 오래된 블록 조회 가능
        self.system_caller.apply_blockhashes_contract_call(
            self.ctx.parent_hash,
            &mut self.evm,
        )?;

        // ③ EIP-4788: 비콘 블록 루트 컨트랙트 호출 (Cancun 이후)
        //    SYSTEM_ADDRESS → BEACON_ROOTS_ADDRESS 호출
        //    calldata = parent_beacon_block_root
        //    → 컨트랙트가 비콘 블록 루트를 저장 → 스테이킹 컨트랙트 등에서 활용
        self.system_caller.apply_beacon_root_contract_call(
            self.ctx.parent_beacon_block_root,
            &mut self.evm,
        )?;

        Ok(())
    }

    // ─────────────────────────────────────────────────
    // 단계 2-A: 트랜잭션 실행 (커밋 없이)
    // ─────────────────────────────────────────────────
    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<EthTxResult<E::HaltReason>, BlockExecutionError> {

        // ExecutableTx → (TxEnv, Recovered<Transaction>) 분리
        // tx.into_parts() 내부: tx.to_tx_env() 호출 → TxEnv 생성
        let (tx_env, tx) = tx.into_parts();

        // ① 블록 가스 한도 검증
        //    현재 남은 블록 가스보다 tx의 가스 한도가 크면 → 거부
        //    (이 tx를 포함하면 블록 가스 한도 초과)
        let block_available_gas = {
            let block_gas_limit = self.evm.block().gas_limit();  // 블록 최대 가스
            let cumulative_gas = self.gas_used;                   // 이미 사용한 가스
            block_gas_limit.saturating_sub(cumulative_gas)        // 남은 가스
        };
        if tx_env.gas_limit > block_available_gas {
            return Err(BlockExecutionError::validation(BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                transaction_gas_limit: tx_env.gas_limit,
                block_available_gas,
            }));
        }

        // ② Cancun 이후: blob 트랜잭션의 blob 가스 추출 (type=3 트랜잭션)
        //    blob_gas_used = 각 blob마다 GAS_PER_BLOB(131072) 만큼
        let blob_gas_used = tx_env
            .blob_hashes                         // blob tx의 해시 목록
            .as_ref()
            .map(|hashes| hashes.len() as u64 * <EthBlockExecutor as _>::GAS_PER_BLOB)
            .unwrap_or(0);

        // ③ EVM 실행! (가장 중요한 부분)
        //    evm.transact(tx_env) → revm이 tx를 실행
        //    ResultAndState {
        //      result: Success { gas_used, output, logs } | Revert { gas_used } | Halt { reason },
        //      state:  HashMap<Address, AccountState 변경사항>
        //    }
        let result_and_state = self.evm.transact(tx_env)?;

        // ④ 트랜잭션 타입 추출 (Legacy=0, EIP-2930=1, EIP-1559=2, EIP-4844=3, EIP-7702=4)
        let tx_type = tx.tx().ty();

        // 결과를 EthTxResult로 래핑 (나중에 commit_transaction에서 사용)
        Ok(EthTxResult {
            result: result_and_state,   // 실행 결과 + 상태 변경
            blob_gas_used,              // blob 가스 (Cancun 이후)
            tx,                         // 원본 트랜잭션 (영수증 생성에 필요)
            tx_type,                    // 트랜잭션 타입 (영수증에 들어감)
        })
    }

    // ─────────────────────────────────────────────────
    // 단계 2-B: 트랜잭션 결과 커밋
    // ─────────────────────────────────────────────────
    fn commit_transaction(
        &mut self,
        output: EthTxResult<E::HaltReason>,
    ) -> Result<u64, BlockExecutionError> {

        let EthTxResult { result: ResultAndState { result, state }, blob_gas_used, tx, tx_type } = output;

        // ① 상태 훅 호출 (모니터링/디버깅용)
        //    system_caller가 내부 훅을 갖고 있으면 호출
        self.system_caller.on_state(
            StateChangeSource::Transaction(self.receipts.len()),  // "몇 번째 tx인지"
            &state,
        );

        // ② 가스 누적
        let gas_used = result.gas_used();
        self.gas_used += gas_used;           // 블록 누적 가스 업데이트

        // ③ blob 가스 누적 (Cancun 이후)
        self.blob_gas_used += blob_gas_used;

        // ④ 영수증 생성
        //    영수증에 들어가는 것들:
        //      tx_type: 트랜잭션 종류 (EIP-2930, EIP-1559 등)
        //      success: 실행 성공 여부 (EIP-658)
        //      cumulative_gas_used: 이 블록에서 이 tx까지의 누적 가스
        //      logs: 트랜잭션이 emit한 이벤트 로그들
        let receipt = self.receipt_builder.build_receipt(ReceiptBuilderCtx {
            tx: tx.as_ref(),            // 원본 트랜잭션 참조
            tx_type,                    // 트랜잭션 타입
            result: &result,            // 실행 결과
            cumulative_gas_used: self.gas_used,  // 누적 가스 (이 tx 이후 총량)
            evm: &self.evm,             // EVM 참조 (추가 정보 접근용)
        });
        self.receipts.push(receipt);  // 영수증 저장

        // ⑤ 상태 변경 커밋 (실제 state mutation)
        //    state: HashMap<Address, Account 변경사항>
        //    evm.db_mut().commit(state) → State<DB>의 캐시에 변경사항 반영
        //    아직 디스크에 쓰이지 않음! 캐시 레벨에서만 변경.
        //
        //    State<DB>의 내부 구조:
        //      cache: 인메모리 계정 캐시
        //      transition_state: 이번 tx의 변경사항 기록
        //      bundle_state: 이번 블록까지의 모든 변경사항 요약
        self.evm.db_mut().commit(state);

        Ok(gas_used)
    }

    // ─────────────────────────────────────────────────
    // 단계 3: 후처리 + 결과 반환 (finish)
    // ─────────────────────────────────────────────────
    fn finish(
        mut self,
    ) -> Result<(Self::Evm, BlockExecutionResult<R::Receipt>), BlockExecutionError> {

        // ① EIP-6110: 디포짓 요청 처리 (Prague 이후)
        //    → 이더리움 2.0 스테이킹 디포짓을 영수증에서 파싱
        //    → 디포짓 컨트랙트 이벤트 로그(DepositEvent)를 슬라이스하여 추출
        let deposit_requests = if self.spec.is_prague_active_at_timestamp(block_timestamp) {
            eip6110::parse_deposits_from_receipts(&self.spec, &self.receipts)?
            //          ^^^^^^^^^^^^^^^^^^^^^^^^
            // 모든 영수증의 로그를 순회하며 DEPOSIT_CONTRACT_ADDRESS에서 온
            // DepositEvent 로그를 찾아 파싱
        } else {
            Default::default()
        };

        // ② Prague 이후 시스템 콜들로부터 요청 수집
        //    requests: Requests = Vec<(request_type: u8, data: Bytes)>
        let mut requests = Requests::default();
        if !deposit_requests.is_empty() {
            requests.push_request_with_type(DEPOSIT_REQUEST_TYPE, deposit_requests);
        }

        // EIP-7002: 출금 요청 컨트랙트 호출
        //   SYSTEM_ADDRESS → WITHDRAWAL_REQUEST_PREDEPLOY_ADDRESS 호출
        //   반환값 파싱 → [ (validator_pubkey, amount) ] 형태의 출금 요청 목록
        // EIP-7251: 통합 요청 컨트랙트 호출
        //   SYSTEM_ADDRESS → CONSOLIDATION_REQUEST_PREDEPLOY_ADDRESS 호출
        //   반환값 파싱 → [ (source_pubkey, target_pubkey) ] 형태의 통합 요청 목록
        self.system_caller.append_post_execution_changes(&mut self.evm, &mut requests)?;

        // ③ 잔액 증가 맵 계산 (보상 + Withdrawal)
        //    balance_increments: HashMap<Address, u128> (각 주소별 증가량)
        let mut balance_increments = post_block_balance_increments(
            &self.spec,
            self.evm.block(),              // 현재 블록 환경 (번호, 타임스탬프 등)
            self.ctx.ommers,               // Uncle 블록들 (PoW 보상 계산)
            self.ctx.withdrawals.as_deref(), // Validator 출금 목록
        );

        // ④ DAO fork 특수 처리 (2016년 이더리움 역사적 사건)
        //    DAO 해킹으로 훔쳐진 이더를 DAO Drainer 컨트랙트들에서
        //    DAO Beneficiary 주소로 강제 이전 (하드포크로 결정)
        if self.spec.ethereum_fork_activation(EthereumHardfork::Dao)
            .transitions_at_block(self.evm.block().number().saturating_to())
        {
            // DAO 관련 계정들의 잔액을 모두 0으로 만들고
            let drained_balance: u128 = self.evm.db_mut()
                .drain_balances(dao_fork::DAO_HARDFORK_ACCOUNTS)?
                .into_iter().sum();

            // DAO Beneficiary 계정에 합산 (강제 이전)
            *balance_increments.entry(dao_fork::DAO_HARDFORK_BENEFICIARY).or_default()
                += drained_balance;
        }

        // ⑤ 잔액 증가 적용
        //    increment_balances(): State<DB>의 캐시에 잔액 변경 적용
        //    예: 블록 보상 2 ETH → beneficiary 주소의 잔액 +2 ETH
        //        Withdrawal 1 ETH → validator 주소의 잔액 +1 ETH
        if !balance_increments.is_empty() {
            self.evm.db_mut().increment_balances(balance_increments.clone())?;
        }

        // ⑥ 상태 훅 호출 (잔액 변경사항 방출)
        self.system_caller.try_on_state_with(|| {
            // 잔액 증가분을 EvmState로 변환하여 훅에 전달
            balance_increment_state(&balance_increments, self.evm.db_mut())
        })?;

        // ⑦ 최종 결과 반환
        //    EVM 인스턴스도 함께 반환 → 호출자가 finish()로 DB를 꺼낼 수 있음
        Ok((
            self.evm,
            BlockExecutionResult {
                receipts: self.receipts,         // 모든 영수증
                requests,                         // 합의 레이어 요청들 (Prague~)
                gas_used: self.gas_used,          // 블록 총 가스 사용량
                blob_gas_used: self.blob_gas_used, // 블록 총 blob 가스
            },
        ))
    }

    // ─────────────────────────────────────────────────
    // 기타 getter 메서드들
    // ─────────────────────────────────────────────────

    fn evm_mut(&mut self) -> &mut E {
        &mut self.evm
    }

    fn evm(&self) -> &E {
        &self.evm
    }

    fn receipts(&self) -> &[R::Receipt] {
        &self.receipts
    }

    fn set_state_hook(&mut self, hook: Option<Box<dyn OnStateHook>>) {
        self.system_caller.with_state_hook(hook);
    }
}

// ─────────────────────────────────────────────────────────────
// EthTxResult - 단일 트랜잭션 실행 결과 래퍼
// ─────────────────────────────────────────────────────────────

/// execute_transaction_without_commit의 반환 타입
/// 실행 결과와 원본 트랜잭션 정보를 함께 보관
pub struct EthTxResult<HaltReason> {
    /// revm 실행 결과
    /// ResultAndState {
    ///   result: ExecutionResult { Success | Revert | Halt, gas_used, logs },
    ///   state:  HashMap<Address, Account 변경사항>
    /// }
    pub result: ResultAndState<HaltReason>,

    /// 이 트랜잭션이 사용한 blob 가스 (type-3 tx만 해당, 나머지는 0)
    pub blob_gas_used: u64,

    /// 원본 트랜잭션 (영수증 생성에 필요)
    /// RecoveredTx이므로 tx 내용 + sender 주소 모두 접근 가능
    pub tx: <impl ExecutableTx<EthBlockExecutor<...>> as ExecutableTxParts<...>>::Recovered,

    /// 트랜잭션 타입 (Legacy=0, EIP-2930=1, EIP-1559=2, EIP-4844=3, EIP-7702=4)
    pub tx_type: TxType,
}

impl<HaltReason> TxResult for EthTxResult<HaltReason> {
    type HaltReason = HaltReason;
    fn result(&self) -> &ResultAndState<HaltReason> {
        &self.result
    }
}
