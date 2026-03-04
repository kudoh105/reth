// ============================================================================
// 파일A: reth/crates/ethereum/evm/src/build.rs
// 역할: 블록 조립기 - 실행 결과로 완성된 블록 만들기 (Payload Building용)
//
// 파일B: reth/crates/evm/evm/src/execute.rs (BasicBlockExecutor 부분)
// 역할: reth의 Executor/BlockBuilder trait을 구현하는 래퍼
// ============================================================================

// ============================================================================
// Part A: EthBlockAssembler (블록 조립기)
//
// [용도]
// Payload Building(새 블록 생성) 시에만 사용.
// 기존 블록 검증 시에는 사용 안 함 (이미 완성된 블록이 있으므로).
//
// [역할]
// 트랜잭션 실행 결과 → 완성된 Block 객체 조립
// 즉, 헤더 필드들을 계산하고 Block 구조체를 만들어 반환
// ============================================================================

/// 이더리움 블록 조립기
#[derive(Debug, Clone)]
pub struct EthBlockAssembler<C> {
    chain_spec: Arc<C>,  // 포크 규칙 확인용 체인 스펙
}

impl<C> EthBlockAssembler<C> {
    pub fn new(chain_spec: Arc<C>) -> Self {
        Self { chain_spec }
    }
}

impl<F, C> BlockAssembler<F> for EthBlockAssembler<C>
where
    // F는 BlockExecutorFactory여야 하고, 이더리움과 호환되는 타입들을 가져야 함
    F: BlockExecutorFactory<EvmFactory: EvmFactory<BlockEnv = BlockEnv, Spec = SpecId>, ...>,
    C: EthChainSpec<Header = Header> + Hardforks,
{
    type Block = Block;  // 출력 타입: 완성된 이더리움 블록

    /// [핵심] 실행 결과로부터 완성된 블록 조립
    ///
    /// 입력: BlockAssemblerInput {
    ///   evm_env: EvmEnv,             // 블록 환경 (번호, 타임스탬프 등)
    ///   execution_ctx: BlockCtx,     // 실행 컨텍스트 (parent_hash 등)
    ///   parent: SealedHeader,        // 부모 블록 헤더
    ///   transactions: Vec<Tx>,       // 실행된 트랜잭션 목록
    ///   output: BlockExecutionResult,// 실행 결과 (receipts, requests, gas_used)
    ///   bundle_state: BundleState,   // 상태 변경 집합
    ///   state_provider: StateProvider,// 원본 DB 접근
    ///   state_root: B256,            // 계산된 상태 루트 (머클 루트)
    /// }
    fn assemble_block(
        &self,
        input: BlockAssemblerInput<'_, '_, F>,
    ) -> Result<Block, BlockExecutionError> {

        // 입력 구조체 분해 (Rust의 구조체 패턴 매칭/분해)
        // Java: 각각 input.getXxx()로 가져오는 것과 동일
        let BlockAssemblerInput {
            evm_env,
            execution_ctx: ctx,
            parent,
            transactions,
            output: BlockExecutionResult { receipts, requests, gas_used, blob_gas_used: _ },
            bundle_state: _,    // 이 구현에서는 직접 사용 안 함
            state_provider: _,  // 이 구현에서는 직접 사용 안 함
            state_root,         // 트라이 계산으로 이미 구해진 상태 루트
        } = input;

        let timestamp = evm_env.block_env.timestamp.saturating_to::<u64>();
        let spec = evm_env.cfg_env.spec;

        // ─────────────────────────────────────────────
        // 헤더 필드 계산
        // ─────────────────────────────────────────────

        // 트랜잭션 루트: 모든 tx를 Merkle Trie로 해시 (노란 페이퍼의 Tₓ)
        // 이 값을 헤더에 넣으면 다른 노드가 tx 목록의 유효성을 검증 가능
        let transactions_root = proofs::calculate_transaction_root(&transactions);

        // 영수증 루트: 모든 receipt를 Merkle Trie로 해시 (노란 페이퍼의 Tᵣ)
        let receipts_root = calculate_receipt_root_optimism_or_ethereum(
            &receipts,
            self.chain_spec.as_ref(),
            timestamp,
        );

        // 로그 블룸 필터: 모든 로그의 주소+토픽을 2048비트 필터에 합산
        // 이벤트 필터링 시 빠른 "아마도 있음/확실히 없음" 판별에 사용
        let logs_bloom = logs_bloom(receipts.iter().flat_map(|r| r.logs()));

        // Shanghai 이후: Withdrawal 루트 계산
        // 출금 목록을 Merkle Trie로 해시 → 헤더의 withdrawals_root 필드
        let withdrawals = if spec.has_withdrawals() {
            ctx.withdrawals.as_deref().cloned()  // EthBlockExecutionCtx에서 가져옴
        } else {
            None
        };
        let withdrawals_root = withdrawals.as_deref().map(calculate_withdrawals_root);

        // Prague 이후: 요청 해시 계산
        // 디포짓/출금/통합 요청들을 해시 → 헤더의 requests_hash 필드
        let requests_hash = if spec.is_prague_active_at_timestamp(timestamp) {
            Some(requests.requests_hash())  // SHA-256 해시
        } else {
            None
        };

        // Cancun 이후: blob 가스 필드
        let blobs_and_excess_gas = if spec.is_cancun_active_at_timestamp(timestamp) {
            let parent_blob_gas_used = parent.blob_gas_used.unwrap_or(0);
            let parent_excess_blob_gas = parent.excess_blob_gas.unwrap_or(0);
            // EIP-4844: excess_blob_gas 계산공식
            let excess_blob_gas = calc_excess_blob_gas(
                parent_excess_blob_gas,
                parent_blob_gas_used,
                /* target_blob_count */
            );
            Some((blob_gas_used, excess_blob_gas))
        } else {
            None
        };

        // ─────────────────────────────────────────────
        // 최종 헤더 + 블록 조립
        // ─────────────────────────────────────────────

        let header = Header {
            // 기본 필드들
            parent_hash: ctx.parent_hash,         // 부모 블록 해시
            ommers_hash: EMPTY_OMMER_HASH,         // PoS에서는 항상 Uncle 없음
            beneficiary: evm_env.block_env.beneficiary(), // 수수료 수신자
            state_root,                            // 이미 계산된 상태 머클 루트
            transactions_root,                     // 방금 계산
            receipts_root,                         // 방금 계산
            withdrawals_root,                      // 방금 계산 (Shanghai~)
            logs_bloom,                            // 방금 계산
            difficulty: U256::ZERO,                // PoS는 항상 0
            number: evm_env.block_env.number().saturating_to(),
            gas_limit: evm_env.block_env.gas_limit(),
            gas_used: *gas_used,                   // 실행 결과에서 가져옴
            timestamp,
            mix_hash: evm_env.block_env.prevrandao().unwrap_or_default(), // PoS 랜덤값
            nonce: B64::ZERO,                      // PoS는 항상 0
            base_fee_per_gas: Some(evm_env.block_env.basefee()),
            blob_gas_used: blobs_and_excess_gas.map(|(used, _)| used),
            excess_blob_gas: blobs_and_excess_gas.map(|(_, excess)| excess),
            parent_beacon_block_root: ctx.parent_beacon_block_root,
            requests_hash,
            extra_data: ctx.extra_data.clone(),
        };

        // 완성된 블록 반환
        Ok(Block {
            header,
            body: BlockBody {
                transactions,         // 실행된 트랜잭션들
                ommers: Vec::new(),   // PoS: Uncle 없음
                withdrawals,          // Validator 출금 (Shanghai~)
            },
        })
    }
}

