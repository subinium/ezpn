# SPEC 08 — Hooks system

**Status:** Draft
**Related issue:** TBD
**PRD:** [v0.10.0](../../prd/v0.10.0.md)
**Category:** B. Automation & Scripting

## 1. Background

tmux's `set-hook -g pane-died "display-message 'pane #{pane_id} died'"`
lets users wire workflow automation onto multiplexer events without
running an external event-stream consumer. Common uses surveyed across
the top 50 starred dotfiles on GitHub:

- Notify on long-running pane exit (`pane-died`).
- Resize / re-equalize layout on `client-attached` (matches editor pane
  ratios on attach).
- Auto-rename window on `pane-focus-in` (per-app titles).
- Trigger session save on `client-detached`.
- Run a project init command after `session-created`.

ezpn currently has no hook surface at all. Users *can* poll the SPEC 07
event stream from a long-running script — but that requires a separate
process and survives only as long as the script does. Hooks are the
"declarative, stateless, in-daemon" alternative for the 95% use case
where a one-shot shell command is all you need.

The PRD locks the design choice in §7 / §9-Q2:

> Hooks: shell-out string contract, or structured action enum?
> **Default proposal**: shell string for v0.10, action enum tracked for v0.11.

> Hooks system invites users to write blocking shell-outs that stall the
> daemon → Run hook commands in a worker thread with a 5 s timeout.

This SPEC formalizes both.

## 2. Goal

A user adds a 3-line `[[hooks]]` block to `~/.config/ezpn/config.toml`,
runs `prefix r` (existing config reload at `src/daemon/keys.rs:396-408`),
and a documented daemon event fires their shell command in a worker
thread with bounded latency, bounded resource use, and zero risk of
stalling the main loop.

## 3. Non-goals

- **Structured action enum.** Deferred to v0.11 per PRD §9-Q2. v0.10 is
  shell-string only.
- **Hook chains / pipelines.** Each event runs its matching hooks in
  parallel. There is no "first hook's stdout feeds the second" wiring.
- **Hook return value affects event.** Nonzero exit is logged but does
  not cancel or alter the originating daemon action. (Designing
  vetoable hooks would require event reification across the entire
  main loop and is rejected scope.)
- **Per-pane hooks.** v0.10 ships global hooks only (matches tmux's
  `-g` flag). Per-pane hooks land with the structured action enum.
- **Pre-event abort.** `before-pane-spawn` cannot prevent the spawn.
  It runs concurrently and is purely observational.
- **Hot-add without reload.** Editing config requires `prefix r` to
  pick up new hooks. That matches existing config reload semantics
  and is intentional — we do not file-watch the config in v0.10.

## 4. Design

### 4.1 The 10 events (Pareto rationale)

Surveyed against tmux's full hook list (~25 events). These 10 cover
~95% of in-the-wild usage:

| # | Event | Why it earns its slot |
|---|---|---|
| 1 | `before-pane-spawn` | Logging, audit, dotenv-loader hooks before `fork()` |
| 2 | `after-pane-spawn`  | Per-pane setup (set title, send initial command) |
| 3 | `pane-died`         | Notification on crash / unexpected exit |
| 4 | `pane-exited`       | Cleanup, save session — fires on clean exit too |
| 5 | `client-attached`   | Welcome message, layout snap, send-keys greeting |
| 6 | `client-detached`   | Snapshot save, "you have unsaved changes" notice |
| 7 | `layout-changed`    | Persist layout to disk, sync with editor |
| 8 | `tab-created`       | Per-tab init script |
| 9 | `tab-closed`        | Cleanup, free per-tab resources |
| 10 | `session-renamed`  | Sync external state (window title, status DB) |

`pane-died` vs `pane-exited`: `pane-died` fires only on nonzero exit OR
killed-by-signal. `pane-exited` fires on every termination. Both fire for
a crash; only `pane-exited` for `exit 0`.

Events explicitly **not** in the v0.10 set, with rejection rationale:

