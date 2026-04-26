# SPEC 03 — Pane Lifecycle GC

**Status:** Draft
**Related issue:** TBD (v0.10.0 milestone)
**PRD:** [v0.10.0](../../prd/v0.10.0.md)
**Category:** A. Stability & Resource Hygiene
**Severity origin:** Audit P2 #4 (`restart_state` leak), P2 #5 (PTY reader thread orphan), Nit #10 (`kill_all_inactive` retains structs)

---

## 1. Background

Three independent leak surfaces all stem from the same omission:
**`Pane` has no shutdown protocol** beyond `child.kill()`, and the
maps that key off pane id are not pruned when a pane is removed.

### (a) `restart_state` / `restart_policies` not pruned on `close_pane`

`src/app/lifecycle.rs:362-375` defines `close_pane`:

```rust
pub(crate) fn close_pane(
    layout: &mut Layout,
    panes: &mut HashMap<usize, Pane>,
    active: &mut usize,
    pane_id: usize,
) {
    if let Some(mut pane) = panes.remove(&pane_id) {
        pane.kill();
    }
    layout.remove(pane_id);
    if *active == pane_id {
        *active = *layout.pane_ids().first().unwrap_or(&0);
    }
}
```

Notice what's missing: `restart_policies` and `restart_state` are
never touched. Both are owned by the daemon `run()` (see
`src/daemon/event_loop.rs:75` and `:100`):

```rust
let mut restart_policies: HashMap<usize, project::RestartPolicy> = HashMap::new();
// …later…
let mut restart_state: HashMap<usize, (Instant, u32)> = HashMap::new();
```

…but `close_pane` doesn't accept them as parameters. Every closed pane
leaves an orphan entry. After a long session the map grows
unboundedly (one `(Instant, u32)` per pane id ever closed).

The same maps live on `Tab` (`src/tab.rs:9-18`):

```rust
pub struct Tab {
    pub name: String,
    pub layout: Layout,
    pub panes: HashMap<usize, Pane>,
    pub active_pane: usize,
    pub restart_policies: HashMap<usize, project::RestartPolicy>,
    pub restart_state: HashMap<usize, (Instant, u32)>,
    pub zoomed_pane: Option<usize>,
    pub broadcast: bool,
}
```

`zoomed_pane` is also stale-prone — already handled in the event loop
at `:419-426`, but `close_pane` doesn't clear it from the
caller-supplied `Option<usize>` either.

### (b) PTY reader thread orphan

`src/pane.rs:161-194` spawns the PTY reader and **drops the
`JoinHandle`**:

```rust
let (tx, rx) = mpsc::sync_channel(32);
std::thread::spawn(move || {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                    wake_main_loop();
                }
                Err(_) => break,
            }
        }
    }));
    // …panic logging…
});
```

When `Pane::kill()` is called, the child process is killed but the
reader thread *does not* immediately exit — it's blocked in
`reader.read(&mut buf)`. It only unblocks when the kernel notices the
slave PTY end is closed, which happens when the master is dropped.
That dropping happens when the `Pane` itself is dropped, which
in `kill_all_inactive` (see (c)) **never happens**.

In `close_pane` the `Pane` *is* dropped, so the master falls out and
the reader eventually hits EOF — but there is no upper bound on how
long that takes, no observable join, and no way to assert the thread
is gone in tests.

### (c) `kill_all_inactive` leaves `Pane` structs allocated

`src/tab.rs:172-178`:

```rust
pub fn kill_all_inactive(&mut self) {
    for tab in &mut self.tabs {
        for pane in tab.panes.values_mut() {
            pane.kill();
        }
    }
}
```

This sends SIGHUP to every child but **does not drain `tab.panes`**.
`Tab` instances stay in `TabManager::tabs` with their `Pane` structs,
each holding:

- A `Box<dyn MasterPty + Send>` (file descriptor — `master`).
- A `Box<dyn Write + Send>` writer (separate fd via `take_writer`).
- An `mpsc::Receiver<Vec<u8>>` connected to the still-running reader thread.
- A `vt100::Parser` with up to 100 K lines of scrollback.
- An OSC 52 buffer up to 256 KB.
- `initial_env: HashMap<String, String>`.

