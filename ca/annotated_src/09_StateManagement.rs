// ============================================================================
// 파일A: reth/crates/revm/src/database.rs
// 파일B: reth/crates/revm/src/cached.rs
// 역할: 상태 관리 - reth의 StateProvider를 revm의 DB 인터페이스로 연결
//
// [핵심 이해: EVM 상태 관리 스택]
//
// revm::RevmEvm (EVM 실행)
//   └─ revm::State<DB> (캐시 + 번들 상태)
//       └─ StateProviderDatabase<SP> (reth → revm 어댑터)
//           └─ SP: StateProvider (reth의 DB 추상화)
//               └─ HistoricalStateProvider (실제 DB 읽기)
//                   └─ MDBX (on-disk database)
//
// [각 레이어의 역할]
// revm::State:       현재 블록 실행 중의 인메모리 캐시 + 변경 추적
// StateProviderDatabase: reth DB API를 revm DB 인터페이스로 변환
// StateProvider:     reth의 DB 추상화 (특정 블록 높이에서 상태 읽기)
//
// [Java 대응]
// State<DB>              ≈ EntityManager (영속성 컨텍스트 = 1차 캐시)
// StateProviderDatabase  ≈ DataSource / JDBC Connection
// StateProvider          ≈ JPA Repository
// ============================================================================

// ─────────────────────────────────────────────────────────────
// Part A: StateProviderDatabase - 핵심 어댑터
// ─────────────────────────────────────────────────────────────

/// reth의 StateProvider를 revm의 Database trait으로 변환하는 어댑터
///
/// [Adapter 패턴]
/// revm은 Database trait을 통해 상태를 조회
/// reth는 StateProvider trait을 통해 상태를 제공
/// StateProviderDatabase가 이 둘을 연결!
///
/// 제네릭 파라미터:
///   DB: EvmStateProvider - reth의 상태 제공자 (StateProvider 서브셋)
///     메서드: basic_account, bytecode_by_hash, storage, block_hash 등
pub struct StateProviderDatabase<DB>(pub DB);

// ─────────────────────────────────────────────────────────────
// DatabaseRef 구현: 불변 참조로 데이터 조회
// ─────────────────────────────────────────────────────────────

impl<DB: EvmStateProvider> DatabaseRef for StateProviderDatabase<DB> {
    type Error = ProviderError;

    /// 계정 기본 정보 조회 (잔액, nonce, 코드 해시)
    ///
    /// StateProvider → revm::AccountInfo 변환
    ///
    /// 반환값:
    ///   None: 계정이 존재하지 않음 (or 빈 계정)
    ///   Some(AccountInfo { balance, nonce, code_hash, code }): 계정 정보
    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        Ok(self
            .0
            .basic_account(&address)? // reth DB 조회 (Option<Account>)
            .map(|account| AccountInfo {
                balance: account.balance,
                nonce: account.nonce,
                code_hash: account.bytecode_hash.unwrap_or(KECCAK_EMPTY),
                code: None, // 코드는 code_by_hash로 별도 조회 (lazy loading)
            }))
    }

    /// 코드 해시로 바이트코드 조회
    ///
    /// 코드가 아직 로드되지 않은 경우(code=None) 이 메서드로 가져옴
    /// KECCAK_EMPTY: 코드가 없는 EOA 계정의 코드 해시
    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        Ok(self
            .0
            .bytecode_by_hash(&code_hash)? // reth DB 조회 (Option<Bytecode>)
            .unwrap_or_default()) // 없으면 기본값 (빈 바이트코드)
    }

    /// 스토리지 슬롯 조회
    ///
    /// 예: ERC-20 컨트랙트에서 특정 주소의 잔액을 담은 슬롯
    ///     SLOAD 오피코드 실행 시 호출됨
    ///
    /// address: 컨트랙트 주소
    /// index:   스토리지 슬롯 번호 (32바이트 정수)
    ///
    /// 반환: 슬롯 값 (32바이트 정수, 없으면 0)
    fn storage_ref(&self, address: Address, index: U256) -> Result<U256, Self::Error> {
        Ok(self
            .0
            .storage(&address, StorageKey::from(index))? // reth DB 조회
            .unwrap_or_default()) // 없으면 0
    }

    /// 블록 번호로 블록 해시 조회
    ///
    /// BLOCKHASH 오피코드 실행 시 호출됨
    /// EIP-2935 이전: 최근 256블록만 지원
    /// EIP-2935 이후: 최근 8192블록 지원 (컨트랙트에서)
    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        Ok(self
            .0
            .block_hash(number)? // reth DB 조회
            .unwrap_or(B256::ZERO)) // 없으면 0 (오래된 블록)
    }
}