| Tmux hook | Why deferred |
|---|---|
| `pane-focus-in/out` | High frequency, niche; cover via SPEC 07 stream |
| `pane-mode-changed` | Mode transitions are dense; too noisy for shell-out |
| `session-window-changed` | Subsumed by `tab-created` + `tab-closed` for v0.10 |
| `alert-bell` / `alert-silence` | Bell tracking is a separate v0.11 work item |
| `client-session-changed` | One session per client in v0.10 |

### 4.2 Config schema

Top of `~/.config/ezpn/config.toml` (or per-project `.ezpn.toml`):

```toml
[[hooks]]
event = "pane-died"
command = "notify-send 'pane {pane_id} died with exit {exit_code}'"
shell = true              # default false — exec argv directly
timeout_ms = 5000         # default 5000; max 30000

[[hooks]]
event = "client-attached"
command = ["/usr/local/bin/ezpn-greet", "{client_id}", "{session}"]
# shell = false  → command is argv[]; no shell injection
# timeout_ms unset → 5000

[[hooks]]
event = "after-pane-spawn"
command = "echo 'spawned {pane_id}' >> ~/.ezpn-history"
shell = true
```

Schema rules:

- `event` (string, required): one of the 10 names in §4.1.
- `command` (string OR array of strings, required):
  - String → must have `shell = true` (or default false but be a single
    word with no shell metacharacters; otherwise reject at config load).
  - Array → exec'd via `Command::new(argv[0]).args(&argv[1..])`. No
    shell. **Recommended for any hook involving variable substitution**.
- `shell` (bool, default `false`): if `true` and `command` is a string,
  invoke via `/bin/sh -c <command>`. If `false`, parse the string with
  `shell_words::split` (no `$VAR`, no `|`, no `;`, no `>`).
- `timeout_ms` (u32, default 5000, max 30000): hard kill the child
  process after this many ms. Above 30000 → reject at load with
  `hooks[N].timeout_ms must be ≤ 30000`.

Multiple `[[hooks]]` blocks with the same `event` are all run in
parallel when that event fires.

### 4.3 Variable expansion

Variables are `{name}` placeholders, substituted **before** exec.
Substitution is purely textual — there is no escaping. For
`shell = false`, this is safe because `shell_words::split` happens
*before* substitution, so an injected space in `{command}` cannot break
out into a new argv element. For `shell = true`, the user is on the
hook for quoting (same contract as tmux).

The complete variable set per event:

| Event | Variables |
|---|---|
| `before-pane-spawn` | `{session}`, `{tab_index}`, `{pane_id}` (= future ID), `{shell}`, `{cols}`, `{rows}` |
| `after-pane-spawn`  | `{session}`, `{tab_index}`, `{pane_id}`, `{shell}`, `{cwd}`, `{cols}`, `{rows}` |
| `pane-died`         | `{session}`, `{tab_index}`, `{pane_id}`, `{exit_code}`, `{signal}`, `{command}` |
| `pane-exited`       | `{session}`, `{tab_index}`, `{pane_id}`, `{exit_code}`, `{signal}`, `{command}` |
| `client-attached`   | `{session}`, `{client_id}`, `{client_name}`, `{mode}`, `{cols}`, `{rows}` |
| `client-detached`   | `{session}`, `{client_id}`, `{client_name}`, `{reason}` |
| `layout-changed`    | `{session}`, `{tab_index}`, `{spec}`, `{pane_count}` |
| `tab-created`       | `{session}`, `{tab_index}`, `{name}` |
| `tab-closed`        | `{session}`, `{tab_index}`, `{name}` |
| `session-renamed`   | `{session}` (= new name), `{old_session}` |

Conventions:

- Missing values → empty string. `{signal}` for a clean exit = `""`.
- `{exit_code}` for a signal-killed child = `""`; consumers should check
  `{signal}` first.
- `{client_name}` is the daemon-assigned label (e.g. `"client-3"`); not
  a remote hostname.
- Unknown placeholders → left as-is (matches tmux's `#{undefined}`
  behaviour); a single `eprintln!` warning is logged on first occurrence.

