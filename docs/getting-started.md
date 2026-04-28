# Getting started

A 5-minute tour. By the end you'll have ezpn installed, a session
running, a project workspace defined, and you'll know where to look
when something doesn't work.

## 1. Install

```sh
cargo install ezpn
```

That installs both `ezpn` (the multiplexer) and `ezpn-ctl` (the control
client). No daemon is running yet; the daemon starts on demand the
first time you run `ezpn`.

Build from source:

```sh
git clone https://github.com/subinium/ezpn
cd ezpn && cargo install --path .
```

Requires Rust 1.74 or newer. macOS and Linux only; Windows is not
supported.

## 2. First session — 30 seconds

```sh
ezpn
```

You get two side-by-side shells. `Ctrl+B d` detaches; the panes keep
running in the background. `ezpn a` reattaches.

```sh
ezpn 2 3        # 2x3 grid
ezpn -l ide     # the "ide" preset: editor + sidebar + 2 bottom
ezpn -l dev     # 7:3 — main + side
```

The full preset list: `dev`, `ide`, `monitor`, `quad`, `stack`, `main`,
`trio`. Custom ratios: `ezpn -l '7:3/5:5'`.

Per-pane commands at startup:

```sh
ezpn -e 'cargo watch -x test' -e 'tail -f log/dev.log'
```

## 3. Project workspace — `.ezpn.toml`

Drop this in your project root and run `ezpn` from that directory:

```toml
# .ezpn.toml
[workspace]
layout = "7:3/1:1"

[[pane]]
name    = "editor"
command = "nvim ."

[[pane]]
name    = "server"
command = "npm run dev"
restart = "on_failure"

[[pane]]
name    = "tests"
command = "npm test -- --watch"

[[pane]]
name    = "logs"
command = "tail -f logs/app.log"
```

Run `ezpn` — no flags. The file is loaded automatically. Everyone on
your team gets the same workspace. No `tmuxinator`, no global config to
sync.

Generate a starter:

```sh
ezpn init                # write a template .ezpn.toml
ezpn from Procfile       # convert a Heroku-style Procfile
```

Full schema: [`docs/configuration.md`](./configuration.md#2-project-config--ezpntoml).

## 4. Keybindings — the 10 you need

Default prefix is `Ctrl+B`. After pressing the prefix, the next key
runs an action.

| Key             | Action                              |
|-----------------|-------------------------------------|
| `C-b "`         | Split horizontally                  |
| `C-b %`         | Split vertically                    |
| `C-b ↑↓←→`      | Move focus                           |
| `C-b x`         | Close pane                          |
| `C-b z`         | Zoom toggle                          |
| `C-b c`         | New tab                              |
| `C-b n` / `p`   | Next / previous tab                 |
| `C-b 0`–`9`     | Jump to tab N                        |
| `C-b [`         | Copy mode (vi keys, `y` to yank)    |
| `C-b d`         | Detach                               |

Three direct shortcuts (no prefix):

| Key          | Action            |
|--------------|-------------------|
| `M-↑↓←→`     | Move focus        |
| `Ctrl+D`     | Split horizontal  |
| `Ctrl+E`     | Split vertical    |

Full list: [`README`](../README.md#keybindings). Customise:
[`docs/configuration.md`](./configuration.md#15-keymaptable-frozen-vocabulary--issue-84).

## 5. Sessions

```sh
ezpn -S work          # start a session named "work"
ezpn ls               # list running sessions
ezpn a work           # reattach by name
ezpn kill work        # tear down
ezpn rename old new   # rename
```

Multiple clients can attach to the same session: `ezpn a --shared`.
Read-only attach for screen-sharing: `ezpn a --readonly`. See
[`README.md`](../README.md) §Sessions for the full flag list.

## 6. Copy mode

`C-b [` enters copy mode. vi-style:

```
h j k l        — move
w / b          — word forward / back
0 / $ / ^      — line start / end / first non-blank
g / G          — top / bottom
v / V          — character / line selection
y / Enter      — yank (copies via OSC 52, see clipboard.md)
/ / ?          — search forward / backward
n / N          — next / previous match
q / Esc        — exit
```

The yank emits an OSC 52 envelope through your terminal stack to the
system clipboard. If yanking doesn't work, walk through
[clipboard.md §4.1](./clipboard.md#41-diagnosing-a-broken-chain).

## 7. Configuration

`~/.config/ezpn/config.toml`:

```toml
[global]
border = "rounded"          # single | rounded | heavy | double | none
shell  = "/bin/zsh"
scrollback = 50000
status_bar = true
tab_bar    = true

[keys]
prefix = "b"

[clipboard]
osc52_set = "confirm"       # allow | confirm | deny
osc52_get = "deny"
```

Reload without restarting: `Ctrl+B r` or `kill -HUP <pid>`.

Full reference: [`docs/configuration.md`](./configuration.md).

## 8. Scripting

Drive ezpn from external tools via `ezpn-ctl`:

```sh
ezpn-ctl list                                      # current panes
ezpn-ctl --json list                               # machine-readable
ezpn-ctl split horizontal                          # split active pane
ezpn-ctl exec 0 'cargo test'                       # run in pane 0
ezpn-ctl save .ezpn-snapshot.json                  # snapshot
ezpn-ctl events --filter pane.exited               # subscribe to events
```

Patterns and the full event vocabulary: [`docs/scripting.md`](./scripting.md).

## 9. When something doesn't work

| Symptom                                       | Where to look |
|-----------------------------------------------|---------------|
| Key doesn't do what you expect                | [migration-from-tmux.md](./migration-from-tmux.md#1-key-for-key-cheat-sheet); `ezpn-ctl --json config show` |
| `.ezpn.toml` ignored                          | Run from the directory containing it; check stderr for parse errors |
| Copy mode doesn't yank                        | [clipboard.md](./clipboard.md#41-diagnosing-a-broken-chain) |
| Hyperlinks don't work                         | Known v0.12 limitation: [terminal-protocol.md §7](./terminal-protocol.md#7-osc-8--hyperlinks-76) |
| Daemon won't start                            | Check the socket: `ls -l ${XDG_RUNTIME_DIR:-/tmp}/ezpn-*.sock`; permissions must be `0600` and owned by you |
| Send-keys script races                        | Use `ezpn-ctl send-keys --await-prompt` (#81) — needs OSC 133 D from your shell ([shell-integration.md](./shell-integration.md)) |

Trace logging:

```sh
EZPN_LOG=trace ezpn 2>ezpn.log
```

## 10. Where to next

* Coming from tmux? Read [migration-from-tmux.md](./migration-from-tmux.md).
* Want to script ezpn? [scripting.md](./scripting.md).
* Need to debug terminal escape sequences? [terminal-protocol.md](./terminal-protocol.md).
* Worried about clipboard / OSC 52 security? [security.md](./security.md) + [clipboard.md](./clipboard.md).
* Need the wire-protocol spec? [protocol/v1.md](./protocol/v1.md).