// Database 구현: mutable 참조로 조회 (내부적으로 DatabaseRef에 위임)
impl<DB: EvmStateProvider> Database for StateProviderDatabase<DB> {
    type Error = ProviderError;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.basic_ref(address) // DatabaseRef에 위임
    }
    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.code_by_hash_ref(code_hash)
    }
    fn storage(&mut self, address: Address, index: U256) -> Result<U256, Self::Error> {
        self.storage_ref(address, index)
    }
    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.block_hash_ref(number)
    }
}

// ─────────────────────────────────────────────────────────────
// revm의 State<DB> - 인메모리 캐시 + 상태 추적
// (이 구조체는 revm 크레이트에 있지만, 이해를 위해 설명)
// ─────────────────────────────────────────────────────────────

// State<DB>의 내부 (개념적 설명):
//
// pub struct State<DB> {
//     // ① 인메모리 계정 캐시
//     //    HashMap<Address, CacheAccount>
//     //    CacheAccount { info: AccountInfo, storage: HashMap<U256, StorageSlot> }
//     //    한 번 조회한 계정은 여기 캐시됨 (같은 블록 내 재조회 시 DB 미조회)
//     cache: CacheState,
//
//     // ② 전환 상태 (Transition State)
//     //    각 트랜잭션 실행 후의 변경사항을 기록
//     //    evm.db_mut().commit(state) 호출 시 여기에 기록
//     //    구조: [ tx0_changes, tx1_changes, tx2_changes, ... ]
//     transition_state: Option<TransitionState>,
//
//     // ③ 번들 상태 (Bundle State)
//     //    블록 전체의 변경사항 요약
//     //    각 주소별 { pre_state, post_state, storage_changes, revert_info }
//     //    merge_transitions() 호출 시 transition_state → bundle_state로 합산
//     bundle_state: BundleState,
//
//     // ④ 실제 DB (StateProviderDatabase)
//     database: DB,
//
//     // ⑤ 상태 초기화 플래그
//     //    Spurious Dragon(EIP-161) 이후: 빈 계정 자동 삭제 활성
//     with_state_clear: bool,
// }

// State<DB>의 주요 메서드:
//
// commit(state: EvmState):
//   트랜잭션 실행 결과(ResultAndState.state)를 캐시에 적용
//   → cache의 계정 정보 업데이트
//   → transition_state에 변경사항 추가
//
// merge_transitions(BundleRetention):
//   transition_state → bundle_state로 병합
//   BundleRetention::Reverts: revert 정보도 보관 (체인 재조직 대비)
//   BundleRetention::PlainState: revert 정보 제외 (최종 영속화용)
//
// take_bundle() → BundleState:
//   bundle_state를 꺼내옴 (State를 소비)
//   BlockExecutionOutput에 담겨서 반환됨
//   이후 DB 영속화는 상위 레이어(Provider)가 담당

// ─────────────────────────────────────────────────────────────
// Part B: CachedReads - Payload Building 최적화
// ─────────────────────────────────────────────────────────────

/// Payload Building(새 블록 생성) 최적화를 위한 읽기 캐시
///
/// [문제 상황]
/// Payload Building에서는 같은 부모 블록 상태에서 여러 번 시뮬레이션 실행.
/// 예: 트랜잭션 풀에서 tx를 하나씩 추가하며 최적 블록 탐색.
/// → 동일한 계정/컨트랙트 정보를 반복 조회 → DB 왕복 비용
///
/// [해결책]
/// CachedReads: 이미 읽은 데이터를 메모리에 보관
/// 다음 시뮬레이션에서 같은 데이터 요청 시 메모리에서 즉시 반환
///
/// [Java 대응]
/// CachedReads ≈ Spring의 @Cacheable 또는 직접 구현한 인메모리 캐시
pub struct CachedReads {
    /// 계정 정보 캐시
    /// 키: 이더리움 주소 (20바이트)
    /// 값: CachedAccount { info, code, storage (별도 수준) }
    pub accounts: AddressMap<CachedAccount>,

    /// 컨트랙트 바이트코드 캐시
    /// 키: 코드 해시 (32바이트 keccak256)
    /// 값: Bytecode (컴파일된 EVM 바이트코드)
    pub contracts: B256Map<Bytecode>,

    /// 블록 해시 캐시
    /// 키: 블록 번호
    /// 값: 블록 해시
    pub block_hashes: HashMap<u64, B256>,
}

/// 개별 계정의 캐시 데이터
pub struct CachedAccount {
    /// 계정 기본 정보 (잔액, nonce, code_hash)
    /// None이면 존재하지 않는 계정
    pub info: Option<AccountInfo>,

    /// 스토리지 슬롯 캐시
    /// 키: 슬롯 번호, 값: 슬롯 값
    pub storage: HashMap<U256, U256>,
}

