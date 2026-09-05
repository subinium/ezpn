<p align="center">
  <img src="assets/hero.png" width="720" alt="ezpn demo">
</p>

<h1 align="center">ezpn</h1>

<p align="center">
  <strong>Terminal panes, instantly.</strong><br>
  Mouse-friendly terminal multiplexer for macOS and Linux, with persistent sessions and familiar prefix keys.
</p>

<p align="center">
  <a href="https://crates.io/crates/ezpn"><img src="https://img.shields.io/crates/v/ezpn?style=flat-square&color=orange" alt="crates.io"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="MIT License"></a>
  <a href="https://github.com/subinium/ezpn/actions"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/ci.yml?style=flat-square&label=CI" alt="CI"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey?style=flat-square" alt="Platform">
</p>

<p align="center">
  <b>English</b> | <a href="docs/README.ko.md">한국어</a> | <a href="docs/README.ja.md">日本語</a> | <a href="docs/README.zh.md">中文</a> | <a href="docs/README.es.md">Español</a> | <a href="docs/README.fr.md">Français</a>
</p>

---

## Start Working

```sh
cargo install ezpn --locked
ezpn                 # two shells
ezpn 2 3             # a 2-by-3 grid
ezpn -S work         # create or reattach to a named session
```

Rust 1.88 or newer is required to build. [GitHub Releases](https://github.com/subinium/ezpn/releases)
provide macOS and Linux binaries; use the accompanying checksums when available.
ezpn is an executable terminal multiplexer, not an embeddable Rust GUI library.

## Sessions and SSH

```sh
ezpn a work
ezpn a work --shared
ezpn a work --readonly
ezpn ls
ezpn kill work
```

Press `Ctrl+B`, then `d`, to detach only your client. Shell processes remain alive,
including jobs in inactive tabs. Reattaching reconnects to those processes.
Readonly viewers cannot type into the session or resize writable clients' workspace.

Install ezpn on the remote host and make it available on its PATH:

```sh
ssh -t host 'ezpn -S work'
ssh -t host 'ezpn a work'
ssh -J bastion -t host 'ezpn a work'
```

SSH must allocate a PTY. A disconnected SSH client does not end the remote daemon.
SSH encryption, authentication, host-key verification and forwarding remain OpenSSH's
responsibility. Do not expose ezpn's local Unix sockets over an unauthenticated network.

## Mouse and Keyboard

| Interaction | Result |
| --- | --- |
| Click pane content | Focus the pane |
| Drag a separator | Resize the split |
| Title-bar split buttons | Split the selected pane |
| Title-bar close button | Ask before closing |
| Click a tab | Switch tabs |
| Scroll | Scroll history, or forward to a mouse-aware application |
| Drag text | Select and copy |
| Shift + drag | Select ezpn text instead of sending mouse input to the application |
| Double-click non-mouse application content | Toggle zoom |
| F1 / F2 | Settings / equalize |
| Alt + arrows | Navigate panes; configure Option as Meta on macOS |

Application mouse clicks, motion, wheel and release events use the application's
requested mouse encoding. Keys such as `Ctrl+D`, `Ctrl+E` and `Ctrl+W` go to the
shell unless explicitly rebound. These no longer split panes or request shutdown.

Prefix keys use `Ctrl+B`, followed by:

| Key | Action |
| --- | --- |
| `%` / `"` | Split columns / rows |
| `o` / arrows | Navigate panes |
| `x` | Confirm pane close |
| `z` | Toggle zoom |
| `R` | Resize mode |
| `Space` / `E` | Equalize |
| `c` / `n` / `p` | New / next / previous tab |
| `0`–`9` | Select tab by zero-based index |
| `,` / `&` | Rename / confirm tab close |
| `[` | Copy mode |
| `:` | Command palette |
| `r` | Reload global config |
| `B` | Toggle broadcast input |
| `d` | Detach this client |
| `?` | Help |
| `Ctrl+B` | Send the prefix key to the application |

Copy mode supports vi navigation, `v`/`V` selection, `y` or Enter to copy,
`/`/`?` search, `n`/`N` next/previous match, and `q`/Escape to exit.
Supported common tmux bindings are not complete tmux command compatibility.

## Layouts Without Losing Work

```sh
ezpn -l dev       # 7:3
ezpn -l ide       # 7:3/1:1
ezpn -l quad      # 2-by-2
ezpn -l '7:3/5:5'
ezpn -b none
```

Inside the command palette, `select-layout` rearranges existing processes.
It refuses layouts with a different pane count; split or close panes explicitly.
A failed split or snapshot load does not destroy the current workspace.

## Trusted Project Workspaces

Inspect repository commands before opting into startup:

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

`--trust-project` authorizes automatic `.ezpn.toml` / Procfile execution.
Use an explicit grid, such as `ezpn 1 2`, to start plain shells without loading
repository commands. `doctor` checks syntax without executing commands or resolving secrets.

Project environment interpolation supports environment/file/secret references.
External values are never printed by diagnostics. Panes whose configuration reads
external values are excluded from executable snapshot metadata and history:
restoring those panes opens clean shells. This deliberately favors privacy over
silently saving resolved credentials. See [configuration](docs/configuration.md)
and [security](docs/security.md).

## Configuration and Recovery

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

Themes: `ezpn-dark`, `ezpn-light`, `nord`, `gruvbox-dark`, `solarized-dark`.
User keymaps live in `[keymap.normal]`, `[keymap.prefix]`, and `[keymap.copy_mode]`.
`Ctrl+B r` reloads supported fields from one validated file snapshot.
Settings-panel persistence reports failures instead of claiming a successful save.

Disk snapshots are different from a live detached session. `ezpn --restore FILE`
**starts new processes**. Opt-in persisted history restores text, not a running
editor, process memory, terminal graphics or exact alternate-screen state.
Snapshot files have size/decompression limits and private permissions.

## Compatibility and Evidence

- Supported platform scope: macOS and Linux with a UTF-8 ANSI terminal and Unix PTYs.
  Native Windows is not supported.
- Child keyboard negotiation is distinct from host capabilities. Legacy applications
  receive legacy sequences; supported Kitty enhancements are opt-in.
- Clipboard writes from applications require the configured OSC 52 policy.
  Over SSH, user copies prefer the attached terminal rather than the remote desktop clipboard.
- Rendering is bounded and clips tiny viewports. See [terminal compatibility](docs/terminal-protocol.md)
  for parser limits, supported sequences and untested GUI-emulator combinations.
- `--features render-diff` enables an optional bounded ANSI delta path; unsupported
  frames fall back to the original output. It is not a universal speed guarantee.
- Real PTY tests cover attach/detach, resize, shared/readonly clients and interrupted
  transports. A separate isolated loopback SSH test distinguishes actual SSH from simulation.
- Long-duration soak tests and tmux/Zellij performance comparisons are separate evidence.
  No claim is made that ezpn is always faster or uses less memory than either project.

The [release audit](docs/audits/v0.14.0.md) records results and remaining limitations.
The [preflight script](scripts/preflight.py) records PASS/FAIL/SKIP and real exit codes;
failed tests are not hidden behind ignored placeholders.

## Documentation

[Getting started](docs/getting-started.md) · [Configuration](docs/configuration.md) ·
[SSH and terminal protocols](docs/terminal-protocol.md) · [Clipboard](docs/clipboard.md) ·
[Security](docs/security.md) · [Scripting limits](docs/scripting.md) ·
[Contributing](CONTRIBUTING.md) · [Changelog](CHANGELOG.md)

## License

[MIT](LICENSE)
