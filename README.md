<p align="center">
  <img src="assets/hero.gif" width="680" alt="ezpn in action">
</p>

# ezpn

A terminal pane splitter. Click to select. Drag to resize. No config needed.

[![CI](https://img.shields.io/badge/build-passing-brightgreen)]()
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Crate](https://img.shields.io/badge/crates.io-v0.1.0-orange)](https://crates.io/crates/ezpn)

## Install

```bash
cargo install --path .
```

## Usage

```bash
ezpn              # 2 panes, side by side
ezpn 4            # 4 horizontal panes
ezpn 3 -d v       # 3 vertical panes
ezpn 2 3          # 2×3 grid
```

Then split any pane further with `Ctrl+D` (horizontal) or `Ctrl+E` (vertical).

## Controls

**Mouse** — the primary way to interact:

| | |
|---|---|
| Click pane | Focus |
| Click `×` | Close pane |
| Drag border | Resize |
| Scroll | Scroll active pane |

**Keyboard:**

| | |
|---|---|
| `Ctrl+D` | Split left \| right |
| `Ctrl+E` | Split top / bottom |
| `F2` | Equalize all sizes |
| `Ctrl+]` | Next pane |
| `Ctrl+G` | Settings |
| `Ctrl+\` | Quit |

<details>
<summary>macOS: Alt+Arrow for directional navigation</summary>

`Alt+Arrow` navigates between panes directionally. This requires your terminal to send Option as Meta:

- **iTerm2**: Preferences → Profiles → Keys → Left Option Key → `Esc+`
- **Terminal.app**: Settings → Profiles → Keyboard → Use Option as Meta Key
</details>

## Features

**Flexible layouts** — Start with a grid, split individual panes, drag to resize. Auto-equalizes on split. Press `F2` to reset sizes.

```
╭────────┬────╮       ╭────────┬────╮
│        │ 2  │       │        │ 2  │
│   1    ├────┤  ──>  │   1    ├──┬─┤
│        │ 3  │       │        │3 │4│
╰────────┴────╯       ╰────────┴──┴─╯
```

**Settings panel** — `Ctrl+G` opens a live overlay. Change border style, split panes, toggle the status bar. Mouse or keyboard.

**Border styles** — `--border` flag or change in settings:

```
single           rounded (default) heavy            double
┌──────┬──────┐  ╭──────┬──────╮  ┏━━━━━━┳━━━━━━┓  ╔══════╦══════╗
│      │      │  │      │      │  ┃      ┃      ┃  ║      ║      ║
└──────┴──────┘  ╰──────┴──────╯  ┗━━━━━━┻━━━━━━┛  ╚══════╩══════╝
```

**Dead pane recovery** — When a shell exits, the pane dims and shows `[exited]`. Press `Enter` to respawn, or `×` to close.

**Nesting prevention** — Running `ezpn` inside an ezpn pane is blocked via `$EZPN` (like tmux's `$TMUX`).

## Options

| Flag | Values | Default |
|---|---|---|
| `-d` | `h`, `v` | `h` |
| `-b` | `single`, `rounded`, `heavy`, `double` | `rounded` |
| `-s` | shell path | `$SHELL` |

## How it works

Each pane owns a PTY pair ([portable-pty](https://crates.io/crates/portable-pty)) running an independent shell. Output is parsed by a per-pane VT100 emulator ([vt100](https://crates.io/crates/vt100)). The layout is a binary split tree where each node is either a leaf (pane) or a split with a direction and ratio. Borders use a `BorderMap` to produce correct junction characters at every intersection.

```
src/
├── main.rs       Event loop, pane lifecycle
├── layout.rs     Binary split tree (split, remove, navigate, equalize)
├── render.rs     BorderMap renderer
├── settings.rs   Settings overlay
└── pane.rs       PTY + VT100 emulation
```

## vs. tmux / Zellij

|  | tmux | Zellij | ezpn |
|---|---|---|---|
| Config | `.tmux.conf` | KDL files | CLI flags |
| Split | `Ctrl+B %` | Mode switch | `Ctrl+D` / click |
| Resize | `:resize-pane` | Resize mode | Drag |
| Select | `Ctrl+B` arrow | Click | Click |
| Detach | Yes | Yes | No |

Use ezpn when you want split terminals without learning anything. Use tmux/Zellij when you need session persistence.

## License

[MIT](LICENSE)
