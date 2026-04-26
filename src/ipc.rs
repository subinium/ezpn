use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Number of worker threads serving IPC requests. See SPEC 01
/// `docs/spec/v0.10.0/01-daemon-io-resilience.md` §4.2.
pub(crate) const IPC_POOL_SIZE: usize = 4;

/// Maximum number of pending accepted-but-not-handled connections. When
/// the queue saturates, new accepts are rejected with a structured
/// `IpcResponse::error("ezpn ipc pool saturated; retry")` and closed.
pub(crate) const IPC_QUEUE_CAPACITY: usize = 16;

/// Per-connection read timeout. A hostile/buggy `ezpn-ctl` that opens
/// the socket and never sends a request is reaped after this interval.
pub(crate) const IPC_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Per-connection write timeout. Bounds time spent sending one
/// `IpcResponse` on a misbehaving peer.
pub(crate) const IPC_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Maximum size of a single `send-keys` payload (sum of all token bytes).
/// Mirrors the existing 16 MiB protocol payload cap from `protocol.rs` so a
/// hostile script cannot flood a pane in one IPC round-trip. Per SPEC 06 §10.
pub(crate) const SEND_KEYS_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Maximum number of token entries in a single `send-keys` payload. The byte
/// cap (`SEND_KEYS_MAX_BYTES`) does not bound element count: a hostile peer
/// could submit `keys = ["", "", … 100M …]` (sum = 0) and force a 100M-entry
/// `Vec<String>` allocation during JSON parse. 4096 is generous (full
/// keyboard macros run hundreds of tokens at most) and safely caps memory.
pub(crate) const SEND_KEYS_MAX_TOKENS: usize = 4096;

/// Per-line byte cap for newline-delimited JSON requests on the IPC socket.
/// Without this, a hostile peer that opens the socket and writes 1 GiB
/// without a `\n` would force `BufRead::read_line` to grow its `String`
/// buffer to 1 GiB before returning. The cap is one order of magnitude
/// over `SEND_KEYS_MAX_BYTES` so legitimate clients are never truncated.
pub(crate) const IPC_MAX_LINE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

/// Where a SPEC 06 `send-keys` payload should land. The enum keeps the wire
/// format forward-compatible with future targeting modes (by-name, by-index,
/// cross-tab) without breaking existing CLI consumers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaneTarget {
    /// Numeric pane ID as shown by `ezpn-ctl list`.
    Id { value: usize },
    /// The active pane on the active tab — resolved server-side at dispatch
    /// time, so there is no race between resolution and delivery.
    Current,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IpcRequest {
    Split {
        direction: SplitDirection,
        pane: Option<usize>,
    },
    Close {
        pane: usize,
    },
    Focus {
        pane: usize,
    },
    Equalize,
    List,
    Layout {
        spec: String,
    },
    Exec {
        pane: usize,
        command: String,
    },
    Save {
        path: String,
    },
    Load {
        path: String,
    },
    /// Drop scrollback above the visible screen for a single pane. SPEC 02.
    ClearHistory {
        pane: usize,
    },
    /// Resize a pane's scrollback ring. Capped against
    /// `EzpnConfig::scrollback_max_lines`. SPEC 02.
    SetHistoryLimit {
        pane: usize,
        lines: usize,
    },
    /// Deliver a sequence of keystrokes / text into a pane's PTY write half.
    /// Per SPEC 06: tokens are concatenated with no separator between them,
    /// and parsed via `keymap::keyspec` unless `literal` is set (in which
    /// case the bytes are written verbatim). Wire field uses a serde-default
    /// for `literal` so older `ezpn-ctl` builds that omit the flag still
    /// deserialize cleanly.
    SendKeys {
        target: PaneTarget,
        keys: Vec<String>,
        #[serde(default)]
        literal: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneInfo {
    pub index: usize,
    pub id: usize,
    pub cols: u16,
    pub rows: u16,
    pub alive: bool,
    pub active: bool,
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub panes: Option<Vec<PaneInfo>>,
}

impl IpcResponse {
    pub fn success(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: Some(message.into()),
            error: None,
            panes: None,
        }
    }

    pub fn with_panes(panes: Vec<PaneInfo>) -> Self {
        Self {
            ok: true,
            message: None,
            error: None,
            panes: Some(panes),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: None,
            error: Some(message.into()),
            panes: None,
        }
    }
}

pub type ResponseSender = std::sync::mpsc::SyncSender<IpcResponse>;

pub fn socket_path() -> PathBuf {
    socket_path_for_pid(std::process::id())
}

pub fn socket_path_for_pid(pid: u32) -> PathBuf {
    // Prefer XDG_RUNTIME_DIR (per-user, mode 0700) over /tmp
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(format!("{}/ezpn-{}.sock", dir, pid))
}

pub fn start_listener() -> anyhow::Result<mpsc::Receiver<(IpcRequest, ResponseSender)>> {
    let path = socket_path();
    let _ = std::fs::remove_file(&path);

    let listener = UnixListener::bind(&path)?;

    // Restrict socket to owner only (0o600)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    let (tx, rx) = mpsc::channel();

    // Spawn a fixed worker pool to handle accepted connections. Per SPEC 01
    // §4.2, this caps the daemon's IPC thread budget regardless of how many
    // clients connect — eliminating the unbounded `spawn-per-connection`
    // leak that prior versions exhibited.
    let pool = spawn_ipc_pool(tx);

    std::thread::spawn(move || {
        // Outer accept loop must survive any per-client panic; otherwise the
        // ezpn-ctl IPC interface dies after one malformed request.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        // Hand off to the worker pool. If the queue is full
                        // we reject the connection with a structured error
                        // rather than spawning an unbounded thread.
                        if let Err(crossbeam_channel::TrySendError::Full(mut s)) =
                            pool.work_tx.try_send(stream)
                        {
                            let resp = IpcResponse::error("ezpn ipc pool saturated; retry");
                            let _ = write_response(&mut s, &resp);
                            drop(s);
                        }
                    }
                    Err(_) => break,
                }
            }
            // Acceptor exiting — drop the work channel so workers also exit.
            drop(pool);
        }));
    });

    Ok(rx)
}

