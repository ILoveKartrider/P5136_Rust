# P5136 Rust 서버

한국 카트라이더 P5136 클라이언트를 위한 독립 Rust 서버·접속기입니다. 원본 C# 프로젝트는 프로토콜 동작을 확인하는 읽기 전용 참고 자료로만 사용하며 이 저장소에 포함하지 않습니다.

이 프로젝트는 아직 모든 상용 서비스 기능을 재현한 완성 서버가 아닙니다. 현재 목표는 친구들과 같은 LAN에서 방 생성·입장, 게임 시작, 주행, 결과·시상식, 다시 방으로 돌아오는 멀티플레이 사이클을 안정적으로 지원하는 것입니다. Lucci, 보너스 아이템, 팀 플래그와 일부 이벤트·소셜·상점 기능은 범위 밖이거나 제한적으로 응답합니다.

## 빠른 시작

Rust 1.94 이상에서 다음 명령으로 릴리스 빌드를 만듭니다.

```powershell
cargo build --release -p p5136-cli
```

현재 저장소의 고정 빌드 경로는 다음과 같습니다.

```text
target/p5136-finish-kart-abilities/release/p5136.exe
```

`p5136.exe`를 인자 없이 실행하면 한글 GUI가 열립니다. 하나의 실행 파일 안에 서버와 접속기가 들어 있으며 GUI 탭으로 구분됩니다.

1. 서버 탭에서 `클라이언트 또는 Profile 경로`에 P5136 클라이언트 루트를 지정합니다. 예: `C:\Games\KartRider_5136`.
2. 다른 PC가 접속한다면 `내 LAN IPv4로 자동 설정`을 누릅니다. 여러 어댑터가 있으면 목록에서 실제 LAN 주소를 선택합니다.
3. 새 원격 닉네임으로 처음 접속한다면 `LAN의 새 닉네임 허용`을 체크합니다.
4. `서버 시작`을 누릅니다.
5. 접속기 탭에 게임 디렉터리, 닉네임, 서버 IPv4를 넣고 `클라이언트 준비 및 실행`을 누릅니다.

접속기는 원본 파일의 불변 백업과 프로세스 잠금을 사용해 PIN/XML을 준비한 뒤 Windows UAC, Wine, CrossOver 또는 macOS Sikarugir wrapper로 클라이언트를 실행합니다. Sikarugir의 Wine prefix와 wrapper를 수동으로 준비하는 방법은 [macOS Sikarugir walkthrough](MACOS_SIKARUGIR.md)를 참고하세요.

## 클라이언트 데이터 자동 탐지

클라이언트 루트, `Profile` 폴더 또는 `Data` 폴더를 지정할 수 있습니다. 서버는 `Data` 폴더를 자동으로 찾고 다음 원본을 제한형·읽기 전용으로 직접 해석합니다. C# 서버가 생성하던 `Profile/KartCatalog.xml`은 필요하지 않습니다.

- `Data/kart.rho`와 RHO5 overlay: 카트 이름과 주행 물리
- RHO5 `zeta_/kr/shop/data/item.kml`: 인벤토리 카탈로그
- `Data/item.rho`: 카트별 아이템 변환 규칙과 개인전·팀전 확률표
- `Data/track_common.rho`: 모드·랜덤 selector별 트랙 목록
- 그 밖의 RHO5 데이터: 엠블럼 등 지원 데이터

아이템 확률표의 기본 모드는 `서버 시작 시 자동 적용`입니다. 시작 버튼을 누르면 실제 `item.rho`를 먼저 읽어 개인전·팀전 항목 수와 적용 경로를 GUI에 표시하고, 그때 읽은 정확한 스냅샷을 해당 서버 실행에 넘깁니다. 로드 실패 시 서버를 시작하지 않습니다. `불러와 고정`이나 XML을 사용한 경우에만 이후 클라이언트 경로 변경과 무관한 수동 값이 됩니다.

세베크 V1 같은 카트의 아이템 변환도 `item.rho`의 `transformByKart`를 직접 읽어 서버가 최종 획득 아이템을 결정합니다. 예를 들어 세베크 V1의 황금 실드는 클라이언트가 자동 변환하는 것이 아니라 서버 규칙으로 처리됩니다. 서버 시작 시 만든 카탈로그는 불변 메모리 스냅샷이며 RHO나 클라이언트 폴더를 수정하지 않습니다.

## 닉네임별 카트 인벤토리

서버를 정지한 상태에서 GUI의 `닉네임별 인벤토리 편집`을 펼치면 강화 상태가 다른 동일 카트를 여러 대 소유하도록 추가할 수 있습니다.

