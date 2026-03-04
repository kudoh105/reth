// ============================================================================
// 파일: reth/crates/ethereum/evm/src/lib.rs (전체)
// 역할: ConfigureEvm 트레이트의 이더리움 구현체 (EthEvmConfig)
//
// [Java 대응]
// EthEvmConfig ≈ @Configuration 클래스 (spring bean 설정)
//   → EthBlockExecutorFactory를 만들고 (executor_factory 필드)
//   → EthBlockAssembler를 만들고 (block_assembler 필드)
//   → 이 둘을 조합해서 EVM 실행의 모든 것을 구성
//
// [이 파일의 핵심 역할]
// 1. evm_env(), next_evm_env(): 블록 헤더로부터 EvmEnv 구성
// 2. context_for_block(), context_for_next_block(): 실행 컨텍스트 구성
// 3. executor(db): 블록 실행기 생성 (ConfigureEvm::executor() 기본구현으로 위임)
// ============================================================================

use alloy_evm::{
    eth::{EthBlockExecutionCtx, EthBlockExecutorFactory},
    EthEvmFactory, // 이더리움 기본 EVM 팩토리
};
use reth_evm::{ConfigureEvm, NextBlockEnvAttributes};

/// 이더리움 EVM 설정의 핵심 구조체
///
/// 제네릭 파라미터:
///   C = ChainSpec 타입 (기본값: ChainSpec)
///   EvmFactory = EVM 팩토리 타입 (기본값: EthEvmFactory)
///               커스텀 프리컴파일을 추가하려면 여기서 다른 팩토리를 주입
///
/// 사용 예:
///   let evm_config = EthEvmConfig::mainnet();
///   let executor = evm_config.executor(state_db);
///   let output = executor.execute(&block)?;
#[derive(Debug, Clone)]
pub struct EthEvmConfig<C = ChainSpec, EvmFactory = EthEvmFactory> {
    /// 블록 실행 팩토리
    ///
    /// 타입: EthBlockExecutorFactory<RethReceiptBuilder, Arc<C>, EvmFactory>
    ///
    /// 역할:
    ///   - 내부에 EvmFactory(EthEvmFactory)를 포함
    ///   - create_executor(evm, ctx) → EthBlockExecutor를 생성
    ///   - RethReceiptBuilder: 실행 결과로 reth 영수증 생성
    ///   - Arc<C>: 체인 스펙 공유 (클론 비용 없음)
    pub executor_factory: EthBlockExecutorFactory<RethReceiptBuilder, Arc<C>, EvmFactory>,

    /// 블록 조립기
    ///
    /// 역할:
    ///   - 트랜잭션 실행 결과를 받아 완성된 Block을 조립
    ///   - 헤더 필드 계산 (transactions_root, receipts_root, logs_bloom 등)
    pub block_assembler: EthBlockAssembler<C>,
}

// ─────────────────────────────────────────────────────────────
// 생성자들
// ─────────────────────────────────────────────────────────────

impl EthEvmConfig {
    /// Ethereum mainnet용 EthEvmConfig 생성
    pub fn mainnet() -> Self {
        Self::ethereum(MAINNET.clone()) // MAINNET = lazy_static으로 선언된 mainnet ChainSpec
    }
}

impl<ChainSpec> EthEvmConfig<ChainSpec> {
    /// 주어진 체인 스펙으로 EthEvmConfig 생성
    pub fn new(chain_spec: Arc<ChainSpec>) -> Self {
        Self::ethereum(chain_spec)
    }

    /// 이더리움 전용 생성자 (기본 EthEvmFactory 사용)
    pub fn ethereum(chain_spec: Arc<ChainSpec>) -> Self {
        Self::new_with_evm_factory(chain_spec, EthEvmFactory::default())
    }
}

impl<ChainSpec, EvmFactory> EthEvmConfig<ChainSpec, EvmFactory> {
    /// 커스텀 EvmFactory를 주입하여 생성
    ///
    /// 사용 예 (커스텀 프리컴파일 추가):
    ///   let config = EthEvmConfig::new_with_evm_factory(chain_spec, MyEvmFactory::default());
    pub fn new_with_evm_factory(chain_spec: Arc<ChainSpec>, evm_factory: EvmFactory) -> Self {
        Self {
            block_assembler: EthBlockAssembler::new(chain_spec.clone()),
            executor_factory: EthBlockExecutorFactory::new(
                RethReceiptBuilder::default(), // 이더리움 영수증 빌더
                chain_spec,                    // 체인 스펙 (Arc이므로 클론 없이 공유)
                evm_factory,                   // EVM 팩토리
            ),
        }
    }

    /// 내부 체인 스펙 참조 반환
    pub const fn chain_spec(&self) -> &Arc<ChainSpec> {
        self.executor_factory.spec() // EthBlockExecutorFactory에서 꺼냄
    }
}