### 4.4 Execution model

```
main loop                    hook worker pool (sync_channel(64))
   │
   ▼ event fires
collect matching hooks ──┐
   │                     ▼
   │          enqueue HookJob{event, vars, hook_def}
   │                     │
   │                     ▼ (1 of N worker threads picks it up)
   │              variable expansion
   │                     │
   │                     ▼ Command::new(...).spawn()
   │                     │
   │                     ▼ wait_timeout(timeout_ms)
   │                     │
   ▼ continue loop      ┌─┴──────────────────────────┐
   (no blocking)        │ exit 0   → debug! log only │
                        │ exit !=0 → warn! log       │
                        │ timeout  → kill + warn!    │
                        └─────────────────────────────┘
```

**Worker pool** lives in `src/daemon/hooks.rs` and reuses the design from
SPEC 04 (snapshot worker pool):

- Fixed pool of 4 worker threads, spawned at daemon startup.
- Bounded `mpsc::sync_channel(64)` for jobs. If full, the main loop drops
  the new job and logs `warn!("hooks queue full, dropping <event>")`.
  This is consistent with SPEC 07's "diagnostic > transactional" stance.
- On daemon shutdown, `drop(tx)` causes workers to exit naturally;
  in-flight children are killed via `Child::kill()`.

**Timeout**: implemented with `wait_timeout::ChildExt::wait_timeout`
(crate `wait-timeout = "0.2"`). On timeout, send `SIGTERM`, wait 500 ms,
then `SIGKILL`. Children are spawned in their own process group so
`kill()` reaches grandchildren.

**Stdout/stderr**: redirected to `Stdio::null()` by default. A future
flag `capture_output = true` could buffer to logs but is out of scope.

### 4.5 Failure handling

| Failure | Effect |
|---|---|
| Hook spawn fails (`ENOENT`, etc.) | `warn!` log; daemon continues; originating event unaffected |
| Hook returns nonzero | `warn!` log: `hook <event> exited <N>`; event unaffected |
| Hook times out | `SIGTERM` → 500 ms grace → `SIGKILL`; `warn!` log; event unaffected |
| Hooks queue full (64 in flight) | New job dropped; `warn!` log once per second (rate-limited) |
| Hook config invalid at load | `apply_config_to_settings` (`src/config.rs`) returns Err; existing hooks remain active; `prefix r` shows `error: hooks reload failed: …` |

### 4.6 Reload

`prefix r` (`src/daemon/keys.rs:396-408`) currently calls
`config::load_config()` + `config::apply_config_to_settings(&cfg, settings)`.
We extend this path:

1. Load fresh `Vec<HookDef>` from TOML.
2. Validate (event names, timeout caps, command shape).
3. On validation failure: log error, leave existing hooks in place.
4. On success: atomically swap the daemon's `hooks: Arc<Vec<HookDef>>`.
   In-flight worker jobs continue against the old definitions; new events
   match against the new set.

`Arc` swap means hot-reload is lock-free on the read path (event firing).

## 5. Surface changes

### IPC / wire protocol

None for v0.10. Hooks are purely server-internal; they emit shell-outs,
not IPC events. (Programmatic hook registration over IPC is plausible
but deferred — config-file reload is the v0.10 management surface.)

The new `protocol.rs` constants from SPEC 07 (`S_EVENT`, `C_SUBSCRIBE`)
are deliberately re-used in spirit: every event that fires a hook also
emits an SPEC 07 envelope. The two systems share the same emit-site
list (§6 Touchpoints).

### CLI (`ezpn-ctl`)

No new subcommand for v0.10. Possible future additions (out of scope):

- `ezpn-ctl hooks list` — dump active hooks as JSON.
- `ezpn-ctl hooks fire <event>` — synthetic event, for testing.

These are tracked as v0.11 follow-ups.

### Config (TOML)

