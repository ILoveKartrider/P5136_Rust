# Windows Server 2012 x64 호환 빌드 메모

## 결론

현재 P5136 소스에는 Windows Server 2012용 분기나 코드 수정이 필요하지
않다. 일반 릴리스와 동일한 소스에서 **별도 Rust 타깃과 정적 CRT 빌드
플래그만 사용**해 호환 빌드를 만드는 방향으로 관리한다.

단, 아래 절차는 호환 가능성을 높이는 별도 빌드 방법이지 Windows Server
2012 실기 동작을 보증하는 것은 아니다. 실제 배포 전 해당 운영체제에서
실행, GUI 초기화, 서버 시작과 클라이언트 접속을 검증해야 한다.

## 일반 x64 릴리스가 실행되지 않을 수 있는 이유

기본 `x86_64-pc-windows-msvc` 타깃의 현재 공식 기준선은 Windows 10 및
Windows Server 2016 이상이다. 최신 MSVC 동적 런타임도 같은 범위가 공식
지원 대상이므로, 일반 릴리스는 Windows Server 2012의 로더 또는 CRT
초기화 단계에서 실행되지 않을 수 있다.

- Rust Windows MSVC 지원 조건:
  <https://doc.rust-lang.org/rustc/platform-support/windows-msvc.html>
- Microsoft VC++ 재배포 패키지 지원 조건:
  <https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist?view=msvc-170>

GUI만 실패하는 경우에는 Server Core, Desktop Experience, 그래픽 드라이버,
OpenGL 또는 `winit` 창 초기화 문제도 따로 확인해야 한다.

## 호환 빌드 정책

- 일반 Windows 릴리스는 계속 `x86_64-pc-windows-msvc`로 만든다.
- Server 2012용 파일은 `x86_64-win7-windows-msvc`로 별도 빌드한다.
- `crt-static`을 사용해 최신 VC++ 재배포 패키지에 대한 런타임 의존성을
  줄인다.
- 일반 릴리스를 호환 빌드로 교체하지 않고, 릴리스 자산도 별도 이름으로
  등록한다.
- 소스에 전역 `.cargo` 타깃 설정이나 호환성 전용 `cfg`를 추가하지 않는다.

`x86_64-win7-windows-msvc`는 Rust 1.94에서 인식되는 Tier 3 타깃이지만
`rustup target add`로 미리 컴파일된 표준 라이브러리를 설치할 수 없다.
따라서 nightly의 `build-std`와 `rust-src`가 필요하다.

## 빌드 명령

Windows의 MSVC 빌드 도구와 Windows SDK가 준비된 빌드 PC에서 실행한다.

```powershell
rustup toolchain install nightly --component rust-src

$previousRustFlags = $env:RUSTFLAGS
$env:RUSTFLAGS = "-C target-feature=+crt-static"

cargo +nightly build `
  --release `
  --locked `
  -p p5136-cli `
  --target x86_64-win7-windows-msvc `
  --target-dir target/p5136-win2012 `
  -Z build-std=std,panic_abort

$env:RUSTFLAGS = $previousRustFlags
```

예상 결과물은 다음과 같다.

```text
target/p5136-win2012/x86_64-win7-windows-msvc/release/p5136.exe
```

배포할 때에는 일반 x64 파일과 구분할 수 있도록 다음처럼 이름을 바꾼다.

```text
p5136-win2012-x64.exe
```

nightly는 시간이 지나면서 결과가 바뀔 수 있다. 실기 검증에 성공한 첫
빌드에서는 `rustup show`와 `rustc +nightly -vV` 결과를 기록하고, 이후
릴리스부터 같은 날짜의 nightly를 고정해서 사용하는 것이 좋다.

## 실기 판별 순서

먼저 Windows Server 2012의 명령 프롬프트에서 실행한다.

```powershell
p5136-win2012-x64.exe --version
```

- 버전 출력 전 실패: PE 로더, 누락 DLL, CRT 또는 운영체제 API 문제를
  우선 확인한다.
- 버전은 출력되지만 GUI만 실패: Desktop Experience, 그래픽 드라이버,
  OpenGL과 창 초기화 문제를 우선 확인한다.
- GUI가 열림: 서버 시작, `127.0.0.1` 접속, LAN 접속, 종료 후 재실행까지
  확인한다.

실패 시 정확한 팝업 문구, 종료 코드, 이벤트 뷰어의 `Application Error`
및 `Windows Error Reporting` 항목을 함께 보존한다.

## 현재 검증 상태

이 문서는 소스 변경 없는 호환 빌드 절차만 정의한다. 아직 Windows Server
2012 실기에서 검증된 공식 릴리스는 아니며, 첫 성공 빌드의 해시와 테스트
결과가 확보되기 전에는 실험용 자산으로 표기해야 한다.
