use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Horizontal,
    Vertical,
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

    std::thread::spawn(move || {
        // Outer accept loop must survive any per-client panic; otherwise the
        // ezpn-ctl IPC interface dies after one malformed request.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let tx = tx.clone();
                        std::thread::spawn(move || {
                            // Per-client panics never propagate.
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    handle_client(stream, tx);
                                }));
                            if let Err(payload) = result {
                                let reason = panic_payload_to_string(&payload);
                                eprintln!("ezpn-ctl: handler thread panicked: {}", reason);
                            }
                        });
                    }
                    Err(_) => break,
                }
            }
        }));
    });

    Ok(rx)
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
    let reader = BufReader::new(read_stream);
    let mut writer = stream;

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
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
