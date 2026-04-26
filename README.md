<p align="center">
  <img src="assets/hero.png" width="720" alt="ezpn demo">
</p>

<h1 align="center">ezpn</h1>

<p align="center">
  <strong>Terminal panes, instantly.</strong><br>
  Zero-config terminal multiplexer with session persistence and tmux-compatible keys.
</p>

<p align="center">
  <a href="https://crates.io/crates/ezpn"><img src="https://img.shields.io/crates/v/ezpn?style=flat-square&color=orange" alt="crates.io"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="MIT License"></a>
  <a href="https://github.com/subinium/ezpn/actions"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/ci.yml?style=flat-square&label=CI" alt="CI"></a>
  <a href="https://github.com/subinium/ezpn/actions/workflows/gitleaks.yml"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/gitleaks.yml?style=flat-square&label=gitleaks" alt="gitleaks"></a>
  <a href="https://github.com/subinium/ezpn/actions/workflows/supply-chain.yml"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/supply-chain.yml?style=flat-square&label=audit" alt="audit"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey?style=flat-square" alt="Platform">
</p>

<p align="center">
  <b>English</b> | <a href="docs/README.ko.md">한국어</a> | <a href="docs/README.ja.md">日本語</a> | <a href="docs/README.zh.md">中文</a> | <a href="docs/README.es.md">Español</a> | <a href="docs/README.fr.md">Français</a>
</p>

---

## Why ezpn?

```bash
$ ezpn                # split your terminal, instantly
$ ezpn 2 3            # 2x3 grid of shells
$ ezpn -l dev         # preset layout
```

No config files, no setup, no learning curve. Sessions persist in the background — `Ctrl+B d` to detach, `ezpn a` to come back.

**In a project**, drop `.ezpn.toml` in your repo and run `ezpn` — everyone gets the same workspace:

```toml
[session]
name = "myproject"           # pin the session name (collisions become myproject-1, -2...)

[workspace]
layout = "7:3/1:1"
persist_scrollback = true    # scrollback survives detach/reattach

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
$ ezpn         # reads .ezpn.toml, starts everything
$ ezpn doctor  # validates env interpolation + secret refs before you run
```

No tmuxinator. No YAML. Just a TOML file in your repo.

## Install

```bash
cargo install ezpn
```

