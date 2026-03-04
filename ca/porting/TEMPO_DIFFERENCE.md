# Tempo vs Reth-Commonware Porting (차이점 분석)

이 문서는 기존 [Tempo](https://github.com/commonwarexyz/tempo) 아키텍처와 현재 이식(Porting)된 `Reth-Commonware` 프라이빗 노드의 핵심적인 차이점을 설명합니다.

## 1. 기반 실행 엔진 (Execution Engine)

- **Tempo**: `Reth` 0.x (구 버전) 기반으로 제작되었으며, 여러 커스텀 헤더 구조(`TempoBlock`, `TempoHeader`)와 자체적인 봉투(Envelope) 포맷을 정의하여 블록/트랜잭션 검증 시스템을 우회 및 수정해야 했습니다.
- **Reth-Commonware (현재)**: 최신 **Reth 1.10.2** (Alloy 기반)를 사용합니다. 커스텀 헤더와 블록 구조를 억지로 사용하지 않고, 이더리움 메인넷 스펙의 `ExecutionPayload` 및 `EthPayloadBuilderAttributes`를 **그대로 준수**합니다. 

## 2. API와 종속성 (Alloy RPC Types)

- **Execution Payload 분리**: Tempo 개발 당시의 구 버전 Reth에서는 `ExecutionData`에 페이로드 정보가 단순하게 병합되어 통신했습니다. 현재 Reth는 Alloy 프레임워크의 도입으로 인해, 블록을 검증할 때 튜플 형태 `(alloy_rpc_types_engine::ExecutionPayload, ExecutionPayloadSidecar)`를 받아 `ExecutionData` 인터페이스에 정확히 나누어 매핑해야 합니다 (Cancun 업그레이드 이후 구조화 방식 대응).
- **타입 의존성 해결**: Tempo가 구현한 `node_handle.rs`는 `EthApiServer` Trait를 하드코딩으로 종속하고 있어 최신 Reth(제네릭 기반의 `EthApiTypes`)와 컴파일이 호환되지 않았습니다. 현재 코드는 `RpcHandle`의 Generic 파라미터를 활용하여 타입 우회 및 확장이 가능하도록 변경되었습니다 (`EthApi: reth_rpc_eth_api::EthApiTypes` 바운딩 적용).

## 3. 합의 계층 (Commonware) 버전 업데이트

Tempo가 의존하던 과거 Commonware 커밋 대신 최신 `2026.2.0` 릴리즈를 사용하면서 P2P 및 합의 설정에 많은 API 파단(Breaking Change)이 발생했고, 이를 모두 반영했습니다.

- **네트워크 설정 (Config)**: 
  - 과거: `commonware_p2p::authenticated::network::Config`
  - 현재: `commonware_p2p::authenticated::lookup::Config` 및 `Network::new()`
- **파라미터 변경**: `signer` -> `crypto`, `address` -> `listen` 등으로 직관적인 필드 포맷으로 업데이트되었습니다. 또한 `HashMap` 기반으로 받던 `tracked_peer_sets`가 단순 라우팅 제한 변수 `usize` 형태로 바뀌어 이를 `0`으로 일괄 통일했습니다.
- **SystemTime 유틸리티**: `ContextCell` 내부에서 시간을 가져오던 기존 동작이 Commonware 유틸리티 분리로 인해 `commonware_utils::SystemTimeExt`의 `epoch_millis` 확장 트레잇 명시적 Import 방식으로 변경되었습니다.
- **비동기 퓨처 중첩(Future Nesting)**: `subscribe()` 등 일부 합의 메시징 내부 동작이 반환하는 Future가 한 단계 더 래핑되었습니다. 패닉을 방지하기 위해 `map_err().await` 구문을 `.await.await.map_err()`와 같이 안전하게 이중 Evaluation 하도록 수정되었습니다.
- **Reporters 튜플 캐스팅**: P2P `mailbox`들을 넘겨줄 때 `Reporters::from`이 단순 전달을 넘어, 다중 Mailbox에 대응하도록 내부적으로 `(A, B)` 튜플 형태로 감싸 전달 방식이 일관성 있게 구조화되었습니다.

## 4. DKG 및 블록 부가 데이터(Extra Data) 제거

- **Tempo**: 블록을 제안할 때, 다자간 서명 체계(DKG) 관련 정보를 블록의 `extra_data` 필드에 임베딩하여 네트워크 상태 머신을 변경하려 했습니다.
- **Reth-Commonware (현재)**: 복잡한 DKG 상태 머신 로직 및 `subblocks` 개념을 과감히 제거했습니다. 순수하게 `B256::ZERO` 등 기본 초기화 값을 사용하여 **표준 이더리움 블록 호환성**을 100% 포기하지 않는 프라이빗 검증 네트워크에 초점을 맞췄습니다.

## 5. 실행 프로세스 이원화

- **기존 (Tempo)**: `try_join`을 사용하여 네트워크 리스너와 합의 엔진 워커를 하나로 묶어 결합도가 매우 높았습니다.
- **현재**: 비동기 작업 중단의 안정성(Graceful Shutdown)을 확보하기 위해 `tokio::select!` 매크로를 사용하여 두 엔진 `ret = network_handle => {...}`, `ret = engine_handle => {...}` 중 하나에 이벤트 패닉이 발생하면 독립적이고 안전하게 리프 포트를 닫을 수 있도록 에러 캐칭 부분을 격리했습니다.
