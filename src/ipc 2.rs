use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc;

/// Commands that can be sent to ezpn via IPC.
#[derive(Debug)]
pub enum AppCommand {
    Split {
        direction: String,
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
}

/// Response sent back to the IPC client.
pub struct AppResponse {
    pub ok: bool,
    pub message: String,
}

impl AppResponse {
    pub fn success(msg: &str) -> Self {
        Self {
            ok: true,
            message: msg.to_string(),
        }
    }

    pub fn error(msg: &str) -> Self {
        Self {
            ok: false,
            message: msg.to_string(),
        }
    }

    pub fn to_json(&self) -> String {
        if self.ok {
            format!("{{\"ok\":true,\"message\":\"{}\"}}", self.message)
        } else {
            format!("{{\"ok\":false,\"error\":\"{}\"}}", self.message)
        }
    }
}

/// Get the socket path for this ezpn instance.
pub fn socket_path() -> PathBuf {
    let pid = std::process::id();
    PathBuf::from(format!("/tmp/ezpn-{}.sock", pid))
}

/// Start the IPC listener in a background thread.
/// Returns a receiver for incoming commands.
pub fn start_listener() -> anyhow::Result<mpsc::Receiver<(AppCommand, ResponseSender)>> {
    let path = socket_path();
    // Clean up stale socket
    let _ = std::fs::remove_file(&path);

    let listener = UnixListener::bind(&path)?;
    listener.set_nonblocking(false)?;

    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let tx = tx.clone();
                    std::thread::spawn(move || {
                        handle_client(stream, tx);
                    });
                }
                Err(_) => break,
            }
        }
    });

    Ok(rx)
}

/// Cleanup the socket file on exit.
pub fn cleanup() {
    let _ = std::fs::remove_file(socket_path());
}

pub type ResponseSender = std::sync::mpsc::SyncSender<AppResponse>;

fn handle_client(
    stream: std::os::unix::net::UnixStream,
    tx: mpsc::Sender<(AppCommand, ResponseSender)>,
) {
    let Ok(read_stream) = stream.try_clone() else {
        return;
    };
    let reader = BufReader::new(read_stream);
    let mut writer = stream;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        match parse_command(&line) {
            Some(cmd) => {
                let (resp_tx, resp_rx) = mpsc::sync_channel(1);
                if tx.send((cmd, resp_tx)).is_err() {
                    break;
                }
                let resp = resp_rx
                    .recv()
                    .unwrap_or_else(|_| AppResponse::error("internal error"));
                let json = resp.to_json();
                let _ = writeln!(writer, "{}", json);
                let _ = writer.flush();
            }
            None => {
                let _ = writeln!(writer, "{{\"ok\":false,\"error\":\"unknown command\"}}");
                let _ = writer.flush();
            }
        }
    }
}

/// Parse a simple command string (not JSON, for simplicity).
/// Format: "split horizontal [pane]", "close 2", "focus 1", "equalize", "list",
///         "layout 7:3/5:5", "exec 1 cargo test"
fn parse_command(line: &str) -> Option<AppCommand> {
    let parts: Vec<&str> = line.splitn(3, ' ').collect();
    match parts.first().copied()? {
        "split" => {
            let dir = parts.get(1).unwrap_or(&"horizontal").to_string();
            let pane = parts.get(2).and_then(|s| s.parse().ok());
            Some(AppCommand::Split {
                direction: dir,
                pane,
            })
        }
        "close" => {
            let pane = parts.get(1)?.parse().ok()?;
            Some(AppCommand::Close { pane })
        }
        "focus" => {
            let pane = parts.get(1)?.parse().ok()?;
            Some(AppCommand::Focus { pane })
        }
        "equalize" => Some(AppCommand::Equalize),
        "list" => Some(AppCommand::List),
        "layout" => {
            let spec = parts.get(1)?.to_string();
            Some(AppCommand::Layout { spec })
        }
        "exec" => {
            let pane = parts.get(1)?.parse().ok()?;
            let command = parts.get(2)?.to_string();
            Some(AppCommand::Exec { pane, command })
        }
        _ => None,
    }
}
