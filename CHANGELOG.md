# Changelog

All notable changes to **ezpn** are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
ezpn adheres to [Semantic Versioning](https://semver.org/) (`0.MINOR.PATCH` until 1.0).

Entries are written in **functional-only style**: every bullet describes an observable change. No narrative, no rationale — *why* lives in commit bodies.

## [Unreleased]

### Added
- **`send-keys` API** (#38): new `ezpn-ctl send-keys [--pane N | --target current] [--literal] -- <key>...` subcommand and matching `IpcRequest::SendKeys` variant deliver chord-token sequences (or raw bytes via `--literal`) into a pane's PTY write half. KeySpec grammar in `src/keymap/keyspec.rs` covers Ctrl/Alt/Shift modifiers and the standard Named keys (Enter, Tab, Esc, Space, Backspace, Delete, arrows, Home/End, PageUp/PageDown, F1–F12). `--literal` writes bytes verbatim and rejects tokens that would compile to a Named key with `--literal forbids named keys (got 'X')`. Wire format uses `keys: Vec<String>` to keep multi-char literal arguments unambiguous; `IpcResponse::message` reports `"sent N bytes"` on success. No protocol bump.
- **Scrollback memory hygiene** (#34): per-pane `[[pane]] scrollback_lines` override in `.ezpn.toml`, new `[scrollback]` config table (`default_lines`, `max_lines`, `warn_bytes`), runtime IPC commands `IpcRequest::ClearHistory` and `SetHistoryLimit`, matching `ezpn-ctl clear-history --pane N` and `ezpn-ctl set-scrollback --pane N --lines L` subcommands. Daemon emits a one-shot `WARN` log line when a pane's estimated scrollback exceeds the configured byte budget (default 50 MiB).

### Fixed
- **Pane lifecycle GC** (#35): `Pane` is now an RAII handle with a deterministic `Drop` that signals its reader thread to exit, releases the PTY master fd, and joins the reader within a 250 ms deadline (warns and `mem::forget`s on timeout). Field declaration order ensures `master` drops before `reader_handle` so the blocking `read()` unblocks via EOF. `close_pane` accepts and prunes `restart_policies`, `restart_state`, and `zoomed_pane`. `TabManager::kill_all_inactive` now drains `tab.panes` and clears per-tab restart bookkeeping. PTY reader threads are now named `ezpn-pty-<pid>` for diagnostics.

### Performance
- **Render-loop micro-perf** (#37): per-PTY-chunk raw-byte scan no longer duplicates `?2004h`/`l` (bracketed paste) detection — `Pane::bracketed_paste` is read from `vt100::Screen::bracketed_paste()` after each `process()`. The remaining scanner is renamed to `track_focus_events` and only handles `?1004h`/`l`. Wake channel (`pane::WAKE_TX`) is now a bounded `mpsc::sync_channel(64)` with `try_send` — wake messages are idempotent so dropping overflow is safe and prevents unbounded growth when the main loop transiently stalls. `TabManager::tabs` storage is now `VecDeque<Tab>` for ~2× constant-factor improvement on tab switch with large N (public API unchanged).
- **Async snapshot pipeline** (#36): snapshot writes (and gzip+bincode for `persist_scrollback = true`) move off the daemon main loop into a dedicated `ezpn-snapshot` worker thread. Auto-saves are debounced with a 150 ms window so rapid attach/detach storms coalesce into ≤ 1 disk write per quiet period. User-initiated `ezpn-ctl save` keeps a synchronous-from-the-caller contract via an ack channel (30 s timeout); queue saturation surfaces a structured `IpcResponse::error("ezpn snapshot worker queue full; retry")`. Disk writes are atomic (temp file + rename). On `run()` return the worker drains pending captures within a 5 s deadline.

### Changed
- **Daemon I/O resilience** (#33): each attached client now drains a bounded `mpsc::sync_channel(64)` through a dedicated writer thread with `set_write_timeout(50ms)`; clients are evicted after 3 consecutive `WouldBlock`/`TimedOut`. The IPC socket is now served by a fixed pool of 4 worker threads (`crossbeam-channel::bounded(16)`) with `set_read_timeout(5s)` + `set_write_timeout(2s)`; surplus connections receive `IpcResponse::error("ezpn ipc pool saturated; retry")` and idle peers receive `IpcResponse::error("idle timeout")`.

### Compatibility
- **Wire protocol**: unchanged (still v1).
- **JSON IPC**: additive variants only (`ClearHistory`, `SetHistoryLimit`); old daemons reject the new commands with the existing `invalid request: …` path.
- **Config**: existing flat `scrollback = N` still works; new `[scrollback]` table is opt-in.
- **New deps**: `crossbeam-channel 0.5` (runtime).

## [0.9.0] — 2026-04-26 — Codebase & Release Hygiene

### Added
- **Module decomposition**: `src/main.rs` (2951 → 144 lines) split into `cli/`, `app/`, `direct.rs`. `src/server.rs` (2700+ → 16 lines) split into `daemon/{state,router,snapshot,render,dispatch,keys,event_loop}.rs`. CONTRIBUTING.md gains a "Module anatomy" section.
- **Property tests** (proptest 1.x, 128 cases each): 4 layout invariants (`prop_layout_render_no_overlap`, `prop_layout_all_panes_within_bounds`, `prop_layout_split_min_size`, `prop_layout_navigate_reachable`) + 4 snapshot invariants (`prop_snapshot_roundtrip`, `prop_snapshot_v2_to_v3_migration_no_loss`, `prop_pane_id_unique`, `prop_layout_in_snapshot_valid`).
- **Integration recordings** (5 in `tests/integration_recordings.rs`, 3 active): `attach_streams_until_eof`, `panic_in_one_pane_others_alive` (M1 #8 regression), `signal_term_writes_snapshot` (M1 #11 regression). Two `#[ignore]`d pending follow-up harness work.
- **Soak harness**: `benches/soak_10min.rs` opt-in via `--features soak` for nightly stability runs.
- **Coverage gate**: `scripts/coverage.sh` enforcing 65% floor via `cargo-llvm-cov`. CI runs weekly and on PRs labeled `area:test`.
- **CI matrix**: split into `check`, `integration`, `property`, and `coverage` jobs. All `dtolnay/rust-toolchain` actions pinned to `@1.95.0` to match `rust-toolchain.toml`.
- **Conventional Commits gate**: `commitlint` workflow validates PR titles + every commit against the type enum (`feat fix perf refactor chore docs test ci style release`). `release` is a first-class type.
- **Branch-name gate**: `branch-naming` workflow enforces `<type>/<slug>` (skips `dependabot/*` and `revert-*`).
- **PR labeler**: `actions/labeler@v5` auto-applies `area:*` labels based on touched paths.
- **Release drafter**: `release-drafter` aggregates merged PRs into a draft release note grouped by type.
- **Secret scanning**: `gitleaks` runs on every push and PR with full-history scan + custom rules for Cargo registry tokens and PEM private-key blocks.
- **Supply-chain audit**: weekly `cargo audit` (Mon 06:17 UTC) plus per-PR `cargo deny check --all-features` for advisories, license allowlist, banned wildcards, and unknown sources.
- **CHANGELOG enforcement**: PRs to `main` touching `src/**` or `Cargo.toml` must also edit `CHANGELOG.md`. Bypass via `skip-changelog` label, `chore(release):`/`release:` title, or `dependabot/*` head branch.
- **MAINTENANCE.md** gains a "Performance profiling" section with `cargo flamegraph` instructions and a `bench` workflow note.
- **README badges**: gitleaks + audit status added next to the existing CI badge.

### Changed
- `.clone()` call sites in the daemon code are annotated with `[perf:cold]`, `[perf:init]`, or `[perf:hot]` classifications. Three `TODO(perf)` flags placed on `Layout::clone()` cold-start sites and the per-mouse-event `cache.inner().clone()` for follow-up `Arc<…>` conversion.

### Compatibility
- **Wire protocol**: unchanged (still v1).
- **Snapshot schema**: unchanged (still v3).
- **New deps**: `proptest 1` (dev-only), `flate2`/`bincode 1.3`/`base64 0.22` already shipped in 0.8. No new runtime deps.

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