// ============================================================================
// Part B: BasicBlockExecutor (reth의 Executor 래퍼)
//
// [역할]
// ConfigureEvm::executor()가 반환하는 타입.
// 내부에 State<DB>를 직접 보유하면서, 여러 블록을 연속 실행.
//
// BlockExecutorFactory::create_executor()가 반환하는 EthBlockExecutor와 달리,
// BasicBlockExecutor는 State<DB>를 직접 소유하고 관리.
// ============================================================================

/// reth의 Executor trait을 구현하는 핵심 래퍼
///
/// 이 구조체가 evm_config.executor(db)의 반환값!
///
/// 포함 요소:
///   strategy_factory: F (ConfigureEvm 구현체 = EthEvmConfig)
///   db: State<DB> (EVM 실행 상태를 담는 revm의 상태 관리 구조체)
pub struct BasicBlockExecutor<F, DB> {
    /// 블록 실행 전략 팩토리 (= ConfigureEvm 구현체)
    /// 블록마다 EVM과 BlockExecutor를 새로 만들 때 사용
    pub(crate) strategy_factory: F,

    /// EVM 실행 중 상태를 보관하는 구조체 (revm::State)
    ///
    /// State<DB>의 내부:
    ///   cache:           CacheState (인메모리 계정/코드 캐시)
    ///   transition_state: TransitionState (각 tx의 변경사항 추적)
    ///   bundle_state:    BundleState (모든 tx 변경사항의 압축 요약)
    ///   database:        DB (실제 데이터 접근, StateProviderDatabase)
    ///   with_bundle_update: bool (bundle 누적 여부)
    pub(crate) db: State<DB>,
}

