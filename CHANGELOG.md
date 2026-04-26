# Changelog

All notable changes to **ezpn** are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
ezpn adheres to [Semantic Versioning](https://semver.org/) (`0.MINOR.PATCH` until 1.0).

Entries are written in **functional-only style**: every bullet describes an observable change. No narrative, no rationale — *why* lives in commit bodies.

## [Unreleased]

## [0.8.0] — 2026-04-26 — Workflows that Stick

### Added
- **`.ezpn.toml` env interpolation**: `${HOME}`, `${env:VAR}`, `${file:.env.local}`, `${secret:keychain:KEY}` now expand in pane env values. Recursion capped at depth 8 with cycle detection.
- **`.env.local` auto-merge**: file beside `.ezpn.toml` is loaded automatically and overrides `[env]` keys. Format: `KEY=VALUE`, `# comments`, `KEY="quoted"`.
- **macOS Keychain backend** for `${secret:keychain:KEY}` via the `security` CLI; Linux uses `secret-tool`; both fall through to `${env:KEY}` with a warning when unavailable.
- **`ezpn doctor`** subcommand: validates `.ezpn.toml` and prints per-pane env resolution with `✓` / `✗ Missing reference: …`. Exits non-zero on any failure.
- **Settings persistence**: every change in `Ctrl+B Shift+,` is atomically written to `~/.config/ezpn/config.toml` (tmp + rename, pid-suffixed). Failures warn but never crash.
- **`Ctrl+B r` hot reload** of `~/.config/ezpn/config.toml` — apply external edits without detaching.
- **Settings panel footer** shows the path settings are saved to.
- **TOML theme system** (`src/theme.rs`): 18-color `Theme` with `Rgb`, `Theme::adapt(TermCaps)` quantizing to truecolor / 256 / 16 based on `$COLORTERM` and `$TERM`.
- **5 built-in themes** embedded at compile time: `default`, `tokyo-night`, `gruvbox-dark`, `solarized-dark`, `solarized-light`. Selectable via `theme = "..."` in config.
- **User themes**: drop a TOML at `~/.config/ezpn/themes/<name>.toml` and reference it by name. Corrupt files fall back to `default` with a one-line warning.
- **Scrollback persistence in snapshots** (v3 schema, opt-in). Toggle via `persist_scrollback = true` globally or `[workspace] persist_scrollback = true` per project. Encoded as base64(gzip(bincode(rows))) with a 5 MiB-per-pane hard cap; oldest rows truncated first.
- **Session pin**: `[session].name` in `.ezpn.toml` overrides `basename($PWD)`. CLI `-S` still wins. New `--new` / `--force-new` flag bypasses auto-attach to existing sessions.
- **Atomic collision counter**: `repo`, `repo-1`, …, `repo-99`, then `repo-{millis}-{pid}` fallback. Dead sockets are reaped during the scan instead of going stale.
- **`SessionResolution::{New, AttachExisting}`** lets callers distinguish "spawned a new daemon" from "joined an existing one" without re-probing the socket.

### Changed
- **Snapshot schema bumped to v3** (`SNAPSHOT_VERSION = 3`). v2 snapshots load transparently with `migrate_v2` — they simply have no scrollback. v3 snapshots written without `persist_scrollback` are bit-compatible with v2 readers (`scrollback_blob` is `skip_serializing_if`).
- **Rendering colors are no longer hardcoded.** Every `Color::Rgb` literal in `src/render.rs` and `src/settings.rs` was replaced with field access on `AdaptedTheme`. The active theme is loaded once at startup and threaded through every render path.
- **Bind-time `EADDRINUSE`** triggers one in-place retry after re-probing socket liveness, eliminating a narrow race when two `ezpn` invocations resolve the same name within microseconds.

### Compatibility
- **Wire protocol**: unchanged (still v1).
- **Snapshot schema**: bumped to v3. v2 snapshots auto-migrate; v3 snapshots without scrollback remain readable by v2 code.
- **New deps**: `flate2`, `bincode 1.3`, `base64 0.22` (all for snapshot blobs).

## [0.7.0] — 2026-04-26 — Native Feel & Perf

### Added
- `MIN_PANE_W`, `MIN_PANE_H`, `can_split()` in `layout` so callers can pre-check before invoking a split that would produce a sub-3-cell pane.
- `EZPN_ALT_LEGACY=1` opt-out for users on legacy shells that still expect the ESC-prefix Alt encoding.

### Changed
- **Cold/warm attach is no longer polling-driven.** `spawn_server` hands the daemon an inherited pipe; the daemon writes one byte after `UnixListener::bind` succeeds and the parent `poll(2)`s for it. Eliminates the 50 ms wake quantum that capped warm attach latency.
- **Alt+Char encodes as CSI u** (`\x1b[<code>;<mods>u`), matching the existing Alt+Arrow / Alt+Function encoding. Resolves bash / zsh / vim binding mismatches where Alt+letter and Alt+arrow used different protocols.
- `clear_rect` / `clear_title` reuse a shared `BLANK_ROW_BUF` instead of `" ".repeat(width)` per call. Removes the dominant heap traffic during resize / scroll bursts.

### Fixed
- Search highlight no longer over-paints adjacent cells on emoji / wide-char queries — match length is now display width, not byte length.
- `Layout::split_area` no longer collapses one child to 1 cell at extreme ratios.

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
