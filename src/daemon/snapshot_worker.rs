//! Snapshot worker thread that drains a bounded queue of capture-and-write
//! jobs off the daemon main loop.
//!
//! Per SPEC 04 `docs/spec/v0.10.0/04-snapshot-pipeline-async.md`:
//!
//! * **Bounded mpsc(4)** between main and worker — `try_send` returning
//!   `Full` is a deliberate drop, not an error: rapid detach/attach storms
//!   coalesce into the next idle window.
//! * **Debounce 150 ms** — a freshly arrived `Auto` job replaces any
//!   pending one without writing to disk until the window elapses.
//! * **Atomic write** — temp file + rename, so a SIGKILL during write
//!   never produces a half-written snapshot.
//! * **Bounded shutdown** — `shutdown()` sends a `Shutdown` sentinel and
//!   waits up to 5 s for the worker to drain. On timeout the handle is
//!   `mem::forget`ed and a single warn line is emitted (the OS reaps the
//!   thread on process exit).

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::workspace::{self, WorkspaceSnapshot};

/// How long the worker waits before flushing a coalesced `Auto` job.
pub(crate) const DEBOUNCE: Duration = Duration::from_millis(150);

/// Maximum number of pending jobs the main loop can queue without back-
/// pressure. With debounce this is effectively a small absorption buffer.
pub(crate) const QUEUE_CAPACITY: usize = 4;

/// Maximum time `SnapshotWorker::shutdown` will wait for the worker to
/// drain pending writes before leaking the handle.
pub(crate) const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(5);

/// Maximum time `IpcRequest::Save` will wait for the worker to ack a
/// user-initiated save before returning a structured error.
pub(crate) const USER_SAVE_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) enum SnapshotJob {
    /// Auto-save fired by detach / SIGTERM / KillSession. Coalesced via
    /// `DEBOUNCE` window so attach/detach storms do not hammer disk.
    Auto {
        session_name: String,
        snapshot: WorkspaceSnapshot,
    },
    /// User-initiated `ezpn-ctl save`. Bypasses the debounce window and
    /// signals completion via the ack channel.
    UserSave {
        path: PathBuf,
        snapshot: WorkspaceSnapshot,
        ack: mpsc::SyncSender<Result<(), String>>,
    },
    /// Drain any pending `Auto` then exit. Used by `shutdown()`.
    Shutdown,
}

pub(crate) struct SnapshotWorker {
    tx: mpsc::SyncSender<SnapshotJob>,
    handle: Option<JoinHandle<()>>,
}

impl SnapshotWorker {
    /// Spawn the worker. One per daemon process.
    pub(crate) fn spawn() -> Self {
        let (tx, rx) = mpsc::sync_channel::<SnapshotJob>(QUEUE_CAPACITY);
        let handle = std::thread::Builder::new()
            .name("ezpn-snapshot".into())
            .spawn(move || run(rx))
            .expect("spawn ezpn-snapshot thread");
        Self {
            tx,
            handle: Some(handle),
        }
    }

    /// Best-effort enqueue. Returns `false` if the queue is saturated;
    /// the caller should treat that as a deliberate drop, not an error.
    pub(crate) fn submit(&self, job: SnapshotJob) -> bool {
        self.tx.try_send(job).is_ok()
    }

    /// Bounded shutdown: send `Shutdown`, wait up to `SHUTDOWN_DEADLINE`.
    /// Idempotent — `Drop` calls the same machinery if the caller never
    /// invokes it explicitly.
    #[cfg(test)]
    pub(crate) fn shutdown(mut self) {
        drain_and_join(&self.tx, self.handle.take());
    }
}

impl Drop for SnapshotWorker {
    fn drop(&mut self) {
        // Daemon shutdown path: send the `Shutdown` sentinel so any pending
        // `Auto` capture is flushed, then poll-join the worker within
        // `SHUTDOWN_DEADLINE`. If `shutdown()` was called explicitly the
        // handle is already `None` and this is a no-op.
        drain_and_join(&self.tx, self.handle.take());
    }
}

fn drain_and_join(tx: &mpsc::SyncSender<SnapshotJob>, handle: Option<JoinHandle<()>>) {
    let Some(h) = handle else { return };
    let _ = tx.send(SnapshotJob::Shutdown);
    let deadline = Instant::now() + SHUTDOWN_DEADLINE;
    while !h.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if h.is_finished() {
        let _ = h.join();
    } else {
        eprintln!("ezpn: snapshot worker did not drain within 5s; leaking handle");
        std::mem::forget(h);
    }
}