impl CachedReads {
    /// CachedReads를 DatabaseRef로 사용하기 위한 래퍼 생성
    ///
    /// 동작:
    ///   1. 캐시에 있으면 → 즉시 반환 (DB 조회 없음)
    ///   2. 캐시에 없으면 → 원본 DB 조회 후 캐시에 저장 + 반환
    ///
    /// 사용 예:
    ///   let mut cached = CachedReads::default();
    ///   // 첫 번째 블록 시뮬레이션
    ///   let db = cached.as_db_mut(state_provider);
    ///   let state = State::builder().with_database(db).build();
    ///   // ... tx 실행 ...
    ///
    ///   // 두 번째 블록 시뮬레이션: 이전에 캐시된 데이터 재사용
    ///   let db = cached.as_db_mut(same_state_provider);
    ///   // → 이미 읽은 계정들은 캐시 히트!
    pub fn as_db_mut<DB: DatabaseRef>(&mut self, db: DB) -> CachedReadsDbMut<'_, DB> {
        CachedReadsDbMut { cached: self, db }
    }
}

/// CachedReads + 원본 DB를 합친 DatabaseRef 구현체
pub struct CachedReadsDbMut<'a, DB> {
    cached: &'a mut CachedReads,
    db: DB,
}

impl<'a, DB: DatabaseRef> DatabaseRef for CachedReadsDbMut<'a, DB> {
    type Error = DB::Error;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        // 캐시 히트 체크
        if let Some(acc) = self.cached.accounts.get(&address) {
            return Ok(acc.info.clone()); // 캐시에서 반환!
        }

        // 캐시 미스 → 원본 DB 조회
        let info = self.db.basic_ref(address)?;

        // 캐시에 저장 (다음 조회 시 캐시 히트)
        self.cached
            .accounts
            .entry(address)
            .or_insert(CachedAccount { info: info.clone(), storage: HashMap::default() });

        Ok(info)
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        if let Some(code) = self.cached.contracts.get(&code_hash) {
            return Ok(code.clone()); // 캐시 히트!
        }
        let code = self.db.code_by_hash_ref(code_hash)?;
        self.cached.contracts.insert(code_hash, code.clone());
        Ok(code)
    }

    fn storage_ref(&self, address: Address, index: U256) -> Result<U256, Self::Error> {
        // 계정이 캐시에 있고 슬롯도 캐시에 있으면
        if let Some(acc) = self.cached.accounts.get(&address) {
            if let Some(&val) = acc.storage.get(&index) {
                return Ok(val); // 캐시 히트!
            }
        }
        let val = self.db.storage_ref(address, index)?;
        // 캐시에 저장 (계정 엔트리가 없으면 새로 생성)
        self.cached
            .accounts
            .entry(address)
            .or_insert(CachedAccount { info: None, storage: HashMap::default() })
            .storage
            .insert(index, val);
        Ok(val)
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        if let Some(&hash) = self.cached.block_hashes.get(&number) {
            return Ok(hash); // 캐시 히트!
        }
        let hash = self.db.block_hash_ref(number)?;
        self.cached.block_hashes.insert(number, hash);
        Ok(hash)
    }
}

// ─────────────────────────────────────────────────────────────
// 상태 변경 커밋의 전체 흐름 요약
// ─────────────────────────────────────────────────────────────

/*
[트랜잭션 실행 → 커밋 → 병합 → 영속화]

1. evm.transact(tx_env)
   → ResultAndState {
       result: ExecutionResult (성공/실패/중단),
       state: EvmState = HashMap<Address, AccountChange>
     }
   아직 아무것도 바뀌지 않음. 변경 예정 내용을 계산만 함.

2. evm.db_mut().commit(state)  ← State<DB>.commit()
   → cache의 계정 정보 업데이트 (인메모리)
   → transition_state에 변경사항 기록
   아직 bundle_state에는 반영 안 됨.

3. db.merge_transitions(BundleRetention::Reverts)  ← 블록 완료 후
   → transition_state의 모든 변경사항을 bundle_state로 집계
   → bundle_state: { 각 주소별 시작상태, 끝상태, 스토리지변경, revert정보 }
   이 시점에 블록의 모든 변경이 bundle에 압축됨.

4. state.take_bundle() → BundleState  ← Executor 반환 후
   → BlockExecutionOutput.state에 담겨 반환
   이 BundleState가 최종 실행 결과 (DB 영속화 입력)

5. [DB 영속화 - Staged Sync의 ExecutionStage]
   BundleState를 MDBX에 씀
   → 각 계정의 최종 상태, 스토리지 변경, 코드 등 저장
   → 이전 값(revert 정보)도 저장 (chain reorg 대비)
*/
