# Reth 프로젝트 분석 컨텍스트

> **이 문서는 매 분석 세션 시작 시 반드시 읽어야 합니다.**
> 최종 수정일: 2026-02-18

---

## 1. 프로젝트 개요

| 항목 | 내용 |
|------|------|
| **프로젝트명** | reth (Rust Ethereum) |
| **버전** | 1.10.2 |
| **라이선스** | MIT / Apache-2.0 |
| **개발사** | Paradigm |
| **언어** | Rust (Edition 2024, MSRV 1.88) |
| **리포지토리** | https://github.com/paradigmxyz/reth |
| **핵심 역할** | 이더리움 Execution Layer (EL) 풀 노드 구현체 |

### reth가 하는 일 (Java 개발자를 위한 설명)
- 이더리움 블록체인의 **실행 레이어(EL)**를 담당합니다.
- Spring Boot로 비유하면, 트랜잭션을 받아서 실행하고, 상태(DB)를 변경하고, RPC API를 제공하는 **백엔드 서버** 같은 역할입니다.
- Consensus Layer(CL, 예: Lighthouse)와 Engine API로 통신하여 블록 합의를 진행합니다.

---

## 2. 분석 목표

### 최종 목표
> reth 소스코드를 철저히 분석하여 **실제 금융 프로젝트에 맞게 커스터마이징**할 수 있는 수준의 이해도를 확보하는 것

### 단계별 목표
1. **[현재] 패키지 구조 분석** — 전체 workspace, crate 구조, 의존성 관계 파악
2. **핵심 데이터 타입 분석** — Block, Transaction, Receipt 등 primitives 이해
3. **스토리지 레이어 분석** — MDBX, Provider 패턴, 데이터 모델
4. **EVM 실행 흐름 분석** — revm 통합, 블록 실행 파이프라인
5. **동기화(Sync) 분석** — Staged Sync 아키텍처, 각 Stage 역할
6. **네트워킹 분석** — P2P 통신, 피어 관리, 프로토콜
7. **RPC API 분석** — JSON-RPC 구현, Engine API
8. **페이로드/블록 빌더 분석** — 블록 생성 프로세스
9. **트랜잭션 풀 분석** — 메모리 풀 관리 전략
10. **커스터마이징 포인트 식별** — 금융 프로젝트 적용을 위한 확장 지점

---

## 3. 분석자 프로필 & 전제조건

### 분석자 배경
- **주 언어**: Java (백엔드 개발자)
- **Rust 경험**: 거의 없음 (기초 수준)
- **블록체인 지식**: EVM 기반 실행 레이어에 대한 프로젝트 수준의 이해 보유

### Rust ↔ Java 대응 개념표

| Rust 개념 | Java 대응 개념 | 설명 |
|-----------|---------------|------|
| `crate` | Maven 모듈 / Gradle 서브프로젝트 | 하나의 라이브러리 또는 바이너리 단위 |
| `Cargo.toml` | `pom.xml` / `build.gradle` | 프로젝트 설정 및 의존성 관리 파일 |
| `workspace` | 멀티 모듈 프로젝트 (parent pom) | 여러 crate를 하나의 프로젝트로 관리 |
| `trait` | `interface` | 행위를 정의하는 추상화. Java의 default method처럼 기본 구현 가능 |
| `struct` | `class` (필드만) | 데이터를 담는 구조체. 메서드는 `impl` 블록에서 정의 |
| `enum` | `sealed class` + `enum` | Rust enum은 값을 가질 수 있어 Java sealed class에 가까움 |
| `impl` | 클래스 내 메서드 정의 | struct에 메서드를 구현하는 블록 |
| `impl Trait for Struct` | `class Foo implements Bar` | 특정 trait을 struct에 구현 |
| `mod` | `package` | 코드 모듈화 단위 |
| `pub` | `public` | 접근 제한자 |
| `pub(crate)` | `package-private` (default) | 같은 crate 내에서만 접근 가능 |
| `Result<T, E>` | `try-catch` / `Optional` | 에러 처리 패턴 (명시적 에러 타입) |
| `Option<T>` | `Optional<T>` | 값이 있을 수도, 없을 수도 있는 타입 |
| `Box<T>` | 일반 참조 (힙 할당) | 힙에 데이터를 할당하는 스마트 포인터 |
| `Arc<T>` | `AtomicReference` / 공유 참조 | 스레드 안전한 참조 카운팅 스마트 포인터 |
| `async/await` | `CompletableFuture` / 리액티브 | 비동기 프로그래밍 |
| `tokio` | Netty / Spring WebFlux | 비동기 런타임 (이벤트 루프) |
| `derive` 매크로 | Lombok `@Data`, `@Builder` 등 | 보일러플레이트 코드 자동 생성 |
| `#[cfg(test)]` | `src/test/java` | 테스트 코드를 같은 파일에 조건부 컴파일 |
| `feature` (flag) | Maven profile | 조건부 컴파일을 위한 기능 플래그 |
| `lib.rs` | 모듈의 진입점 (main class) | 라이브러리 크레이트의 루트 모듈 |
| `main.rs` | `public static void main()` | 바이너리 크레이트의 진입점 |