Worst case after a `KillSession` the daemon retains ~640 MB ×
N_inactive_tabs of vt100 RSS that nobody can free until process exit.
For a session-killer that wants to exit and persist state, this is
fine; for any path that calls `kill_all_inactive` *and continues
running* (a future "graceful shutdown then restart" hook for instance)
it is a leak.

In v0.10 the only call site is the `KillSession` action (event_loop
`:920-929`), which exits immediately, so this is currently latent.
Fix it now to remove the foot-gun before issue 06–11 (which add
hooks/automation that will plausibly call `kill_all_inactive` from
non-exit paths).

---

## 2. Goal

Make `Pane` a **fully-owned RAII handle** with deterministic shutdown:

- Closing a pane drops every map entry keyed off its id.
- Dropping a `Pane` cleanly signals its reader thread, joins with a
  bounded 250 ms timeout, and never leaves a thread or fd dangling.
- Killing a tab drains its panes, ensuring all `Pane` drops fire.

PRD release criterion: thread budget — `ps -o nlwp` constant across
1000 pane-open/close cycles (no thread leak).

---

## 3. Non-goals

- Cross-platform thread cancellation (macOS/Linux differ on POSIX
  cancellation points). Solution uses an explicit shutdown signal,
  not async cancellation.
- Replacing `std::thread::spawn` for the reader with a tokio task.
  Out of scope for v0.10.