Or grab a prebuilt binary from the [latest release](https://github.com/subinium/ezpn/releases/latest) — `ezpn-x86_64-unknown-linux-gnu.tar.gz`, `ezpn-x86_64-apple-darwin.tar.gz`, or `ezpn-aarch64-apple-darwin.tar.gz`.

<details>
<summary>Build from source</summary>

```bash
git clone https://github.com/subinium/ezpn
cd ezpn && cargo install --path .
```

</details>

## Quick Start

```bash
ezpn                  # 2 panes (or load .ezpn.toml if present)
ezpn 2 3              # 2x3 grid
ezpn -l dev           # Layout preset (dev, monitor, quad, stack, trio...)
ezpn -e 'cmd1' -e 'cmd2'   # Per-pane commands
```

### Sessions

```bash
Ctrl+B d               # Detach (session keeps running)
ezpn a                 # Reattach to most recent session
ezpn a myproject       # Reattach by name
ezpn ls                # List active sessions
ezpn kill myproject    # Kill a session
ezpn --new             # Force a new session even if one already exists for $PWD
```

Session names default to `basename($PWD)`. Collisions are resolved deterministically — `repo` → `repo-1` → `repo-2` (dead sockets are reaped during the scan). Pin a name in `.ezpn.toml` via `[session].name = "..."`.

### Tabs

```bash
Ctrl+B c               # New tab
Ctrl+B n / p           # Next / previous tab
Ctrl+B 0-9             # Jump to tab
```

All tmux keys work — `Ctrl+B %` to split, `Ctrl+B x` to close, `Ctrl+B [` for copy mode.

## Features

|                          |                                                                                            |
| ------------------------ | ------------------------------------------------------------------------------------------ |
| **Zero config**          | Works out of the box. No rc files needed.                                                  |
| **Layout presets**       | `dev`, `ide`, `monitor`, `quad`, `stack`, `main`, `trio`                                   |
| **Session persistence**  | Detach/attach like tmux. Background daemon keeps processes alive. Sub-50 ms cold attach.   |
| **Scrollback persist**   | Opt-in `persist_scrollback` survives detach/reattach (gzip+bincode in v3 snapshots).       |
| **Tabs**                 | tmux-style windows with tab bar and mouse click switching.                                 |
| **Mouse-first**          | Click to focus, drag to resize, scroll for history, drag to select & copy.                 |
| **Copy mode**            | Vi keys, visual selection, incremental display-width search, OSC 52 clipboard.             |
| **Command palette**      | `Ctrl+B :` with tmux-compatible commands.                                                  |
| **Broadcast mode**       | Type in all panes simultaneously.                                                          |
| **Project config**       | `.ezpn.toml` per project — layout, commands, env vars, auto-restart.                       |
| **Env interpolation**    | `${HOME}`, `${env:VAR}`, `${file:.env.local}`, `${secret:keychain:KEY}` in pane env.       |
| **Themes**               | TOML palette + 4 builtins (`tokyo-night`, `gruvbox-dark`, `solarized-dark`/`-light`).      |
| **Hot reload**           | `Ctrl+B r` reloads `~/.config/ezpn/config.toml` without detaching.                         |
| **Borderless mode**      | `ezpn -b none` for maximum screen space.                                                   |
| **Kitty keyboard**       | `Shift+Enter`, `Ctrl+Arrow`, Alt+Char (CSI u / RFC 3665) — modified keys work correctly.   |
| **CJK/Unicode**          | Proper width calculation for Korean, Chinese, Japanese, and emoji.                         |
| **Crash isolation**      | One panicking pane can't take down the daemon (signal-safe SIGTERM/SIGCHLD handling).      |
| **Scriptable input**     | `ezpn-ctl send-keys --pane N -- 'cmd' Enter` for editors, AI agents, CI scripts.           |
| **Event stream**         | Long-lived `S_EVENT` subscriptions over the binary protocol (`-CC`-style integrations).    |
| **Hooks**                | Declarative `[[hooks]]` config: shell out on daemon events with worker pool + per-hook timeouts. |
| **Regex search**         | `[copy_mode] search = "regex"` opts copy-mode search into POSIX patterns with smart-case.  |
| **Per-pane history**     | `ezpn-ctl clear-history --pane N` / `set-scrollback --pane N --lines L` for runtime control. |

## Layout Presets

```bash
ezpn -l dev       # 7:3 — main + side
ezpn -l ide       # 7:3/1:1 — editor + sidebar + 2 bottom
ezpn -l monitor   # 1:1:1 — 3 equal columns
ezpn -l quad      # 2x2 grid
ezpn -l stack     # 1/1/1 — 3 stacked rows
ezpn -l main      # 6:4/1 — wide top pair + full bottom
ezpn -l trio      # 1/1:1 — full top + 2 bottom
```

Custom ratios: `ezpn -l '7:3/5:5'`

## Project Config

Drop `.ezpn.toml` in your project root and run `ezpn`. That's it.

**Per-pane options:** `command`, `cwd`, `name`, `env`, `restart` (`never`/`on_failure`/`always`), `shell`

```bash
ezpn init              # Generate .ezpn.toml template
ezpn from Procfile     # Import from Procfile
ezpn doctor            # Validate config + env interpolation, exit non-zero on missing refs
```

### Hooks

Run a shell command on daemon events. Bounded worker pool (4 threads) with per-hook `timeout_ms`; processes are spawned in their own group so SIGTERM → SIGKILL escalation cleans up the whole tree.

```toml
# ~/.config/ezpn/config.toml or .ezpn.toml

[[hooks]]
event = "client-attached"
command = "notify-send 'pane {client_id} attached'"
shell = true
timeout_ms = 2000

[[hooks]]
event = "tab-created"
command = ["/usr/local/bin/ezpn-tab-init", "{name}", "{tab_index}"]
```

v0.11 wires `client-attached`, `client-detached`, `tab-created`, `tab-closed`, `session-renamed`. Variable expansion (`{session}`, `{client_id}`, `{pane_id}`, …) substitutes per-event values into the `command` string before exec.

### Env interpolation

Pane env values support four reference forms:

```toml
[[pane]]
command = "npm run dev"
env = {
  HOME       = "${HOME}",                    # process env
  NODE_ENV   = "${env:NODE_ENV}",            # explicit env
  DB_URL     = "${file:.env.local}",         # dotenv-style file lookup
  GH_TOKEN   = "${secret:keychain:GH_TOKEN}",# macOS Keychain (Linux: secret-tool)
}
```

`.env.local` next to `.ezpn.toml` is auto-merged and overrides `[env]`. `${secret:keychain:KEY}` falls back to `${env:KEY}` with a warning when the OS keychain isn't available. Recursion is depth-capped at 8 to catch cycles.

### Themes

```toml
# .ezpn.toml or ~/.config/ezpn/config.toml
theme = "tokyo-night"   # default | tokyo-night | gruvbox-dark | solarized-dark | solarized-light
```

User themes load from `~/.config/ezpn/themes/<name>.toml`. ezpn auto-detects `$COLORTERM` / `$TERM` and downgrades to 256 or 16 colors when truecolor isn't supported.

<details>
<summary>Global config (~/.config/ezpn/config.toml)</summary>

```toml
border = rounded            # single | rounded | heavy | double | none
shell = /bin/zsh
scrollback = 10000
status_bar = true
tab_bar = true
prefix = b                  # prefix key (Ctrl+<key>)
theme = default             # default | tokyo-night | gruvbox-dark | solarized-dark | solarized-light
persist_scrollback = false  # save scrollback into auto-snapshots (off by default)
```

Settings panel changes (`Ctrl+B Shift+,`) are persisted atomically. Reload from disk with `Ctrl+B r`.

</details>

## Keybindings

**Direct shortcuts:**

| Key      | Action           |
| -------- | ---------------- |
| `Ctrl+D` | Split horizontal |
| `Ctrl+E` | Split vertical   |
| `Ctrl+N` | Next pane        |
| `F2`     | Equalize sizes   |

**Prefix mode** (`Ctrl+B`, then):

| Key         | Action          |
| ----------- | --------------- |
| `%` / `"`   | Split H / V     |
| `o` / Arrow | Navigate panes  |
| `x`         | Close pane      |
| `z`         | Zoom toggle     |
| `R`         | Resize mode     |
| `[`         | Copy mode       |
| `B`         | Broadcast       |
| `:`         | Command palette |
| `r`         | Reload config   |
| `d`         | Detach session  |
| `?`         | Help            |

<details>
<summary>Full keybinding reference</summary>

**Tabs:**

| Key              | Action              |
| ---------------- | ------------------- |
| `Ctrl+B c`       | New tab             |
| `Ctrl+B n` / `p` | Next / previous tab |
| `Ctrl+B 0-9`     | Jump to tab         |
| `Ctrl+B ,`       | Rename tab          |
| `Ctrl+B &`       | Close tab           |

**Panes:**

| Key                  | Action                    |
| -------------------- | ------------------------- |
| `Ctrl+B {` / `}`     | Swap pane prev / next     |
| `Ctrl+B E` / `Space` | Equalize                  |
| `Ctrl+B s`           | Toggle status bar         |
| `Ctrl+B q`           | Pane numbers + quick jump |

**Copy mode** (`Ctrl+B [`):

| Key                 | Action                             |
| ------------------- | ---------------------------------- |
| `h` `j` `k` `l`     | Move cursor                        |
| `w` / `b`           | Next / previous word               |
| `0` / `$` / `^`     | Line start / end / first non-blank |
| `g` / `G`           | Top / bottom of scrollback         |
| `Ctrl+U` / `Ctrl+D` | Half page up / down                |
| `v`                 | Character selection                |
| `V`                 | Line selection                     |
| `y` / `Enter`       | Copy and exit                      |
| `/` / `?`           | Search forward / backward          |
| `n` / `N`           | Next / previous match              |
| `q` / `Esc`         | Exit                               |

**Mouse:**

| Action       | Effect             |
| ------------ | ------------------ |
| Click pane   | Focus              |
| Double-click | Zoom toggle        |
| Click tab    | Switch tab         |
| Click `[x]`  | Close pane         |
| Drag border  | Resize             |
| Drag text    | Select + copy      |
| Scroll wheel | Scrollback history |

**macOS note:** Alt+Arrow for directional navigation requires Option as Meta (iTerm2: Preferences > Profiles > Keys > `Esc+`).

</details>

<details>
<summary>Command palette commands</summary>

`Ctrl+B :` opens the command prompt. All tmux aliases are supported.

```
split / split-window         Split horizontally
split -v                     Split vertically
new-tab / new-window         Create new tab
next-tab / prev-tab          Switch tabs
close-pane / kill-pane       Close active pane
close-tab / kill-window      Close current tab
rename-tab <name>            Rename tab
layout <spec>                Change layout
equalize / even              Equalize pane sizes
zoom                         Toggle zoom
broadcast                    Toggle broadcast mode
```

</details>

## ezpn vs. tmux vs. Zellij

|                | tmux                      | Zellij        | **ezpn**                  |
| -------------- | ------------------------- | ------------- | ------------------------- |
| Setup          | `.tmux.conf` required     | KDL config    | **Zero config**           |
| First use      | Empty screen              | Tutorial mode | **`ezpn`**                |
| Sessions       | `tmux a`                  | `zellij a`    | **`ezpn a`**              |
| Project config | tmuxinator (gem)          | —             | **`.ezpn.toml` built-in** |
| Broadcast      | `:setw synchronize-panes` | —             | **`Ctrl+B B`**            |
| Auto-restart   | —                         | —             | **`restart = "always"`**  |
| Kitty keyboard | No                        | Yes           | **Yes**                   |
| Plugin system  | —                         | WASM          | —                         |
| Ecosystem      | Massive (30 years)        | Growing       | New                       |

**Choose ezpn** if you want terminal splits that just work — plus an `ezpn-ctl send-keys` / event-stream / hooks surface for scripting.
**Choose tmux** if you need a deep plugin ecosystem (TPM, etc.).
**Choose Zellij** if you want WASM plugins.

## CLI Reference

```
ezpn [ROWS COLS]         Start with grid layout
ezpn -l <PRESET>         Start with layout preset
ezpn -e <CMD> [-e ...]   Per-pane commands
ezpn -S <NAME>           Named session
ezpn -b <STYLE>          Border style (single/rounded/heavy/double/none)
ezpn --new               Force a new session (skip auto-attach to existing)
ezpn a [NAME]            Attach to session
ezpn ls                  List sessions
ezpn kill [NAME]         Kill session
ezpn rename OLD NEW      Rename session
ezpn init                Generate .ezpn.toml template
ezpn from <FILE>         Import from Procfile
ezpn doctor              Validate .ezpn.toml + env interpolation
```

### `ezpn-ctl` (scripting)

```
ezpn-ctl list                                List panes
ezpn-ctl split [horizontal|vertical] [PANE]  Split a pane
ezpn-ctl close PANE                          Close a pane
ezpn-ctl focus PANE                          Focus a pane
ezpn-ctl save <PATH>                         Save workspace snapshot
ezpn-ctl load <PATH>                         Restore workspace
ezpn-ctl exec PANE <CMD>                     Replace a pane with a new command

ezpn-ctl send-keys [--pane N | --target current] [--literal] -- <key>...
                                             Deliver chord-tokens or raw bytes to a pane PTY.
                                             Examples:
                                               ezpn-ctl send-keys --pane 0 -- 'echo hi' Enter
                                               ezpn-ctl send-keys --target current -- C-c
                                               ezpn-ctl send-keys --pane 0 --literal -- $'#!/bin/sh\nexit 0\n'

ezpn-ctl clear-history --pane N              Drop scrollback above the visible screen
ezpn-ctl set-scrollback --pane N --lines L   Resize the scrollback ring (capped at scrollback_max_lines)
```

## License

[MIT](LICENSE)