// ─────────────────────────────────────────────────────────────
// ConfigureEvm 구현 (핵심!)
// ─────────────────────────────────────────────────────────────

impl<ChainSpec, EvmF> ConfigureEvm for EthEvmConfig<ChainSpec, EvmF>
where
    // ChainSpec 조건:
    //   EthExecutorSpec: deposit_contract_address() 제공 (EIP-6110용)
    //   EthChainSpec<Header = Header>: next_block_base_fee() 등 이더리움 특화 메서드
    //   Hardforks: 하드포크 활성화 체크 가능
    //   'static: Arc로 공유되므로 라이프타임 제한 없어야 함
    ChainSpec: EthExecutorSpec + EthChainSpec<Header = Header> + Hardforks + 'static,

    // EvmF 조건 (커스텀 EVM 팩토리가 만족해야 하는 조건들):
    EvmF: EvmFactory<
            Tx: TransactionEnv
                    + FromRecoveredTx<TransactionSigned>
                    // Recovered<TransactionSigned>로부터 변환 가능
                    + FromTxWithEncoded<TransactionSigned>, // 인코딩된 tx로부터 변환 가능
            Spec = SpecId,                // SpecId(하드포크)를 사용
            BlockEnv = BlockEnv,          // revm::BlockEnv를 사용
            Precompiles = PrecompilesMap, // PrecompilesMap을 사용
        > + Clone
        + Debug
        + Send
        + Sync
        + Unpin
        + 'static,
{
    type Primitives = EthPrimitives; // 이더리움 기본 타입들
    type Error = Infallible; // 에러 없음 (항상 성공)
    type NextBlockEnvCtx = NextBlockEnvAttributes; // 다음 블록 속성

    // 타입 별칭들:
    type BlockExecutorFactory = EthBlockExecutorFactory<RethReceiptBuilder, Arc<ChainSpec>, EvmF>;
    type BlockAssembler = EthBlockAssembler<ChainSpec>;

    fn block_executor_factory(&self) -> &Self::BlockExecutorFactory {
        &self.executor_factory // 그냥 필드 반환
    }

    fn block_assembler(&self) -> &Self::BlockAssembler {
        &self.block_assembler // 그냥 필드 반환
    }

    // ─────────────────────────────────────────────────────
    // [핵심] 블록 헤더 → EvmEnv 변환
    //
    // 이것이 "EVM 실행 준비"의 첫 단계
    // ─────────────────────────────────────────────────────
    fn evm_env(&self, header: &Header) -> Result<EvmEnv<SpecId>, Self::Error> {
        // EvmEnv::for_eth_block()이 내부적으로:
        // 1. chain_spec + timestamp + block_number → SpecId 결정
        //    예: timestamp가 cancun_activation_time 이후이면 SpecId::CANCUN
        // 2. Osaka 이후: tx_gas_limit_cap 설정 (EIP-7825)
        // 3. Cancun 이후: excess_blob_gas → blob_fee 계산 (EIP-4844)
        // 4. Paris(Merge) 이후: difficulty=0, prevrandao=mix_hash
        Ok(EvmEnv::for_eth_block(
            header,                                                       // 블록 헤더
            self.chain_spec(),              // 체인 스펙 (하드포크 정보)
            self.chain_spec().chain().id(), // chain_id (예: 1 = mainnet)
            self.chain_spec().blob_params_at_timestamp(header.timestamp), // blob 파라미터
        ))
    }

    // ─────────────────────────────────────────────────────
    // [Payload Building용] 다음 블록의 EvmEnv 생성
    //
    // 아직 없는 블록을 위한 환경 구성
    // ─────────────────────────────────────────────────────
    fn next_evm_env(
        &self,
        parent: &Header,
        attributes: &NextBlockEnvAttributes,
    ) -> Result<EvmEnv, Self::Error> {
        Ok(EvmEnv::for_eth_next_block(
            parent, // 현재 head 블록 헤더
            // CL이 제공한 속성값들을 NextEvmEnvAttributes로 변환
            NextEvmEnvAttributes {
                timestamp: attributes.timestamp,
                suggested_fee_recipient: attributes.suggested_fee_recipient,
                prev_randao: attributes.prev_randao,
                gas_limit: attributes.gas_limit,
            },
            // next_block_base_fee(): EIP-1559 공식으로 다음 블록의 basefee 계산
            // 예: 이전 블록이 가스를 절반만 사용했으면 basefee 감소
            self.chain_spec().next_block_base_fee(parent, attributes.timestamp).unwrap_or_default(),
            self.chain_spec(),
            self.chain_spec().chain().id(),
            self.chain_spec().blob_params_at_timestamp(attributes.timestamp),
        ))
    }

    // ─────────────────────────────────────────────────────
    // 기존 블록의 실행 컨텍스트 생성
    //
    // EthBlockExecutionCtx에는 EVM 환경과 별도로 필요한 블록 데이터가 담김
    // ─────────────────────────────────────────────────────
    fn context_for_block<'a>(
        &self,
        block: &'a SealedBlock<Block>,
    ) -> Result<EthBlockExecutionCtx<'a>, Self::Error> {
        Ok(EthBlockExecutionCtx {
            // 트랜잭션 수 힌트: Vec::with_capacity()로 사전 할당하여 성능 최적화
            tx_count_hint: Some(block.transaction_count()),

            // EIP-2935 시스템 콜에 사용: blockhashes 컨트랙트에 부모 해시 저장
            parent_hash: block.header().parent_hash,

            // EIP-4788 시스템 콜에 사용: beacon roots 컨트랙트에 저장
            parent_beacon_block_root: block.header().parent_beacon_block_root,

            // Uncle 블록들 (PoW 보상 계산에 필요, PoS에서는 항상 빈 배열)
            // &[] 는 빈 슬라이스(참조)
            ommers: &block.body().ommers,

            // EIP-4895: Validator 출금 목록 (Shanghai 이후)
            // Cow::Borrowed: 블록 데이터를 복사하지 않고 참조만 빌림 (메모리 효율)
            // Cow = Clone-on-Write: 수정이 필요할 때만 복사
            withdrawals: block.body().withdrawals.as_ref().map(Cow::Borrowed),

            // 블록 헤더의 extra_data 필드 (32바이트 자유 데이터)
            extra_data: block.header().extra_data.clone(),
        })
    }

    // ─────────────────────────────────────────────────────
    // Payload Building용 실행 컨텍스트 생성
    //
    // 아직 없는 블록을 위한 컨텍스트
    // ─────────────────────────────────────────────────────
    fn context_for_next_block(
        &self,
        parent: &SealedHeader,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<EthBlockExecutionCtx<'_>, Self::Error> {
        Ok(EthBlockExecutionCtx {
            tx_count_hint: None, // 아직 몇 개의 tx가 들어올지 모름

            // 부모(=현재 head) 블록의 해시
            parent_hash: parent.hash(),

            // CL이 제공한 비콘 블록 루트
            parent_beacon_block_root: attributes.parent_beacon_block_root,

            // 새 블록에는 uncle이 없음 (PoS)
            ommers: &[],

            // Cow::Owned: attributes에서 withdrawals를 가져오므로 소유
            withdrawals: attributes.withdrawals.map(Cow::Owned),

            extra_data: attributes.extra_data,
        })
    }
}

