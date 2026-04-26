# Changelog

All notable changes to **ezpn** are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
ezpn adheres to [Semantic Versioning](https://semver.org/) (`0.MINOR.PATCH` until 1.0).

Entries are written in **functional-only style**: every bullet describes an observable change. No narrative, no rationale — *why* lives in commit bodies.

## [Unreleased]

## [0.11.1] — 2026-04-26 — Security & correctness hotfixes

### Security
- **Hook shell injection (B1)**: `[[hooks]]` entries with `shell = true` now POSIX-single-quote every `{var}` substitution before reaching `/bin/sh -c`. Previously a tab/session/client name containing `; rm -rf $HOME` (or any shell metacharacter) would be re-interpreted by the shell on the next matching event. New `expand_vars_shell` helper does the quoting; literal templates stay as-is so users can still write `echo {a}|grep b` and the pipe still pipes. The non-shell `Argv` path was already exec-style and unaffected.
- **Daemon process-group protection (B2)**: hook child processes call `setsid()` in `pre_exec` to escape the daemon's process group, but the return value was ignored — if `setsid` failed (rare: caller already a session leader) the child stayed in the daemon's pgid, and `kill(-pid, SIGKILL)` on hook timeout would target the daemon itself. New `kill_pgrp_or_pid` helper checks `getpgid(child)` and falls back to a single-pid kill when the setsid invariant doesn't hold.
- **`send-keys --literal` ANSI escape rejection (B3)**: literal mode bypassed the keyspec parser, so a script could inject `\x1B]52;c;<base64>\x07` (OSC 52) to hijack the user's clipboard, or arbitrary CSI sequences to spoof the prompt / poison shell history. The dispatcher now rejects any token containing ESC (`0x1B`) or NUL (`0x00`) with a structured error and steers users toward the non-literal keyspec form for special keys.

### Fixed
- **Regex search compile DoS + cache (C1)**: copy-mode regex search now builds via `RegexBuilder::size_limit(1<<20).dfa_size_limit(1<<20)` so a pathological pattern like `a{1000000}` fails at `build()` instead of stalling the daemon main loop. Compiled regexes are cached on `CopyModeState` keyed by post-smart-case pattern, so incremental search no longer recompiles on every keystroke when the user is just appending characters.
- **SIGTERM auto-save reliability (C2)**: graceful shutdown was using `try_send` for the final snapshot — if the worker queue happened to be saturated at SIGTERM the latest workspace state was silently dropped. New `SnapshotWorker::submit_with_deadline` synthesizes `send_timeout` over std `mpsc` (no native equivalent) with a 300 ms ceiling that absorbs at least one full debounce cycle. Cleanup ordering is now load-bearing and documented inline.
- **Events bus true drop-oldest (C3)**: per-subscriber backlog migrated from `mpsc::sync_channel` to `Arc<Mutex<VecDeque>>` + `Condvar`. Under backpressure subscribers now observe the **most recent** envelopes (the canonical reactive-stream contract); the previous `try_send`-based path silently degraded to drop-newest, leaving slow consumers reading a stale prefix. `EventQueue::push_drop_oldest` returns whether it kicked out an entry so the caller's overflow-notice accounting stays honest.
- **`S_SUBSCRIBE_ERR` (0x8A) for post-handshake failures (C4)**: malformed `C_SUBSCRIBE` payloads and empty topics lists used to be reported via `S_HELLO_ERR` (0x86), which clients reasonably interpret as "version mismatch — close". New tag has its own type (`SubscribeErr`) so consumers can disambiguate. Connection still closes after the err frame; no protocol bump.
- **send-keys element + line caps (C5)**: `SEND_KEYS_MAX_TOKENS = 4096` rejects payloads with absurd token counts before serde_json allocates a giant `Vec<String>` (the existing 16 MiB byte cap allowed `["", "", … 100M …]` to slip through with sum=0). The IPC socket reader now wraps each connection in `take(IPC_MAX_LINE_BYTES = 32 MiB)` so a hostile peer cannot OOM the daemon by sending a multi-GiB line without a newline.
- **Hook worker `wait_timeout` for kill grace (C6)**: the SIGTERM → SIGKILL grace path used `std::thread::sleep(500ms)` which pinned the worker thread for the full grace window even when the child responded promptly. Now uses `child.wait_timeout(grace)` so a well-behaved child releases the worker immediately; the worker pool's effective throughput under stuck-child storms recovers from ~8 hooks/sec/worker to the steady-state rate.
- **Regex smart-case for escape/char-class (C7)**: smart-case judgement now walks the pattern excluding `\X` escape sequences and counting char-class contents — `\D` / `\S` are correctly identified as carrying no literal uppercase (smart-case fires), `[A-Z]+` correctly counts the literal `A`/`Z` inside the class (smart-case skipped). The old `chars().any(is_uppercase)` heuristic misclassified both edges.

### Compatibility
- **Wire protocol**: unchanged (still v1).
- **Binary protocol**: additive tag only (`S_SUBSCRIBE_ERR = 0x8A`). Existing clients keying off `S_HELLO_ERR` for subscribe failures continue to see `S_HELLO_ERR` only on actual handshake errors; subscribe failures now use the dedicated tag.
- **JSON IPC**: no schema changes; new caps (`SEND_KEYS_MAX_TOKENS`, `IPC_MAX_LINE_BYTES`) only reject pathological payloads.
- **Hook templates**: `shell = true` users whose templates *intentionally* allowed substitution-driven shell expansion should switch to argv-form hooks. The new behaviour treats every substitution as a literal value, which is the principle-of-least-surprise default.

### Test totals
- 283 → **294** (+11): hooks shell-quote unit + integration coverage, copy-mode smart-case + cache + size-limit, EventQueue drop-oldest + close-unblocks-waiter.

## [0.11.0] — 2026-04-26 — Automation, UX & parity foundations

### Added
- **Layout primitives for `break-pane` / `join-pane`** (#44, partial): `Layout::detach(id)` extracts a leaf and collapses the parent split into the surviving sibling (returns `None` on the only-leaf case); `Layout::insert_pane(new_id, target, dir, place_after, ratio)` inserts a new leaf next to `target` with caller-chosen direction, slot, and ratio (clamped to `[0.1, 0.9]`). Pane identity is preserved by value: the orchestrating layer moves the `Pane` struct between tabs without touching the PTY, vt100 parser, or child process. 8 unit tests cover SPEC 12 §4.2 cases A/B/C plus the round-trip. IPC variants, CLI subcommands (`ezpn-ctl break-pane` etc.), `TabManager::extract_pane_from_inactive`, and the `prefix !` / `prefix m` / `prefix J` keybindings land in a follow-up.
- **Regex search in copy mode** (#45, partial): `CopyModeState` gains `search_engine: SearchEngine` (Substring | Regex; default Substring). `find_regex` uses the `regex` crate with smart-case (lowercase-only query gets `(?i)`); invalid patterns return zero matches without panicking. The display-width fix from issue #15 (UnicodeWidthStr for highlight length vs byte length) carries over to both backends. New `CopyModeState::new_with_engine(rows, cols, engine)` constructor lets the daemon read `[copy_mode] search` from config and pick the backend at copy-mode entry. Config wiring, `Ctrl+R` toggle binding, named buffer registry, emacs key table all deferred to a follow-up.
- **Action vocabulary module** (#41, partial): new `src/keymap/action.rs` with the typed `Action` enum + parser. Covers every existing v0.9 palette command plus the SPEC 09 vocabulary additions (`select-pane DIR`, `enter-mode MODE`, `leave-mode`, `detach`, `kill-session`, `reload-config`, `toggle X`, `command-palette`). 8 parser unit tests cover the SPEC §4.2 alias matrix and reject paths. Full `[keymap.*]` TOML loader and `daemon/keys.rs` refactor land in a follow-up.
- **`nucleo-matcher` dep for SPEC 10 fuzzy palette** (#42, scaffold): adds the audited matcher dependency (used by Helix, Zed, Yazi). `PaletteState`, render path, and `prefix + p` binding land in a follow-up.
- **Hooks system** (#40): user-defined shell commands fire on daemon events. New `[[hooks]]` config block accepts `event` (one of 10 names), `command` (string with `shell = true` or argv array, default `false`), and `timeout_ms` (default 5000, max 30000). Hook commands run on a 4-thread worker pool with a bounded `mpsc::sync_channel(64)` queue (drops on overflow with a warn line); each child runs in its own process group so SIGTERM → 500 ms grace → SIGKILL reaches grandchildren. Variable expansion (`{session}`, `{client_id}`, `{pane_id}`, etc.) substitutes per-event values into the `command` string before exec; unknown placeholders pass through verbatim. v0.11 wires `client-attached`, `client-detached`, `tab-created`, `tab-closed`, `session-renamed` (re-uses the active-tab rename surface); `pane-died`, `pane-exited`, `before/after-pane-spawn`, `layout-changed` plus `prefix r` hot-reload land in a follow-up. Invalid hook entries are dropped at config load with a warn line — daemon startup never aborts on a bad hook.
- **Event subscription stream** (#39): new binary-protocol surface for long-lived subscribers. `C_SUBSCRIBE` (0x08) registers a connection for one or more topics (`pane`, `client`, `layout`, `tab`, `mode`); the daemon replies `S_SUBSCRIBE_OK` (0x87) and pushes one JSON envelope per `S_EVENT` (0x88) frame thereafter. New `CAP_EVENT_STREAM = 0x0010` bit advertises support in the hello handshake. Per-subscriber `mpsc::sync_channel(256)` plus drop-oldest backpressure (cites SPEC 01) keeps the daemon main loop from ever blocking on a slow consumer; cumulative drops surface inline as a synthetic `_meta`/`overflow` envelope on `S_EVENT_OVERFLOW` (0x89). v0.11 emits `client.attached` / `client.detached` (with `reason ∈ {detach_request, socket_closed}`) and `tab.switched`; remaining topics (`pane.*`, `layout.*`, `mode.*`) and the `ezpn-ctl events` subcommand land in a follow-up PR.
- **`send-keys` API** (#38): new `ezpn-ctl send-keys [--pane N | --target current] [--literal] -- <key>...` subcommand and matching `IpcRequest::SendKeys` variant deliver chord-token sequences (or raw bytes via `--literal`) into a pane's PTY write half. KeySpec grammar in `src/keymap/keyspec.rs` covers Ctrl/Alt/Shift modifiers and the standard Named keys (Enter, Tab, Esc, Space, Backspace, Delete, arrows, Home/End, PageUp/PageDown, F1–F12). `--literal` writes bytes verbatim and rejects tokens that would compile to a Named key with `--literal forbids named keys (got 'X')`. Wire format uses `keys: Vec<String>` to keep multi-char literal arguments unambiguous; `IpcResponse::message` reports `"sent N bytes"` on success. No protocol bump.

### Compatibility
- **Wire protocol**: unchanged (still v1).
- **JSON IPC**: additive variants only (`SendKeys`, `PaneTarget`); old daemons reject the new variant with the existing `invalid request: …` path.
- **Binary protocol**: additive tags only (`C_SUBSCRIBE`, `S_SUBSCRIBE_OK`, `S_EVENT`, `S_EVENT_OVERFLOW`); `CAP_EVENT_STREAM` bit advertised in `S_HELLO_OK`.
- **New runtime deps**: `wait-timeout 0.2` (hooks worker pool), `nucleo-matcher 0.3` (palette foundation, not yet wired), `regex 1` (copy-mode search; previously a transitive via criterion, now an explicit prod dep with `default-features = false` + `["std", "unicode"]`).
- **Partial scopes**: SPEC 09–13 each ship a foundation in v0.11.0 but defer the user-facing surface (TOML loaders, render paths, IPC variants, CLI subcommands, additional event hooks). Tracked as follow-up PRs against the same SPEC numbers.

## [0.10.0] — 2026-04-26 — Daemon stability & perf hygiene

### Added
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