1. 서버의 `클라이언트 또는 Profile 경로`와 `프로필 저장 경로`를 먼저 지정합니다.
2. `카트 목록 불러오기`를 눌러 클라이언트 RHO의 실제 이름과 ID를 읽습니다.
3. 인벤토리를 편집할 닉네임을 입력합니다. `접속기 닉네임 사용`으로 현재 접속기 값을 복사할 수도 있습니다.
4. `기간테스 V1`, 공백을 뺀 `기간테스v1`, 또는 숫자 ID `1410`을 입력하고 후보 드롭다운에서 카트를 선택합니다.
5. `선택 카트 추가`를 누릅니다. 같은 카트를 다시 누르면 다음 고유 serial로 한 대가 더 추가됩니다.

기본 카탈로그 카트는 모두 serial 1로 제공됩니다. 추가 소유분은 닉네임별 프로필의 `GrantedKarts`에 serial 2 이상으로 원자적 저장되며, 장착·플랜트·파츠 데이터가 사용하는 `(kart_id, serial)` 키와 같으므로 각 복사본을 서로 다르게 강화할 수 있습니다. 할당기는 현재 grant뿐 아니라 남아 있는 `PlantData.json`/`PartsData.json` serial도 예약하므로 과거 강화 상태를 새 복사본이 잘못 물려받지 않습니다. 프로필이 아직 없는 닉네임은 첫 추가 시 생성됩니다. 이미 접속한 클라이언트에는 재접속 후 반영됩니다.

편집 중에는 서버와 같은 프로필 루트 잠금을 잠깐 획득하고 저장소 고유 ID도 재검증합니다. 이 GUI뿐 아니라 다른 프로세스의 서버가 실행 중이어도 추가를 거부하므로 서버를 먼저 종료해야 합니다. 카트 목록을 읽은 뒤 클라이언트 경로를 바꾸면 목록과 선택은 무효화되며, 추가 직전에도 현재 `Data` 실제 경로가 같은지 다시 대조합니다.

현재 편집기는 의도적으로 중복 카트 추가만 지원합니다. 일반 카탈로그 아이템은 서버가 이미 기본 인벤토리에 제공하므로, 수량형 아이템 구매·지급은 가격·재화·만료·재시도 정책을 갖춘 별도 economy 기능으로 남겨 두었습니다.

## 랜덤 트랙

서버는 클라이언트의 RHO 1.0 `track_common.rho`를 제한형·읽기 전용으로 해석합니다. 스피드/아이템 모드와 selector `0, 1, 3~8, 23, 30, 33, 40`에 맞는 실제 후보 풀에서 프로세스 난수로 선택하며, 같은 방에서는 현재 풀을 소진하기 전에 같은 트랙을 다시 고르지 않습니다. AI가 있으면 `basicAi` 트랙을 우선하고 목록이 비면 원래 풀로 되돌아갑니다.

GUI의 `랜덤 트랙 설정`에서 클라이언트 목록을 미리 읽으면 풀별 후보가 체크박스로 표시됩니다. 개별 맵 선택과 `모두 선택`, `모두 해제`, `클라이언트 기본값` 복원을 지원합니다. 수동 설정이 없는 풀은 클라이언트 기본값을 그대로 사용하며, 빈 사용자 지정 목록은 붉은 경고를 표시하고 서버 시작 전에 거부합니다. 수동 설정은 원본 ID를 검증해야 하므로 클라이언트 `Data` 경로 없이 사용할 수 없으며, 경로가 사라지면 서버 시작 전에 오류로 중단합니다.

## 방 제목 S0~S7 물리

방 제목에 독립된 `S0`~`S7` 토큰을 넣으면 C# 서버와 같은 현대 물리 프리셋을 경기 시작 시 적용합니다.

```text
[S0] 초보방
친선 S2
S6 무한부스터
```

토큰은 대소문자를 구분하지 않으며 ASCII 영숫자 경계를 따릅니다. 따라서 `TESTS1ROOM`이나 `S10`은 물리 토큰으로 인식하지 않습니다. 방 목록에 보이는 기본 speed byte를 억지로 바꾸는 방식이 아니라, 각 플레이어의 카트·펫·장비와 S0~S7 기본값을 합성한 235바이트 경기 시작 물리 블록을 선택합니다.

## LAN 주소와 도메인