- Fixing the same shape in *every* daemon-spawned thread — only the
  PTY reader is in scope (it's the one with leak symptoms). The IPC
  pool is covered by SPEC 01; the snapshot worker by SPEC 04.
- A `Drop` impl on `Tab` or `TabManager`. The fix is at `Pane` level
  plus explicit drains.

---

## 4. Design

### 4.1 Pane shutdown channel

Each `Pane` now owns the reader thread's `JoinHandle` and a
**shutdown signal**. Add to `src/pane.rs:38-68`:

```rust
pub struct Pane {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    reader_rx: Receiver<Vec<u8>>,
    /// Send `()` to ask the reader thread to exit. Set to `None`
    /// after we've already signalled (idempotent). The thread also
    /// exits naturally on PTY EOF; this just bounds shutdown latency.
    shutdown_tx: Option<mpsc::SyncSender<()>>,
    /// `Some` until the reader thread has been joined. `take()`-d
    /// inside `Drop`.
    reader_handle: Option<std::thread::JoinHandle<()>>,
    parser: vt100::Parser,
    // …existing fields…
}
```

The reader spawn (`src/pane.rs:161-194`) becomes:

```rust
let (data_tx, data_rx) = mpsc::sync_channel::<Vec<u8>>(32);
let (shutdown_tx, shutdown_rx) = mpsc::sync_channel::<()>(1);
let reader_handle = std::thread::Builder::new()
    .name(format!("ezpn-pty-{}", child.process_id().unwrap_or(0)))
    .spawn(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut reader = reader;
            let mut buf = [0u8; 4096];
            loop {
                // Cheap shutdown check between reads. The blocking read
                // below will also unblock when the master fd is dropped
                // (Pane drop closes it), so the only purpose of this
                // signal is to bound shutdown latency when the child is
                // a slow / sleeping process that isn't producing output.
                if shutdown_rx.try_recv().is_ok() {
                    break;
                }
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if data_tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                        wake_main_loop();
                    }
                    Err(_) => break,
                }
            }
        }));
    })
    .expect("ezpn-pty thread spawn");
```

Drop:

```rust
impl Drop for Pane {
    fn drop(&mut self) {
        // 1. Make sure the child is dead.
        if self.alive {
            let _ = self.child.kill();
            self.alive = false;
        }
        // 2. Tell the reader to stop.
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.try_send(());
        }
        // 3. Drop the master fd so the reader's blocking read unblocks
        //    on EOF. We do this by replacing master with a sentinel.
        //    Box<dyn MasterPty> doesn't impl Default, so we use a
        //    `mem::replace` with a freshly-opened tiny PTY *only* if
        //    needed; in practice the field is dropped here implicitly
        //    when Pane drops. Order is: shutdown_tx (signal) → master
        //    (close fd) → handle (join).
        // 4. Bounded join.
        if let Some(handle) = self.reader_handle.take() {
            let join_deadline = Instant::now() + Duration::from_millis(250);
            // std::thread doesn't have a "join with timeout"; emulate
            // by polling `is_finished()` (stable since 1.61). If the
            // thread refuses to die (e.g. blocked in a kernel syscall
            // we can't interrupt on this platform), leak the handle —
            // OS reaps it on process exit. Log once at warn level.
            while !handle.is_finished() && Instant::now() < join_deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                eprintln!(
                    "ezpn: PTY reader thread for pid {} did not exit \
                     within 250ms; leaking handle",
                    self.child.process_id().unwrap_or(0)
                );
                std::mem::forget(handle);
            }
        }
    }
}
```

Note on step 3: dropping `master: Box<dyn MasterPty + Send>` is what
triggers the EOF on the reader. Rust's drop order processes fields
top-to-bottom in declaration order, so we re-order the struct so
`master` drops *before* `reader_handle`:

```rust
pub struct Pane {
    // child first so SIGHUP propagates before the master drops
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    shutdown_tx: Option<mpsc::SyncSender<()>>,
    master: Box<dyn MasterPty + Send>,   // drops here → EOF on reader
    reader_handle: Option<std::thread::JoinHandle<()>>,
    reader_rx: Receiver<Vec<u8>>,
    // …rest…
}
```

### 4.2 `close_pane` prunes restart maps

Update the signature to require the maps:

```rust
pub(crate) fn close_pane(
    layout: &mut Layout,
    panes: &mut HashMap<usize, Pane>,
    active: &mut usize,
    pane_id: usize,
    restart_policies: &mut HashMap<usize, project::RestartPolicy>,
    restart_state: &mut HashMap<usize, (Instant, u32)>,
    zoomed_pane: &mut Option<usize>,
) {
    panes.remove(&pane_id);          // Drop fires here — see §4.1
    layout.remove(pane_id);
    restart_policies.remove(&pane_id);
    restart_state.remove(&pane_id);
    if *zoomed_pane == Some(pane_id) {
        *zoomed_pane = None;
    }
    if *active == pane_id {
        *active = *layout.pane_ids().first().unwrap_or(&0);
    }
}
```

The explicit `pane.kill()` line is removed: dropping the `Pane` from
the map invokes the new `Drop` impl which already kills the child.

### 4.3 `kill_all_inactive` drains panes

Update `src/tab.rs:172-178`:

```rust
pub fn kill_all_inactive(&mut self) {
    for tab in &mut self.tabs {
        // Drain — drop semantics on each Pane do the SIGHUP + reader join.
        for (_, mut pane) in tab.panes.drain() {
            pane.kill(); // explicit for clarity; drop would also do it
        }
        tab.restart_policies.clear();
        tab.restart_state.clear();
        tab.zoomed_pane = None;
    }
    // After this, every inactive tab is empty. We keep the Tab structs
    // so the caller can still iterate metadata (e.g. for a future
    // "show closed tabs" UI); a follow-up may also drain self.tabs.
}
```

Note: we don't drain `self.tabs` itself because the only current
caller (`TabAction::KillSession` at `event_loop.rs:920-929`) returns
immediately afterwards and the whole `TabManager` drops. Draining
inside the loop is forward-compatible with non-terminal callers
(SPEC 08 hooks).

### 4.4 Call-site updates

All callers of `close_pane` need the new arguments. Search hits at the
time of writing:

- `src/app/input_dispatch.rs` — IPC `Close` handler
- `src/daemon/dispatch.rs` — keymap `close pane` / Ctrl+B x
- (anywhere else `close_pane` is called)

Each site already has access to `restart_policies`, `restart_state`,
and `zoomed_pane` in its outer scope (event_loop owns them; the
dispatch functions take them as `&mut`). Plumb through.

---

## 5. Surface changes

### IPC / wire protocol

None. This is an internal hygiene fix.

### CLI (ezpn-ctl)

None.

### Config (TOML)

None.

---

## 6. Touchpoints

| File | Lines (approx) | Change |
|---|---|---|
| `src/pane.rs` | 38–68 | Add `shutdown_tx`, `reader_handle`; reorder fields for drop-order. |
| `src/pane.rs` | 121–218 | Update spawn site; build named thread; capture `shutdown_rx`; store handle. |
| `src/pane.rs` | new | `impl Drop for Pane` with bounded join. |
| `src/pane.rs` | 347–350 | `kill()` leaves drop semantics intact (still SIGHUP-only; full reap is in `Drop`). |
| `src/app/lifecycle.rs` | 362–375 | `close_pane` accepts and prunes restart maps + zoomed_pane. |
| `src/app/lifecycle.rs` | 276–280 | `kill_all_panes` already drains via `panes.drain()`; verify and add a unit test. |
| `src/tab.rs` | 172–178 | `kill_all_inactive` drains `tab.panes` and clears restart maps. |
| `src/app/input_dispatch.rs` | wherever `close_pane` is called | Pass new args. |
| `src/daemon/dispatch.rs` | wherever `close_pane` is called | Pass new args. |
| `src/daemon/event_loop.rs` | call sites of close_pane (via dispatch helpers) | Same. |
| `tests/daemon_lifecycle.rs` | + ~150 LoC | New tests; see §8. |

Approximate net: +200 / −30 LoC.

---

## 7. Migration / backwards-compat

- **Wire protocol**: unchanged.
- **Config**: unchanged.
- **Snapshots**: unchanged. Snapshots only record alive panes; closed
  panes never made it in regardless.
- **Behavioural**: panes now exit faster (bounded 250 ms reader join).
  No user-visible change beyond reduced RSS over time.
- **`close_pane` signature** is `pub(crate)` — only daemon code
  imports it. No external API break.

---

## 8. Test plan

### Unit tests — `src/pane.rs`

- `pane_drop_joins_reader_within_300ms`: spawn a `Pane` running
  `/bin/sleep 60`, drop it, assert the reader thread is gone within
  300 ms (`std::thread::available_parallelism` style sampling, or
  `ps -L`).
- `pane_drop_handles_unkillable_child`: spawn a pane, monkey with the
  child to ignore SIGHUP, drop the pane, assert the handle is leaked
  (warning logged) but the process doesn't deadlock.
- `pane_kill_then_drop_no_double_kill`: call `pane.kill()` explicitly,
  then drop; assert no panic, no second `kill()` call (use a mock).

### Unit tests — `src/app/lifecycle.rs`

- `close_pane_prunes_restart_maps`: build a `panes` map with
  `restart_policies[pid] = Always` and `restart_state[pid] = …`. Call
  `close_pane`. Assert both entries gone and `zoomed_pane` cleared if
  it was `Some(pid)`.
- `close_pane_picks_new_active_when_closing_active`: covered by
  existing tests; re-verify after signature change.

### Unit tests — `src/tab.rs`

- `kill_all_inactive_drains_panes`: build a `TabManager` with two
  inactive tabs each holding 2 panes. Call `kill_all_inactive`.
  Assert each `tab.panes.is_empty()` and each tab's restart maps
  cleared.

### Integration tests — `tests/daemon_lifecycle.rs`

- `pane_open_close_loop_no_thread_growth`: start daemon with a
  workspace, fire 1000 IPC `Split` + `Close` cycles, sample thread
  count from `/proc/<pid>/status` (Linux) or `proc_pidinfo` (macOS).
  Assert delta ≤ 4 across the run. **This is the PRD release criterion.**
- `kill_session_drops_inactive_tab_rss`: spawn a multi-tab session,
  fill each tab's panes with 50 K-line scrollback, fire
  `TabAction::KillSession`. Assert the daemon process exits within
  500 ms (drop-cascade time).
- `restart_state_does_not_grow_after_close`: spawn a pane with
  `RestartPolicy::OnFailure`, kill its child a few times to populate
  `restart_state`, then `Close` it via IPC. Assert `restart_state`
  is empty (verified via debug accessor or by closing+reopening
  rapidly and checking retry-counter resets).

### Soak / perf

- Extend `benches/soak_10min.rs` to include a recurring split/close
  inner loop (one cycle per second). Assert RSS drift ≤ 1 MB and
  thread count delta ≤ 2 over 10 min.

### Manual smoke

1. Start a daemon: `ezpn new --session lc`.
2. In a script: `for i in $(seq 1 1000); do ezpn-ctl split horizontal;
   ezpn-ctl close --pane $(ezpn-ctl list | jq '.panes[-1].id'); done`.
3. `ps -o nlwp= -p $(pgrep -f ezpn-server)` before and after; assert
   delta ≤ 2.
4. `ps -o rss= -p $(pgrep -f ezpn-server)` — should not grow
   meaningfully (< 5 MB delta).

---

## 9. Acceptance criteria

- [ ] `Pane` struct holds `shutdown_tx: Option<SyncSender<()>>` and
      `reader_handle: Option<JoinHandle<()>>`.
- [ ] Field order ensures `master` drops before `reader_handle`.
- [ ] PTY reader thread is named `ezpn-pty-<pid>` for diagnostics.
- [ ] `impl Drop for Pane` signals shutdown, drops master fd, joins
      with a 250 ms deadline, and warns + leaks on timeout.
- [ ] `close_pane` removes entries from `restart_policies`,
      `restart_state`, and clears `zoomed_pane` when it matches.
- [ ] All callers of `close_pane` pass the new arguments.
- [ ] `kill_all_inactive` drains `tab.panes` and clears its restart
      maps.
- [ ] `pane_open_close_loop_no_thread_growth` integration test
      passes (PRD release criterion).
- [ ] No new clippy warnings; `cargo test` green.
- [ ] No new external dep; pure restructuring of existing types.

---

## 10. Risks

| Risk | Mitigation |
|---|---|
| `is_finished()` requires stable Rust 1.61+. | `Cargo.toml` already pins MSRV ≥ 1.74; safe. |
| 250 ms join deadline + `kill_all_panes` of 100 panes ⇒ up to 25 s on shutdown. | Drops happen in parallel logically (each thread is independent), but the join loop is sequential. Mitigation: parallelise the drains in `kill_all_panes` using `std::thread::scope` if soak shows wall-clock issues. Out of scope for v0.10 unless tests fail. |
| Reader thread blocks in `reader.read()` past the master-drop on macOS (where PTY EOF can be delayed by kernel buffering). | The bounded join + warn-and-leak path handles this; the leak is per-pane-close, not per-frame. Acceptable until v0.11 platform-specific shutdown. |
| `mpsc::sync_channel::<()>(1)` for shutdown signal allocates an extra channel per pane. | Negligible (one `Arc<Inner>`); cost dominated by the data channel that already exists. |
| Renaming `close_pane` signature breaks downstream callers in feature branches. | All callers are in-tree; check via `cargo check --workspace` after the change lands. Document the migration in the PR. |
| Test flakiness around 250 ms join window on slow CI. | Allow 500 ms in tests; the hard 250 ms is for production.  |

---

## 11. Open questions

1. Should `Drop` log every leaked thread, or only after N leaks?
   **Default proposal:** log every leak (single line). They should be
   rare; spam means a real bug.
2. Should `close_pane` accept a `&mut Tab` instead of three `&mut
   HashMap` arguments, hiding the field plumbing? **Default
   proposal:** no — the daemon also calls `close_pane` against
   *unpacked* maps owned by `event_loop`'s `run()` locals, and
   building a fake `Tab` just to call the helper is more painful than
   the wider signature.
3. Should `kill_all_inactive` also drain `self.tabs` itself, freeing
   the `Tab` headers? **Default proposal:** no for v0.10 (only call
   site exits immediately); revisit when SPEC 08 (hooks) introduces
   non-exit callers.
4. Should the warning on join timeout be downgraded after the first
   occurrence per pane to avoid log flood on a stuck PTY? **Default
   proposal:** the warning fires exactly once per pane (in `Drop`,
   which runs once); no de-dup needed.
