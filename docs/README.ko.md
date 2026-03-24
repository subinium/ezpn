<p align="center">
  <img src="../assets/hero.png" width="720" alt="ezpn 실행 화면">
</p>

# ezpn

명령어 하나로 터미널을 분할하세요. 클릭, 드래그, 끝.

[![License](https://img.shields.io/badge/license-MIT-blue)](../LICENSE)
[![Crate](https://img.shields.io/badge/crates.io-v0.2.0-orange)](https://crates.io/crates/ezpn)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey)]()

[English](../README.md) | **한국어** | [日本語](README.ja.md) | [中文](README.zh.md) | [Español](README.es.md) | [Français](README.fr.md)

## 설치

```bash
cargo install ezpn
```

## 사용법

```bash
ezpn              # 2분할 (좌우)
ezpn 4            # 4분할 (가로)
ezpn 3 -d v       # 3분할 (세로)
ezpn 2 3          # 2×3 그리드
ezpn --layout '7:3/1:1'   # 비율 지정 레이아웃
ezpn -e 'make watch' -e 'npm dev'   # 패널별 명령어
```

`-e/--exec`로 전달된 명령어는 `$SHELL -l -c`로 실행됩니다.

## 조작법

**마우스:**

| 동작 | 효과 |
|------|------|
| 패널 클릭 | 포커스 |
| `×` 클릭 | 패널 닫기 |
| 보더 드래그 | 크기 조절 |
| 스크롤 | 활성 패널 스크롤 |

**키보드 (직접 단축키):**

| 키 | 동작 |
|---|---|
| `Ctrl+D` | 좌우 분할 |
| `Ctrl+E` | 상하 분할 |
| `Ctrl+N` | 다음 패널 |
| `Ctrl+G` | 설정 패널 (j/k로 이동) |
| `Ctrl+W` | 종료 |

**tmux 호환 키 (`Ctrl+B` 후):**

| 키 | 동작 |
|---|---|
| `%` | 좌우 분할 |
| `"` | 상하 분할 |
| `o` | 다음 패널 |
| `Arrow` | 방향 이동 |
| `x` | 패널 닫기 |
| `[` | 스크롤 모드 (j/k/g/G, q로 나가기) |
| `d` | 종료 (확인 대화상자) |

## 주요 기능

- **자유 레이아웃** — 그리드, 비율 지정, 개별 분할, 드래그 리사이즈
- **패널별 명령어** — `-e 'htop' -e 'npm dev' -e 'tail -f log'`
- **타이틀 바 버튼** — `[━] [┃] [×]` 클릭으로 분할/닫기
- **tmux 프리픽스 키** — `Ctrl+B` 후 tmux 키 사용 가능
- **스크롤 모드** — `Ctrl+B [` → vim 키로 히스토리 탐색
- **설정 패널** — `Ctrl+G` → 어두운 모달, vim 키 네비게이션
- **IPC 원격 제어** — `ezpn-ctl`로 자동화
- **워크스페이스 저장/복원** — `ezpn-ctl save/load`
- **중첩 방지** — `$EZPN` 환경변수로 자동 차단

## 비교

|  | tmux | Zellij | ezpn |
|---|---|---|---|
| 설정 | `.tmux.conf` | KDL 파일 | CLI 플래그 |
| 분할 | `Ctrl+B %` | 모드 전환 | `Ctrl+D` / 클릭 |
| 크기 조절 | `:resize-pane` | 리사이즈 모드 | 드래그 |
| Detach | 가능 | 가능 | 불가 |

세션 유지가 필요하면 tmux/Zellij. 빠르게 화면만 분할하고 싶으면 ezpn.

## 라이선스

[MIT](../LICENSE)
