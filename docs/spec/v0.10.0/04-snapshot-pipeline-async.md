# SPEC 04 — Async Snapshot Pipeline

**Status:** Draft
**Related issue:** TBD (v0.10.0 milestone)
**PRD:** [v0.10.0](../../prd/v0.10.0.md)
**Category:** A. Stability & Resource Hygiene
**Severity origin:** Audit P2 #6 (synchronous gzip+bincode on detach blocks main loop)

---

## 1. Background

When the last attached client detaches — or the daemon receives
`SIGTERM`, or `ezpn-ctl save` runs — the daemon synchronously walks
every pane in every tab, encodes scrollback into a gzip+bincode blob,
and writes JSON to disk. *All on the main loop*.

`src/daemon/event_loop.rs:722-743` (auto-save on last-client-detach):

```rust
if had_clients
    && clients.is_empty()
    && (!detach_ids.is_empty() || !disconnect_ids.is_empty())
{
    // All clients gone — auto-save and reset input state
    let snapshot = capture_workspace(
        &tab_mgr,
        &tab_name,
        &layout,
        &panes,
        active,
        zoomed_pane,
        broadcast,
        &restart_policies,
        &default_shell,
        settings.border_style,
        settings.show_status_bar,
        settings.show_tab_bar,
        effective_scrollback,
        persist_scrollback,
    );
    workspace::auto_save(session_name, &snapshot);
```

`capture_workspace` (in `src/daemon/snapshot.rs:25-57`) is a thin
wrapper around `WorkspaceSnapshot::from_live`, which calls
`encode_scrollback` on every pane. The encode pipeline lives in
`src/snapshot_blob.rs:46-79`:

```rust
pub fn encode_scrollback(parser: &vt100::Parser) -> String {
    let screen = parser.screen();
    let (_rows, cols) = screen.size();
    if cols == 0 {
        return String::new();
    }

    let mut rows: Vec<Vec<u8>> = screen.rows_formatted(0, cols).collect();

    match try_encode(&rows) {
        Ok(s) if s.len() <= SCROLLBACK_BLOB_MAX_BYTES => s,
        _ => {
            let drop_n = rows.len() / 2;
            rows.drain(0..drop_n);
            // …retry…
        }
    }
}
```

`try_encode` then runs bincode → gzip → base64. For the documented
per-pane cap of 5 MB encoded × N panes, the worst-case CPU time is
hundreds of milliseconds (gzip is the bottleneck; on a Mac M1 it's
roughly 100–300 ms per 1 MB). With 4 panes near the cap and a multi-tab
workspace, the main loop can stall for 1+ seconds at detach time.

The same path also runs at `src/daemon/event_loop.rs:1175-1191`
(SIGTERM handler) and `src/daemon/event_loop.rs:938-959` (IPC `Save`).

The user symptom: rapid attach/detach (someone reattaching after a
network blip) triggers a snapshot every cycle. Today each cycle is
synchronous, so 10 cycles in a second take 10 × snapshot-time and
the daemon is unresponsive throughout. PRD release criterion: **rapid
attach/detach (10 cycles in 1 second) triggers ≤ 2 snapshot writes.**

---

## 2. Goal

Move snapshot encoding off the main loop entirely. Specifically:

1. The main thread only **takes a cheap, immutable view** of pane
   state and ships it to a worker — never blocking on gzip/bincode/IO.
2. A bounded **debounce window** (150 ms) coalesces rapid bursts so
   10 detach cycles in 1 second produce at most 2 disk writes.
3. Disk writes are **atomic** (temp file + rename) so a daemon crash
   mid-write never produces a torn snapshot.
4. On graceful shutdown, the worker drains pending requests with a
   bounded deadline (5 s) before exiting.

PRD release criteria touched: rapid attach/detach debounce (≤ 2 writes
per 10 cycles), 7-day soak (no snapshot leaks), `clear-history`
latency (snapshot work no longer interferes).

---

## 3. Non-goals

- Compressing snapshots in the main thread but writing async (the
  expensive step is gzip — moving only the IO is a half-measure).
- Replacing JSON with a binary format. The on-disk schema stays at
  `v3` (snapshot blob already binary-inside-base64-inside-JSON).
