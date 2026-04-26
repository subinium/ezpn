<p align="center">
  <img src="../assets/hero.png" width="720" alt="ezpn 데모">
</p>

<h1 align="center">ezpn</h1>

<p align="center">
  <strong>터미널 패널, 즉시.</strong><br>
  설정 없이 세션 영속성과 tmux 호환 키를 제공하는 터미널 멀티플렉서.
</p>

<p align="center">
  <a href="https://crates.io/crates/ezpn"><img src="https://img.shields.io/crates/v/ezpn?style=flat-square&color=orange" alt="crates.io"></a>
  <a href="../LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="MIT License"></a>
  <a href="https://github.com/subinium/ezpn/actions"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/ci.yml?style=flat-square&label=CI" alt="CI"></a>
  <a href="https://github.com/subinium/ezpn/actions/workflows/gitleaks.yml"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/gitleaks.yml?style=flat-square&label=gitleaks" alt="gitleaks"></a>
  <a href="https://github.com/subinium/ezpn/actions/workflows/supply-chain.yml"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/supply-chain.yml?style=flat-square&label=audit" alt="audit"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey?style=flat-square" alt="Platform">
</p>

<p align="center">
  <a href="../README.md">English</a> | <b>한국어</b> | <a href="README.ja.md">日本語</a> | <a href="README.zh.md">中文</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a>
</p>

---

## 왜 ezpn?

```bash
$ ezpn                # 터미널을 즉시 분할
$ ezpn 2 3            # 2x3 셸 그리드
$ ezpn -l dev         # 프리셋 레이아웃
```

설정 파일도, 셋업도, 러닝 커브도 없습니다. 세션은 백그라운드에서 유지 — `Ctrl+B d`로 분리, `ezpn a`로 복귀.

**프로젝트에서**, `.ezpn.toml`을 레포에 넣고 `ezpn`을 실행하면 모두가 같은 워크스페이스를 사용합니다:

```toml
[session]
name = "myproject"           # 세션 이름 고정 (충돌 시 myproject-1, -2... 로 처리)

[workspace]
layout = "7:3/1:1"
persist_scrollback = true    # 스크롤백이 분리/재연결 후에도 유지됨

[[pane]]
name = "editor"
command = "nvim ."

[[pane]]
name = "server"
command = "npm run dev"
restart = "on_failure"
env = { NODE_ENV = "${env:NODE_ENV}", DB_URL = "${file:.env.local}" }

[[pane]]
name = "tests"
command = "npm test -- --watch"

[[pane]]
name = "logs"
command = "tail -f logs/app.log"
```

```bash
$ ezpn         # .ezpn.toml을 읽고 전부 시작
$ ezpn doctor  # 실행 전에 환경 변수 보간과 시크릿 참조를 검증
```

tmuxinator도 없고. YAML도 없고. 레포에 TOML 파일 하나면 끝.

## 설치

```bash
cargo install ezpn
```