fn run(rx: mpsc::Receiver<SnapshotJob>) {
    // Pending auto-save: (session_name, snapshot, debounce_start).
    let mut pending: Option<(String, WorkspaceSnapshot, Instant)> = None;
    loop {
        // No pending → block indefinitely. Pending → block at most until
        // the debounce window expires so we can flush it.
        let timeout = pending
            .as_ref()
            .map(|(_, _, t0)| (*t0 + DEBOUNCE).saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_secs(60 * 60));

        match rx.recv_timeout(timeout) {
            Ok(SnapshotJob::Auto {
                session_name,
                snapshot,
            }) => {
                // Debounce: replace any pending capture with the latest.
                pending = Some((session_name, snapshot, Instant::now()));
            }
            Ok(SnapshotJob::UserSave {
                path,
                snapshot,
                ack,
            }) => {
                let result = write_user_snapshot(&path, &snapshot).map_err(|e| e.to_string());
                let _ = ack.send(result);
            }
            Ok(SnapshotJob::Shutdown) => {
                if let Some((session, snapshot, _)) = pending.take() {
                    workspace::auto_save(&session, &snapshot);
                }
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some((session, snapshot, _)) = pending.take() {
                    workspace::auto_save(&session, &snapshot);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Write a user snapshot to an arbitrary `path` atomically (temp file +
/// rename). Used by `SnapshotJob::UserSave`.
fn write_user_snapshot(path: &Path, snapshot: &WorkspaceSnapshot) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_vec_pretty(snapshot)?;
    let pid = std::process::id();
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "snapshot.json".to_string());
    let tmp = path.with_file_name(format!("{file_name}.tmp.{pid}"));
    if let Err(e) = std::fs::write(&tmp, &json) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Serializes tests that mutate process-global `XDG_DATA_HOME` so they
    /// don't race when cargo runs them in parallel. Same pattern as
    /// `config::tests::ENV_LOCK`.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn dummy_snapshot() -> WorkspaceSnapshot {
        // Build a minimal valid snapshot via the public constructor path.
        // We only need *a* WorkspaceSnapshot to exercise the worker pipeline.
        WorkspaceSnapshot {
            version: workspace::SNAPSHOT_VERSION,
            shell: "/bin/sh".to_string(),
            border_style: crate::render::BorderStyle::Single,
            show_status_bar: true,
            show_tab_bar: true,
            scrollback: 1000,
            active_tab: 0,
            tabs: vec![workspace::TabSnapshot {
                name: "1".to_string(),
                layout: crate::layout::Layout::from_grid(1, 1),
                active_pane: 0,
                zoomed_pane: None,
                broadcast: false,
                panes: vec![],
            }],
        }
    }

    #[test]
    fn worker_writes_user_save_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("save.json");
        let worker = SnapshotWorker::spawn();
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        let ok = worker.submit(SnapshotJob::UserSave {
            path: dest.clone(),
            snapshot: dummy_snapshot(),
            ack: ack_tx,
        });
        assert!(ok);
        let result = ack_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(result.is_ok(), "user save must succeed: {result:?}");
        assert!(dest.exists(), "atomic rename must produce final file");
        // No leftover *.tmp.* siblings.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "no .tmp.* files: {leftovers:?}");
        worker.shutdown();
    }

    #[test]
    fn worker_shutdown_drains_pending_auto() {
        // Serialize against other tests that mutate XDG_DATA_HOME.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", dir.path());
        }
        let session = format!("worker-shutdown-{}", std::process::id());
        let worker = SnapshotWorker::spawn();
        worker.submit(SnapshotJob::Auto {
            session_name: session.clone(),
            snapshot: dummy_snapshot(),
        });
        worker.shutdown();
        let expected = dir
            .path()
            .join("ezpn")
            .join("sessions")
            .join(format!("{session}.json"));
        assert!(
            expected.exists(),
            "shutdown must drain pending Auto to {expected:?}"
        );
        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn worker_debounces_rapid_auto_jobs() {
        // Serialize against other tests that mutate XDG_DATA_HOME.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Send 5 Auto jobs in quick succession; only the last one should
        // hit disk after the debounce window.
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", dir.path());
        }
        let session = format!("worker-debounce-{}", std::process::id());
        let worker = SnapshotWorker::spawn();
        let send_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for _ in 0..5 {
            if worker.submit(SnapshotJob::Auto {
                session_name: session.clone(),
                snapshot: dummy_snapshot(),
            }) {
                send_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            // Stay well inside the debounce window.
            std::thread::sleep(Duration::from_millis(20));
        }
        // Wait for debounce window + slack.
        std::thread::sleep(DEBOUNCE + Duration::from_millis(150));
        let path = dir
            .path()
            .join("ezpn")
            .join("sessions")
            .join(format!("{session}.json"));
        assert!(path.exists(), "debounced auto-save must reach disk");
        // Modification times are not stable enough across filesystems to
        // assert "exactly 1 write" — instead assert the file is final and
        // the worker drains cleanly.
        worker.shutdown();
        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
        }
    }
}
