//! Session naming, discovery, and server process spawning.

use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::protocol;

/// Runtime directory for session sockets.
fn runtime_dir() -> PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

/// Socket path for a named session.
pub fn socket_path(name: &str) -> PathBuf {
    runtime_dir().join(format!("ezpn-session-{}.sock", name))
}

/// Probe if a session socket is alive by sending C_PING and waiting for S_PONG.
/// This does NOT trigger client detach on the server side.
fn is_alive(path: &std::path::Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(path) else {
        return false;
    };
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .ok();
    stream
        .set_write_timeout(Some(Duration::from_millis(200)))
        .ok();
    if protocol::write_msg(&mut stream, protocol::C_PING, &[]).is_err() {
        return false;
    }
    matches!(protocol::read_msg(&mut stream), Ok((protocol::S_PONG, _)))
}

/// Auto-generate a session name from the current directory.
pub fn auto_name() -> String {
    let base = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "default".to_string());

    // Sanitize: only keep alphanumeric, dash, underscore, dot
    let base: String = base
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();

    let path = socket_path(&base);
    if !path.exists() {
        return base;
    }
    for i in 1..100 {
        let name = format!("{}-{}", base, i);
        if !socket_path(&name).exists() {
            return name;
        }
    }
    format!("{}-{}", base, std::process::id())
}

/// List all active sessions. Returns `(name, socket_path)` sorted by mtime (most recent first).
/// Uses C_PING to check liveness without detaching connected clients.
pub fn list() -> Vec<(String, PathBuf)> {
    let dir = runtime_dir();
    let mut sessions = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().into_owned();
            if let Some(name) = fname
                .strip_prefix("ezpn-session-")
                .and_then(|s| s.strip_suffix(".sock"))
            {
                let path = entry.path();
                if is_alive(&path) {
                    sessions.push((name.to_string(), path));
                } else {
                    // Stale socket, clean up
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }

    // Sort by modification time (most recent first)
    sessions.sort_by(|a, b| {
        let a_mtime = std::fs::metadata(&a.1).and_then(|m| m.modified()).ok();
        let b_mtime = std::fs::metadata(&b.1).and_then(|m| m.modified()).ok();
        b_mtime.cmp(&a_mtime)
    });
    sessions
}

/// Find a session by name, or the most recently used if name is None.
pub fn find(name: Option<&str>) -> Option<(String, PathBuf)> {
    if let Some(n) = name {
        let path = socket_path(n);
        if is_alive(&path) {
            return Some((n.to_string(), path));
        }
        return None;
    }
    // Return most recent session (sorted by mtime, first = most recent)
    list().into_iter().next()
}

/// Spawn the server as a detached daemon process.
/// Returns the socket path once the server is ready.
pub fn spawn_server(session_name: &str, original_args: &[String]) -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let sock = socket_path(session_name);

    let mut cmd = Command::new(exe);
    cmd.arg("--server").arg(session_name);
    // Forward original layout/config args
    for arg in original_args {
        cmd.arg(arg);
    }

    // Detach from terminal: new session, null stdio
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    cmd.spawn()?;

    // Wait for the server to create its socket (up to 3 seconds)
    // Use is_alive() to confirm via C_PING without side effects
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if is_alive(&sock) {
            return Ok(sock);
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    anyhow::bail!("server did not start within 3 seconds")
}

/// Clean up the session socket for this session.
pub fn cleanup(name: &str) {
    let _ = std::fs::remove_file(socket_path(name));
}