또는 [최신 릴리스](https://github.com/subinium/ezpn/releases/latest)에서 미리 빌드된 바이너리를 받으세요 — `ezpn-x86_64-unknown-linux-gnu.tar.gz`, `ezpn-x86_64-apple-darwin.tar.gz`, `ezpn-aarch64-apple-darwin.tar.gz`.

<details>
<summary>소스에서 빌드</summary>

```bash
git clone https://github.com/subinium/ezpn
cd ezpn && cargo install --path .
```

</details>

## 빠른 시작

```bash
ezpn                  # 2패널 (또는 .ezpn.toml 로드)
ezpn 2 3              # 2x3 그리드
ezpn -l dev           # 레이아웃 프리셋 (dev, monitor, quad, stack, trio...)
ezpn -e 'cmd1' -e 'cmd2'   # 패널별 명령어
```

### 세션

```bash
Ctrl+B d               # 분리 (세션은 계속 실행)
ezpn a                 # 가장 최근 세션에 재연결
ezpn a myproject       # 이름으로 재연결
ezpn ls                # 활성 세션 목록
ezpn kill myproject    # 세션 종료
ezpn --new             # $PWD에 기존 세션이 있어도 새 세션을 강제로 생성
```

세션 이름은 기본적으로 `basename($PWD)`를 따릅니다. 충돌은 결정론적으로 해결됩니다 — `repo` → `repo-1` → `repo-2` (스캔 중 죽은 소켓은 정리됨). `.ezpn.toml`의 `[session].name = "..."`로 이름을 고정할 수 있습니다.

### 탭

```bash
Ctrl+B c               # 새 탭
Ctrl+B n / p           # 다음 / 이전 탭
Ctrl+B 0-9             # 번호로 탭 이동
```

모든 tmux 키가 동작합니다 — `Ctrl+B %`로 분할, `Ctrl+B x`로 닫기, `Ctrl+B [`로 복사 모드.

## 주요 기능

| | |
|---|---|
| **제로 설정** | 바로 사용 가능. rc 파일 불필요. |
| **레이아웃 프리셋** | `dev`, `ide`, `monitor`, `quad`, `stack`, `main`, `trio` |
| **세션 유지** | tmux처럼 분리/연결. 백그라운드 데몬이 프로세스 유지. 콜드 어태치 50ms 미만. |
| **스크롤백 저장** | 옵션 `persist_scrollback`으로 분리/재연결 후에도 스크롤백 유지 (v3 스냅샷에서 gzip+bincode). |
| **탭** | tmux 스타일 윈도우. 탭 바와 마우스 클릭 전환 지원. |
| **마우스 우선** | 클릭으로 포커스, 드래그로 크기 조절, 스크롤로 히스토리, 드래그로 선택 & 복사. |
| **복사 모드** | Vi 키, 비주얼 선택, 표시 폭 기반 증분 검색, OSC 52 클립보드. |
| **커맨드 팔레트** | `Ctrl+B :` tmux 호환 명령어. |
| **브로드캐스트 모드** | 모든 패널에 동시 입력. |
| **프로젝트 설정** | `.ezpn.toml` — 레이아웃, 명령어, 환경변수, 자동 재시작. |
| **환경 변수 보간** | 패널 env에서 `${HOME}`, `${env:VAR}`, `${file:.env.local}`, `${secret:keychain:KEY}` 사용. |
| **테마** | TOML 팔레트 + 4개 빌트인 (`tokyo-night`, `gruvbox-dark`, `solarized-dark`/`-light`). |
| **핫 리로드** | `Ctrl+B r`로 분리 없이 `~/.config/ezpn/config.toml` 재로드. |
| **보더리스 모드** | `ezpn -b none`으로 화면 공간 극대화. |
| **Kitty 키보드** | `Shift+Enter`, `Ctrl+Arrow`, Alt+Char (CSI u / RFC 3665) — 수정 키 정상 동작. |
| **CJK/유니코드** | 한국어, 중국어, 일본어, 이모지 정확한 폭 계산. |
| **크래시 격리** | 패닉이 발생한 패널 하나가 데몬을 죽이지 못함 (시그널 안전한 SIGTERM/SIGCHLD 처리). |
| **스크립트 가능 입력** | `ezpn-ctl send-keys --pane N -- 'cmd' Enter` — 에디터, AI 에이전트, CI 스크립트용. |
| **이벤트 스트림** | 바이너리 프로토콜 위 장수명 `S_EVENT` 구독 (`-CC` 스타일 통합용). |
| **훅** | 선언적 `[[hooks]]` 설정: 데몬 이벤트마다 셸 실행, 워커 풀 + 훅별 타임아웃. |
| **정규식 검색** | `[copy_mode] search = "regex"` — 복사 모드 검색을 POSIX 패턴 + 스마트 케이스로. |
| **패널별 히스토리** | `ezpn-ctl clear-history --pane N` / `set-scrollback --pane N --lines L` 런타임 제어. |

## 레이아웃 프리셋

```bash
ezpn -l dev       # 7:3 — 메인 + 사이드
ezpn -l ide       # 7:3/1:1 — 에디터 + 사이드바 + 하단 2개
ezpn -l monitor   # 1:1:1 — 3열 균등
ezpn -l quad      # 2x2 그리드
ezpn -l stack     # 1/1/1 — 3행 쌓기
ezpn -l main      # 6:4/1 — 상단 넓은 쌍 + 하단 전체
ezpn -l trio      # 1/1:1 — 상단 전체 + 하단 2개
```

커스텀 비율: `ezpn -l '7:3/5:5'`

## 프로젝트 설정

프로젝트 루트에 `.ezpn.toml`을 넣고 `ezpn`을 실행하세요. 끝.

**패널별 옵션:** `command`, `cwd`, `name`, `env`, `restart` (`never`/`on_failure`/`always`), `shell`

```bash
ezpn init              # .ezpn.toml 템플릿 생성
ezpn from Procfile     # Procfile에서 가져오기
ezpn doctor            # 설정 + 환경 변수 보간 검증, 참조 누락 시 비-0 종료
```

### 훅

데몬 이벤트마다 셸 명령을 실행. 4-스레드 워커 풀에 훅별 `timeout_ms`; 각 자식 프로세스는 자체 그룹에서 spawn되어 SIGTERM → SIGKILL 단계적 종료가 트리 전체에 도달합니다.

```toml
# ~/.config/ezpn/config.toml 또는 .ezpn.toml

[[hooks]]
event = "client-attached"
command = "notify-send 'pane {client_id} attached'"
shell = true
timeout_ms = 2000

[[hooks]]
event = "tab-created"
command = ["/usr/local/bin/ezpn-tab-init", "{name}", "{tab_index}"]
```

v0.11에서 `client-attached`, `client-detached`, `tab-created`, `tab-closed`, `session-renamed` 5개 이벤트를 와이어. 변수 치환(`{session}`, `{client_id}`, `{pane_id}`, …)은 exec 전 `command` 문자열에 이벤트별 값을 삽입.

### 환경 변수 보간

패널 env 값은 네 가지 참조 형식을 지원합니다:

```toml
[[pane]]
command = "npm run dev"
env = {
  HOME       = "${HOME}",                    # 프로세스 env
  NODE_ENV   = "${env:NODE_ENV}",            # 명시적 env
  DB_URL     = "${file:.env.local}",         # dotenv 스타일 파일 조회
  GH_TOKEN   = "${secret:keychain:GH_TOKEN}",# macOS Keychain (Linux: secret-tool)
}
```

`.ezpn.toml` 옆의 `.env.local`은 자동으로 머지되어 `[env]`를 덮어씁니다. OS 키체인을 사용할 수 없을 때 `${secret:keychain:KEY}`는 경고와 함께 `${env:KEY}`로 폴백합니다. 순환을 잡기 위해 재귀 깊이는 8로 제한됩니다.

### 테마

```toml
# .ezpn.toml 또는 ~/.config/ezpn/config.toml
theme = "tokyo-night"   # default | tokyo-night | gruvbox-dark | solarized-dark | solarized-light
```

사용자 테마는 `~/.config/ezpn/themes/<name>.toml`에서 로드됩니다. ezpn은 `$COLORTERM` / `$TERM`을 자동 감지하며 트루컬러가 지원되지 않을 때 256색 또는 16색으로 다운그레이드합니다.

<details>
<summary>글로벌 설정 (~/.config/ezpn/config.toml)</summary>

```toml
border = rounded            # single | rounded | heavy | double | none
shell = /bin/zsh
scrollback = 10000
status_bar = true
tab_bar = true
prefix = b                  # 프리픽스 키 (Ctrl+<key>)
theme = default             # default | tokyo-night | gruvbox-dark | solarized-dark | solarized-light
persist_scrollback = false  # 자동 스냅샷에 스크롤백 저장 (기본 비활성)
```

설정 패널 변경(`Ctrl+B Shift+,`)은 원자적으로 저장됩니다. 디스크에서 다시 불러오려면 `Ctrl+B r`.

</details>

## 키 바인딩

**직접 단축키:**

| 키 | 동작 |
|---|---|
| `Ctrl+D` | 좌우 분할 |
| `Ctrl+E` | 상하 분할 |
| `Ctrl+N` | 다음 패널 |
| `F2` | 크기 균등화 |

**프리픽스 모드** (`Ctrl+B` 후):

| 키 | 동작 |
|---|---|
| `%` / `"` | 좌우 / 상하 분할 |
| `o` / Arrow | 패널 이동 |
| `x` | 패널 닫기 |
| `z` | 줌 토글 |
| `R` | 크기 조절 모드 |
| `[` | 복사 모드 |
| `B` | 브로드캐스트 |
| `:` | 커맨드 팔레트 |
| `r` | 설정 재로드 |
| `d` | 디태치 |
| `?` | 도움말 |

<details>
<summary>전체 키 바인딩 참조</summary>

**탭:**

| 키 | 동작 |
|---|---|
| `Ctrl+B c` | 새 탭 |
| `Ctrl+B n` / `p` | 다음 / 이전 탭 |
| `Ctrl+B 0-9` | 번호로 탭 이동 |
| `Ctrl+B ,` | 탭 이름 변경 |
| `Ctrl+B &` | 탭 닫기 |

**패널:**

| 키 | 동작 |
|---|---|
| `Ctrl+B {` / `}` | 이전 / 다음 패널과 교환 |
| `Ctrl+B E` / `Space` | 크기 균등화 |
| `Ctrl+B s` | 상태 바 토글 |
| `Ctrl+B q` | 패널 번호 + 빠른 이동 |

**복사 모드** (`Ctrl+B [`):

| 키 | 동작 |
|---|---|
| `h` `j` `k` `l` | 커서 이동 |
| `w` / `b` | 다음 / 이전 단어 |
| `0` / `$` / `^` | 줄 시작 / 끝 / 첫 문자 |
| `g` / `G` | 스크롤백 맨 위 / 맨 아래 |
| `Ctrl+U` / `Ctrl+D` | 반 페이지 위 / 아래 |
| `v` | 문자 선택 |
| `V` | 줄 선택 |
| `y` / `Enter` | 복사 후 종료 |
| `/` / `?` | 앞으로 / 뒤로 검색 |
| `n` / `N` | 다음 / 이전 일치 |
| `q` / `Esc` | 종료 |

**마우스:**

| 동작 | 효과 |
|---|---|
| 패널 클릭 | 포커스 |
| 더블클릭 | 줌 토글 |
| 탭 클릭 | 탭 전환 |
| `[x]` 클릭 | 패널 닫기 |
| 보더 드래그 | 크기 조절 |
| 텍스트 드래그 | 선택 + 복사 |
| 스크롤 휠 | 스크롤백 히스토리 |

**macOS 참고:** Alt+Arrow 방향 이동은 Option을 Meta로 설정해야 합니다 (iTerm2: Preferences > Profiles > Keys > `Esc+`).

</details>

<details>
<summary>커맨드 팔레트 명령어</summary>

`Ctrl+B :` 명령어 프롬프트. tmux 별칭 모두 지원.

```
split / split-window         좌우 분할
split -v                     상하 분할
new-tab / new-window         새 탭
next-tab / prev-tab          탭 전환
close-pane / kill-pane       패널 닫기
close-tab / kill-window      탭 닫기
rename-tab <name>            탭 이름 변경
layout <spec>                레이아웃 변경
equalize / even              크기 균등화
zoom                         줌 토글
broadcast                    브로드캐스트 토글
```

</details>

## ezpn vs. tmux vs. Zellij

| | tmux | Zellij | **ezpn** |
|---|---|---|---|
| 설정 | `.tmux.conf` 필요 | KDL 설정 | **제로 설정** |
| 첫 사용 | 빈 화면 | 튜토리얼 모드 | **`ezpn`** |
| 세션 | `tmux a` | `zellij a` | **`ezpn a`** |
| 프로젝트 설정 | tmuxinator (gem) | — | **`.ezpn.toml` 내장** |
| 브로드캐스트 | `:setw synchronize-panes` | — | **`Ctrl+B B`** |
| 자동 재시작 | — | — | **`restart = "always"`** |
| Kitty 키보드 | 미지원 | 지원 | **지원** |
| 플러그인 | — | WASM | — |
| 생태계 | 거대 (30년) | 성장중 | 신규 |

**ezpn** — 설정 없이 바로 쓰는 터미널 분할 + `ezpn-ctl send-keys` / 이벤트 스트림 / 훅 스크립팅 표면.
**tmux** — 깊은 플러그인 생태계(TPM 등)가 필요할 때.
**Zellij** — WASM 플러그인을 원할 때.

## CLI 레퍼런스

```
ezpn [ROWS COLS]         그리드 레이아웃으로 시작
ezpn -l <PRESET>         레이아웃 프리셋으로 시작
ezpn -e <CMD> [-e ...]   패널별 명령어
ezpn -S <NAME>           이름 지정 세션
ezpn -b <STYLE>          보더 스타일 (single/rounded/heavy/double/none)
ezpn --new               새 세션 강제 (기존 세션 자동 연결 건너뛰기)
ezpn a [NAME]            세션 연결
ezpn ls                  세션 목록
ezpn kill [NAME]         세션 종료
ezpn rename OLD NEW      세션 이름 변경
ezpn init                .ezpn.toml 템플릿 생성
ezpn from <FILE>         Procfile에서 가져오기
ezpn doctor              .ezpn.toml + 환경 변수 보간 검증
```

### `ezpn-ctl` (스크립팅)

```
ezpn-ctl list                                패널 목록
ezpn-ctl split [horizontal|vertical] [PANE]  패널 분할
ezpn-ctl close PANE                          패널 닫기
ezpn-ctl focus PANE                          패널 포커스
ezpn-ctl save <PATH>                         워크스페이스 스냅샷 저장
ezpn-ctl load <PATH>                         워크스페이스 복원
ezpn-ctl exec PANE <CMD>                     패널을 새 명령으로 교체

ezpn-ctl send-keys [--pane N | --target current] [--literal] -- <key>...
                                             코드 토큰 또는 raw 바이트를 패널 PTY로 전송.
                                             예시:
                                               ezpn-ctl send-keys --pane 0 -- 'echo hi' Enter
                                               ezpn-ctl send-keys --target current -- C-c
                                               ezpn-ctl send-keys --pane 0 --literal -- $'#!/bin/sh\nexit 0\n'

ezpn-ctl clear-history --pane N              화면 위 스크롤백 제거
ezpn-ctl set-scrollback --pane N --lines L   스크롤백 링 크기 변경 (scrollback_max_lines로 제한)
```

## 라이선스

[MIT](../LICENSE)
