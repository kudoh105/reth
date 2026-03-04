// ============================================================================
// 파일: alloy-evm-0.27.2/src/block/system_calls/ (디렉토리 전체)
//   mod.rs:     SystemCaller 구조체 및 메서드
//   eip2935.rs: EIP-2935 블록해시 히스토리 시스템 콜
//   eip4788.rs: EIP-4788 비콘 블록 루트 시스템 콜
//   eip7002.rs: EIP-7002 출금 요청 시스템 콜
//   eip7251.rs: EIP-7251 통합 요청 시스템 콜
//
// [역할]
// 시스템 콜 = 일반 트랜잭션이 아닌, 이더리움 프로토콜이 직접 실행하는 컨트랙트 호출
// SYSTEM_ADDRESS(0xfffffffe)가 caller가 되어 특정 컨트랙트를 실행
//
// [Java 대응]
// SystemCaller ≈ @Component 역할의 SystemContractService
// ============================================================================

// ─────────────────────────────────────────────────────────────
// mod.rs - SystemCaller 구조체
// ─────────────────────────────────────────────────────────────

/// 시스템 콜을 관리하는 구조체
///
/// EthBlockExecutor가 내부에 이 구조체를 보유하고
/// 전처리/후처리 단계에서 사용
pub struct SystemCaller<Spec> {
    /// 체인 스펙 (어느 하드포크가 활성화됐는지 확인)
    spec: Spec,

    /// 상태 변경 훅 (선택적)
    ///
    /// 각 시스템 콜 실행 후 상태 변경사항을 외부에 알리는 콜백
    /// 용도: 모니터링, 이벤트 스트리밍, 디버깅
    /// None이면 훅 미사용 (일반 실행)
    hook: Option<Box<dyn OnStateHook>>,
}

impl<Spec> SystemCaller<Spec> {
    pub fn new(spec: Spec) -> Self {
        Self { spec, hook: None }
    }

    /// 상태 훅 설정 (빌더 패턴)
    pub fn with_state_hook(&mut self, hook: Option<Box<dyn OnStateHook>>) {
        self.hook = hook;
    }

    /// 상태 변경 훅 호출
    pub fn on_state(&mut self, source: StateChangeSource, state: &EvmState) {
        if let Some(hook) = &mut self.hook {
            hook.on_state(source, state);
        }
    }

    /// 지연(lazy) 방식으로 훅 호출
    /// f가 반환하는 상태가 필요한 경우에만 계산 (훅이 없으면 f 미호출)
    pub fn try_on_state_with<E>(
        &mut self,
        f: impl FnOnce() -> Result<EvmState, E>,
    ) -> Result<(), E> {
        if let Some(hook) = &mut self.hook {
            hook.on_state(StateChangeSource::System, &f()?);
        }
        Ok(())
    }
}

// 전처리 관련 메서드들
impl<Spec: EthereumHardforks> SystemCaller<Spec> {
    /// EIP-2935 블록해시 시스템 콜 실행
    pub fn apply_blockhashes_contract_call<E: Evm>(
        &mut self,
        parent_block_hash: B256,
        evm: &mut E,
    ) -> Result<(), BlockExecutionError> {
        let result = transact_blockhashes_contract_call(&self.spec, parent_block_hash, evm)?;
        if let Some(ResultAndState { state, .. }) = result {
            self.on_state(StateChangeSource::System, &state);
            evm.db_mut().commit(state);
        }
        Ok(())
    }

    /// EIP-4788 비콘 루트 시스템 콜 실행
    pub fn apply_beacon_root_contract_call<E: Evm>(
        &mut self,
        parent_beacon_block_root: Option<B256>,
        evm: &mut E,
    ) -> Result<(), BlockExecutionError> {
        let result = transact_beacon_root_contract_call(&self.spec, parent_beacon_block_root, evm)?;
        if let Some(ResultAndState { state, .. }) = result {
            self.on_state(StateChangeSource::System, &state);
            evm.db_mut().commit(state);
        }
        Ok(())
    }
}