// ─────────────────────────────────────────────────────────────
// ConfigureEngineEvm 구현 (Engine API용 - 페이로드 검증 시 사용)
//
// ExecutionPayload(CL에서 온 페이로드)로부터 환경을 구성하는 추가 구현
// ─────────────────────────────────────────────────────────────
#[cfg(feature = "std")]
impl<ChainSpec, EvmF> ConfigureEngineEvm<ExecutionData> for EthEvmConfig<ChainSpec, EvmF>
where
/* 위와 동일한 제약 */
{
    /// ExecutionPayload로부터 EvmEnv 생성
    /// Engine API를 통해 받은 페이로드(다른 노드가 만든 블록)를 검증할 때 사용
    fn evm_env_for_payload(&self, payload: &ExecutionData) -> Result<EvmEnvFor<Self>, Self::Error> {
        // 페이로드에서 timestamp, block_number 등을 꺼내서 EvmEnv 구성
        // evm_env()와 유사하지만 ExecutionPayload를 입력으로 받음
        let timestamp = payload.payload.timestamp();
        let spec =
            revm_spec_by_timestamp_and_block_number(self.chain_spec(), timestamp, block_number);
        // ... EvmEnv 구성 ...
    }

    /// ExecutionPayload에서 트랜잭션 이터레이터 생성
    /// 페이로드의 RLP 인코딩된 tx들을 decode하고 서명을 복구함
    fn tx_iterator_for_payload(
        &self,
        payload: &ExecutionData,
    ) -> Result<impl ExecutableTxIterator<Self>, Self::Error> {
        let txs = payload.payload.transactions().clone();
        let convert = |tx: Bytes| {
            // RLP 디코드: raw bytes → TransactionSigned
            let tx = TransactionSigned::decode_2718_exact(tx.as_ref())?;
            // 서명 복구: signer 주소 복구
            let signer = tx.try_recover()?;
            Ok(tx.with_signer(signer)) // Recovered<TransactionSigned> 반환
        };
        Ok((txs, convert)) // (tx_bytes_vec, convert_fn) 튜플 반환
    }
}