```toml
# ~/.config/ezpn/config.toml

# ── Existing v0.9 settings (unchanged) ──
shell = "/bin/zsh"
prefix_key = "b"
scrollback = 10000

# ── New v0.10 hooks ──
[[hooks]]
event = "pane-died"
command = "notify-send 'pane {pane_id} died ({exit_code})'"
shell = true

[[hooks]]
event = "after-pane-spawn"
command = ["/Users/me/.ezpn/setup-pane.sh", "{pane_id}", "{cwd}"]
timeout_ms = 2000
```

A complete `[[hooks]]` table accepts:

| Key          | Type                 | Default | Required |
|--------------|----------------------|---------|----------|
| `event`      | string (one of §4.1) | —       | yes      |
| `command`    | string OR array      | —       | yes      |
| `shell`      | bool                 | `false` | no       |
| `timeout_ms` | u32 (≤ 30000)        | `5000`  | no       |

## 6. Touchpoints

| File | Lines | Change |
|---|---|---|
| `src/config.rs` | (existing `Config` struct) | Add `pub hooks: Vec<HookDef>` field; deserialize `[[hooks]]` table |
| `src/config.rs` | (load + validate) | New `validate_hooks(&[HookDef])` returning Result with per-hook error |
| `src/daemon/hooks.rs` | new (~300 LOC) | `HookDef`, `HookEvent` enum, worker pool, variable expansion, `dispatch(event, vars)` |
| `src/daemon/event_loop.rs` | 75-90 | Initialize hook worker pool right after `restart_policies` setup |
| `src/daemon/event_loop.rs` | 280-360 | Fire `before-pane-spawn` / `after-pane-spawn` / `pane-died` / `pane-exited` |
| `src/daemon/event_loop.rs` | 752-931 | Fire `tab-created` / `tab-closed` / `session-renamed` |
| `src/daemon/event_loop.rs` | 1162-1167 | Fire `pane-died` from SIGCHLD reap path |
| `src/daemon/router.rs` (or `event_loop.rs:512-552`) | accept_client path | Fire `client-attached` after successful attach |
| `src/daemon/event_loop.rs` | 693-749 | Fire `client-detached` on detach/disconnect |
| `src/daemon/keys.rs` | 396-408 | Extend `prefix r` to also reload hooks via `Arc::swap` |
| `src/app/lifecycle.rs` | 320-360 | Fire `layout-changed` after every successful `do_split` / `close_pane` |
| `Cargo.toml` | dependencies | Add `wait-timeout = "0.2"` (dual-licensed MIT/Apache, deny.toml allowlist) |
| `tests/hooks.rs` | new | Integration tests (see §8) |

## 7. Migration / backwards-compat

- **Config schema**: additive only. Users without `[[hooks]]` blocks see
  zero behavioural change. Existing `config.toml` continues to load.
- **Config-load errors** for invalid hooks do **not** abort daemon
  startup. The daemon logs `warn!` and runs with no hooks, matching the
  existing tolerance for missing/malformed optional sections.
- **No protocol bump**, no IPC change, no client recompile required.
- **Workspace snapshots** (`workspace::WorkspaceSnapshot`) do **not**
  serialize hook definitions — hooks live in config, not session state.

## 8. Test plan

1. **Unit — config parse**:
   - Valid TOML with all 10 events → parses to `Vec<HookDef>` of length 10.
   - `event = "bogus"` → validation error names the event.
   - `timeout_ms = 60000` → validation error: max 30000.
   - `shell = true, command = ""` → error: empty command.
2. **Unit — variable expansion**: golden test for each event's variable
   set (matrix from §4.3). Unknown `{foo}` → left as-is + warn.
3. **Unit — worker pool backpressure**: enqueue 100 jobs against a pool
   of 4 with 1 s sleep each; assert at least 30 are dropped, no panic,
   no main-loop blockage (test runs the dispatcher synchronously and
   measures latency).
4. **Integration — `pane-died` fires**:
   ```
   start daemon with hook `pane-died` → write {pane_id} to /tmp/ezpn-died-<random>
   spawn pane that exits 1 immediately (e.g. `sh -c 'exit 1'`)
   wait 500ms
   assert /tmp/ezpn-died-<random> contains the pane id
   ```