// 후처리 관련 메서드들
impl<Spec: EthereumHardforks + EthExecutorSpec> SystemCaller<Spec> {
    /// Prague 이후 후처리 시스템 콜 실행 (EIP-7002, EIP-7251)
    /// 결과를 requests에 추가
    pub fn append_post_execution_changes<E: Evm>(
        &mut self,
        evm: &mut E,
        requests: &mut Requests,
    ) -> Result<(), BlockExecutionError> {
        // 두 시스템 콜 순서대로 실행 (순서가 중요!)
        self.apply_withdrawal_requests_contract_call(evm, requests)?;
        self.apply_consolidation_requests_contract_call(evm, requests)?;
        Ok(())
    }

    /// EIP-7002: 출금 요청 컨트랙트 호출
    fn apply_withdrawal_requests_contract_call<E: Evm>(
        &mut self,
        evm: &mut E,
        requests: &mut Requests,
    ) -> Result<(), BlockExecutionError> {
        if !self.spec.is_prague_active_at_timestamp(/* timestamp */) {
            return Ok(());
        }
        let result = evm.transact_system_call(
            SYSTEM_ADDRESS,                       // caller: 0xfffffffe
            WITHDRAWAL_REQUEST_PREDEPLOY_ADDRESS, // EIP-7002 컨트랙트 주소
            Bytes::new(),                         // calldata 없음 (조회만 함)
        )?;
        // 반환값 파싱 → 출금 요청 목록 (각 56바이트: pubkey(48) + amount(8))
        let withdrawal_reqs = parse_withdrawal_requests_from_system_result(result.result)?;
        requests.push_request_with_type(WITHDRAWAL_REQUEST_TYPE, withdrawal_reqs);
        evm.db_mut().commit(result.state);
        Ok(())
    }