- Per-pane incremental snapshotting. v0.10 still snapshots whole
  workspace; partial incremental updates are v0.11+.
- Encrypted-at-rest snapshots. Out of scope.
- Replacing `flate2` with zstd. Tracked separately if soak shows gzip
  is the bottleneck on huge sessions.

---

## 4. Design

### 4.1 Snapshot worker thread

A single dedicated worker thread, started at daemon bring-up:

```
main loop                                snapshot worker (1 thread)
   ─┬─                                       ─┬─
    │  CaptureRequest { snapshot, dest }      │
    │ ──────────────(bounded mpsc 4)────────► │
    │                                         │  (debounce 150 ms)
    │                                         │  encode_scrollback × panes
    │                                         │  serde_json::to_vec
    │                                         │  write tmp + rename
    │                                         │
    ◄── OK / Err logged on stderr ────────────┘
```

The main thread calls `capture_workspace` in two flavours:

- **Live capture** (cheap): walk panes and produce a
  `WorkspaceSnapshotDraft` containing per-pane *unencoded* row data
  (`Vec<Vec<u8>>` of `rows_formatted` results) plus all metadata. The
  snapshot blob is **not** encoded yet — the worker does that.
- The worker turns the draft into a `WorkspaceSnapshot` (existing
  type) by encoding each draft pane and then writes it.

```rust
// src/daemon/snapshot.rs additions

pub(crate) struct PaneSnapshotDraft {
    pub id: usize,
    pub launch: crate::pane::PaneLaunch,
    pub name: Option<String>,
    pub shell: Option<String>,
    pub cwd: Option<PathBuf>,
    pub env: HashMap<String, String>,
    pub restart: project::RestartPolicy,
    /// Unencoded rows pulled via `screen.rows_formatted(0, cols)`.
    /// Worker calls `try_encode(&rows)` to produce the final blob.
    pub rows: Option<Vec<Vec<u8>>>,
    pub size: (u16, u16),
}

pub(crate) struct TabSnapshotDraft {
    pub name: String,
    pub layout: Layout,
    pub panes: Vec<PaneSnapshotDraft>,
    pub active_pane: usize,
    pub zoomed_pane: Option<usize>,
    pub broadcast: bool,
}

pub(crate) struct WorkspaceSnapshotDraft {
    pub tabs: Vec<TabSnapshotDraft>,
    pub active_tab: usize,
    pub shell: String,
    pub border_style: BorderStyle,
    pub show_status_bar: bool,
    pub show_tab_bar: bool,
    pub scrollback: usize,
}

pub(crate) fn capture_draft(
    tab_mgr: &TabManager,
    tab_name: &str,
    layout: &Layout,
    panes: &HashMap<usize, Pane>,
    active: usize,
    zoomed_pane: Option<usize>,
    broadcast: bool,
    restart_policies: &HashMap<usize, project::RestartPolicy>,
    default_shell: &str,
    border_style: BorderStyle,
    show_status_bar: bool,
    show_tab_bar: bool,
    effective_scrollback: usize,
    persist_scrollback: bool,
) -> WorkspaceSnapshotDraft;
```

The cost of `rows_formatted(0, cols).collect()` is ~one allocation per
visible row plus a memcpy per cell. For 80×24 it's microseconds; for a
500×500 pane it's ~1 ms. Either way it's *bounded* and proportional to
the visible screen, not the scrollback. Per-pane scrollback encoding
(the actual bottleneck) stays on the worker.

### 4.2 Worker thread

`src/daemon/snapshot_worker.rs` (new file):