impl<F, DB: Database> BasicBlockExecutor<F, DB> {
    pub fn new(strategy_factory: F, db: DB) -> Self {
        // State 빌더로 구성:
        //   with_bundle_update(): bundle_state를 누적함 (여러 블록 실행에 필요)
        //   without_state_clear(): 초기 state_clear_flag = false
        let db = State::builder()
            .with_database(db)
            .with_bundle_update()    // 여러 블록에 걸친 상태 변경 추적
            .without_state_clear()   // Spurious Dragon 이전 호환성
            .build();
        Self { strategy_factory, db }
    }
}

// Executor<DB>: 블록 실행 인터페이스
// [Java 대응] interface Executor { ExecutionOutput execute(Block block); }
impl<F, DB> Executor<DB> for BasicBlockExecutor<F, DB>
where
    F: ConfigureEvm,  // EthEvmConfig가 ConfigureEvm을 구현
    DB: Database,
{
    type Primitives = F::Primitives;    // EthPrimitives
    type Error = BlockExecutionError;

    /// [블록 검증 메인 메서드] 블록 하나를 실행
    ///
    /// 내부 흐름:
    ///   1. executor_for_block(): EthBlockExecutor 생성
    ///   2. execute_block(): 전처리 + 트랜잭션들 실행 + 후처리
    ///   3. merge_transitions(): 이번 블록의 변경을 bundle에 합산
    fn execute_one(
        &mut self,
        block: &RecoveredBlock<Block>,  // 서명 복구된 트랜잭션들을 포함한 블록
    ) -> Result<BlockExecutionResult<Receipt>, BlockExecutionError> {

        // ① EVM + 컨텍스트 + BlockExecutor 한 번에 생성
        //    내부: evm_env(header) → evm_with_env(db) → context_for_block(block)
        //         → block_executor_factory().create_executor(evm, ctx)
        let result = self.strategy_factory
            .executor_for_block(&mut self.db, block)?

            // ② 전체 블록 실행 (전처리 + tx들 + 후처리)
            //    transactions_recovered(): &[Recovered<TransactionSigned>] 이터레이터
            .execute_block(block.transactions_recovered())?;

        // ③ 이번 블록의 상태 변경을 bundle_state에 병합
        //    BundleRetention::Reverts: revert 정보도 함께 저장
        //    (체인 재조직(reorg) 시 상태를 되돌릴 수 있게)
        self.db.merge_transitions(BundleRetention::Reverts);

        Ok(result)
    }

    /// State 훅과 함께 블록 실행 (디버깅/모니터링용)
    fn execute_one_with_state_hook<H: OnStateHook + 'static>(
        &mut self,
        block: &RecoveredBlock<Block>,
        state_hook: H,  // 각 상태 변경마다 호출될 콜백
    ) -> Result<BlockExecutionResult<Receipt>, BlockExecutionError> {
        let result = self.strategy_factory
            .executor_for_block(&mut self.db, block)?
            .with_state_hook(Some(Box::new(state_hook)))  // 훅 등록
            .execute_block(block.transactions_recovered())?;

        self.db.merge_transitions(BundleRetention::Reverts);
        Ok(result)
    }

    /// 내부 State<DB>를 소비하고 반환
    /// 배치 실행 완료 후 최종 상태를 꺼낼 때 사용
    fn into_state(self) -> State<DB> {
        self.db
    }

    /// 현재 bundle_state의 추정 메모리 크기 반환 (성능 모니터링용)
    fn size_hint(&self) -> usize {
        self.db.bundle_state.size_hint()
    }
}

// ============================================================================
// BasicBlockBuilder - BlockBuilder trait을 구현하는 래퍼 (Payload Building용)
// ============================================================================

/// Block Building용 래퍼
/// BasicBlockExecutor와 달리 BlockAssembler도 함께 갖고 있어서
/// 실행 결과로 완성된 Block 객체를 만들 수 있음
pub struct BasicBlockBuilder<'a, F, Executor, Builder, N> {
    /// 실제 트랜잭션 실행기 (EthBlockExecutor)
    pub executor: Executor,

    /// 블록 실행 컨텍스트 (parent_hash, withdrawals 등)
    pub ctx: <F as BlockExecutorFactory>::ExecutionCtx<'a>,

    /// 블록 조립기 (실행 결과 → Block)
    pub assembler: &'a Builder,

    /// 부모 블록 헤더 (다음 블록 번호 계산 등에 사용)
    pub parent: &'a SealedHeader<N::BlockHeader>,

    /// 실행된 트랜잭션 목록 (나중에 block body에 들어감)
    pub transactions: Vec<Recovered<N::SignedTx>>,
}

