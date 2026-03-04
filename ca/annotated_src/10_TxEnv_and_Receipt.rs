// ============================================================================
// 파일A: reth/crates/ethereum/evm/src/receipt.rs
// 파일B: alloy-evm-0.27.2/src/eth/receipt_builder.rs
// 파일C: 트랜잭션 → TxEnv 변환 (FromRecoveredTx의 이더리움 구현)
//
// 역할: 트랜잭션과 영수증의 변환 로직
// ============================================================================

// ─────────────────────────────────────────────────────────────
// Part A: 트랜잭션 → TxEnv 변환
//
// 실행 흐름에서:
//   Recovered<TransactionSigned>
//   → FromRecoveredTx::from_recovered_tx()
//   → TxEnv (revm이 이해하는 포맷)
// ─────────────────────────────────────────────────────────────

/// Trait: "Recovered<T>(서명 복구된 트랜잭션)로부터 TxEnv를 만들 수 있다"
///
/// [Java 대응]
/// interface TxEnvConverter {
///     TxEnv fromRecoveredTx(Recovered<T> tx);
/// }
pub trait FromRecoveredTx<T> {
    /// Recovered<T>를 Self(TxEnv)로 변환
    fn from_recovered_tx(tx: &T, sender: Address) -> Self;
}

/// TxEnv가 TransactionSigned로부터 변환 가능하게 구현
///
/// TransactionSigned: 이더리움 서명된 트랜잭션
///   - tx: TxEnvelope (Legacy, EIP-2930, EIP-1559, EIP-4844, EIP-7702 중 하나)
///   - signature: 서명 (v, r, s)
///   - hash: 트랜잭션 해시 (캐시됨)
///
/// Recovered<TransactionSigned>: 서명에서 sender 주소를 복구한 것
///   - tx: TransactionSigned
///   - signer: Address (복구된 sender 주소)
impl FromRecoveredTx<TransactionSigned> for TxEnv {
    fn from_recovered_tx(tx: &TransactionSigned, sender: Address) -> Self {
        let mut tx_env = TxEnv::default();

        // ① 공통 필드 설정
        tx_env.caller = sender; // sender 주소 (서명 복구값)
        tx_env.gas_limit = tx.tx().gas_limit(); // 가스 한도
        tx_env.value = tx.tx().value(); // 전송 ETH (wei)
        tx_env.data = tx.tx().input().clone(); // calldata
        tx_env.nonce = tx.tx().nonce(); // nonce
        tx_env.chain_id = tx.tx().chain_id(); // chain ID
        tx_env.access_list = tx.tx().access_list().cloned().unwrap_or_default(); // EIP-2930

        // ② to 필드: CREATE or CALL
        //    TxKind::Create: 컨트랙트 배포 (to 없음)
        //    TxKind::Call(addr): 함수 호출 또는 ETH 전송
        tx_env.kind = match tx.tx().to() {
            TxKind::Create => TransactTo::Create,
            TxKind::Call(addr) => TransactTo::Call(addr),
        };

        // ③ 가스 가격 설정 (트랜잭션 타입별로 다름)
        match tx.tx().inner() {
            // Legacy(Type0) + EIP-2930(Type1): gas_price 직접 지정
            TxEnvelope::Legacy(tx) => {
                tx_env.gas_price = tx.tx().gas_price;
                tx_env.max_fee_per_gas = tx.tx().gas_price;
                tx_env.max_priority_fee_per_gas = None;
            }
            // EIP-1559(Type2): max_fee_per_gas + max_priority_fee_per_gas
            TxEnvelope::Eip1559(tx) => {
                tx_env.gas_price = tx.tx().max_fee_per_gas;
                tx_env.max_fee_per_gas = tx.tx().max_fee_per_gas;
                tx_env.max_priority_fee_per_gas = Some(tx.tx().max_priority_fee_per_gas);
            }
            // EIP-4844(Type3): blob 트랜잭션
            TxEnvelope::Eip4844(tx) => {
                tx_env.gas_price = tx.tx().tx().max_fee_per_gas;
                tx_env.max_fee_per_gas = tx.tx().tx().max_fee_per_gas;
                tx_env.max_priority_fee_per_gas = Some(tx.tx().tx().max_priority_fee_per_gas);
                // blob 관련 필드 (Cancun 이후)
                tx_env.blob_hashes = Some(tx.tx().tx().blob_versioned_hashes.clone());
                tx_env.max_fee_per_blob_gas = Some(tx.tx().tx().max_fee_per_blob_gas);
            }
            // EIP-7702(Type4): 코드 위임 트랜잭션 (Prague 이후)
            TxEnvelope::Eip7702(tx) => {
                tx_env.gas_price = tx.tx().max_fee_per_gas;
                // authorization_list: 위임할 코드 목록
                tx_env.authorization_list = Some(tx.tx().authorization_list.clone());
            }
        }

        tx_env
    }
}