```rust
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(150);
const QUEUE_CAPACITY: usize = 4;

pub(crate) enum SnapshotJob {
    /// Persist a workspace draft. `dest` is the *final* path; the worker
    /// writes to `dest.with_extension("json.tmp")` and renames atomically.
    Auto {
        session_name: String,
        draft: WorkspaceSnapshotDraft,
    },
    UserSave {
        path: PathBuf,
        draft: WorkspaceSnapshotDraft,
        ack: mpsc::SyncSender<Result<(), String>>,
    },
    /// Drain queue, write last pending Auto, then exit.
    Shutdown,
}

pub(crate) struct SnapshotWorker {
    tx: mpsc::SyncSender<SnapshotJob>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl SnapshotWorker {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::sync_channel::<SnapshotJob>(QUEUE_CAPACITY);
        let handle = std::thread::Builder::new()
            .name("ezpn-snapshot".into())
            .spawn(move || run_worker(rx))
            .expect("snapshot worker spawn");
        Self { tx, handle: Some(handle) }
    }

    /// Best-effort enqueue. Returns false if the queue is full (rare:
    /// only when 4 captures back up faster than the worker can encode
    /// one). The dropped capture is acceptable — debounce already
    /// allows coalescing.
    pub fn submit(&self, job: SnapshotJob) -> bool {
        self.tx.try_send(job).is_ok()
    }

    /// Bounded shutdown: send Shutdown sentinel and wait up to 5 s for
    /// the worker to drain. Called from SIGTERM / KillSession paths.
    pub fn shutdown(mut self) {
        let _ = self.tx.send(SnapshotJob::Shutdown);
        if let Some(h) = self.handle.take() {
            // is_finished poll loop with 5 s deadline.
            let deadline = Instant::now() + Duration::from_secs(5);
            while !h.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            if h.is_finished() {
                let _ = h.join();
            } else {
                eprintln!("ezpn: snapshot worker did not drain within 5s; leaking");
                std::mem::forget(h);
            }
        }
    }
}
```

The worker loop:

```rust
fn run_worker(rx: mpsc::Receiver<SnapshotJob>) {
    let mut pending_auto: Option<(String, WorkspaceSnapshotDraft, Instant)> = None;
    loop {
        // Two modes:
        //   - No pending: block until a job arrives.
        //   - Pending: wait at most until `pending.deadline` so the
        //     debounce window can elapse.
        let timeout = pending_auto
            .as_ref()
            .map(|(_, _, t0)| (*t0 + DEBOUNCE).saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_secs(60 * 60)); // effectively infinite

        match rx.recv_timeout(timeout) {
            Ok(SnapshotJob::Auto { session_name, draft }) => {
                // Debounce: replace any pending auto with the latest.
                pending_auto = Some((session_name, draft, Instant::now()));
            }
            Ok(SnapshotJob::UserSave { path, draft, ack }) => {
                // Explicit user saves are not debounced — they have an ack channel.
                let result = encode_and_write(&path, draft);
                let _ = ack.send(result.map_err(|e| e.to_string()));
            }
            Ok(SnapshotJob::Shutdown) => {
                if let Some((session, draft, _)) = pending_auto.take() {
                    let path = workspace::auto_save_path(&session);
                    let _ = encode_and_write(&path, draft);
                }
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some((session, draft, _)) = pending_auto.take() {
                    let path = workspace::auto_save_path(&session);
                    if let Err(e) = encode_and_write(&path, draft) {
                        eprintln!("ezpn: auto-save failed: {e}");
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn encode_and_write(path: &Path, draft: WorkspaceSnapshotDraft) -> anyhow::Result<()> {
    let snapshot = draft_to_snapshot(draft); // runs encode_scrollback per pane
    let json = serde_json::to_vec_pretty(&snapshot)?;
    write_atomic(path, &json)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow::anyhow!("no parent"))?;
    std::fs::create_dir_all(parent)?;
    let pid = std::process::id();
    let tmp = path.with_file_name(format!(
        "{}.tmp.{pid}",
        path.file_name().unwrap().to_string_lossy()
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
```

### 4.3 Wiring into the event loop

`src/daemon/event_loop.rs:39` (`run()`) gains:

```rust
let snapshot_worker = crate::daemon::snapshot_worker::SnapshotWorker::spawn();
```

The auto-save site at `:727-743` becomes:

```rust
let draft = crate::daemon::snapshot::capture_draft(
    &tab_mgr, &tab_name, &layout, &panes, active, zoomed_pane,
    broadcast, &restart_policies, &default_shell,
    settings.border_style, settings.show_status_bar,
    settings.show_tab_bar, effective_scrollback, persist_scrollback,
);
let _ = snapshot_worker.submit(crate::daemon::snapshot_worker::SnapshotJob::Auto {
    session_name: session_name.to_string(),
    draft,
});
```