impl<'a, F, DB, Executor, Builder, N> BlockBuilder
    for BasicBlockBuilder<'a, F, Executor, Builder, N>
where
    F: BlockExecutorFactory<Transaction = N::SignedTx, Receipt = N::Receipt>,
    Executor: BlockExecutor<...>,
    Builder: BlockAssembler<F, Block = N::Block>,
    N: NodePrimitives,
{
    type Primitives = N;
    type Executor = Executor;

    /// 전처리 위임 (EthBlockExecutor::apply_pre_execution_changes 호출)
    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.executor.apply_pre_execution_changes()
    }

    /// 트랜잭션 실행 + 선택적 커밋
    ///
    /// f 클로저: 실행 결과를 받아서 CommitChanges::Yes/No 반환
    /// CommitChanges::Yes → 커밋 + 트랜잭션을 self.transactions에 추가
    /// CommitChanges::No  → 커밋 안 함 (예: 가스 한도 초과로 포함 불가)
    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutorTx<Self::Executor>,
        f: impl FnOnce(&ExecutionResult<...>) -> CommitChanges,
    ) -> Result<Option<u64>, BlockExecutionError> {
        let (tx_env, tx) = tx.into_parts();

        // EthBlockExecutor에 실행 위임 (tx와 Recovered<Tx>의 참조를 함께 전달)
        if let Some(gas_used) =
            self.executor.execute_transaction_with_commit_condition((tx_env, &tx), f)?
        {
            self.transactions.push(tx);  // 커밋된 tx만 목록에 추가
            Ok(Some(gas_used))
        } else {
            Ok(None)  // 미포함 tx
        }
    }

    /// [블록 완성] 상태 루트 계산 + 블록 조립
    ///
    /// 이 메서드 호출 후 완성된 블록을 반환
    fn finish(
        self,
        state: impl StateProvider,  // 원본 DB 접근 (상태 루트 계산에 필요)
    ) -> Result<BlockBuilderOutcome<N>, BlockExecutionError> {

        // ① BlockExecutor의 finish() 호출 → (EVM, 실행 결과)
        let (evm, result) = self.executor.finish()?;

        // ② EVM에서 DB(State<DB>)와 EvmEnv 분리
        let (db, evm_env) = evm.finish();

        // ③ 상태 변경사항을 bundle로 병합
        //    BundleRetention::Reverts: revert 데이터도 보관
        db.merge_transitions(BundleRetention::Reverts);

        // ④ 상태 루트(State Root) 계산
        //    hashed_post_state: 모든 변경된 계정의 해시 (Merkle Trie 입력)
        //    state_root: 최종 Merkle Patricia Trie 루트 해시
        //    trie_updates: 변경된 노드 목록 (DB에 영속화에 사용)
        let hashed_state = state.hashed_post_state(&db.bundle_state);
        let (state_root, trie_updates) = state
            .state_root_with_updates(hashed_state.clone())
            .map_err(BlockExecutionError::other)?;

        // ⑤ 트랜잭션 목록을 (tx, signer) 쌍으로 분리
        let (transactions, senders) =
            self.transactions.into_iter().map(|tx| tx.into_parts()).unzip();
        //                                                              ^^^^^
        //  .unzip(): [(tx1, addr1), (tx2, addr2)] → ([tx1, tx2], [addr1, addr2])
        //  Java: two separate lists from a list of pairs

        // ⑥ 블록 조립기(EthBlockAssembler)로 완성된 블록 생성
        let block = self.assembler.assemble_block(BlockAssemblerInput {
            evm_env,                    // 블록 환경 (번호, 타임스탬프 등)
            execution_ctx: self.ctx,    // 컨텍스트 (parent_hash 등)
            parent: self.parent,        // 부모 헤더
            transactions,               // 실행된 tx 목록
            output: &result,            // 영수증, 가스, requests
            bundle_state: &db.bundle_state, // 상태 변경 집합
            state_provider: &state,     // 원본 DB 접근
            state_root,                 // 계산된 상태 루트
        })?;

        // ⑦ 서명 복구된 블록으로 포장
        //    RecoveredBlock: block + senders(각 tx의 서명자 주소 목록)
        let block = RecoveredBlock::new_unhashed(block, senders);

        // ⑧ 최종 결과 반환
        Ok(BlockBuilderOutcome {
            execution_result: result,     // 영수증, 요청, 가스
            hashed_state,                 // 해시된 상태 (DB 영속화에 사용)
            trie_updates,                 // 트라이 변경사항 (DB 영속화에 사용)
            block,                        // 완성된 블록!
        })
    }
}
