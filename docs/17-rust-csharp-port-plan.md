# Rust 엔진 + C# WPF 네이티브 이식 계획

문서 버전: 1.1 · 기준일: 2026-09-29 · 상태: 결정 기준선

개정: 2026-09-27 · UI를 네이티브 WPF로 바꾸고 웹 계층을 제거(D-21). D-5·D-6와 5·6·7절 갱신.

개정: 2026-09-29 · Python 구현 기원 문서(01~16, `README.md`, UI 검토 3종)를 `backup/`으로 분리했다. `docs/` 루트에는 이 문서와 `ui-manual-review-2026-09-26.md`·`.html`만 둔다. 아래 링크의 `backup/` 상대 경로는 그 분리를 반영한 것이며, 내용이 빠진 게 아니다. 특히 `backup/15-implementation-status.md`는 **Python+PySide6 구현(23~27차)의 완료 기록**이므로 이 저장소의 현재 상태로 읽으면 안 된다. 현행 기준선은 이 문서와 `ui-manual-review-2026-09-26.md`다.

[목차](backup/README.md) · [요구사항](backup/02-product-requirements.md) · [화면](backup/04-ui-specification.md) · [아키텍처](backup/05-system-architecture.md) · [도메인](backup/06-domain-model.md) · [SQLite](backup/07-sqlite-schema.md) · [저장](backup/08-editing-save-recovery.md) · [파일 호환](backup/09-file-interoperability.md) · [인터페이스](backup/10-interfaces-and-rust.md) · [검증](backup/11-validation-and-testing.md) · [실행 계획](backup/12-development-roadmap.md) · [추적표](backup/14-traceability-and-decisions.md) · [구현 현황](backup/15-implementation-status.md) · [보존 항목](backup/16-preserved-items-review.md)

이 문서는 새 저장소 `C:\Users\admin\candela_csharp_clone`에서 CANdelaStudio 13 대체 도구 **DiagAuthor**를 새 스택으로 다시 구현하는 아키텍처와 M0~M7 마일스톤을 확정한다. Python 참조 구현 `C:\Users\admin\candela_clone`은 **읽기 전용**이며 이 계획은 그 저장소를 수정하지 않는다. 결정은 모두 `D-n`으로 번호를 붙이고 채택 이유와 기각한 대안을 함께 남긴다.

이 문서는 구현 계획이다. 여기 적힌 명령·테스트의 실행 결과가 아니며, 전체 P0 인수 조건의 통과를 주장하지 않는다.

## 1. 목적과 범위

### 목적

CANdelaStudio 없이도 ECU 진단 데이터를 작성하려는 설계자·개발자·검토자에게 오프라인 Windows 데스크톱 도구를 제공한다. 작업 흐름은 **CDD 가져오기 → 편집 → 검증 → ODX 2.2.0 출력**이며, 첫 출시의 성공 기준은 [02 요구사항](backup/02-product-requirements.md)의 표현대로 "하나의 검증된 진단 부분집합을 생성/가져오기 → 편집 → SQLite 저장 → 재열기 → ODX 출력 → 외부 의미 대조"를 끝까지 완료하는 것이다. 실무 CANdelaStudio 전체 대체를 첫 출시 조건으로 두지 않는다.

### 포함 범위 (결정)

- CANdela 형식 2.x~13.x CDD/CDDT 읽기(확인된 UDS 구조 부분집합), 원문 보존, coverage·미지원 보고 (FR-018, FR-020).
- 프로젝트 생성·열기·명시 저장·복구, 변형·DID·DTC·서비스·타입·상태 편집 (FR-001~FR-017, FR-021~FR-024).
- 범위별 검증과 오류 이동, 선택 변형의 ODX 2.2.0 진단 데이터 출력 (FR-017, FR-019).
- Windows 11 x64 오프라인 단일 사용자, Python 런타임 없는 실행 (NFR-005, NFR-006).

### 제외 범위 (명시)

- **실제 ECU 통신, CAN/CAN FD 하드웨어 제어, 진단 요청 전송, 플래싱은 하지 않는다.** telegram 미리보기는 바이트 배치 계산일 뿐이며 ECU와 통신하지 않는다 ([10 인터페이스](backup/10-interfaces-and-rust.md)의 `build_telegram_preview` 계약).
- 웹 서버·REST/HTTP API·gRPC를 두지 않는다 ([10 인터페이스](backup/10-interfaces-and-rust.md)의 공통 DTO 절).
- Electron·Tauri·브라우저 기반 데스크톱 셸을 쓰지 않는다 (D-5, D-6, D-18).
- SQLite 공유 폴더 다중 작성, 계정·권한 서버, 클라우드 동기화 ([02 요구사항](backup/02-product-requirements.md) 범위 구분).
- CDD/CDDT 쓰기, 템플릿 편집기, AUTOSAR DEXT, PDX, XLSX, HTML 사양서, 번역/TMX는 P1/P2 후속이다 ([12 실행 계획](backup/12-development-roadmap.md) 후속 로드맵).

## 2. 스택 선정 근거와 결정 기록

이 절의 결정은 기존 문서의 ADR-01(Python+PySide6 Qt Widgets)과 ADR-06(Rust는 측정 후 배치 함수에만)을 **대체한다** ([14 추적표](backup/14-traceability-and-decisions.md) 설계 결정 기록). 대체 이유는 두 가지다. 첫째, 새 저장소는 처음부터 Python 런타임을 배포 조건에서 빼는 것을 목표로 한다(NFR-006). 둘째, [중요] 이식 기준 저장소 `candb_csharp_clone`(CANdb Studio, DBC 편집기)에서 **Rust 자식 프로세스 + C# WPF + `rusqlite` bundled** 조합이 이미 동작함을 확인했다. 단 `candb_csharp_clone`의 도메인(DBC)은 전혀 재사용할 수 없고 배관만 재사용한다.

### 결정 표