5. **Integration — timeout kills child**:
   ```
   hook command = "sleep 30", timeout_ms = 200
   trigger event
   measure: child exits within 1 s (300 ms timeout + grace)
   assert: log contains "hook timed out"
   ```
6. **Integration — reload via `prefix r`**:
   ```
   start daemon with hook H1
   edit config: replace H1 with H2
   send `prefix r` (via SPEC 06 send-keys)
   trigger the event
   assert H2 ran, H1 did not
   ```
7. **Soak — hook leak check**: 10 000 `pane-died` events firing a hook
   that exits 0 immediately; assert daemon `ps -o nlwp` returns to the
   pre-test value within 1 s of finish (no thread leak; no zombie
   children).

## 9. Acceptance criteria

- [ ] All 10 events from §4.1 emit at the documented sites.
- [ ] `[[hooks]]` config block parses, validates, and reloads on
      `prefix r` without daemon restart.
- [ ] Hook commands run in worker pool (4 threads); main-loop median
      input latency under heavy hook load < 16 ms (PRD §6 perf gate).
- [ ] `timeout_ms` enforced via SIGTERM → SIGKILL escalation.
- [ ] Variable expansion matches the §4.3 matrix.
- [ ] Invalid config rejected with hook-index-pointing error message.
- [ ] All 7 test categories in §8 pass.
- [ ] `wait-timeout` added to `deny.toml` allowlist.
- [ ] CHANGELOG entry under v0.10.0 / Automation.
- [ ] `cargo clippy --all-targets -- -D warnings` clean.

## 10. Risks

| Risk | Mitigation |
|---|---|
| User writes a slow shell-out (curl, ssh) and stalls workflow automation | Per-hook `timeout_ms` defaulting to 5 s, hard cap 30 s. PRD §7 explicitly mandates this. |
| Worker pool exhaustion under burst | Bounded `sync_channel(64)`; main loop never blocks on enqueue (dropped jobs logged, originating event unaffected). |
| Shell-injection via variables in `shell = true` mode | Document the contract: `shell = true` puts the user on the hook for quoting. Default is `shell = false` + array `command`, which is injection-safe. |
| Zombie children from killed hooks | Spawn each hook in its own process group; SIGTERM/SIGKILL targets the group; combined with the existing SIGCHLD reaper (`src/daemon/event_loop.rs:1158-1170`) zombies are reaped within one main-loop iteration. |
| Reload races a firing event | `Arc::swap` of the hook list is atomic. In-flight workers see the snapshot they were dispatched against; new firings see the new list. No visible inconsistency. |
| `wait-timeout` crate is unmaintained | Cross-checked: last release 0.2.0 (2022), still depends only on `libc`. ≤ 200 LOC; if it goes stale we can vendor it (`std::process::Child::wait` + a sleeping reaper thread is ~30 LOC). |
| `client-attached` hook runs slow → blocks attach? | No — the hook is dispatched *after* `accept_client` returns (event_loop.rs:516-552). The new client is already attached and rendering by the time the hook starts in a worker thread. |

## 11. Open questions

1. Should `command` allow `~/` and `$HOME`? **Default proposal**: only
   `~/` (expanded by `shellexpand` at load time). Env-var expansion only
   when `shell = true` (the shell does it).
2. Should we wire stdout/stderr to a per-hook log file? **Default
   proposal**: no for v0.10 — `Stdio::null()`. Add `log_file = "..."`
   field in v0.11 if real users ask.
3. Should hooks run for events triggered *by* IPC (e.g. SPEC 06
   `send-keys` causing a `pane-exited`)? **Default proposal**: yes,
   transparently. The hook system observes daemon state changes; it
   doesn't care who caused them. Documented behaviour.
4. Should there be a `chdir` field for hook commands? **Default
   proposal**: no — users can wrap with `cd /path && cmd` under
   `shell = true`. Keeps the schema small.
