# Maintenance

ezpn is a Rust binary that ships to `crates.io` and runs as a long-lived daemon. This file is the operational handbook — what to update when, what never to break, and how to release.

---

## Update cadence

| Scope | Frequency | Trigger |
|---|---|---|
| **Dependencies** | Monthly | `cargo update` + audit; major bumps require an issue |
| **MSRV** | Yearly or when forced | A blocking dependency requires newer Rust |
| **`vt100` / `portable-pty`** | Per upstream release | Both are PTY/parser-critical; review changelogs |
| **Translations** | Per release | English README is the source of truth |
| **Terminal compatibility matrix** | Per release | Re-validate macOS Terminal, iTerm2, Alacritty, Kitty, WezTerm, Ghostty, GNOME Terminal, tmux-nested |
| **Wire-protocol version** | Per breaking schema change | Must be bumped together with handshake compatibility |
| **Snapshot version** | Per breaking schema change | Migration code must be added in the same PR |

---

## 🛑 Never do

1. **Never break the wire protocol without bumping `PROTOCOL_VERSION` and adding a handshake fallback.** A 0.5.x client must either work against a 0.6.x daemon or refuse with a clear message. Silent corruption is a release blocker.
2. **Never break snapshot compatibility without a migration.** Old `.json` snapshots in `$XDG_DATA_HOME/ezpn/sessions/` must round-trip through the new `WorkspaceSnapshot` parser. Add `migrate_v{N}_to_v{N+1}` and a fixture test.
3. **Never `unwrap()` in a worker thread, reader thread, render path, or signal handler.** A single unhandled panic must NOT take down the daemon. Wrap thread bodies with `catch_unwind` and degrade the affected pane only.
4. **Never block in the render loop.** No `mutex.lock()` held across PTY reads, no `recv()` without a timeout, no synchronous I/O on the path that wakes the renderer.
5. **Never grow a buffer unbounded.** Scrollback, OSC 52 queue, IPC response queue, render buffer — every queue has a documented cap. New code must declare the cap.
6. **Never commit secrets, `.env*`, `CLAUDE.md`, or AI session files.** `.gitignore` covers it; verify with `git ls-files | grep -iE '\.env|\.pem|id_rsa|CLAUDE.md|AGENTS.md'` before every release.
7. **Never force-push to `main`.** Releases are immutable. If a release is wrong, cut a patch (`vX.Y.Z+1`).
8. **Never bump only one of `Cargo.toml` and `CHANGELOG.md`.** They ship together.
9. **Never edit a translation without editing the same section in English `README.md`** — the English version is canonical. Translations follow, never lead.
10. **Never `cargo publish` without a green CI on the tagged commit.**

## ✅ Always do

1. **Run the full pre-CI gate before pushing:**
   ```bash
   cargo fmt -- --check && \
   cargo clippy --all-targets -- -D warnings && \
   cargo test && \
   cargo build --release
   ```
2. **Update `CHANGELOG.md` in functional-only style.** Every bullet describes an observable change. No narrative. No "we decided to". If you're explaining *why*, that goes in the commit body.
3. **For perf changes, attach `cargo bench --bench render_hotpaths` before/after numbers** in the PR description. Regressions need an explicit waiver.
4. **For protocol/schema changes, add a fixture test** that loads an old serialized blob and verifies it migrates correctly.
5. **For new keybindings, add them to:**
   - `README.md` keybinding table
   - All translated READMEs
   - In-app help overlay (`Ctrl+B ?`)
   - Status bar hint (per-mode shortcuts)
6. **Sync all four translations together** when a new feature lands.
7. **Bump `MSRV` only with an explicit issue and a justification.**

---

## 🚀 Release process

ezpn ships to `crates.io` as a binary crate. There is no separate release-notes file — `CHANGELOG.md` is the in-repo source of truth, and GitHub release notes are user-facing extracts cut at tag time.

Steps, in order:

1. **Finalize `CHANGELOG.md`.** Move `[Unreleased]` entries under a new `[X.Y.Z] — YYYY-MM-DD` heading. Functional-only style.
2. **Bump `Cargo.toml` version.** Update `version = "X.Y.Z"`.
3. **Run pre-CI:**
   ```bash
   cargo fmt -- --check && cargo clippy --all-targets -- -D warnings && cargo test && cargo build --release
   ```
