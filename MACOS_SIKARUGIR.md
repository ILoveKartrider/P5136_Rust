# P5136 + Sikarugir 수동 설정 walkthrough (macOS)

최종 검토: 2026-08-11. 다른 사용자·프로토콜 문서는
[전체 문서 안내](DOCUMENTATION.md)를 참고하세요.

이 문서는 Sikarugir가 만든 wrapper의 Wine prefix를 P5136 클라이언트에
맞게 설정하는 절차입니다. 게임 EXE, Sikarugir 앱 또는 별도 wrapper를
만들거나 배포하지 않습니다.

## 전제 조건

- Sikarugir가 설치되어 있어야 합니다.
- 사용할 `KartRider.exe`와 게임 데이터가 한 폴더에 있어야 합니다.
- P5136 Rust를 직접 빌드했거나 macOS 릴리스 바이너리를 준비해야 합니다.

아래 예시에서는 wrapper 경로를 `WRAPPER`, 게임 폴더를 `GAME_DIR`로
표기합니다.

## 1. Sikarugir wrapper 만들기

Sikarugir에서 **Install Software**를 선택하고 `KartRider.exe`를 지정해
wrapper를 만듭니다. Test Run만 한 경우 Finder의 Applications 목록에
wrapper가 보이지 않을 수 있으므로 설치가 끝난 뒤 생성된 `.app`을
사용해야 합니다.

예시 경로:

```text
/Users/<사용자>/Applications/Sikarugir/kartrider.app
```

## 2. 게임을 prefix의 C: 드라이브에 연결

다음 값을 실제 경로로 바꿉니다. `GAME_DIR` 바로 아래에
`KartRider.exe`가 있어야 합니다.

```bash
WRAPPER="/Users/<사용자>/Applications/Sikarugir/kartrider.app"
GAME_DIR="/Users/<사용자>/Games/KartRider_5136"
PREFIX="$WRAPPER/Contents/SharedSupport/prefix"

mkdir -p "$PREFIX/drive_c/Nexon"
ln -s "$GAME_DIR" "$PREFIX/drive_c/Nexon/KartRider_5136"
```

`KartRider_5136`가 이미 존재한다면 덮어쓰지 말고 먼저 대상과 유형을
확인합니다. 일반 폴더라면 wrapper를 새로 만들거나 내용을 직접
검토하십시오.

```bash
ls -ld "$PREFIX/drive_c/Nexon/KartRider_5136"
```

## 3. RootPath 레지스트리 설정

`KartRider_5136.reg` 파일을 만들어 다음 내용을 저장합니다.

```reg
Windows Registry Editor Version 5.00

[HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Nexon\KartRider_5136\M01]
"RootPath"="C:\\Nexon\\KartRider_5136"
```

Sikarugir wrapper에 포함된 Wine으로 가져옵니다.

```bash
WINE="$WRAPPER/Contents/SharedSupport/wine/bin/wine"
DYLD_FALLBACK_LIBRARY_PATH="$WRAPPER/Contents/Frameworks:$WRAPPER/Contents/SharedSupport/wine/lib" \
WINEPREFIX="$PREFIX" "$WINE" regedit "$GAME_DIR/KartRider_5136.reg"
```

게임 폴더에 올바른 `KartRider_5136.reg`가 이미 있다면 그대로 사용해도
됩니다.

## 4. wrapper의 작업 디렉터리 지정

Sikarugir wrapper는 보통 `prefix/drive_c/exec*.bat`를 시작합니다. 실제
파일명을 먼저 확인하고 수정 전 백업합니다.

```bash
ls "$PREFIX/drive_c"/exec*.bat
cp "$PREFIX/drive_c/exec2006615976.bat" \
  "$PREFIX/drive_c/exec2006615976.bat.backup"
```

해당 배치 파일에서 EXE를 시작하기 전에 게임 폴더로 이동합니다.

```bat
@echo off
cd /d "Z:\Users\<사용자>\Games\KartRider_5136"
"KartRider.exe" -profile:launcher
```

Wine의 `Z:`는 macOS 루트(`/`)입니다. 따라서 `GAME_DIR`의 macOS 경로를
`Z:\` 뒤에 백슬래시 형태로 적습니다. 작업 디렉터리가 다르면 첫 연결
후 Nexon 인증 단계가 실패할 수 있습니다.

## 5. CoreAudio에서 멈출 때만 오디오 비활성화

`LastRunWine.log`에 `mmdevapi` assertion 또는 CoreAudio 채널 레이아웃
오류가 있고 게임이 멈출 때만 다음 레지스트리를 추가합니다. 이 설정을
적용하면 게임 소리가 나지 않을 수 있습니다.

```reg
Windows Registry Editor Version 5.00

[HKEY_CURRENT_USER\Software\Wine\DllOverrides]
"winecoreaudio.drv"="disabled"
```

별도 `.reg` 파일로 저장한 뒤 3단계와 같은 `regedit` 명령으로
가져옵니다.

## 6. P5136에서 실행

GUI의 **접속기** 탭에서 다음 값을 입력합니다.

- 게임 디렉터리: `GAME_DIR`
- 실행 파일: 비움 (`KartRider.exe` 기본값)
- 실행 방식: `Sikarugir wrapper`
- Sikarugir wrapper 앱: `WRAPPER`

그 뒤 **클라이언트 준비 및 실행**을 누릅니다. 명령행에서는 다음과
같습니다.

```bash
p5136 connect \
  --game-dir "$GAME_DIR" \
  --username player \
  --server <P5136_서버_IP> \
  --runner sikarugir \
  --sikarugir-app "$WRAPPER"
```

`KartRider.exe` 이외의 파일명을 검증 대상으로 명시해야 한다면
`--game-exe`에 게임 폴더 기준 상대 경로나 절대 경로를 지정할 수
있습니다. Sikarugir가 실제로 실행하는 파일은 wrapper의 `exec*.bat`가
결정하므로 그 배치 파일도 같은 EXE를 가리켜야 합니다.

옵저버 방장으로 접속하려면 `--observer`, 정적으로 복원한 익명 리그
pmap 1798을 사용하려면 `--anonymous-league`를 추가합니다. 두 역할은
동시에 선택할 수 없습니다.

## 확인 순서

1. P5136 로그에서 `PqLogin` 수신과 `PrLogin` 송신이 보이면 인증 및
   로그인까지 성공한 것입니다.
2. 게임 UDP와 P2P UDP 패킷이 이어지면 P5136 연결도 완료된 것입니다.
3. 멈추면 wrapper의
   `Contents/SharedSupport/Logs/LastRunWine.log`를 확인합니다.
