# Commonware + Reth 통합 구현 상세 (Implementation)

이 문서는 Reth(실행 계층)와 Commonware(합의 계층 및 P2P)를 단일 프로세스(`private-node`)에서 어떻게 통합하고 구현했는지에 대한 기술적 세부 사항을 다룹니다.

## 1. 아키텍처 및 생명주기 (Lifecycle)

이 프로젝트는 Ethereum의 일반적인 "Execution Client + Consensus Client" 분리 구조를 **단일 바이너리(Single Process)**로 결합한 형태입니다.

- **바이너리 진입점 (`private-node`)**: `bin/private-node/src/main.rs`
- **스레드 분리**: 
  - 메인 스레드는 Reth의 기본 `NodeBuilder`를 통해 이더리움 실행 계층(EL) 노드를 띄우고, `tokio` 비동기 런타임 위에서 동작합니다.
  - Commonware 합의 런타임은 별도의 OS 스레드로 분리되어 `commonware_runtime::tokio::Runner`를 통해 자체 비동기 태스크 환경을 구축합니다.
- **통신(In-Process)**: HTTP/REST나 WebSockets를 통한 전통적인 Engine API 호출이 아니라, Reth가 제공하는 내부 mpsc 채널(인프로세스 메시지 패싱)을 통해 합의 계층과 실행 계층이 직접 통신합니다. 지연(Latency)이 획기적으로 낮아집니다.

## 2. PrivateNodeHandle: 실행 엔진 제어

`crates/commonware-consensus/src/node_handle.rs`에 정의된 `PrivateNodeHandle`은 통합의 핵심 브리지 역할을 합니다.

- Reth가 노드를 성공적으로 런칭했을 때 반환하는 `FullNode` 객체에는 막대한 양의 컴포넌트가 존재합니다.
- 합의 엔진(Commonware)이 필요로 하는 의존성만 최소화하여 추출한 것이 `PrivateNodeHandle`입니다.
  - **`ConsensusEngineHandle`**: `new_payload`, `fork_choice_updated` 등의 Engine API를 큐에 직접 밀어넣습니다.
  - **`PayloadBuilderHandle`**: 새로운 블록을 제안(Propose)할 때 사용될 페이로드 작업 상태를 관리합니다.
  - **`ProviderHandle`**: 블록 번호와 해시를 직접 쿼리하여 부모 블록 상태를 동기화합니다.

## 3. 합의 및 블록 생성 흐름 (`application/actor.rs`)

Commonware의 Simplex 합의가 새로운 라운드에 진입하면 `Propose`, `Verify` 단계를 거칩니다.

### 3.1 블록 제안 (handle_propose)
1. **부모 조회**: `self.state.marshal.subscribe(...)`를 통해 부모 블록(Parent Block)의 해시와 상태를 가져옵니다.
2. **Payload Attributes 구성**: 이더리움 스펙에 맞는 `PayloadAttributes`를 구축합니다. 기존 이더리움과 달리 무작위성(`prev_randao`)은 사용되지 않으므로 `B256::ZERO`를 주입합니다.
3. **FCU 호출**: `ConsensusEngineHandle::fork_choice_updated`를 호출하여 실행 계층에 블록 페이로드 생성을 트리거합니다.
4. Payload가 조립(Build) 완료되면 해당 해시를 합의 시스템에 제안(Propose)합니다.

### 3.2 블록 검증 (handle_verify)
1. P2P 네트워크를 통해 수신된 제안(Propose) 블록 구조체를 `alloy_rpc_types_engine::ExecutionData` 스펙으로 패킹(`from_block_unchecked` 사용)합니다.
2. Reth 스펙에 맞추어 `ExecPayloadSidecar` 분리 등의 처리를 수행합니다.
3. `ConsensusEngineHandle::new_payload`를 호출하여 트랜잭션의 유효성, 논스(Nonce), 가스 한도 등을 즉각적으로 실행(Execution) 검증합니다.
4. `ValidationStatus`가 `VALID`일 경우에만 합의 메시지(Vote)를 서명하여 네트워크에 전파합니다.

## 4. 실행 결과 확정 (`executor/actor.rs`)

블록에 대해 다수의 서명(Threshold Signature)이 모여 블록이 확정(Finalized)되면 `executor/actor.rs`가 동작합니다.
- `commonware_consensus::types::Update::Finalize` 메시지가 수신됩니다.
- Reth에 `fork_choice_updated` API를 `HeadOrFinalized::Finalized` 깃발과 함께 호출합니다.
- Reth 실행 트리의 `finalized_block_hash` 포인터가 최신으로 업데이트되며, 영구적인 상태(State) 기록이 완료됩니다.

## 5. P2P 네트워킹 (`lib.rs`)

Reth 노드의 기본 네트워킹(`devp2p` 및 `discv5`)은 비활성화됩니다.
- `bin/private-node/src/main.rs`에서 `builder.config_mut().network.discovery.disable_discovery = true;`를 통해 이더리움 기반 피어스캐닝을 차단했습니다.
- 통신 메인 채널은 **Commonware Authenticated P2P 커스텀 런타임**으로 대체되었습니다.
- 합의 위원회(Validator Set) 간의 서명 교환 보장과 BFT 통신 안정성을 위해 Commonware 자체 P2P(`commonware_p2p::authenticated::lookup::Network`)를 사용하여 블록체인 노드를 연결합니다.