4. **Commit:**
   ```bash
   git commit -am "chore: release vX.Y.Z"
   ```
5. **Push the commit** and wait for green CI on `main`.
6. **Create an annotated tag:**
   ```bash
   git tag -a vX.Y.Z -m "vX.Y.Z — <one-line summary>"
   git push origin vX.Y.Z
   ```
7. **Create the GitHub release** (notes are auto-generated from CHANGELOG, or written to a temp file):
   ```bash
   gh release create vX.Y.Z --title "vX.Y.Z" --notes-file /tmp/ezpn-vX.Y.Z-notes.md
   ```
8. **Publish to crates.io:**
   ```bash
   cargo publish
   ```
   Verify with `cargo search ezpn`.
9. **Verify install:**
   ```bash
   cargo install ezpn --version X.Y.Z --force
   ezpn --version
   ```

If any step fails, do NOT force-push or move the tag. Cut a patch.

---

## 🔒 Load-bearing invariants

If any of these regress, a higher-level guarantee breaks.

| Invariant | Violation symptom | Enforced by |
|---|---|---|
| Daemon survives any single-pane panic | One bad shell crashes whole session | issue #1 + integration test |
| Wire protocol versioned with handshake | Old client + new daemon = silent corruption | `protocol::PROTOCOL_VERSION` + `Hello`/`HelloAck` |
| Snapshot schema versioned with migrations | Reattach loses tabs / panes | `WorkspaceSnapshot::version` + `migrate_*` |
| Every queue has a documented cap | Memory grows under sustained output | code review + soak test |
| `cargo fmt`, `cargo clippy -D warnings`, `cargo test` all pass | CI red | CI |
| MSRV is `1.82` | Users on stable can't build | CI `msrv` job |
| README + 5 translations stay in sync on commands/flags | Non-English users see stale flags | review |

---

## Performance profiling

The daemon's hot paths are the per-frame render and the per-event
dispatch tree. To investigate a regression or validate an Arc/clone
change, capture a flamegraph against a small but representative
workload:

- Install: `cargo install flamegraph`
- Measure: `cargo flamegraph --bin ezpn -- 2 2 -d`
- Compare baselines before/after a refactor.

The `2 2 -d` invocation spawns a 2x2 grid in direct (single-process)
mode so the flamegraph captures the live render loop without server
IPC noise. Pair with `cargo bench --bench render_hotpaths` for
microbenchmark deltas — keep both numbers in any perf-tagged PR.

---

## 🎭 Recently decided (don't re-argue)

- **GitHub Flow only — no `develop` branch.** Decided 2026-04 (commit `6560e42`).
- **Snapshots are layout + commands + env, NOT scrollback** in v2. Scrollback persistence is opt-in starting v0.8.0 (#14).
- **Multi-client attach is Steal / Shared / Readonly.** Decided in v0.5.0 (PR #7).
- **`/tmp/ezpn-session-{name}.sock` is the per-session daemon socket.** Mode 0o600. Cleanup on graceful shutdown.
- **Session naming auto-derives from `basename($PWD)` with timestamp suffix on collision.** Pin via `[session].name` in `.ezpn.toml` lands in v0.8.0 (#15).
- **Prefix key is configurable; per-binding remap is not** until a clear request lands.
- **No plugin system.** Smaller surface, faster iteration. Reconsider after 1.0.

---

## How future AI sessions / contributors should read this

1. This file (`MAINTENANCE.md`) — operational rules + release process
2. [`CONTRIBUTING.md`](CONTRIBUTING.md) — workflow + branch naming + PR rules
3. [`README.md`](README.md) — user-facing surface
4. [`CHANGELOG.md`](CHANGELOG.md) — what shipped when
5. Recent open issues (`gh issue list --milestone "v0.X.0"`) — what's in flight

Do not propose structural rewrites on a first contact. Confirm with the maintainer before touching the wire protocol, snapshot schema, or signal handling.
