<p align="center">
  <img src="assets/hero.gif" width="680" alt="ezpn in action">
</p>

# ezpn

A terminal pane splitter. Click to select. Drag to resize. No config needed.

[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Crate](https://img.shields.io/badge/crates.io-v0.1.0-orange)](https://crates.io/crates/ezpn)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey)]()

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
ezpn --layout '7:3/1:1'
ezpn --layout '1:1:1' -e 'cargo watch -x test' -e 'npm run dev' -e 'tail -f app.log'
ezpn --restore .ezpn-session.json
```

Then split any pane further with `Ctrl+D` (horizontal) or `Ctrl+E` (vertical).
Commands passed with `-e/--exec` run via `$SHELL -l -c`, so pipes, redirects, and shell syntax work as expected.

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

**Flexible layouts** — Start with a grid, use ratio layouts with `--layout`, split individual panes, and drag to resize. Auto-equalizes on split. Press `F2` to reset sizes.

```
╭────────┬────╮       ╭────────┬────╮
│        │ 2  │       │        │ 2  │
│   1    ├────┤  ──>  │   1    ├──┬─┤
│        │ 3  │       │        │3 │4│
╰────────┴────╯       ╰────────┴──┴─╯
```

**Per-pane commands** — Launch each pane with a different command:

```bash
ezpn --layout '1/1:1' -e 'htop' -e 'npm run dev' -e 'tail -f app.log'
```

**Settings panel** — `Ctrl+G` opens a larger borderless modal. Change border style, split panes, and toggle the status bar with either the mouse or keyboard.

**Border styles** — `--border` flag or change in settings:

```
single           rounded (default) heavy            double
┌──────┬──────┐  ╭──────┬──────╮  ┏━━━━━━┳━━━━━━┓  ╔══════╦══════╗
│      │      │  │      │      │  ┃      ┃      ┃  ║      ║      ║
└──────┴──────┘  ╰──────┴──────╯  ┗━━━━━━┻━━━━━━┛  ╚══════╩══════╝
```

**Dead pane recovery** — When a shell exits, the pane dims and shows `[exited]`. Press `Enter` to respawn, or `×` to close.

**IPC + automation** — Control a live instance from another terminal:

```bash
ezpn-ctl list
ezpn-ctl split horizontal
ezpn-ctl exec 1 'cargo test'
ezpn-ctl save .ezpn-session.json
ezpn-ctl load .ezpn-session.json
```

**Workspace snapshots** — Save layout ratios, active pane, commands, shell path, and UI settings. Restore them later with `ezpn --restore`.

**Nesting prevention** — Running `ezpn` inside an ezpn pane is blocked via `$EZPN` (like tmux's `$TMUX`).

## Options

| Flag | Values | Default |
|---|---|---|
| `-l` | layout spec (`7:3/1:1`) | — |
| `-e` | shell command (repeatable) | interactive `$SHELL` |
| `-r` | snapshot file path | — |
| `-d` | `h`, `v` | `h` |
| `-b` | `single`, `rounded`, `heavy`, `double` | `rounded` |
| `-s` | shell path | `$SHELL` |

## ezpn-ctl

`ezpn-ctl` talks to a running ezpn instance over a Unix socket using JSON messages.

```bash
ezpn-ctl list
ezpn-ctl --pid 12345 focus 2
ezpn-ctl --json list
```

Commands:

- `split horizontal [pane]`
- `split vertical [pane]`
- `close <pane>`
- `focus <pane>`
- `equalize`
- `layout <spec>`
- `exec <pane> <command>`
- `save <path>`
- `load <path>`

## How it works

Each pane owns a PTY pair ([portable-pty](https://crates.io/crates/portable-pty)) running either an interactive shell or a shell command. Output is parsed by a per-pane VT100 emulator ([vt100](https://crates.io/crates/vt100)). The layout is a binary split tree where each node is either a leaf (pane) or a split with a direction and ratio. Rendering caches border geometry and redraws only dirty panes unless the layout chrome changes.

```
src/
├── main.rs          Event loop, pane lifecycle, restore flow
├── layout.rs        Binary split tree (split, remove, navigate, equalize)
├── pane.rs          PTY + VT100 emulation + launch metadata
├── render.rs        Dirty render path + border cache
├── settings.rs      Large borderless settings modal
├── ipc.rs           JSON IPC protocol + Unix socket listener
├── workspace.rs     Snapshot save/load and validation
└── bin/ezpn-ctl.rs  External control client
```

## vs. tmux / Zellij

|  | tmux | Zellij | ezpn |
|---|---|---|---|
| Config | `.tmux.conf` | KDL files | CLI flags |
| Split | `Ctrl+B %` | Mode switch | `Ctrl+D` / click |
| Resize | `:resize-pane` | Resize mode | Drag |
| Select | `Ctrl+B` arrow | Click | Click |
| Detach | Yes | Yes | No |

Use ezpn when you want split terminals without learning anything. Use tmux/Zellij when you need full detach/reattach session persistence.

## License

[MIT](LICENSE)