| ID | 결정 | 기각한 대안 | 채택 이유 / 기각 사유 |
|---|---|---|---|
| D-1 | 엔진은 Rust 단일 크레이트 `diagauthor-engine`, 바이너리 `diagauthor-engine.exe`, edition 2024 | Python+PyO3+maturin(ADR-06), C# 엔진 | 오프라인 정적 바이너리, SQLite 번들, 기존 배관 재사용. Python 이중 언어는 도입 비용·배포 복잡도가 큼. Rust 1.98.1 toolchain이 edition 2024를 지원함을 확인 |
| D-2 | 프로세스 간 통신은 자식 프로세스 stdin/stdout의 **UTF-8 JSON Lines** | FFI/P-Invoke, named pipe, HTTP/REST, gRPC | 메모리 소유권·수명 버그를 피하고 CLI와 자동 테스트가 같은 코어를 씀. `candb_csharp_clone`에 FFI(`cdylib`·`DllImport`)가 전혀 없고 이 방식으로 동작함을 확인 |
| D-3 | 요청은 **한 번에 하나(in-flight 1개)** 를 직렬 처리하고 응답은 단조 증가 `id`로 상관 | 동시 다중 요청·멀티플렉싱 | SQLite 단일 writer, undo 순서 보장, 재현·디버깅 용이. `EngineClient.cs`가 `SemaphoreSlim`으로 이미 직렬화함 |
| D-4 | stdout은 프로토콜 전용, 로그는 stderr | 로그를 stdout에 혼합 | 파싱 오염 방지. `candb` PROTOCOL 문서와 동일 규약 |
| D-5 | GUI 호스트는 C# WPF, `net10.0-windows`, `UseWPF=true`, `OutputType=WinExe`, `RuntimeIdentifier=win-x64`, 프로젝트 `DiagAuthor.Desktop`이며 **`PackageReference`가 0건**이다(NuGet 패키지 없음, `System.Text.Json`과 직접 작성한 `INotifyPropertyChanged`만 사용) | WebView2 + HTML(추가 런타임 경계, 2만 행 가상화 비용, WebView2 런타임 배포 의존, `04-ui-specification.md:7` 위반), WinForms(약한 데이터 그리드·XAML 스타일링 부재), Electron/Tauri(비네이티브·큰 설치 크기) | 엔진 자식 프로세스 소유, 창/UI 소유, RPC 중계만 담당. `EngineClient.cs`(53줄)·`SmokeTest.cs` 패턴 재사용 |
| D-6 | UI는 **네이티브 XAML**로 구현한다. 목업 `docs/ui-manual-review-2026-09-26.html`은 **시각·레이아웃 명세**로만 취급하며, CSS 디자인 토큰과 영역 비율을 `Theme.xaml` ResourceDictionary와 창 레이아웃에 옮긴다. 목업 마크업은 적재하지 않고 배포하지도 않는다 | 목업 HTML을 웹 컨트롤에 적재(대체됨, D-21 참조) | 네이티브 `DataGrid` 가상화, 런타임 경계 제거, 웹 런타임 배포 의존 제거 |
| D-7 | **SQLite는 Rust 엔진만 연다.** C# 호스트는 SQLite를 직접 열지 않는다 | C#이 `Microsoft.Data.Sqlite`로 읽기 전용 열기 | SQLite는 writer 락이 거칠어 두 프로세스가 같은 `.diagproj`를 쓰면 손상·락 경합 위험. WAL에서도 교차 프로세스 가시성·스키마 드리프트·이중 쿼리 모델 문제가 남음. [05 아키텍처](backup/05-system-architecture.md)의 "단일 쓰기 커넥션 소유" 규칙과 일치 |
| D-8 | `.diagproj`는 zip이 아니라 **단일 SQLite 파일** | zip+JSON (ADR-02) | 참조 무결성·부분 갱신·트랜잭션·쿼리 관리에 적합 ([07 SQLite](backup/07-sqlite-schema.md) 파일·연결 계약) |
| D-9 | 작업본은 workspace 디렉터리의 WAL DB, 명시 저장은 `journal_mode=DELETE` 단일 파일 스냅샷 | 제자리 DELETE 저장, WAL 파일을 그대로 전달 | 전달본에 `-wal`/`-shm` sidecar가 남지 않고 원본 파일이 보존됨 (FR-003, [07 SQLite](backup/07-sqlite-schema.md)) |
| D-10 | 와이어 키는 **끝까지 snake_case**, C# 표시 계층이 UI용 이름으로 변환 | camelCase 와이어 | Rust `serde` 관례와 `candb` 예시 일치. UI만 표현 계층에서 이름을 바꿈 |
| D-11 | `hello` op의 정수 `protocol` 버전 핸드셰이크 | 버전 없음, semver 문자열 | 호환 거부를 단순 비교로 처리. [10 인터페이스](backup/10-interfaces-and-rust.md)의 revision·버전 추적 정신 |
| D-12 | `rusqlite` + `bundled` feature(정적 SQLite) | 시스템 SQLite, `sqlx` | 외부 SQLite 설치 불필요, 런타임 의존 제거 (NFR-006) |
| D-13 | `application_id=0x44494147`(DIAG), `user_version`, `project_meta.schema_version` 삼중 가드 | `user_version` 단독 | [07 SQLite](backup/07-sqlite-schema.md) 계약을 그대로 이어받고 미래 파일을 안전하게 거부 |
| D-14 | **M3(UI 셸)를 M2보다 앞당긴다**. 사용자가 UI를 먼저 요구 | 백엔드 우선(가져오기 먼저) | 사용자 우선순위를 일정에 반영. M3는 가짜 데이터 없이 `hello` 왕복과 렌더까지만 보장 |
| D-15 | 목업 HTML은 **S1 셸의 시각·구조 참조로만** 사용하고 그대로 배포하지 않는다 | HTML을 그대로 제품 UI로 사용, 백지 재작성 | 목업은 검토용이며 하드코딩 데이터·검토 크롬이 섞여 있음 (6절) |
| D-16 | 기존 Python 285 테스트와 9개 CDD corpus를 **golden oracle**로 사용 | oracle 없이 새로 작성 | 회귀 안전망. 동일 입력의 결과·오류를 Rust와 대조 ([11 검증](backup/11-validation-and-testing.md) TC-036) |
| D-17 | `application.py` 단일 클래스(3,110줄)를 Rust 모듈로 **분해**한다 | 그대로 이식 | 명령 디스패처·쿼리·경로 시스템·CDD persister가 한 클래스에 겹쳐 있음 (3절) |
| D-18 | 빌드 스크립트가 PATH를 설정하거나 절대 경로를 쓴다 | ambient PATH 의존 | .NET SDK 10.0.401(`C:\Program Files\dotnet`)과 Rust(`C:\Users\admin\.cargo\bin`) 모두 PATH에 없음이 확인됨 |
| D-19 | 엔진 프로세스 수명은 GUI가 소유, 프로세스당 세션 1개 | 데몬/다중 세션 서버 | 단일 작업본·단일 사용자. `candb`와 동일 |
| D-20 | 웹 서버·HTTP·gRPC·Electron·Tauri를 도입하지 않는다 | 위 기술 도입 | D-2·D-5의 결합. 이 결정을 뒤집으면 이 문서 전체를 다시 쓴다 |
| D-21 | WebView2 결정(원래 D-5/D-6)을 사용자 검토 후 **네이티브 WPF로 번복**한다. 목업 HTML은 시각·레이아웃 설계 참조로만 남긴다 | WebView2 + HTML(원래 D-5/D-6, 번복됨) | 사용자 지적("C#에 웹을 붙이는 이유")이 이 제품에 타당함(엔진 자식 프로세스 + C# WPF + `rusqlite` bundled 조합, 2만 행 가상화, 런타임 경계·배포 의존 제거). 번복을 기록해 향후 독자가 문서를 내부 모순으로 오해하지 않게 한다 |

### 재사용 경계 (확인)

`candb_csharp_clone`에서 재사용 가능한 것은 배관뿐이다. 엔진 serve loop(`engine/src/main.rs`), `EngineClient.cs`(직렬화·60초 타임아웃), `storage.rs`(저장당 트랜잭션, `revisions` 스냅샷, `application_id`/`user_version` 가드, `PRAGMA quick_check`), `scripts/bootstrap.ps1`/`env.ps1`/`build.ps1` 로컬 toolchain 패턴, `SmokeTest.cs`의 사내 `--smoke-test` 하네스. **CDD/ODX/UDS/DID 코드는 하나도 재사용할 수 없다**(저장소에 존재하지 않음). 도메인은 [06 도메인 모델](backup/06-domain-model.md)·[09 파일 호환](backup/09-file-interoperability.md)을 기준으로 새로 구현한다.

### Rust 이식 대상 (Python 참조 → Rust, 약 9,000줄)

| Python 모듈 | 줄 수 | Rust 모듈(안) | 비고 |
|---|---:|---|---|
| `application.py` | 3,110 | `session`, `command`, `query`, `navigation`, `cdd_apply` | **D-17 분해 대상**, critical path |
| `cdd_model.py` | 829 | `cdd::model` | CDD 의미 모델 |
| `interchange.py` | 758 | `interchange` | 버전별 CDD 읽기, ODX 쓰기 진입 |
| `comparison.py` | 680 | `comparison` | P1 |
| `source_views.py` | 377 | `source` | 원문 보존 뷰 |
| `presentation.py` | 328 | `present` | 표시 DTO |
| `domain.py` | 571 | `domain` | 값 객체·수치·배치 |
| `export_scope.py` | 156 | `export::scope` | 폐쇄 그래프 |
| `references.py` | 97 | `references` | 역참조 |
| `persistence.py`·`migrations.py`·`schema.sql` | 861 + 531 | `storage`, `migrate` | SQL 재설계(5절) |
| `ui.py`·`dialogs.py`·`panels.py`·`pages.py`·`tools.py`·`layout_view.py`·`compare_view.py`·`overviews.py`·`cell_edit.py`·`find_dialog.py`·`main.py` | 약 6,500 | 없음(C#/XAML 재구현) | `ui.py`→WPF 셸, 나머지→XAML 창·대화상자·뷰모델. Qt 의존, 버린다 |
| `workers.py` | 68 | 없음 | Qt 스레드 브리지, IPC가 대체 (D-2) |

## 3. 프로세스 아키텍처

```text
+--------------------------------------------------------------+
|  DiagAuthor.Desktop.exe  (C# WPF, net10.0-windows, WinExe)    |
|                                                              |
|   +---------------------------+   +------------------------+ |
|   |  MainWindow.xaml          |   |  EngineClient.cs       | |
|   |  (native XAML shell:      |<->|  (SemaphoreSlim 직렬화) | |
|   |   tree/list/tabs/dock)    |   |                        | |
|   +---------------------------+   +-----------+------------+ |
|                                               | stdin/stdout  |
+-----------------------------------------------|--------------+
                                                | JSON Lines
                                    +-----------v--------------+
                                    |  diagauthor-engine.exe    |
                                    |  (Rust, edition 2024)     |
                                    |  serve loop / domain /    |
                                    |  validation / export      |
                                    +-----------+--------------+
                                                | rusqlite bundled
                                    +-----------v--------------+
                                    |  workspace\*.diagproj     |
                                    |  (SQLite, WAL 작업본)      |
                                    +--------------------------+
```

### 소유권 규칙 (결정)

1. **DB 연결은 엔진 프로세스만 소유한다.** C# 호스트는 SQLite 파일을 열지 않는다 (D-7). GUI의 모든 읽기·쓰기는 엔진 RPC를 거친다.
2. **세션은 프로세스당 하나다.** 엔진 프로세스는 현재 프로젝트, revision, undo/redo 커서, 현재 세션 상태를 소유한다 (D-19, [08 저장·복구](backup/08-editing-save-recovery.md)).
3. **GUI는 엔진 저장 여부와 미적용 편집을 별도로 관리한다.** 전환·종료 시 저장/버리기/취소 정책은 호스트(C#)가 소유한다 ([챈드b PROTOCOL] 규약).
4. **엔진 수명은 GUI가 소유한다.** GUI 종료 시 자식 프로세스를 종료한다. stdin EOF는 정상 종료 신호다.
5. **로그는 stderr로만 나간다.** stdout에는 응답 JSON 한 줄만 쓴다 (D-4).
6. **미지원 원문은 삭제하지 않는다.** 원본 바이트·미지원 노드·import 이슈는 엔진이 DB에 보존한다 (FR-020, [16 보존 항목](backup/16-preserved-items-review.md)).
7. **장시간 작업은 revision 스냅샷으로 격리한다.** 결과 revision이 현재와 다르면 stale로 폐기한다 (NFR-003, [05 아키텍처](backup/05-system-architecture.md)).
8. **UI는 엔진을 직접 호출하지 않는다.** 모든 접근은 `EngineClient`를 통한다. XAML 뒤의 창·뷰모델은 프로세스나 파이프를 직접 만지지 않는다 (D-5).

요청 흐름은 **UI 이벤트 → `EngineClient` → 엔진 → `rusqlite` → 응답 → UI 갱신**이다. 웹 계층 직렬화 단계는 없앴다.

### 모듈 책임 (Rust 엔진)

| 모듈 | 책임 | 하지 않는 일 |
|---|---|---|
| `main` / `serve` | stdin/stdout JSON Lines 루프, id 상관, 직렬 디스패치 | 도메인 규칙 |
| `protocol` | envelope 파싱·직렬화, snake_case, 오류 코드 | UI 표현 |
| `session` | 열린 프로젝트, revision, dirty, undo 커서 | SQL 직접 실행 분산 |
| `storage` | rusqlite 연결, 트랜잭션, 마이그레이션, 스냅샷 저장 | 진단 규칙 |
| `domain` | 값 객체, 수치(Decimal/u64), 배치, 참조 그래프 | 파일 IO, 프로세스 IPC |
| `command` | 편집 명령, undo/redo, 원자성, change set | 위젯·표현 |
| `query` | 페이지 조회(200행 기본), 정렬·필터, cursor | 전체 검증 |
| `cdd` | XML 파싱, 템플릿 참조 해석, coverage·미지원 분류 | DB 직접 수정(적용 단계는 transaction) |
| `validation` | VAL 규칙, severity, blocks_export | UI 이동 |
| `odx` | export 범위 계산, XSD·의미 검사, 원자적 교체 | ECU 통신 |

### `application.py` 분해 (D-17 명시)

Python `application.py`는 물리 108~3270행의 단일 클래스이며 동시에 (a) 명령 디스패처 `execute_command`(594~716), (b) 쿼리 API `query_objects`(2307)·`details`(2711), (c) 탐색·경로 시스템(1408~1943), (d) CDD 적용 persister `apply_cdd`(2987)다. 이 네 책임을 각각 `command`, `query`, `navigation`, `cdd_apply` 모듈로 나누는 것을 **M1/M2의 명시적 작업**으로 둔다. 한 파일에 다시 몰아넣지 않는다.

## 4. IPC 프로토콜 명세

### envelope

요청은 한 줄에 JSON 객체 하나다 (D-2, D-3).

```json
{"id":1,"op":"hello","args":{}}
{"id":2,"op":"project_open","args":{"path":"C:\\work\\demo.diagproj"}}
```

성공 응답:

```json
{"id":1,"ok":true,"result":{"protocol":1,"engine_version":"0.1.0","schema_version_max":1}}
{"id":2,"ok":true,"result":{"project":{"uuid":"...","revision":0,"schema_version":1},"dirty":false,"read_only":false}}
```

실패 응답:

```json
{"id":2,"ok":false,"error":"INVALID_PROJECT: application_id 불일치"}
```

규칙:

- 키는 끝까지 snake_case다. C# 표시 계층만 UI용 이름으로 바꾼다 (D-10).
- `id`는 클라이언트가 발급하는 양의 정수이며 엔진은 응답에 그대로 되돌려준다 (D-3).
- 응답 순서는 요청 순서와 같다(직렬 처리). 클라이언트는 응답 `id`가 보낸 `id`와 같은지 검증하고 다르면 프로토콜 오류로 종료한다.
- 한 줄에 JSON 객체 하나. 개행은 `\n`. 인코딩은 UTF-8. 로그는 stderr (D-4).

### 상관과 타임아웃

- 클라이언트는 요청을 보낼 때마다 `id`를 1씩 올린다.
- `EngineClient.cs`는 `SemaphoreSlim`으로 요청을 직렬화하고 60초 타임아웃을 둔다(`candb` 검증 패턴). 타임아웃 시 프로세스 상태를 신뢰하지 않고 세션 재시작 여부를 사용자에게 묻는다.
- 진짜 진행 중 취소가 필요하면 후속 버전에서 `cancel` op를 정의한다. M1~M3 범위에서는 요청이 짧고(가져오기 미리보기 포함) 취소는 다음 요청 전에 stdin을 비우는 방식으로 제한한다.

### 오류 모델

`error`는 **안정 오류 코드 + 사람이 읽는 메시지** 형식의 문자열이다. 코드는 [10 인터페이스](backup/10-interfaces-and-rust.md)의 오류 코드 표를 그대로 쓴다.

| 코드 | 의미·재시도 |
|---|---|
| `VALIDATION_FAILED` / `DUPLICATE_IDENTIFIER` / `REFERENCE_IN_USE` | 입력 수정 후 새 명령 |
| `REVISION_CONFLICT` / `CURSOR_STALE` | 현재 상태 재조회 |
| `READ_ONLY` / `UNSUPPORTED_FEATURE` / `UNSUPPORTED_VERSION` | 지원 범위·모드 변경 |
| `INVALID_XML` / `INVALID_PROJECT` / `BROKEN_REFERENCE` | 입력·보고서 확인 |
| `VALIDATION_UNAVAILABLE` / `EXPORT_BLOCKED` | XSD/규칙 확보 또는 차단 사유 해소 |
| `IO_ERROR` / `DISK_FULL` / `FILE_CHANGED` / `BUSY` | 경로·공간·충돌 해결 후 재시도 |
| `CANCELLED` | 오류 알림 없이 취소 상태 |
| `INTERNAL_ERROR` | job/revision 표시, 구조화 로그 참조 |

예상 검증 오류를 대량의 예외로 전달하지 않는다. `validation` 결과는 `result.issues` 목록으로 반환한다 ([05 아키텍처](backup/05-system-architecture.md) 오류와 운영). 미지원 노드·가져오기 오류는 정상 응답(`ok:true`) 안의 데이터로 반환한다.

### op 표 (M1~M3)

| op | 단계 | args | result | 주요 오류 |
|---|---|---|---|---|
| `hello` | M1 | `{}` | `protocol`, `engine_version`, `schema_version_max` | `INTERNAL_ERROR` |
| `project_create` | M1 | `path`, `name`, `ecu_qualifier`, `default_locale`, `profile_id`, `profile_version` | `project{uuid,schema_version,revision}` | `IO_ERROR`, `INVALID_PROJECT`, `VALIDATION_FAILED` |
| `project_open` | M1 | `path` | `project{...}`, `dirty`, `read_only` | `INVALID_PROJECT`, `UNSUPPORTED_VERSION`, `BUSY`, `FILE_CHANGED` |
| `project_close` | M1 | `{}` | `{closed:true}` | `BUSY` |
| `project_save` | M1 | 선택 `path`(다른 이름 저장) | `receipt{revision,state_token,file_hash,path}` | `IO_ERROR`, `DISK_FULL`, `FILE_CHANGED`, `VALIDATION_FAILED` |
| `project_snapshot` | M1 | `{}` | `project{uuid,revision,schema_version,dirty,read_only,state_token}` | `INVALID_PROJECT` |
| `cdd_preview` | M2 | `path` | `preview_id`, `source{sha256,format_version}`, `coverage{mapped,unsupported,failed}`, `issues[]` | `INVALID_XML`, `UNSUPPORTED_VERSION`, `BUSY`, `CANCELLED` |
| `cdd_apply` | M2 | `preview_id`, `expected_revision` | `revision`, `state_token`, `affected_ids`, `coverage` | `REVISION_CONFLICT`, `VALIDATION_FAILED`, `BROKEN_REFERENCE` |
| `import_issues` | M2 | 선택 `source_file_id`, 선택 `severity` | `issues[]` | `INVALID_PROJECT` |
| `unsupported_list` | M2 | 선택 `scope` | `nodes[{id,node_path,owner_object_id,reason_code,blocks_export}]` | `INVALID_PROJECT` |
| `project_summary` | M3 | 선택 `scope` | `counts{variants,dids,dtcs,services,data_types,unsupported}` | `INVALID_PROJECT` |
| `query_objects` | M3 | `scope{kind,variant_id}`, `filter{...}`, `sort{key,dir}`, `page{cursor,limit}` | `rows[]`, `next_cursor`, `total_count`, `revision` | `CURSOR_STALE`, `READ_ONLY` |

`project_summary`와 `query_objects`는 셸이 실제 개수를 표시하기 위한 **읽기 전용** op다. M3는 이 op들로 렌더까지만 보장하고 편집 op는 M4에서 추가한다.

### 버전 관리

- `protocol`은 정수이며 M1에서 `1`로 시작한다. envelope 구조가 깨질 때만 올린다.
- 클라이언트는 첫 요청으로 `hello`를 보내고 `result.protocol`이 자신이 아는 값과 다르면 사용자에게 알리고 엔진을 종료한다 ([챈드b PROTOCOL] `hello.protocol` 규약).
- `schema_version_max`는 엔진이 처리할 수 있는 최대 프로젝트 스키마 버전이다. 열기 시 파일 `schema_version`이 더 크면 `UNSUPPORTED_VERSION`으로 거부한다 (FR-002, FR-023).
- op 추가는 protocol을 올리지 않는다. op 제거·args 의미 변경만 올린다.

## 5. 저장소와 스키마 설계

### `.diagproj` 형식

`.diagproj`는 zip이 아니라 **단일 SQLite 데이터베이스 파일**이다 (D-8). 열기 규칙([07 SQLite](backup/07-sqlite-schema.md) 계약):

- `PRAGMA application_id = 0x44494147`(DIAG), `PRAGMA user_version`과 `project_meta.schema_version`을 함께 검증한다 (D-13).
- 각 연결에서 `foreign_keys=ON`.
- 쓰기 연결은 autocommit이 아니라 명시적 `BEGIN IMMEDIATE`/`COMMIT`을 쓴다.

### 작업본과 명시 저장 (D-9)

- 편집 중 최신 상태는 workspace의 **WAL** 작업본이 기준이다. `journal_mode=WAL`, `synchronous=FULL`, `busy_timeout=5000ms`.
- workspace 경로(안): `%LOCALAPPDATA%\DiagAuthor\workspace\<project_uuid>\working.diagproj`. 엔진이 시작 시 workspace를 스캔해 비정상 종료 작업본을 복구 후보로 제시한다 (FR-004).
- **명시 저장**은 단일 파일 스냅샷을 만든다. `journal_mode=DELETE`로 전환한 뒤 clean close해 `-wal`/`-shm` sidecar 없는 사본을 목적지에 원자적으로 교체한다. WAL 파일의 존재를 저장 완료로 간주하지 않는다 (FR-003, [07 SQLite](backup/07-sqlite-schema.md)).
- 저장 실패 시 원본과 dirty 상태를 유지하고 임시 파일만 정리한다 (FR-003, TC-003).

### 스키마와 마이그레이션

- 새 엔진은 CDD 지향 **schema v1**로 시작한다. [07 SQLite](backup/07-sqlite-schema.md)의 최신 schema 10 의미(예: `structure_members.kind`의 `dtc`·`iteration`·`fault_record`·`mux`, `snapshot_records`, `muxes`/`mux_cases`, `data_types.encoding`의 `bcd`)를 잃지 않도록 v1 테이블 집합에 필요한 형태를 반영하고, 이후 마이그레이션으로 의미를 확장한다. 버전 번호는 새 출발점이지만 **표현해야 할 의미는 기존 schema 10과 동등**을 목표로 한다.
- 최소 v1 테이블(안): `project_meta`, `schema_migrations`, `objects`, `localized_texts`, `source_files`, `source_mappings`, `ecu`, `variants`, `identifier_scopes`, `dids`, `dtcs`, `variant_dids`, `variant_dtcs`, `data_types`, `data_objects`, `structures`, `structure_members`, `conversion_entries`, `diagnostic_classes`, `diagnostic_instances`, `services`, `telegrams`, `state_groups`, `states`, `service_conditions`, `service_transitions`, `unsupported_nodes`, `import_issues`, `change_log`, `undo_stack`, `history_state`. P1 의미(스냅샷·확장·MUX 등)는 후속 마이그레이션으로 추가한다.
- 마이그레이션 프레임워크: `schema_migrations(to_version, from_version, script_sha256, applied_at)`에 연속 적용만 허용한다(`to_version=from_version+1`). 적용 절차는 `BEGIN IMMEDIATE → migration SQL → domain 재검증 → foreign_key_check → user_version/project_meta 갱신 → audit → COMMIT`이고 실패는 `ROLLBACK` 후 원본을 유지한다 (FR-023, [07 SQLite](backup/07-sqlite-schema.md) 마이그레이션).
- 원본 백업: DDL 적용 전 원본을 백업 API로 보존한다. 미래 버전은 읽기 전용 확인 또는 열기 거부이며 절대 쓰지 않는다.
- 무결성: 열기와 저장 전 `PRAGMA quick_check`/`foreign_key_check`를 실행한다(엔진 저장 계층). 이는 진단 의미 검사를 대체하지 않는다 ([11 검증](backup/11-validation-and-testing.md) 판정 계약).

### 수치 표현

업무 코드(u16/u24), revision, ordinal은 `INTEGER`다. 원시 정수 리터럴·선형 계수·경계값은 `TEXT`다. u64 최댓값 `18446744073709551615`를 `REAL`로 바꾸지 않는다 (NFR-002). Rust에서는 `i64`/`String` 정규화로 다루고 `f64` 암묵 변환을 금지한다. UUID·시각 형식은 [07 SQLite](backup/07-sqlite-schema.md) 규칙을 따른다.

## 6. UI 재사용 전략

이 절은 정직하게 쓴다. **기존 목업 HTML(`docs/ui-manual-review-2026-09-26.html`)을 그대로 제품 UI로 출하할 수 없다.**

### 확인한 사실 (grep 검증)

- **자체 완결성: 있다.** 외부 참조가 0건이다. `http://`, `https://`, `<script src>`, `<link>`, `<img>`, `url(`, `@font-face`가 모두 0건임을 grep으로 확인했다. `file://`에서 오프라인으로 렌더된다.
- **정체: 검토용 목업 / 클릭 가능 프로토타입.** 제목은 "DiagAuthor UI 목업"(title·nav h1)이다. [04 화면 명세](backup/04-ui-specification.md)와 [UI 도움말 검토](ui-manual-review-2026-09-26.md)가 제안한 **미구현 상호작용 변경 29개(M1-1 … M6-7)를 시각화**한 것이지, 제품 UI가 아니다.
- **데이터: 전부 하드코딩된 JS 배열.** 변수 `DTC`(가상 14개, `mkDtc` 호출 14회), `DID`(6개), `VARIANTS`(4개), `RI`(DTC 레코드 항목 4개)가 코드에 박혀 있다. 상단 배지는 서비스 18로 표기하지만 이 역시 정적 표시다. 핸들러는 `toast(...)` 목업이고 백엔드가 없으며 새로고침하면 편집이 사라진다.
- **검토 크롬이 섞여 있다.** 왼쪽 화면 목록 사이드바(`aside.nav` + `#navlist`), 오른쪽 `#notes` 검토 주석 패널, `data-m` 태그 배지(36곳), 9개 피어 목업 화면(`S1`, `S3`~`S10`)을 잇는 `route()` 해시 라우터가 들어 있다. 목업 검토용이며 제품에 남기면 안 된다.
- **낡은 흔적이 있다.** 2행에 `<!-- saved from url=... candela_clone/docs/... -->` 주석이, 231행에 옛 Python 프로젝트를 가리키는 절대 `file:///C:/Users/admin/candela_clone/...` 네비 링크가 있다. 둘 다 제거 대상이다.
- **고정 픽셀 크기가 있다.** `height:900px`, `height:780px`, `min-width:1040px`가 있어 XAML 레이아웃에 그대로 옮기면 창 크기·DPI 배율에서 잘린다.
- **설계 기준선이 웹 임베드를 금지한다.** [04 화면 명세](backup/04-ui-specification.md) 7행(`04-ui-specification.md:7`)은 MVP가 QML이나 웹 임베드 컨트롤에 의존하지 않는다고 못 박는다. 네이티브 WPF가 이 기준을 잇는다.

### 이식 방법 (결정)

목업의 마크업은 옮기지 않는다. **디자인을 XAML로 다시 구현한다.**

1. **디자인 토큰을 옮긴다.** 목업 8행의 CSS 변수를 값 그대로 `Theme.xaml`의 `Color`/`SolidColorBrush` 리소스로 적는다(아래 "시각 명세로 가져올 것").
2. **셸 레이아웃을 XAML로 다시 만든다.** 트리, 필터 영역, 가상화 목록, 탭 패널, 접이식 dock, 상태 표시줄을 목업의 영역 비율에 맞춰 `MainWindow.xaml`에 배치한다. 검토 크롬(왼쪽 화면 목록, 오른쪽 `#notes`, `data-m` 태그, `route()` 해시 라우터, 9개 피어 화면)은 옮기지 않는다.
3. **낡은 흔적을 옮기지 않는다.** 2행 `saved from` 주석과 231행 절대 `file:///` 링크는 XAML 어디에도 들어가지 않는다.
4. **데이터를 엔진에서 받는다.** 하드코딩 상수(`DTC`·`DID`·`VARIANTS`·`RI`·`LAYOUT_*`)는 옮기지 않고 `query_objects`·`project_summary` RPC 결과로 채운다. 목업의 `toast(...)` 핸들러는 엔진 op 호출로 바꾼다.
5. **가상화 목록을 쓴다.** 목록은 `DataGrid`/`ListView`의 UI 가상화로 2만 행을 처리한다. HTML 표의 수동 가상 스크롤은 필요 없다.
6. **DPI를 XAML에서 맞춘다.** 목업의 고정 `height:900px`·`height:780px`·`min-width:1040px`는 옮기지 않고 `Grid`/`DockPanel` 비율로 바꾼다. 100/150/200% 배율에서 검증한다 ([04 화면 명세](backup/04-ui-specification.md) 공통 레이아웃: 최소 1280×720).
7. **M3에서는 셸과 `hello` 왕복까지만** 보장한다. 목록·편집·검증의 실제 동작은 M4~M5다. M3에서 29개 제안을 모두 구현했다고 주장하지 않는다.

### 시각 명세로 가져올 것 (결정)

목업은 시각·레이아웃 명세로만 쓰며 코드 재사용률은 **0**이다. 아래 값을 XAML로 다시 구현한다.

- **디자인 토큰(목업 8행, 값 그대로):** `--bg:#f4f6fa`, `--panel:#fff`, `--ink:#1b283d`, `--muted:#677a92`, `--line:#dce3ed`, `--line2:#cbd5e3`, `--head:#edf2f9`, `--headInk:#50617b`, `--sel:#dbe8ff`, `--selInk:#163668`, `--accent:#205db8`, `--accentSoft:#e6efff`, `--warn:#a06b19`, `--err:#b3261e`, `--ok:#2f7d5b`, `--grey:#8a94a6`, `--alt:#f8fafd`, `--tag:#6d3fd6`. `Theme.xaml`의 `Color`/`SolidColorBrush` 리소스로 만든다.
- **셸 영역 레이아웃(목업 CSS 클래스, 괄호는 목업 행):** `.win`(41), `.titlebar`(42), `.menubar`(45~47), `.pathbar`(75~80), `.work`(84), `.tree`(84~90), `.split`·`.listzone`(91), `.filters`·`.tablewrap`·`.grid`(91~116), `.pane`·`.tabsbar`·`.tabbody`(117~129), `.dock`·`.dockbody`·`.logwrap`(130~138), `.statusbar`(141). 각 클래스의 크기·색·배치를 XAML 컨트롤 스타일로 옮긴다.
- **S1 화면 객체:** `S1`(목업 439~584행)의 셸 구성을 창 레이아웃의 기준으로 삼는다.
- **글꼴:** UI는 `Malgun Gothic`, 등폭·16진 표시는 `Consolas`.

목업은 이제 **순수한 시각 명세**다. 코드 재사용률은 **0**이고, 디자인을 XAML로 다시 구현한다. 목업을 재사용 코드로 보고하지 않는다.

## 7. 마일스톤 로드맵

각 마일스톤은 산출물, 선행, 완료 조건, 검증 명령을 가진다. 모든 명령은 저장소 루트에서 실행한다.

| ID | 이름 | 산출물 | 선행 | 완료 조건 | 검증 명령 |
|---|---|---|---|---|---|
| M0 | Toolchain & scaffold | 솔루션, Rust workspace, `DiagAuthor.Desktop` 프로젝트 뼈대, `scripts\build.cmd`, `scripts\test.cmd`, `scripts\env.ps1`(PATH 설정) | 없음 | 빌드·테스트 스크립트가 0으로 종료 | `scripts\build.cmd` · `scripts\test.cmd` (둘 다 exit 0) |
| M1 | Engine skeleton | `diagauthor-engine.exe`, JSON Lines serve loop, `hello`, 프로젝트 create/open/close/save/snapshot, CDD 지향 schema v1, 마이그레이션 프레임워크, `application.py` 분해 골격 | M0 | `cargo test` 통과, `hello` 왕복 응답 유효 | `cargo test` · `echo {"id":1,"op":"hello","args":{}} \| diagauthor-engine.exe serve` |
| M2 | CDD import | XML 파싱(2.x~13.x), 템플릿 참조 해석, 원문 보존(`unsupported_nodes.raw_xml`), coverage·issue 보고, `cdd_preview`/`cdd_apply` | M1 | `examples/`의 `.cdd` 9개가 모두 가져와지고 coverage 보고 | 통합 테스트 + `examples` 9종 import 스크립트 (`cargo test --test cdd_corpus`) |
| M3 | UI shell (사용자 우선) | 네이티브 WPF 셸: `Theme.xaml`(디자인 토큰), `MainWindow.xaml`(트리/필터된 가상화 목록/탭 패널/접이식 dock/상태 표시줄), `EngineClient` 배선, 엔진 부재 시 목업 모드, `project_summary`/`query_objects` 읽기 op | M0, M1 | WPF 앱이 실행되어 셸을 렌더하고 상태 표시줄에 `hello` 왕복이 보임. 목록이 2만 행에서 가상화로 동작하고 `.csproj`에 `PackageReference`가 0건 | `dotnet build DiagAuthor.slnx` · `scripts\run-gui.cmd --smoke-test` |
| M4 | Domain editing + undo/redo | 명령 계약, 트랜잭션 undo 로그, 저장·복구, 변형·DID·DTC·타입·서비스·상태 편집 op | M1, M3 | 명령 원자성·undo/redo·저장·복구가 통과, 화면에서 편집이 엔진에 반영 | `cargo test` · `scripts\run-gui.cmd --smoke-test` |
| M5 | Validation engine | VAL 규칙, severity·blocks_export, navigate-to-issue, stale 결과 폐기 | M4 | 규칙별 보고와 오류 필드 이동, 손상·경계 fixture 통과 | `cargo test --test validation` · GUI 스모크 |
| M6 | ODX export | export 범위, ID map, XSD·의미 대조, 원자적 파일 교체, 미지원 차단 | M5 | Python이 막은 곳은 그대로 막고, 통과한 것만 출력(공식 2.2.0 XSD 필요) | `cargo test --test odx` (XSD 없으면 `VALIDATION_UNAVAILABLE`로 실패) |
| M7 | Packaging | self-contained publish, 설치·로그·업그레이드, clean-machine 절차 | M6 | Python/Rust SDK 없는 Windows 11 x64에서 실행·저장·복구·출력 | `scripts\publish.cmd` · clean-machine 체크리스트 |

### 마일스톤별 진입·종료 기준

- **M0 진입**: 빈 저장소(`docs/`, `examples/`만 존재). **종료**: `scripts\build.cmd`와 `scripts\test.cmd`가 0으로 종료하고 로컬 toolchain을 PATH에 넣는다. .NET SDK 10.0.401과 Rust 1.98.1 MSVC가 확인됨.
- **M1 진입**: M0. **종료**: `cargo test` 통과, `hello`가 `protocol`·`engine_version`을 반환하며 `application.py` 분해 골격(`session`/`command`/`query`/`navigation`/`cdd_apply`)이 존재.
- **M2 진입**: M1. **종료**: `examples/`의 9개 `.cdd`(중복 포함: `ABS_ESP-Example.cdd`, `CddTools.cdd`, `Synthetic.cdd`, `candela_example/Multilingual-Example.cdd`, `candela_example/ABS_ESP-Example.cdd`, `web_samples/cantools_example.cdd`, `cantools_example-diddatarefs.cdd`, `cantools_invalid-bo-example.cdd`, `cantools_le-example.cdd`)를 모두 가져오고 coverage(mapped/unsupported/failed)를 보고. 원본 바이트는 무변경.
- **M3 진입**: M0+M1. **종료**: WPF 앱이 셸을 렌더하고 상태 표시줄에 `hello` 왕복 결과가 보인다. 목록 컨트롤이 2만 행에서 가상화로 동작하고 `.csproj`에 `PackageReference`가 0건이다. 목업 HTML은 적재되지 않고 디자인 토큰과 영역 비율만 `Theme.xaml`·`MainWindow.xaml`로 옮겨졌다.
- **M4 진입**: M1+M3. **종료**: 명령이 원자적이고 undo/redo가 참조·순서를 복원하며, 명시 저장·복구가 동작.
- **M5 진입**: M4. **종료**: VAL-001~VAL-021 중 구현 범위의 규칙이 개별 보고되고, 오류 선택이 필드로 이동하며, stale 결과를 확정 결과로 쓰지 않음.
- **M6 진입**: M5. **종료**: 공식 ODX 2.2.0 XSD와 include/import를 확보한 경우에만 통과를 주장. 확보 전에는 `VALIDATION_UNAVAILABLE`로 실패하며 검증 완료 파일을 제공하지 않음 (FR-019, VAL-017).
- **M7 진입**: M6. **종료**: Python/Rust SDK 없는 Windows 11 x64에서 설치·실행·저장·복구·출력 (NFR-006, TC-031).

### 정직한 한계 (마일스톤이 약속하지 않는 것)

- 어떤 마일스톤도 **전체 CANdelaStudio 패리티**를 약속하지 않는다. [15 구현 현황](backup/15-implementation-status.md)은 ODX 출력 의미 확장, 공식 ODX XSD·외부 도구 대조, 규칙별 보고 확대, 출시 환경 확인을 P0 미완으로 남긴다. 이 계획은 그 P0 항목을 M5·M6·M7로 이어받되, 완료 증거 없이 통과로 표시하지 않는다.
- M6은 공식 XSD가 없으면 **출력을 막는 것이 정상 동작**이다 ([09 파일 호환](backup/09-file-interoperability.md)).
- [16 보존 항목](backup/16-preserved-items-review.md)의 원문 보존 항목과 MUX·특성 곡선·BCD 등은 해당 마이그레이션·모듈에서 별도로 다룬다. M2에서 해석 못 한 것을 임의로 채우지 않는다.

## 8. 검증 전략

### Rust 단위 테스트

- `domain`: u64·signed·비트 배치·packed BCD·Decimal 변환의 경계(1/7/8/16/32/64비트, u64 최대, 앞뒤 padding) ([11 검증](backup/11-validation-and-testing.md) 경계 fixture).
- `storage`/`migrate`: FK·CHECK·UNIQUE 위반, `application_id`/`user_version` 가드, 마이그레이션 중간 실패 rollback (FR-023, TC-027, TC-030).
- `protocol`: envelope 파싱, id 상관, 오류 코드, snake_case 왕복.

### 통합 테스트 (실제 corpus)

- `examples/`의 9개 `.cdd`를 모두 가져오는 통합 테스트. 정의/참조/미지원 수를 **분리 보고**하고, 원본 파일 해시가 무변경임을 확인한다 (FR-018, FR-020, TC-022).
- 손상·미래 스키마·깨진 참조·외부 엔터티 fixture로 열기·복구를 검증한다 (FR-002, FR-004, TC-002, TC-005, TC-033).

### GUI 스모크 테스트

- `candb_csharp_clone`의 `SmokeTest.cs`를 본떠 앱 내장 `--smoke-test` 하네스를 둔다. 앱을 실제로 띄워 셸 렌더, 프로젝트 열기, 저장, 종료를 자동 수행하고 로그를 남긴다. 브라우저·콘솔 출력에 의존하지 않는다.
- 창 스크린샷을 남기고, 상태 표시줄에 표시된 `hello` 왕복 값(`protocol`·`engine_version`)을 로그에 기록한다.
- 2만 행 DTC fixture로 목록 컨트롤의 가상화를 실측한다(스크롤 응답·메모리·UI 스레드 점유). 가상화가 깨지면 실패로 기록한다.
- [15 구현 현황](backup/15-implementation-status.md)의 GUI 스모크 저장 단계 한계(저장 창 대체 경로)를 반복하지 않도록, 저장 단계는 정식 경로를 스모크가 누르게 한다.

### Python suite를 golden oracle로 사용

- 참조 저장소의 285개 통과 테스트를 **기대값 oracle**로 삼는다. Rust에 같은 입력을 넣어 결과·오류가 동등한지 대조한다 (D-16, TC-036).
- 대조 대상: CDD 가져오기 coverage·미지원 분류, 수치 변환(TC-011, TC-012), 비트 배치(TC-013), 서비스 헤더(TC-014), 상태 전이(TC-015), undo(TC-019), 스키마 마이그레이션(TC-027).
- oracle과 다르면 **어느 쪽이 명세인지 먼저 판정**한다. Python 동작을 무조건 정답으로 복사하지 않는다. 차이는 실패로 기록한다.

### 성능

- [11 검증](backup/11-validation-and-testing.md) 성능 측정 계약을 따른다. 1만 DID + 1만 DTC에서 필터 500ms, 열기·저장 5초 목표(NFR-004, TC-032). 기준 장비·버전·DB/BLOB 크기·전원 모드를 함께 보고한다.
- IPC 비용을 포함해 측정한다. 셀별 네이티브 호출이나 전체 문서 반복 직렬화를 하지 않는다 ([10 인터페이스](backup/10-interfaces-and-rust.md) Rust 계약 정신).

### 출시 증거

테스트 결과에는 입력 해시, 엔진/GUI/스키마 버전, 실행 시각, 결과, 로그·출력 해시를 연결한다 ([11 검증](backup/11-validation-and-testing.md) 출시 증거). 외부 ODX 검증 도구가 없으면 상호운용 게이트는 미통과로 남긴다.

## 9. 위험과 완화

기존 RISK-01~10([14 추적표](backup/14-traceability-and-decisions.md))을 이어받고, 새 스택 고유 위험을 `PORT-RISK-nn`으로 이 문서에서 처음 정의한다(정의를 한 곳에만 두는 [README](backup/README.md) 관례).

| ID | 위험 | 완화 | 관련 마일스톤 |
|---|---|---|---|
| RISK-01 | CDD 전체 스키마·OEM 차이 미검증 | corpus 버전별 매핑·손실 보고, 지원 범위 제한, 9개 실제 파일 통합 테스트 | M2 |
| RISK-02 | 정식 ODX 2.2 XSD·규칙 확보 미완료 | 자료 경로·버전·해시 기록 후 XSD 테스트. 없으면 출력 차단 | M6 |
| RISK-03 | 외부 ODX 도구 검증 기능 미확인 | 외부 재열기·필드 대조 기록. 설치 존재만으로 통과 금지 | M6, M7 |
| RISK-04 | 공유 DID/인스턴스 분리 오류 | Base/High 그래프 비교와 undo 회귀 | M4 |
| RISK-05 | 조건·전이 ODX 의미 손실 | 정식 매핑 검증 전 출력 차단 | M5, M6 |
| RISK-06 | UI 지연(구 Qt model) | 네이티브 `DataGrid` 가상화로 2만 행을 렌더, 엔진 페이지 조회(200행), 측정 후 최적화 | M3, M4 |
| RISK-07 | WAL·저장 교체 중단 시점 | live backup, 강제 종료·토큰 대조, `journal_mode=DELETE` 스냅샷 | M1, M4 |
| RISK-08 | SQLite unsigned64·Decimal 정밀도 | typed TEXT, Rust `i64`/`String`, 경계 테스트 | M1, M4 |
| RISK-09 | migration 실패·미래 파일 쓰기 | 작업본 migration, 원본 backup, 미래 버전 쓰기 금지 | M1 |
| RISK-10 | 원본 fixture만 통과하는 과적합 | 자체 경계·변형·비정상 fixture, 사용자 corpus 확대 | M2, M5 |
| PORT-RISK-01 | `application.py` 단일 클래스 분해 실패로 이식 지연 | D-17로 네 책임을 모듈로 선분해, M1/M2에서 골격 먼저 | M1, M2 |
| PORT-RISK-02 | IPC 직렬화 오버헤드 | 페이지 단위 응답, 셀별 호출 금지, 프로파일 측정 | M3, M7 |
| PORT-RISK-03 | `.NET 데스크톱 런타임` 미설치/버전 차이 | self-contained publish 또는 런타임 설치 안내, 오프라인 배포 경로 문서화 | M7 |
| PORT-RISK-04 | 목업과 제품의 괴리(검토 크롬·하드코딩 잔존) | 6절 제거 체크리스트를 M3 종료 조건에 포함 | M3 |
| PORT-RISK-05 | protocol/schema 드리프트 | `hello.protocol`·`schema_version_max` 가드, 버전 올림 규칙 문서화 | M1, M7 |
| PORT-RISK-06 | golden oracle과의 결과 불일치 | 차이를 먼저 명세 판정, 무조건 Python 복사 금지 | M2, M5 |
| PORT-RISK-07 | 목업 디자인을 XAML로 재현하는 비용·재현 편차 | 디자인 토큰을 `Theme.xaml`에 값 그대로 옮기고, 목업 스크린샷과 나란히 리뷰 | M3 |

## 10. 추적표

요구사항 ID는 [02 요구사항](backup/02-product-requirements.md)·[11 검증](backup/11-validation-and-testing.md)·[12 실행 계획](backup/12-development-roadmap.md)에 실제로 존재하는 것만 인용한다.

### 기능·품질 → 마일스톤

| 요구사항 | 마일스톤 | 비고 |
|---|---|---|
| FR-001 | M1 | 프로젝트 생성 |
| FR-002, FR-003, FR-004, FR-023 | M1, M4 | 열기·명시 저장·복구·마이그레이션 |
| FR-005 | M4 | ECU 편집 |
| FR-006, FR-007, FR-008 | M4 | 변형·DID 편집 |
| FR-009, FR-010 | M4 | 타입·배치 |
| FR-011, FR-012 | M4 | 서비스·상태 편집 |
| FR-013, FR-014, FR-024 | M4 | DTC·CSV·일괄 편집 |
| FR-015 | M4 | undo/redo |
| FR-016 | M3, M4 | 필터·정렬·페이지 조회 |
| FR-017 | M5 | 검증·오류 이동 |
| FR-018 | M2 | CDD 가져오기 |
| FR-019 | M6 | ODX 출력 |
| FR-020 | M2, M6 | 원문 보존·차단 |
| FR-021 | M4 | 삭제 영향 |
| FR-022 | M2, M4 | 다국어 보존 |
| NFR-001 | M1, M4 | DB 무결성·원본 보존 |
| NFR-002 | M4 | 수치 무손실 |
| NFR-003 | M3, M5 | UI는 SQL/XML/전체 검증 미실행 |
| NFR-004 | M7 | 성능 목표 |
| NFR-005 | M2 | 오프라인·외부 자원 차단 |
| NFR-006 | M7 | Python 없는 배포 |
| NFR-007 | M2, M6 | 버전·출처 추적 |
| NFR-008 | M0, M7 | 로그에 원문 비노출 |

### 검증·테스트 → 마일스톤

| ID | 마일스톤 |
|---|---|
| VAL-001, VAL-002, VAL-003, VAL-004, VAL-005, VAL-006, VAL-007, VAL-008, VAL-009, VAL-010, VAL-011, VAL-012, VAL-013, VAL-014, VAL-016, VAL-019, VAL-020, VAL-021 | M5 |
| VAL-015 | M2, M4 (CSV) |
| VAL-017, VAL-018 | M6 |
| INF-001 | M2 (원문 보존 표시) |
| TC-001, TC-002, TC-003, TC-004, TC-005, TC-006, TC-007, TC-008, TC-009, TC-010, TC-011, TC-012, TC-013, TC-014, TC-015, TC-016, TC-017, TC-018, TC-019, TC-020, TC-021 | M4, M5 |
| TC-022, TC-023, TC-033 | M2 |
| TC-024, TC-025, TC-034 | M6 |
| TC-027, TC-030, TC-037, TC-038 | M1, M4 |
| TC-029 | M3, M5 |
| TC-031, TC-035 | M7 |
| TC-032 | M7 (측정) |
| TC-036 | M2, M5 (golden oracle) |

### UI·시나리오 → 마일스톤

| ID | 마일스톤 |
|---|---|
| UI-01 | M3 |
| UI-02 | M3, M4 |
| UI-03, UI-04, UI-05, UI-06, UI-07, UI-08 | M4 |
| UI-09 | M2, M4 |
| UI-10 | M5 |
| UI-11 | M6 |
| UI-12 | M4 |
| UC-01 | M1, M4 |
| UC-02 | M2 |
| UC-03, UC-04, UC-05, UC-06, UC-07, UC-08 | M4 |
| UC-09 | M5 |
| UC-10 | M6 |
| UC-11, UC-12 | M4 |

### 작업 → 마일스톤 (기존 TASK 이어받기)

| TASK | 마일스톤 |
|---|---|
| TASK-01 | M0 |
| TASK-02, TASK-13 | M2 |
| TASK-03 | M0, M7 |
| TASK-04, TASK-07 | M1 |
| TASK-05, TASK-06 | M4 |
| TASK-08 | M3 |
| TASK-09, TASK-10, TASK-11, TASK-12 | M4 |
| TASK-14 | M5 |
| TASK-15 | M6 |
| TASK-16 | M7 |
| TASK-17, TASK-18 | M7 |
| TASK-19 (선택적 Rust 최적화) | 불필요(엔진이 Rust로 대체) |

## 11. 용어와 상태 표기

상태 표기는 [README](backup/README.md) 관례를 따른다.

- **확인:** 실제 도구·예제·환경을 읽어 확인한 사실. 여기서는 예: 목업 자체 완결성, `.cdd` 9개, toolchain 버전.
- **결정:** 새 도구가 따라야 하는 설계. D-1~D-21과 마일스톤.
- **목표:** 측정하지 않은 성능·일정. NFR-004 성능 목표.
- **보류:** 외부 증거가 필요한 사항. 공식 ODX 2.2.0 XSD, 외부 ODX 검증 도구, 서명 인증서.

| 용어 | 의미 |
|---|---|
| 엔진 | Rust 자식 프로세스 `diagauthor-engine.exe`; 도메인·DB·가져오기·검증·출력을 전담 |
| 호스트 | C# WPF 앱 `DiagAuthor.Desktop`; 네이티브 XAML UI와 엔진 수명만 담당 |
| IPC | stdin/stdout JSON Lines 자식 프로세스 RPC (D-2) |
| envelope | `{"id","op","args"}` / `{"id","ok","result"|"error"}` |
| 작업본 | workspace의 WAL SQLite DB; 편집 중 최신 상태 |
| 전달본 | 명시 저장으로 만든 `journal_mode=DELETE` 단일 `.diagproj` 스냅샷 |
| protocol | 엔진 RPC envelope 버전 정수. M1에서 1 |
| schema_version | `.diagproj` 스키마 버전. `application_id`/`user_version`과 함께 검증 |
| golden oracle | Python 285 테스트·9 corpus를 Rust 결과 대조 기준으로 쓰는 것 |

### 참고한 문서와 코드

- 설계 기준: [05 아키텍처](backup/05-system-architecture.md), [07 SQLite](backup/07-sqlite-schema.md), [08 저장·복구](backup/08-editing-save-recovery.md), [09 파일 호환](backup/09-file-interoperability.md), [10 인터페이스·Rust](backup/10-interfaces-and-rust.md), [11 검증·테스트](backup/11-validation-and-testing.md).
- 이식 기준 배관: `C:\Users\admin\candb_csharp_clone`의 `docs/PROTOCOL.md`, `docs/IMPLEMENTATION_PLAN.md`, `engine/src/main.rs`, `engine/src/storage.rs`, `gui/CanDb.Desktop/EngineClient.cs`, `scripts/*.ps1`, `SmokeTest.cs`.
- UI 자산: `docs/ui-manual-review-2026-09-26.html`(S1 셸 참조), [UI 도움말 검토](ui-manual-review-2026-09-26.md).
