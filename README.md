<p align="center">
  <img src="assets/hero.png" width="720" alt="ezpn in action">
</p>

# ezpn

Split your terminal in one command. Click, drag, done.

[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Crate](https://img.shields.io/badge/crates.io-v0.3.0-orange)](https://crates.io/crates/ezpn)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey)]()

**English** | [한국어](docs/README.ko.md) | [日本語](docs/README.ja.md) | [中文](docs/README.zh.md) | [Español](docs/README.es.md) | [Français](docs/README.fr.md)

## Install

```bash
cargo install ezpn
```

Or build from source:

```bash
git clone https://github.com/subinium/ezpn
cd ezpn
cargo install --path .
```

## Usage

```bash
ezpn                # 2 panes side by side (or load .ezpn.toml)
ezpn 4              # 4 horizontal panes
ezpn 3 -d v         # 3 vertical panes
ezpn 2 3            # 2x3 grid

# Layout presets
ezpn -l dev          # 70/30 split
ezpn -l ide          # editor + sidebar + 2 bottom panes
ezpn -l monitor      # 3 equal columns
ezpn -l quad         # 2x2 grid
ezpn -l stack        # 3 stacked rows
ezpn -l main         # wide top pair + full bottom
ezpn -l trio         # full top + 2 bottom

# Custom ratios
ezpn -l '7:3/1:1'
ezpn -l '1:1:1' -e 'cargo watch -x test' -e 'npm run dev' -e 'tail -f app.log'

# Project config
ezpn init            # generate .ezpn.toml template
ezpn from Procfile   # import from Procfile
ezpn                 # auto-loads .ezpn.toml or Procfile

# Restore session
ezpn --restore .ezpn-session.json
```

Commands passed with `-e/--exec` run via `$SHELL -l -c`, so pipes, redirects, and shell syntax work as expected.

## Project Config (.ezpn.toml)

Place `.ezpn.toml` in your project root. Run `ezpn init` to generate a template.

```toml
[workspace]
layout = "ide"    # or "7:3", "1:1:1", "dev", "monitor", etc.

[[pane]]
name = "server"
command = "npm run dev"
cwd = "./frontend"
restart = "on_failure"    # never | on_failure | always

[pane.env]
NODE_ENV = "development"
PORT = "3000"

[[pane]]
name = "tests"
command = "cargo watch -x test"
restart = "always"

[[pane]]
name = "logs"
command = "tail -f /var/log/app.log"
shell = "/bin/bash"
```

Supported fields per pane:
- `command` — shell command to run (default: interactive shell)
- `cwd` — working directory (relative to .ezpn.toml)
- `name` — custom title bar label
- `env` — environment variables table
- `restart` — `never` (default), `on_failure`, or `always`
- `shell` — per-pane shell override

Also auto-detects `Procfile` when no `.ezpn.toml` exists.

## Controls

**Mouse** — the primary way to interact:

| | |
|---|---|
| Click pane | Focus |
| Double-click pane | Zoom toggle |
| Click `[x]` | Close pane |
| Drag border | Resize |
| Scroll wheel | Scroll through output (scrollback) |

**Keyboard (direct shortcuts):**

| | |
|---|---|
| `Ctrl+D` | Split left \| right |
| `Ctrl+E` | Split top / bottom |
| `F2` | Equalize all sizes |
| `Ctrl+N` | Next pane |
| `Ctrl+G` | Settings (j/k/Enter to navigate) |
| `Ctrl+W` | Quit |

**tmux-compatible prefix keys (`Ctrl+B` then...):**

| | |
|---|---|
| `%` | Split left \| right |
| `"` | Split top / bottom |
| `o` | Next pane |
| `Arrow` | Navigate directionally |
| `x` | Close pane |
| `z` | Zoom / unzoom pane (full screen) |
| `B` | Broadcast mode (type in all panes) |
| `R` | Resize mode (arrow/hjkl to resize, q to exit) |
| `q` | Show pane numbers, press 1-9 to jump |
| `{` `}` | Swap pane with prev / next |
| `E` | Equalize |
| `[` | Scroll mode (j/k/g/G/PgUp/PgDn, q to exit) |
| `s` | Toggle status bar |
| `?` | Help overlay |
| `d` | Quit (with confirmation if panes are live) |

<details>
<summary>macOS: Alt+Arrow for directional navigation</summary>

`Alt+Arrow` navigates between panes directionally. This requires your terminal to send Option as Meta:

- **iTerm2**: Preferences → Profiles → Keys → Left Option Key → `Esc+`
- **Terminal.app**: Settings → Profiles → Keyboard → Use Option as Meta Key
</details>

## Features

**Layout presets** — Named presets for common workflows:

```bash
ezpn -l dev       # 7:3 split — main + side
ezpn -l ide       # 7:3/1:1 — editor + sidebar + 2 bottom
ezpn -l monitor   # 1:1:1 — 3 equal columns
ezpn -l quad      # 2x2 grid
```

Presets also work in `.ezpn.toml`: `layout = "ide"`.

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

**Broadcast mode** — `Ctrl+B B` sends your keystrokes to all panes simultaneously. Status bar shows `BROADCAST`. Press `Ctrl+B B` again to stop.

**Mouse wheel scrollback** — Scroll through terminal output with the mouse wheel. Shows `[SCROLL]` indicator in the title bar. New output snaps back to live view.

**Auto-restart** — Panes with `restart = "on_failure"` or `restart = "always"` in `.ezpn.toml` automatically respawn when they exit. Includes backoff to prevent restart loops.

**Title bar** — Each pane shows its number, custom name, and running command. `[━]` `[┃]` `[×]` buttons for split/close.

**Zoom mode** — `Ctrl+B z` or double-click expands any pane to full terminal. Press again to restore.

**Keyboard resize** — `Ctrl+B R` enters resize mode. Use arrow keys or `h`/`j`/`k`/`l` to grow/shrink.

**Pane swap** — `Ctrl+B {` and `Ctrl+B }` swap the active pane with the previous/next.

**Quick jump** — `Ctrl+B q` overlays pane numbers. Press `1`-`9` to jump.

**Scroll mode** — `Ctrl+B [` enters scroll mode. Navigate with `j`/`k`, `g`/`G`, `PgUp`/`PgDn`, `Ctrl+U`/`Ctrl+D`.

**Settings panel** — `Ctrl+G` opens a dark modal. Navigate with `j`/`k`, apply with `Enter`, quick-select borders with `1`-`4`.

**Dead pane recovery** — When a process exits, the pane dims and shows "Process exited". Press `Enter` to respawn, or `×` to close.

**Config file** — Reads `~/.config/ezpn/config.toml` for defaults (border style, status bar, shell, scrollback). CLI flags override.

**Border styles** — `--border` flag or change in settings:

```
single           rounded (default) heavy            double
┌──────┬──────┐  ╭──────┬──────╮  ┏━━━━━━┳━━━━━━┓  ╔══════╦══════╗
│      │      │  │      │      │  ┃      ┃      ┃  ║      ║      ║
└──────┴──────┘  ╰──────┴──────╯  ┗━━━━━━┻━━━━━━┛  ╚══════╩══════╝
```

**Procfile import** — `ezpn from Procfile` generates `.ezpn.toml` from a Procfile. Also auto-detects Procfile when no config exists.

**IPC + automation** — Control a live instance from another terminal:

```bash
ezpn-ctl list
ezpn-ctl split horizontal
ezpn-ctl exec 1 'cargo test'
ezpn-ctl save .ezpn-session.json
ezpn-ctl load .ezpn-session.json
```

**Workspace snapshots** — Save layout ratios, active pane, commands, shell path, and UI settings. Restore them later with `ezpn --restore`.

**Nesting prevention** — Running `ezpn` inside an ezpn pane is blocked via `$EZPN`.

## Options

| Flag | Values | Default |
|---|---|---|
| `-l` | layout spec or preset (`ide`, `dev`, `7:3/1:1`) | — |
| `-e` | shell command (repeatable) | interactive `$SHELL` |
| `-r` | snapshot file path | — |
| `-d` | `h`, `v` | `h` |
| `-b` | `single`, `rounded`, `heavy`, `double` | `rounded` |
| `-s` | shell path | `$SHELL` |
| `-V` | show version | — |

## Layout Presets

| Name | Layout | Panes | Use Case |
|------|--------|-------|----------|
| `dev` | 7:3 | 2 | Main editor + side terminal |
| `ide` | 7:3/1:1 | 4 | Editor + sidebar + 2 bottom |
| `monitor` | 1:1:1 | 3 | Dashboards, logs, monitoring |
| `quad` | 2x2 | 4 | Equal grid |
| `stack` | 1/1/1 | 3 | Stacked rows |
| `main` | 6:4/1 | 3 | Wide top pair + full bottom |
| `trio` | 1/1:1 | 3 | Full top + 2 bottom |

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

Each pane owns a PTY pair ([portable-pty](https://crates.io/crates/portable-pty)) running either an interactive shell or a shell command. Output is parsed by a per-pane VT100 emulator ([vt100](https://crates.io/crates/vt100)) with configurable scrollback. The layout is a binary split tree where each node is either a leaf (pane) or a split with a direction and ratio. Rendering caches border geometry and redraws only dirty panes unless the layout chrome changes.

```
src/
├── main.rs          Event loop, prefix key state machine, pane lifecycle
├── layout.rs        Binary split tree + named presets
├── pane.rs          PTY + VT100 emulation + scrollback + mouse forwarding
├── render.rs        Dirty render path + border cache + title bar buttons
├── settings.rs      Dark modal with vim navigation
├── project.rs       .ezpn.toml parsing + Procfile import
├── ipc.rs           JSON IPC protocol + Unix socket listener
├── workspace.rs     Snapshot save/load and validation
├── config.rs        Config file loading (~/.config/ezpn/config.toml)
├── tab.rs           Tab manager (prepared)
└── bin/ezpn-ctl.rs  External control client
```

## vs. tmux / Zellij

|  | tmux | Zellij | ezpn |
|---|---|---|---|
| Config | `.tmux.conf` | KDL files | `.ezpn.toml` / CLI flags |
| Split | `Ctrl+B %` | Mode switch | `Ctrl+D` / click |
| Resize | `:resize-pane` | Resize mode | Drag / `Ctrl+B R` |
| Select | `Ctrl+B` arrow | Click | Click |
| Project setup | tmuxinator | — | `.ezpn.toml` / Procfile |
| Broadcast | `:setw synchronize-panes` | — | `Ctrl+B B` |
| Auto-restart | — | — | `restart = "always"` |
| Scrollback | `Ctrl+B [` | Scroll mode | Mouse wheel / `Ctrl+B [` |
| Detach | Yes | Yes | No |

Use ezpn when you want split terminals with zero config. Use tmux/Zellij when you need detach/reattach session persistence.

## License

[MIT](LICENSE)