// TxEnv 구조체 설명 (revm의 타입)
struct TxEnv {
    /// sender 주소 (서명에서 복구)
    pub caller: Address,

    /// 가스 한도 (이 tx가 최대로 쓸 수 있는 가스)
    pub gas_limit: u64,

    /// 실행에 사용할 가스 가격 (기본: max_fee_per_gas)
    pub gas_price: u128,

    /// EIP-1559: 최대 가스 가격 (base_fee + tip을 합쳐 이 이상은 안 냄)
    pub max_fee_per_gas: u128,

    /// EIP-1559: 최대 팁 (validator에게 주는 최대 우선순위 가격)
    pub max_priority_fee_per_gas: Option<u128>,

    /// 전송 ETH 양 (wei 단위)
    pub value: U256,

    /// 호출 대상: Create(컨트랙트 배포) 또는 Call(주소)
    pub kind: TransactTo,

    /// calldata (함수 호출 인코딩 또는 컨트랙트 배포 bytecode)
    pub data: Bytes,

    /// EIP-2930: 상태 접근 목록 (사전에 warm 처리할 주소/슬롯 목록)
    pub access_list: AccessList,

    /// nonce (replay attack 방지용 증가하는 카운터)
    pub nonce: u64,

    /// chain ID (네트워크 식별자, replay attack 방지)
    pub chain_id: Option<u64>,

    /// EIP-4844: blob 해시 목록 (blob tx만 해당)
    pub blob_hashes: Option<Vec<B256>>,

    /// EIP-4844: 최대 blob 가스 가격
    pub max_fee_per_blob_gas: Option<u128>,

    /// EIP-7702: 코드 위임 목록
    pub authorization_list: Option<Vec<SignedAuthorization>>,
}

// ─────────────────────────────────────────────────────────────
// Part B: ReceiptBuilder trait + AlloyReceiptBuilder
// ─────────────────────────────────────────────────────────────

/// 트랜잭션 실행 결과 → 영수증 변환 인터페이스
///
/// [Java 대응]
/// interface ReceiptBuilder {
///     Receipt buildReceipt(ReceiptBuilderCtx ctx);
/// }
pub trait ReceiptBuilder {
    type Transaction; // 원본 트랜잭션 타입
    type Receipt; // 생성할 영수증 타입
    type Builder<T>: ReceiptBuilder; // 제네릭 영수증 타입으로 변환용

    /// [핵심] 실행 컨텍스트로부터 영수증 생성
    fn build_receipt<E: Evm>(&self, ctx: ReceiptBuilderCtx<'_, TxType, E>) -> Self::Receipt;
}

/// 영수증 생성에 필요한 컨텍스트
pub struct ReceiptBuilderCtx<'a, TxType, E> {
    /// 원본 트랜잭션 참조 (영수증에 tx_type 등을 포함하기 위해)
    pub tx: &'a impl RecoveredTx<Self::Tx>,

    /// 트랜잭션 타입 (0=Legacy, 1=EIP-2930, 2=EIP-1559, 3=EIP-4844, 4=EIP-7702)
    pub tx_type: TxType,

    /// EVM 실행 결과
    /// ExecutionResult {
    ///   Success { output: Bytes, gas_used, gas_refunded, logs }
    ///   Revert { output: Bytes, gas_used }
    ///   Halt { reason: HaltReason, gas_used }
    /// }
    pub result: &'a ExecutionResult<E::HaltReason>,