The IPC `Save` handler at `:938-959` becomes:

```rust
let (ack_tx, ack_rx) = mpsc::sync_channel(1);
let draft = capture_draft(/* … */);
if !snapshot_worker.submit(SnapshotJob::UserSave {
    path: PathBuf::from(path),
    draft,
    ack: ack_tx,
}) {
    let _ = resp_tx.send(IpcResponse::error("snapshot worker queue full; retry"));
    continue;
}
// User save is synchronous from the caller's POV — block until ack.
// Worker is not reentrant for a single save; bounded wait of 30 s.
match ack_rx.recv_timeout(Duration::from_secs(30)) {
    Ok(Ok(())) => {
        let _ = resp_tx.send(IpcResponse::success(format!("saved {}", path)));
    }
    Ok(Err(e)) => {
        let _ = resp_tx.send(IpcResponse::error(e));
    }
    Err(_) => {
        let _ = resp_tx.send(IpcResponse::error("snapshot worker timed out"));
    }
}
```

The SIGTERM handler at `:1172-1201` and the `Kill` paths at `:500-510` /
`:644-654` / `:920-929` all change to:

```rust
let draft = capture_draft(/* live state */);
let _ = snapshot_worker.submit(SnapshotJob::Auto {
    session_name: session_name.to_string(),
    draft,
});
snapshot_worker.shutdown(); // bounded 5 s drain
```

Ordering note: shutdown must happen *before* the kill loop, so the
worker has access to live pane state. Today the SIGTERM path snapshots
first then kills panes; the new path is the same order.

### 4.4 Path helpers

A small new helper in `src/workspace.rs`:

```rust
pub fn auto_save_path(session_name: &str) -> PathBuf {
    let dir = auto_save_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    dir.join(format!("{session_name}.json"))
}
```

`auto_save` itself is updated to call `auto_save_path` and use the
shared `write_atomic` helper. Existing tests for `auto_save` continue
to pass since the destination path is unchanged.

---

## 5. Surface changes

### IPC / wire protocol

JSON IPC: unchanged shape. Behaviour change: `IpcRequest::Save` may
now return `IpcResponse::error("snapshot worker queue full; retry")`
or `IpcResponse::error("snapshot worker timed out")`. Both flow
through the existing error string field (`src/ipc.rs:56-94`).

Binary client/server protocol: unchanged.

### CLI (ezpn-ctl)

`ezpn-ctl save <path>` may surface the new error strings. Document in
help text:

```
save <path>    Save the current workspace as a JSON snapshot. Returns
               an error if the snapshot worker is saturated; retry
               after a moment.
```

### Config (TOML)

No new keys for v0.10. A future `[snapshot]` table with `debounce_ms`
is plausible (deferred to v0.11).

---

## 6. Touchpoints

| File | Lines (approx) | Change |
|---|---|---|
| `src/daemon/snapshot.rs` | 25–57 | Keep `capture_workspace` for back-compat (only used by tests after migration); add `capture_draft` + draft types. |
| `src/daemon/snapshot_worker.rs` | new ~220 LoC | Worker thread, debounce, atomic write. |
| `src/daemon/mod.rs` | + 1 | `pub(crate) mod snapshot_worker;` |
| `src/daemon/event_loop.rs` | 39–248 | Spawn `SnapshotWorker` near other init. |
| `src/daemon/event_loop.rs` | 727–743 | Auto-save site → `submit(SnapshotJob::Auto)`. |
| `src/daemon/event_loop.rs` | 938–959 | IPC `Save` → `submit(SnapshotJob::UserSave)` + bounded ack wait. |
| `src/daemon/event_loop.rs` | 500–510, 644–654, 920–929, 1172–1201 | SIGTERM / Kill paths → submit + `worker.shutdown()`. |
| `src/workspace.rs` | + ~30 LoC | `auto_save_path`; `write_atomic` helper (or expose existing tmp+rename code from `config.rs`). |
| `src/snapshot_blob.rs` | unchanged | Encode logic is reused as-is by the worker. |
| `tests/property_snapshot.rs` | + ~80 LoC | Worker round-trip tests; see §8. |
| `tests/daemon_lifecycle.rs` | + ~150 LoC | Debounce + atomicity integration tests. |