/// Bounded worker pool serving accepted IPC connections.
struct IpcPool {
    work_tx: crossbeam_channel::Sender<UnixStream>,
    /// Worker join handles. Kept solely to extend their lifetime to the
    /// pool's; not joined explicitly because the daemon process exit
    /// cleans up all threads.
    _workers: Vec<std::thread::JoinHandle<()>>,
}

fn spawn_ipc_pool(cmd_tx: mpsc::Sender<(IpcRequest, ResponseSender)>) -> IpcPool {
    let (work_tx, work_rx) = crossbeam_channel::bounded::<UnixStream>(IPC_QUEUE_CAPACITY);
    let mut workers = Vec::with_capacity(IPC_POOL_SIZE);
    for worker_id in 0..IPC_POOL_SIZE {
        let rx = work_rx.clone();
        let cmd_tx = cmd_tx.clone();
        let handle = std::thread::Builder::new()
            .name(format!("ezpn-ipc-{worker_id}"))
            .spawn(move || {
                while let Ok(stream) = rx.recv() {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let _ = stream.set_read_timeout(Some(IPC_READ_TIMEOUT));
                        let _ = stream.set_write_timeout(Some(IPC_WRITE_TIMEOUT));
                        handle_client(stream, cmd_tx.clone());
                    }));
                    if let Err(payload) = result {
                        eprintln!(
                            "ezpn-ipc: worker {worker_id} panicked: {}",
                            panic_payload_to_string(&payload)
                        );
                    }
                }
            })
            .expect("spawn ezpn-ipc worker thread");
        workers.push(handle);
    }
    IpcPool {
        work_tx,
        _workers: workers,
    }
}

fn panic_payload_to_string(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "unknown panic payload".to_string()
}

pub fn cleanup() {
    let _ = std::fs::remove_file(socket_path());
}

fn handle_client(stream: UnixStream, tx: mpsc::Sender<(IpcRequest, ResponseSender)>) {
    let Ok(read_stream) = stream.try_clone() else {
        return;
    };
    // Cap total bytes read on this connection so a hostile peer cannot
    // OOM the daemon by sending a multi-GiB line without a newline.
    // After the cap the reader EOFs; an in-flight `read_line` returns the
    // partial buffer (no `\n`) which serde_json rejects with a structured
    // error → loop terminates cleanly.
    let limited = read_stream.take(IPC_MAX_LINE_BYTES as u64);
    let reader = BufReader::new(limited);
    let mut writer = stream;

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            // Idle-timeout (read_timeout fired) or short-circuit recv: emit
            // a structured error and close the connection. Per SPEC 01 §4.2,
            // hostile or wedged peers must be reaped within IPC_READ_TIMEOUT
            // rather than holding a worker thread forever.
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                let _ = write_response(&mut writer, &IpcResponse::error("idle timeout"));
                break;
            }
            Err(_) => break,
        };

        if line.trim().is_empty() {
            continue;
        }

        let request = match serde_json::from_str::<IpcRequest>(&line) {
            Ok(request) => request,
            Err(error) => {
                let response = IpcResponse::error(format!("invalid request: {}", error));
                let _ = write_response(&mut writer, &response);
                continue;
            }
        };

        let (resp_tx, resp_rx) = mpsc::sync_channel(1);
        if tx.send((request, resp_tx)).is_err() {
            let response = IpcResponse::error("listener unavailable");
            let _ = write_response(&mut writer, &response);
            break;
        }

        let response = resp_rx
            .recv()
            .unwrap_or_else(|_| IpcResponse::error("internal error"));
        if write_response(&mut writer, &response).is_err() {
            break;
        }
    }
}

fn write_response(writer: &mut UnixStream, response: &IpcResponse) -> anyhow::Result<()> {
    writeln!(writer, "{}", serde_json::to_string(response)?)?;
    writer.flush()?;
    Ok(())
}
