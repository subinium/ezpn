# Changelog

All notable changes to **ezpn** are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
ezpn adheres to [Semantic Versioning](https://semver.org/) (`0.MINOR.PATCH` until 1.0).

Entries are written in **functional-only style**: every bullet describes an observable change. No narrative, no rationale — *why* lives in commit bodies.

## [Unreleased]

## [0.6.0] — 2026-04-26 — Stability & Safety Net

### Added
- **Wire-protocol versioning + handshake** (`C_HELLO` / `S_HELLO_OK` / `S_HELLO_ERR`). Mismatched majors are rejected with a clear "please upgrade" message instead of silent corruption. Backwards compatible — older clients without `C_HELLO` keep working.
- **POSIX signal handling**: `SIGTERM` / `SIGHUP` snapshot the workspace before clean exit; `SIGCHLD` reaps zombie panes.
- **Per-pane OSC 52 caps** (16 entries / 256 KiB total / 128 KiB single-sequence) — runaway children can no longer exhaust memory via clipboard spam.
- **Daemon integration test harness**: spawns real `ezpn --server`, asserts handshake + ping + signal lifecycle. Wire-protocol regressions now caught in CI.
- Repository conventions: `CONTRIBUTING.md`, `MAINTENANCE.md`, GitHub issue/PR templates, label taxonomy, `CODEOWNERS`.

### Fixed
- **Worker thread panic isolation**: PTY reader, client reader, IPC accept loop and per-client handler are now wrapped with `catch_unwind`. A bad ANSI sequence or malformed message no longer kills the daemon — only the affected pane / client is dropped.
- Pane spawn thread panics are surfaced gracefully: partial workspace continues instead of aborting the whole session.
- Clippy `collapsible_match` warnings (10) that broke CI on Rust 1.95.

### Changed
- `.gitignore` hardened: secrets, AI-session files, profiling artifacts.
- CI splits unit and integration test runs for clearer failure rows.

## [0.5.0] — 2026-04-26

### Added
- Multi-client attach with Steal / Shared / Readonly modes.
- Full-session snapshots: layout, tabs, panes, commands, env, restart policies.
- Settings panel overhaul.

## [0.4.2] — 2026-04

### Fixed
- Session naming uses timestamp suffix instead of `-1`/`-2` to avoid same-minute collisions.
- Mouse clicks use `border_cache` inner rect (fixes borderless mode hit detection).
- Restored mouse drag-to-copy via OSC 52; cursor hidden on status bar row.

## [0.4.1] — 2026-04

### Added
- Pane close (`Ctrl+B x`) and tab close (`Ctrl+B &`) confirmations (`y`/`n`), tmux-compatible.
- Double-click tab name to rename.

### Fixed
- Rename UX pre-fills current name and corrects cursor overlay positioning.

## [0.4.0] — 2026-04

### Added
- Detach / attach session lifecycle.
- Tabs (tmux-style windows) with tab bar + click switching.
- Command palette (`Ctrl+B :`) with tmux-compatible commands.
- Borderless mode (`-b none`).
- Configurable prefix key.
- Copy mode with vi keys, visual selection, incremental search, OSC 52.

### Changed
- Wake-channel event loop: input round-trip latency reduced from ~3 ms to ~0.3 ms.

### Fixed
- Ctrl+non-letter, Alt+Backspace, F-key modifiers correctly encoded.

## [0.3.1] — 2026-03

### Added
- Text selection, status-line clock, exit codes, OSC title forwarding.
- tmux-compatible keys + status-bar styling.

## [0.3.0] — 2026-03

### Added
- `.ezpn.toml` project config.
- Layout presets (`dev`, `ide`, `monitor`, `quad`, `stack`, `main`, `trio`).
- Broadcast mode (`Ctrl+B B`).
- Configurable scrollback.

## [0.2.0] — 2026-02

### Added
- Multilingual READMEs (ko / ja / zh / es / fr).
- Redesigned settings modal.

### Fixed
- Keybinding compatibility for macOS Terminal.app.

## [0.1.0] — 2026-02

### Added
- Initial release. Pane splitting (`ezpn ROWS COLS`), layout DSL, IPC, workspace snapshots, prefix keys.