`바인드 주소`는 서버가 실제로 포트를 열 로컬 인터페이스이고, `클라이언트에 알릴 IPv4`는 로그인·채널 패킷에 넣을 주소입니다. LAN 자동 설정 버튼은 루프백·링크 로컬 주소를 제외하고 물리 어댑터를 가상 어댑터보다 먼저, 그 안에서는 사설 IPv4를 우선합니다. Wi-Fi, Ethernet, WSL, VMware, VPN, Tailscale 등이 함께 보이면 실제 클라이언트와 같은 네트워크의 주소를 직접 선택하세요. 광고 주소의 `0.0.0.0`, 멀티캐스트, 브로드캐스트는 시작 전에 거부합니다.

현재 bind, advertised, 접속기 서버 주소는 IP 리터럴만 받습니다. 특히 advertised 주소는 P5136 패킷에 IPv4 4바이트로 직렬화되므로 도메인을 그대로 넣을 수 없습니다. 도메인을 쓰려면 실행 전에 A 레코드를 조회해 하나의 IPv4로 고정해야 하며, DNS 변경이 실행 중 서버 상태와 어긋날 수 있어 GUI 입력으로 지원하지 않습니다.

Windows에서는 맑은 고딕, macOS에서는 Apple SD Gothic Neo/AppleGothic, Linux에서는 Noto Sans CJK KR·나눔고딕 계열을 순서대로 찾아 한글 GUI 글꼴로 사용합니다. Linux에 해당 글꼴이 없으면 설치 안내를 로그에 남깁니다.

기준 포트가 `39311`이면 다음 포트를 사용합니다.

| 용도 | 프로토콜 | 포트 |
|---|---:|---:|
| 게임 | UDP | 39311 |
| 로그인 | TCP | 39312 |
| P2P/relay | UDP | 39312 |
| 메신저 | TCP | 39313 |

두 PC에서 테스트할 때 서버 PC 방화벽에 위 TCP/UDP 포트를 허용해야 합니다.

## 명령줄 실행

GUI 없이 서버만 실행할 수 있습니다.

```powershell
p5136.exe server `
  --bind 192.168.1.10 `
  --advertise 192.168.1.10 `
  --client-dir C:\Games\KartRider_5136 `
  --allow-remote-profile-creation
```

접속기만 실행하는 예:

```powershell
p5136.exe connect `
  --game-dir C:\Games\KartRider_5136 `
  --username player1 `
  --server 192.168.1.10
```

Sikarugir wrapper를 사용하는 macOS 예:

```bash
p5136 connect \
  --game-dir "/Users/player/Games/KartRider_5136" \
  --username player \
  --server 192.168.1.10 \
  --runner sikarugir \
  --sikarugir-app "/Users/player/Applications/Sikarugir/kartrider.app"
```

`p5136.exe --help`, `p5136.exe server --help`, `p5136.exe connect --help`에서 전체 옵션을 확인할 수 있습니다.

## 로그와 문제 확인

모든 실행은 실행 파일 옆 `logs` 폴더에 새 로그를 만듭니다.

```text
logs/p5136-<timestamp>-<pid>.log
```

GUI 상단에도 현재 로그의 절대 경로가 표시됩니다. 서버가 받은 패킷과 보낸 패킷은 기본 파일 로그에 남습니다. 알 수 없는 패킷은 전체 제한형 원문을 기록한 뒤 응답 없이 소비하므로 선택 기능 하나가 전체 로그인 세션을 끊지 않습니다.

클라이언트 크래시를 조사할 때는 서버 로그와 클라이언트의 `logs` 폴더를 함께 보관하세요.

## 테스트

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

실제 클라이언트 RHO 판독 smoke test는 환경 변수를 지정해 별도로 실행합니다.

```powershell
$env:P5136_CLIENT_DATA_DIR='C:\Games\KartRider_5136\Data'
cargo test -p p5136-server configured_real_client_catalog_matches_the_known_p5136_shape -- --nocapture
```

프로토콜 근거, 완료 범위와 재개 지점은 [PORTING.md](PORTING.md), [PORTING_STATUS.md](PORTING_STATUS.md), [CLIENT_PROTOCOL_FSM.md](CLIENT_PROTOCOL_FSM.md), [ITEM_GAMEPLAY_COVERAGE.md](ITEM_GAMEPLAY_COVERAGE.md)에 정리되어 있습니다.

## 안전성과 라이선스

워크스페이스는 `unsafe_code = "forbid"`를 적용합니다. 외부 파일은 크기·개수·깊이 제한을 둔 읽기 전용 파서로 처리하고, 프로필 저장은 임시 파일과 원자적 교체를 사용합니다.

라이선스는 `AFL-3.0`입니다.