### Rust 핵심 개념 빠른 참조

#### 소유권(Ownership) — Java에 없는 핵심 개념
```
Java:  String a = "hello"; String b = a;  // a,b 모두 사용 가능
Rust:  let a = String::from("hello"); let b = a;  // a는 더 이상 사용 불가 (이동됨)
```
- Rust에서 값은 **하나의 소유자**만 가집니다.
- 값을 다른 변수에 대입하면 소유권이 **이동(move)**합니다.
- 빌려주기(**borrow**, `&`)를 통해 소유권 이동 없이 참조할 수 있습니다.
- 이 시스템 덕분에 GC(가비지 컬렉터) 없이도 메모리 안전성을 보장합니다.

#### 라이프타임(Lifetime) — Java에 없는 개념
- 참조(&)가 유효한 범위를 컴파일러가 추적합니다.
- `'a`와 같은 표기를 보면 "이 참조는 특정 범위 동안 유효하다"는 의미입니다.
- Java의 GC가 하는 일을 컴파일 타임에 처리하는 것이라고 이해하면 됩니다.

#### 제네릭과 트레이트 바운드
```rust
fn process<T: Display + Clone>(item: T) { ... }
// Java로 치면:  <T extends Display & Cloneable>
```

---

## 4. 분석 규칙

1. **모든 분석 결과는 `ca/` 폴더에 마크다운 파일로 작성**
2. **파일 네이밍**: `XX_제목.md` (XX = 순번)
3. **Java 대응 비유 적극 활용**: Rust 고유 개념은 항상 Java 대응 개념 병기
4. **코드 인용 시 경로 명시**: 분석 근거가 되는 소스 파일 경로를 반드시 표기
5. **점진적 심화**: 전체 → 도메인 → 세부 순으로 분석
6. **이 문서를 매 분석 시작 시 읽기**: 분석 맥락과 전제조건 확인

---

## 5. 분석 문서 목록 (인덱스)

| 순번 | 파일명 | 내용 | 상태 |
|------|--------|------|------|
| 00 | `00_ANALYSIS_CONTEXT.md` | 분석 목표, 전제조건, 규칙 (본 문서) | ✅ 완료 |
| 01 | `01_PACKAGE_STRUCTURE.md` | 전체 패키지 구조 분석 | ✅ 완료 |
| 02 | `02_PRIMITIVES.md` | 핵심 데이터 타입 (primitives) | ✅ 완료 |
| 03 | `03_STORAGE_LAYER.md` | 스토리지 레이어 (DB 추상화, MDBX, 테이블, 코덱, Static File, Provider) | ✅ 완료 |
| 04 | `04_EVM_EXECUTION_FLOW.md` | EVM 실행 흐름 (3-Layer 아키텍처, 블록 실행/빌딩, 시스템 콜, 상태 관리) | ✅ 완료 |
| 05 | `05_STAGED_SYNC.md` | 동기화 파이프라인 (15개 Stage, Pipeline 실행/Unwind, StageSet 패턴) | ✅ 완료 |
| 06 | `06_NETWORKING_P2P.md` | P2P 네트워킹 (NetworkManager, RLPx, 피어 관리, ETH 프로토콜) | ✅ 완료 |
| 07 | `07_RPC_API.md` | RPC API (15개 네임스페이스, EthApi, Engine API, 비동기 처리) | ✅ 완료 |
| 08 | `08_PAYLOAD_BUILDER.md` | 페이로드/블록 빌더 (PayloadJob, PayloadBuilderService, CL↔EL 흐름) | ✅ 완료 |
| 09 | `09_TRANSACTION_POOL.md` | 트랜잭션 풀 (4개 서브풀, TX 검증/정렬, 풀 유지보수) | ✅ 완료 |
| 10 | `10_CUSTOMIZATION_POINTS.md` | 커스터마이징 포인트 (ExEx, 28개 예제, Node Builder API, 금융 시나리오) | ✅ 완료 |
| 11 | `11_TX_BLOCK_LIFECYCLE.md` | ERC20 Transfer 라이프사이클 (TX 진입→검증→풀→EVM→DB→Static File, 소스코드 추적) | ✅ 완료 |
