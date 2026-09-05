<p align="center">
  <img src="../assets/hero.png" width="720" alt="ezpn 데모">
</p>

<h1 align="center">ezpn</h1>

<p align="center">
  <strong>터미널 패널, 즉시.</strong><br>
  마우스 조작, 지속되는 세션, 익숙한 접두 키를 지원하는 macOS·Linux용 터미널 멀티플렉서.
</p>

<p align="center">
  <a href="https://crates.io/crates/ezpn"><img src="https://img.shields.io/crates/v/ezpn?style=flat-square&color=orange" alt="crates.io"></a>
  <a href="../LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="MIT License"></a>
  <a href="https://github.com/subinium/ezpn/actions"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/ci.yml?style=flat-square&label=CI" alt="CI"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey?style=flat-square" alt="Platform">
</p>

<p align="center">
  <a href="../README.md">English</a> | <b>한국어</b> | <a href="README.ja.md">日本語</a> | <a href="README.zh.md">中文</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a>
</p>

---

## 작업 시작

```sh
cargo install ezpn --locked
ezpn                 # two shells
ezpn 2 3             # a 2-by-3 grid
ezpn -S work         # create or reattach to a named session
```

빌드에는 Rust 1.88 이상이 필요합니다. [GitHub Releases](https://github.com/subinium/ezpn/releases)에서
macOS·Linux 바이너리를 받을 수 있으며, 체크섬이 제공되면 함께 확인하세요.
ezpn은 실행형 터미널 멀티플렉서이며, Rust 애플리케이션에 임베드하는 GUI 라이브러리가 아닙니다.

## 세션과 SSH

```sh
ezpn a work
ezpn a work --shared
ezpn a work --readonly
ezpn ls
ezpn kill work
```

`Ctrl+B` 다음 `d`를 누르면 현재 클라이언트만 분리됩니다. 비활성 탭의 작업을 포함해
셸 프로세스는 계속 실행됩니다. 다시 연결하면 기존 프로세스에 접속합니다.
읽기 전용 클라이언트는 입력을 보내거나 쓰기 가능한 클라이언트의 작업 영역 크기를 바꿀 수 없습니다.

원격 호스트에 ezpn을 설치하고 PATH에서 실행할 수 있도록 설정하세요.

```sh
ssh -t host 'ezpn -S work'
ssh -t host 'ezpn a work'
ssh -J bastion -t host 'ezpn a work'
```

SSH 연결에는 PTY 할당이 필요합니다. SSH 클라이언트의 연결이 끊겨도 원격 데몬은 종료되지 않습니다.
암호화, 인증, 호스트 키 검증, 포워딩은 OpenSSH가 담당합니다.
ezpn의 로컬 Unix 소켓을 인증되지 않은 네트워크에 노출하지 마세요.

## 마우스와 키보드

| 조작 | 동작 |
| --- | --- |
| 패널 내용 클릭 | 해당 패널로 포커스 이동 |
| 구분선 드래그 | 분할 크기 조정 |
| 제목 표시줄의 분할 버튼 | 선택한 패널 분할 |
| 제목 표시줄의 닫기 버튼 | 확인 후 닫기 |
| 탭 클릭 | 탭 전환 |
| 스크롤 | 기록을 스크롤하거나 마우스를 지원하는 앱에 전달 |
| 텍스트 드래그 | 선택 및 복사 |
| Shift + 드래그 | 앱에 마우스 입력을 보내는 대신 ezpn 텍스트 선택 |
| 마우스를 처리하지 않는 앱의 내용 더블 클릭 | 확대 전환 |
| F1 / F2 | 설정 / 크기 균등화 |
| Alt + 방향키 | 패널 이동. macOS에서는 Option을 Meta로 설정 |

앱으로 전달하는 클릭, 이동, 휠, 버튼 해제 이벤트는 앱이 요청한 마우스 인코딩을 사용합니다.
`Ctrl+D`, `Ctrl+E`, `Ctrl+W` 등의 키는 명시적으로 재지정하지 않는 한 셸로 전달됩니다.
이 키들은 더 이상 패널을 분할하거나 종료를 요청하지 않습니다.

`Ctrl+B`를 누른 뒤 다음 접두 키 명령을 사용할 수 있습니다.

| 키 | 동작 |
| --- | --- |
| `%` / `"` | 열 / 행 분할 |
| `o` / 방향키 | 패널 이동 |
| `x` | 패널 닫기 확인 |
| `z` | 확대 전환 |
| `R` | 크기 조정 모드 |
| `Space` / `E` | 크기 균등화 |
| `c` / `n` / `p` | 새 탭 / 다음 탭 / 이전 탭 |
| `0`–`9` | 0부터 시작하는 인덱스로 탭 선택 |
| `,` / `&` | 탭 이름 변경 / 닫기 확인 |
| `[` | 복사 모드 |
| `:` | 명령 팔레트 |
| `r` | 전역 설정 다시 불러오기 |
| `B` | 동시 입력 전환 |
| `d` | 현재 클라이언트 분리 |
| `?` | 도움말 |
| `Ctrl+B` | 앱에 접두 키 전달 |

복사 모드에서는 vi 이동, `v`/`V` 선택, `y` 또는 Enter로 복사,
`/`/`?` 검색, `n`/`N`으로 다음/이전 일치 항목 이동, `q`/Escape로 나가기를 지원합니다.
일부 익숙한 tmux 키 바인딩을 지원하지만, tmux 명령 전체와 호환되는 것은 아닙니다.

## 작업을 유지하며 레이아웃 변경

```sh
ezpn -l dev       # 7:3
ezpn -l ide       # 7:3/1:1
ezpn -l quad      # 2-by-2
ezpn -l '7:3/5:5'
ezpn -b none
```

명령 팔레트의 `select-layout`은 실행 중인 프로세스를 그대로 두고 재배치합니다.
패널 수가 다른 레이아웃은 거부하므로, 패널은 명시적으로 분할하거나 닫아야 합니다.
분할이나 스냅샷 불러오기가 실패해도 현재 작업 영역은 유지됩니다.

## 신뢰하는 프로젝트 작업 영역

자동 실행을 허용하기 전에 저장소의 명령을 검토하세요.

```toml
# .ezpn.toml
[workspace]
layout = "7:3"

[[pane]]
name = "shell"
cwd = "."

[[pane]]
name = "worker"
command = "printf 'ready\\n'; exec sh"
restart = "on_failure"
```

```sh
ezpn init
ezpn doctor
ezpn --trust-project
```

`--trust-project`는 `.ezpn.toml` / Procfile의 자동 실행을 허용합니다.
저장소 명령을 불러오지 않고 일반 셸을 시작하려면 `ezpn 1 2`처럼 그리드를 명시하세요.
`doctor`는 읽기 전용으로 구문을 검사하며, 명령을 실행하거나 비밀값을 해석하지 않습니다.

프로젝트 환경변수 확장에서는 환경변수·파일·비밀값 참조를 사용할 수 있습니다.
외부 값은 진단 메시지에 출력하지 않습니다. 설정에서 외부 값을 읽은 패널은
실행 가능한 스냅샷 메타데이터와 기록에서 제외되며, 복원 시 깨끗한 셸로 열립니다.
해석된 인증 정보가 그대로 저장되는 것을 막고 개인정보 보호를 우선하는 의도적인 정책입니다.
[설정](../docs/configuration.md)과 [보안](../docs/security.md)을 참고하세요.

## 설정과 복구

```toml
# ~/.config/ezpn/config.toml
[global]
border = "rounded"
scrollback = 10000
persist_scrollback = false

[keys]
prefix = "b"

[theme]
name = "ezpn-dark"
```

테마: `ezpn-dark`, `ezpn-light`, `nord`, `gruvbox-dark`, `solarized-dark`.
사용자 키맵은 `[keymap.normal]`, `[keymap.prefix]`, `[keymap.copy_mode]`에 정의합니다.
`Ctrl+B r`은 검증된 한 번의 파일 읽기 결과를 바탕으로 지원되는 설정을 다시 불러옵니다.
설정 패널에서 저장이 실패하면 성공으로 표시하지 않고 오류를 알립니다.

디스크 스냅샷과 분리된 채 실행 중인 세션은 다릅니다. `ezpn --restore FILE`은
**새 프로세스를 시작합니다**. 저장을 허용한 기록은 텍스트로 복원되며, 실행 중인 편집기,
프로세스 메모리, 터미널 그래픽, 정확한 대체 화면 상태를 복원하지는 않습니다.
스냅샷 파일에는 크기·압축 해제 제한과 비공개 접근 권한이 적용됩니다.

## 호환성과 검증 근거

- 지원 플랫폼 범위는 UTF-8 ANSI 터미널과 Unix PTY를 사용하는 macOS·Linux입니다.
  네이티브 Windows는 지원하지 않습니다.
- 자식 앱의 키보드 프로토콜 협상과 호스트 기능은 별개입니다. 레거시 앱에는 기존 시퀀스를
  전달하며, 지원되는 Kitty 확장 기능은 명시적으로 활성화해야 합니다.
- 앱의 클립보드 쓰기는 설정된 OSC 52 정책을 따릅니다.
  SSH에서 사용자가 복사한 내용은 원격 데스크톱 클립보드보다 연결된 터미널을 우선합니다.
- 렌더링에는 범위 제한이 있으며 작은 화면은 잘라 표시합니다. 파서 제한, 지원 시퀀스,
  미검증 GUI 에뮬레이터 조합은 [터미널 호환성](../docs/terminal-protocol.md)을 참고하세요.
- `--features render-diff`는 크기가 제한된 선택적 ANSI 차이 출력 경로를 활성화합니다.
  지원하지 않는 프레임은 원래 출력으로 돌아갑니다. 모든 상황에서 빨라진다는 보장은 아닙니다.
- 실제 PTY 테스트는 연결·분리, 크기 조정, 공유·읽기 전용 클라이언트, 전송 중단을 다룹니다.
  별도의 격리된 루프백 SSH 테스트로 실제 SSH와 시뮬레이션을 구분합니다.
- 장시간 안정성 테스트와 tmux/Zellij 성능 비교는 별도의 검증 대상입니다.
  ezpn이 두 프로젝트보다 항상 빠르거나 메모리를 적게 쓴다고 주장하지 않습니다.

[릴리스 감사 기록](../docs/audits/v0.14.0.md)에 결과와 남은 제약을 정리합니다.
[사전 점검 스크립트](../scripts/preflight.py)는 PASS/FAIL/SKIP과 실제 종료 코드를 기록하며,
실패한 테스트를 무시된 자리표시자 뒤에 숨기지 않습니다.

## 문서

[시작하기](../docs/getting-started.md) · [설정](../docs/configuration.md) ·
[SSH와 터미널 프로토콜](../docs/terminal-protocol.md) · [클립보드](../docs/clipboard.md) ·
[보안](../docs/security.md) · [스크립팅 제한](../docs/scripting.md) ·
[기여하기](../CONTRIBUTING.md) · [변경 기록](../CHANGELOG.md)

## 라이선스

[MIT](../LICENSE)