    /// 이 tx가 실행된 후의 블록 내 누적 가스 사용량
    /// 영수증의 cumulative_gas_used 필드에 들어감
    pub cumulative_gas_used: u64,

    /// EVM 인스턴스 (추가 정보 접근용, 옵셔널)
    pub evm: &'a E,
}

// ─────────────────────────────────────────────────────────────
// Part C: RethReceiptBuilder (reth의 이더리움 영수증 빌더)
// ─────────────────────────────────────────────────────────────

/// reth 전용 영수증 빌더
///
/// alloy-evm의 AlloyReceiptBuilder와 달리,
/// reth 자체의 Receipt 타입을 생성
#[derive(Debug, Default, Clone)]
pub struct RethReceiptBuilder;

impl ReceiptBuilder for RethReceiptBuilder {
    type Transaction = TransactionSigned;
    type Receipt = Receipt; // reth_ethereum_primitives::Receipt

    fn build_receipt<E: Evm>(&self, ctx: ReceiptBuilderCtx<'_, TxType, E>) -> Receipt {
        let ReceiptBuilderCtx { tx_type, result, cumulative_gas_used, .. } = ctx;

        Receipt {
            /// 트랜잭션 타입 (영수증 타입 식별용)
            /// Type0=Legacy, Type1=EIP-2930, Type2=EIP-1559 등
            tx_type,

            /// 실행 성공 여부 (EIP-658에서 추가된 필드)
            /// true: Success, false: Revert 또는 Halt
            success: result.is_success(),

            /// 이 tx 포함 블록 시작부터의 누적 가스 사용량
            /// 영수증 트라이 계산 및 가스 잔여량 계산에 사용
            cumulative_gas_used,

            /// 이 트랜잭션이 emit한 이벤트 로그들
            /// Log { address, topics: Vec<B256>, data: Bytes }
            /// ERC-20: Transfer(from, to, amount) 같은 이벤트
            logs: result.into_logs().collect(),
        }
    }
}

// reth_ethereum_primitives::Receipt 구조체
struct Receipt {
    /// 트랜잭션 유형 (RLP 인코딩 시 prefix에 사용)
    pub tx_type: TxType, // 0, 1, 2, 3, 4

    /// EIP-658: 실행 결과 상태 코드
    /// true = 성공, false = 실패(revert/halt)
    /// (이전: Byzantium 이전에는 root = post-state root였음)
    pub success: bool,

    /// 블록 내 이 tx까지의 누적 가스 사용량
    /// 다음 tx의 가스 계산 시작점이 됨
    pub cumulative_gas_used: u64,

    /// 이 tx가 emit한 이벤트 로그 목록
    /// Bloom 필터 계산의 입력이 됨
    pub logs: Vec<Log>,
}

// ─────────────────────────────────────────────────────────────
// ExecutionResult - 실행 결과 열거형
// ─────────────────────────────────────────────────────────────

// revm의 ExecutionResult<HaltReason>:
enum ExecutionResult<HaltReason> {
    /// 실행 성공
    Success {
        /// 실행 이후 refund된 가스 (EIP-3529: max refund = gas_used / 5)
        gas_refunded: u64,
        /// 실제 사용된 가스 (refund 제외)
        gas_used: u64,
        /// 실행 결과 출력
        output: Output, // Output::Call(Bytes) 또는 Output::Create(Bytes, Option<Address>)
        /// 이 tx가 emit한 로그
        logs: Vec<Log>,
    },

    /// 실행 중 revert (예: require(false), assert 실패)
    Revert {
        /// 사용된 가스 (남은 가스는 반환됨)
        gas_used: u64,
        /// revert 이유 (보통 ABI 인코딩된 Error 타입)
        output: Bytes,
    },

    /// 실행 중 halt (비정상 종료)
    Halt {
        /// 중단 이유
        reason: HaltReason,
        // OutOfGas: 가스 소진
        // StackOverflow: 스택 1024 깊이 초과
        // InvalidJumpDestination: JUMP 대상이 JUMPDEST가 아님
        // InvalidFEOpcode: INVALID 오피코드 실행
        // ...
        /// 사용된 가스 (전부 소진됨, refund 없음)
        gas_used: u64,
    },
}