    /// EIP-7251: 통합 요청 컨트랙트 호출
    fn apply_consolidation_requests_contract_call<E: Evm>(
        &mut self,
        evm: &mut E,
        requests: &mut Requests,
    ) -> Result<(), BlockExecutionError> {
        if !self.spec.is_prague_active_at_timestamp(/* timestamp */) {
            return Ok(());
        }
        let result = evm.transact_system_call(
            SYSTEM_ADDRESS,
            CONSOLIDATION_REQUEST_PREDEPLOY_ADDRESS,
            Bytes::new(),
        )?;
        // 반환값 파싱 → 통합 요청 목록 (각 116바이트: source_pubkey(48) + target_pubkey(48) + ...)
        let consolidation_reqs = parse_consolidation_requests_from_system_result(result.result)?;
        requests.push_request_with_type(CONSOLIDATION_REQUEST_TYPE, consolidation_reqs);
        evm.db_mut().commit(result.state);
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────
// eip2935.rs - EIP-2935: 블록해시 히스토리 컨트랙트
// ─────────────────────────────────────────────────────────────

/// EIP-2935 시스템 콜의 핵심 함수
///
/// 목적:
///   BLOCKHASH 오피코드의 조회 가능 범위를 256블록에서 8192블록으로 확장
///   이를 위해 블록마다 parent_hash를 전용 컨트랙트에 저장
///
/// 실행 조건:
///   1. Prague 하드포크 이상 (is_prague_active_at_timestamp)
///   2. 제네시스 블록이 아닐 것 (block_number != 0)
pub fn transact_blockhashes_contract_call<HaltReason>(
    spec: impl EthereumHardforks,
    parent_block_hash: B256,
    evm: &mut impl Evm<HaltReason = HaltReason>,
) -> Result<Option<ResultAndState<HaltReason>>, BlockExecutionError> {
    // Prague 이전이면 이 시스템 콜 없음
    let timestamp = evm.block().timestamp().saturating_to::<u64>();
    if !spec.is_prague_active_at_timestamp(timestamp) {
        return Ok(None);
    }

    // 제네시스 블록(0번)은 부모가 없으므로 스킵
    if evm.block().number().is_zero() {
        return Ok(None);
    }

    // HISTORY_STORAGE_ADDRESS: EIP-2935에서 정의한 컨트랙트 주소
    // = 0x0000F90827F1C53a10cb7A3A0b9Cf48b2Ee53D6D
    // 이 컨트랙트는 Prague 하드포크에서 genesis에 배포됨
    //
    // 실행:
    //   caller: SYSTEM_ADDRESS (0xfffffffe)
    //   to:     HISTORY_STORAGE_ADDRESS
    //   data:   parent_block_hash (32바이트)
    //   → 컨트랙트 내부에서 현재 블록 번호를 키로 parent_hash를 저장
    let res = evm
        .transact_system_call(
            SYSTEM_ADDRESS,          // 0xffffffff...fffffffe
            HISTORY_STORAGE_ADDRESS, // EIP-2935 컨트랙트
            parent_block_hash.0.into(),
        )
        .map_err(|e| BlockExecutionError::other(format!("EIP-2935 error: {e}")))?;

    Ok(Some(res))
}

// ─────────────────────────────────────────────────────────────
// eip4788.rs - EIP-4788: 비콘 블록 루트 컨트랙트
// ─────────────────────────────────────────────────────────────

/// EIP-4788 시스템 콜의 핵심 함수
///
/// 목적:
///   이더리움 실행 레이어(EL)에서 합의 레이어(CL)의 비콘 블록 루트를 조회할 수 있게 함
///   이 정보를 활용해 스테이킹 컨트랙트, 브릿지 등이 CL 상태를 검증 가능
///
/// 실행 조건:
///   1. Cancun 하드포크 이상
///   2. parent_beacon_block_root가 Some(_) 일 것 (CL이 제공했을 때)
///
/// 흐름:
///   SYSTEM_ADDRESS → BEACON_ROOTS_ADDRESS 호출
///   calldata = parent_beacon_block_root (32바이트)
///   → 컨트랙트가 timestamp를 키로 루트를 저장
///      (이후 스마트 컨트랙트에서 특정 timestamp의 루트 조회 가능)
pub fn transact_beacon_root_contract_call<HaltReason>(
    spec: impl EthereumHardforks,
    parent_beacon_block_root: Option<B256>,
    evm: &mut impl Evm<HaltReason = HaltReason>,
) -> Result<Option<ResultAndState<HaltReason>>, BlockExecutionError> {
    // Cancun 이전이면 이 시스템 콜 없음
    let timestamp = evm.block().timestamp().saturating_to::<u64>();
    if !spec.is_cancun_active_at_timestamp(timestamp) {
        return Ok(None);
    }

    // parent_beacon_block_root가 없으면 (PoW 블록 등) 스킵
    let Some(parent_beacon_block_root) = parent_beacon_block_root else {
        return Ok(None);
    };

    // BEACON_ROOTS_ADDRESS: EIP-4788에서 정의한 컨트랙트 주소
    // = 0x000F3df6D732807Ef1F25338d7BFB72e35E395a7
    // Cancun 하드포크 genesis에 배포됨
    let res = evm
        .transact_system_call(
            SYSTEM_ADDRESS,
            BEACON_ROOTS_ADDRESS,
            parent_beacon_block_root.0.into(),
        )
        .map_err(|e| BlockExecutionError::other(format!("EIP-4788 error: {e}")))?;

    Ok(Some(res))
}

// ─────────────────────────────────────────────────────────────
// state_changes.rs - 상태 변경 (보상, Withdrawal)
// ─────────────────────────────────────────────────────────────

/// 블록 실행 후 잔액 증가분 계산
///
/// 이 함수는 트랜잭션 실행과 무관한 잔액 변화를 처리:
///   1. 블록 보상 (PoW에서만, Merge 이후에는 0)
///   2. Uncle 보상 (PoW에서만)
///   3. Validator 출금 (Shanghai 이후, EIP-4895)
///
/// 반환: HashMap<Address, u128>
///   키: 잔액이 증가하는 주소
///   값: 증가량 (wei 단위)
///
/// 예시 반환값 (PoS, Shanghai 이후):
///   { 0x1234...validator_addr: 32_000_000_000_000_000_000 }  // 32 ETH 출금
pub fn post_block_balance_increments<H>(
    spec: impl EthereumHardforks,
    block_env: impl Block,             // 블록 환경 (번호, 타임스탬프 등)
    ommers: &[H],                      // Uncle 블록 헤더들
    withdrawals: Option<&Withdrawals>, // Validator 출금 목록
) -> AddressMap<u128>
// HashMap<Address, u128>과 같음
where
    H: BlockHeader,
{
    let mut balance_increments = AddressMap::new();
    let block_number = block_env.number().saturating_to::<u64>();
    let timestamp = block_env.timestamp().saturating_to::<u64>();

    // ① PoW 블록 보상 (Merge 이전에만)
    //    base_block_reward(): 하드포크에 따른 기본 블록 보상
    //      Frontier~Byzantium: 5 ETH
    //      Constantinople 이후: 2 ETH
    //      Merge 이후: 0 (None 반환)
    if let Some(base_block_reward) = calc::base_block_reward(&spec, block_number) {
        // Uncle 포함에 따른 추가 보상과 Uncle 채굴자 보상
        for ommer in ommers {
            // Uncle 채굴자 보상: 깊이에 따라 최소 1/32, 최대 7/8
            *balance_increments.entry(ommer.beneficiary()).or_default() +=
                calc::ommer_reward(base_block_reward, block_number, ommer.number());
        }
        // 블록 채굴자 보상: base + uncle 수에 따른 추가
        *balance_increments.entry(block_env.beneficiary()).or_default() +=
            calc::block_reward(base_block_reward, ommers.len());
    }

    // ② Shanghai 이후: Validator 출금 처리 (EIP-4895)
    //    각 Withdrawal: { validator_index, address, amount_in_gwei }
    //    amount_in_gwei * 1_000_000_000 = wei 단위 변환
    if spec.is_shanghai_active_at_timestamp(timestamp) {
        if let Some(withdrawals) = withdrawals {
            for withdrawal in withdrawals.iter() {
                // amount == 0이면 처리 불필요
                if withdrawal.amount > 0 {
                    *balance_increments.entry(withdrawal.address).or_default() +=
                        withdrawal.amount_wei().to::<u128>();
                    //               ^^^^^^^^^^^^
                    // amount_wei() = amount_gwei * 1_000_000_000
                }
            }
        }
    }

    balance_increments
}

/// 잔액 증가분을 EvmState(상태 변경 맵)로 변환
///
/// increment_balances()나 훅에 전달하기 위한 포맷 변환
/// HashMap<Address, u128> → EvmState(HashMap<Address, Account>)
pub fn balance_increment_state<DB: Database>(
    balance_increments: &AddressMap<u128>,
    state: &mut State<DB>,
) -> Result<EvmState, DB::Error> {
    let mut load_account = |address: &Address| state.load_cache_account(*address)?.info();

    Ok(balance_increments
        .iter()
        .filter(|(_, &amount)| amount != 0) // 0 증가는 무시
        .map(|(address, amount)| {
            let mut account_info = load_account(address)?.unwrap_or_default();
            account_info.balance += U256::from(*amount); // 잔액 증가

            let account = Account {
                info: account_info,
                storage: Default::default(),    // 스토리지 변경 없음
                status: AccountStatus::Touched, // "수정됨" 표시
            };
            (*address, account)
        })
        .collect()) // EvmState(HashMap) 생성
}