Approximate net: +500 / −80 LoC.

---

## 7. Migration / backwards-compat

- **On-disk snapshot schema**: unchanged (still `WorkspaceSnapshot`
  v3). The draft type is purely an in-memory intermediate.
- **Wire protocol**: unchanged.
- **JSON IPC**: additive error strings only.
- **`ezpn-ctl save`**: now genuinely *async-capable* on the daemon
  side, but the IPC is still synchronous from the caller's POV (waits
  for ack). 30-second timeout is well above any realistic encode time.
- **Crash safety**: improved. Mid-write crashes leave a `*.tmp.<pid>`
  file behind that the next daemon startup can clean up (a small
  cleanup pass in `auto_save_dir` resolution; track separately if
  noisy in practice).

---

## 8. Test plan

### Unit tests — `src/daemon/snapshot_worker.rs`

- `worker_debounces_within_window`: spawn worker, fire 5
  `SnapshotJob::Auto` in 100 ms (well inside debounce). Assert exactly
  1 file write occurred (file mtime sampled before/after).
- `worker_does_not_debounce_user_save`: fire 3 `SnapshotJob::UserSave`
  to distinct paths; assert all 3 files exist with the correct content.
- `worker_atomic_write_no_torn_file`: kill the worker mid-encode by
  sending `Shutdown` with a queue full of `Auto`; assert no `.tmp.*`
  files survive (or, if they do, the final `*.json` is either complete
  or absent — never half-written).
- `worker_shutdown_drains_pending_auto`: enqueue an `Auto`, immediately
  call `shutdown()`. Assert the auto-save file exists and parses.
- `worker_shutdown_bounded`: simulate a stuck encode (using a
  `vt100::Parser` with very large scrollback in test mode). Assert
  `shutdown()` returns within 5 s + small slack, with the
  "did not drain within 5s" warning logged.

### Unit tests — `src/daemon/snapshot.rs`

- `capture_draft_includes_all_panes`: build a fake state with 3 tabs,
  4 panes each. Assert the draft contains exactly 12 pane entries with
  correct ids.
- `draft_to_snapshot_matches_legacy_capture_workspace`: capture a
  draft and convert; capture the same state through the old
  `capture_workspace`. Assert the resulting `WorkspaceSnapshot` is
  byte-identical (modulo deterministic field order).

### Integration tests — `tests/daemon_lifecycle.rs`

- `rapid_attach_detach_coalesces_writes`: spawn daemon. Fire 10 attach
  → detach cycles in 1 second. Sample the auto-save file mtime; assert
  ≤ 2 distinct mtimes were written. **PRD release criterion.**
- `main_loop_responsive_during_huge_snapshot`: build a workspace with
  4 panes filled to 50 K lines of scrollback each. Trigger detach.
  From a *concurrent* fast client, send keystrokes; measure
  round-trip latency. Assert median ≤ 16 ms during the snapshot.
- `daemon_crash_during_save_no_torn_file`: SIGKILL the daemon during
  a `Save` IPC. Restart. Assert the snapshot file is either the
  previous version or absent — never half-written.

### Soak / perf

- `benches/snapshot_encode.rs`: criterion bench of `capture_draft +
  draft_to_snapshot + atomic write` for 1, 4, 16 panes at 1 K, 10 K,
  50 K lines. Goal: 4 panes × 10 K lines completes in ≤ 250 ms on M1.

### Manual smoke

1. Start a session with 4 panes; let each fill some scrollback.
2. Run `attach && detach` 10 times in 1 second:
   `for i in 1 2 3 4 5 6 7 8 9 10; do ezpn a -- soak < /dev/null & sleep 0.1;
    kill %1; done`.
3. `stat -f %m ~/.local/share/ezpn/soak.json` (macOS) — confirm the
   mtime changed at most twice.
