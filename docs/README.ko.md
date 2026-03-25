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
[workspace]
layout = "7:3/1:1"

[[pane]]
name = "editor"
command = "nvim ."

[[pane]]
name = "server"
command = "npm run dev"
restart = "on_failure"

[[pane]]
name = "tests"
command = "npm test -- --watch"

[[pane]]
name = "logs"
command = "tail -f logs/app.log"
```

```bash
$ ezpn   # .ezpn.toml을 읽고 전부 시작
```

tmuxinator도 없고. YAML도 없고. 레포에 TOML 파일 하나면 끝.

## 설치

```bash
cargo install ezpn
```

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
```

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
| **세션 유지** | tmux처럼 분리/연결. 백그라운드 데몬이 프로세스 유지. |
| **탭** | tmux 스타일 윈도우. 탭 바와 마우스 클릭 전환 지원. |
| **마우스 우선** | 클릭으로 포커스, 드래그로 크기 조절, 스크롤로 히스토리, 드래그로 선택 & 복사. |
| **복사 모드** | Vi 키, 비주얼 선택, 증분 검색, OSC 52 클립보드. |
| **커맨드 팔레트** | `Ctrl+B :` tmux 호환 명령어. |
| **브로드캐스트 모드** | 모든 패널에 동시 입력. |
| **프로젝트 설정** | `.ezpn.toml` — 레이아웃, 명령어, 환경변수, 자동 재시작. |
| **보더리스 모드** | `ezpn -b none`으로 화면 공간 극대화. |
| **Kitty 키보드** | `Shift+Enter`, `Ctrl+Arrow` 등 수정 키 정상 동작. |
| **CJK/유니코드** | 한국어, 중국어, 일본어, 이모지 정확한 폭 계산. |

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
```

<details>
<summary>글로벌 설정</summary>

`~/.config/ezpn/config.toml`:

```toml
border = rounded        # single | rounded | heavy | double | none
shell = /bin/zsh
scrollback = 10000
status_bar = true
tab_bar = true
prefix = b              # 프리픽스 키 (Ctrl+<key>)
```

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

**ezpn** — 설정 없이 바로 쓰는 터미널 분할.
**tmux** — 깊은 스크립팅과 플러그인 생태계가 필요할 때.
**Zellij** — 모던 UI와 WASM 플러그인을 원할 때.

## CLI 레퍼런스

```
ezpn [ROWS COLS]         그리드 레이아웃으로 시작
ezpn -l <PRESET>         레이아웃 프리셋으로 시작
ezpn -e <CMD> [-e ...]   패널별 명령어
ezpn -S <NAME>           이름 지정 세션
ezpn -b <STYLE>          보더 스타일 (single/rounded/heavy/double/none)
ezpn a [NAME]            세션 연결
ezpn ls                  세션 목록
ezpn kill [NAME]         세션 종료
ezpn rename OLD NEW      세션 이름 변경
ezpn init                .ezpn.toml 템플릿 생성
ezpn from <FILE>         Procfile에서 가져오기
```

## 라이선스

[MIT](../LICENSE)