4. While the snapshot is encoding (use a 100K-line pane to pad
   timing), ssh into another terminal, attach, and type — confirm
   responsive.

---

## 9. Acceptance criteria

- [ ] `SnapshotWorker` spawned at daemon bring-up; one named thread
      `ezpn-snapshot`.
- [ ] `capture_draft` returns within ≤ 5 ms for typical workspaces
      (4 panes × 80×24).
- [ ] Auto-save path uses bounded `mpsc::sync_channel(4)` and
      `try_send` (drops on overflow are acceptable; debounce will
      coalesce on the next idle).
- [ ] Debounce window of 150 ms coalesces rapid `Auto` jobs.
- [ ] User-initiated `Save` jobs bypass debounce and surface result
      via an ack channel.
- [ ] Atomic write (tmp file + rename) used for both auto-save and
      user save.
- [ ] SIGTERM, `Kill` IPC, and `KillSession` paths submit a final
      `Auto` then call `worker.shutdown()` (5 s bounded drain).
- [ ] PRD: 10 attach/detach cycles in 1 s produce ≤ 2 disk writes.
- [ ] PRD: median local-input latency ≤ 16 ms during a heavy snapshot.
- [ ] Snapshot v3 on-disk schema unchanged.
- [ ] No new clippy warnings; `cargo test` green.
- [ ] PROTOCOL_VERSION unchanged.

---

## 10. Risks

| Risk | Mitigation |
|---|---|
| `capture_draft` allocates one `Vec<Vec<u8>>` per pane (visible rows). For 16 panes × 24 rows × 200 cols ≈ 75 KB per draft. | Negligible vs. the encode pipeline cost; freed as soon as the worker consumes the draft. |
| Worker queue full → auto-save dropped silently. User loses 150 ms of state. | This is the *correct* behaviour: rapid detach/attach storms shouldn't hammer disk. The dropped capture is *not* the only one; the next idle still triggers an auto-save. Documented. |
| User `Save` stuck for 30 s if worker is genuinely overloaded. | Bounded; returns `IpcResponse::error("snapshot worker timed out")`. Normal saves complete in < 1 s. |
| SIGTERM during a long encode delays daemon exit by up to 5 s. | Acceptable — graceful shutdown trade-off. Hard kill (SIGKILL) bypasses entirely. |
| Atomic rename across mount points fails on Linux. | `auto_save_dir` always uses `~/.local/share/ezpn/` which is on the same fs as the tmp; the `write_atomic` helper writes the tmp file in the same parent dir as the final path. |
| `*.tmp.<pid>` files leak if daemon SIGKILLed mid-write. | Add a one-line cleanup in `auto_save_dir` resolution: scan for `*.tmp.*` siblings older than 1 hour and unlink. Optional follow-up; not required for v0.10 acceptance. |
| Worker thread holds references to large `Vec<Vec<u8>>` row buffers; if many drafts accumulate, RSS spikes. | Queue capacity 4 caps this. Worst case: 4 × 16 panes × 24 rows × 200 cols ≈ 300 KB. |
| Tests using filesystem timing (`mtime`) flake on filesystems with second-resolution mtime. | Use `std::fs::metadata().modified()` which on macOS/Linux is nanosecond-resolution; assert *count* of distinct mtimes, not absolute timing. |

---

## 11. Open questions

1. Should the worker queue be size 4 or size 1? **Default proposal:**
   4. Size 1 is too aggressive — a debounce-then-encode-then-arrive
   sequence could miss the latest state.
2. Should `Auto` jobs *replace* a pending auto for a different session
   (multi-session daemon)? **Default proposal:** v0.10 ezpn is
   single-session per daemon (one socket per pid); the question is
   moot. Revisit when multi-session daemons land.
3. Should we proactively clean stale `*.tmp.<pid>` files on startup?
   **Default proposal:** yes, one-line scan in `auto_save_dir`. Track
   as a small follow-up if it bloats this SPEC.
4. Should `ezpn-ctl save` gain a `--async` flag that returns
   immediately without waiting for the ack? **Default proposal:** no
   for v0.10. The ack is what makes the command useful in scripts.
   Consider for v0.11 if a real workflow needs fire-and-forget.
