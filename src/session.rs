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

    // First choice: bare directory name (e.g. "myproject")
    if !socket_path(&base).exists() {
        return base;
    }

    // Collision: use short timestamp suffix (e.g. "myproject-1422" from HH:MM)
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let hhmm = format!("{:02}{:02}", (now / 3600) % 24, (now / 60) % 60);
    let name = format!("{}-{}", base, hhmm);
    if !socket_path(&name).exists() {
        return name;
    }

    // Rare: same minute, add seconds
    let name = format!("{}-{}{:02}", base, hhmm, now % 60);
    if !socket_path(&name).exists() {
        return name;
    }

    // Fallback: PID
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

/// Environment variable used to hand the daemon the write end of a
/// parent-owned pipe. The daemon writes one byte after `UnixListener::bind`
/// succeeds; the parent `poll(2)`s for that byte instead of polling the
/// socket every 50 ms. See [`spawn_server`].
pub const READY_FD_ENV: &str = "EZPN_READY_FD";

/// Spawn the server as a detached daemon process.
/// Returns the socket path once the server is ready.
///
/// Uses an inherited pipe ([`READY_FD_ENV`]) for the ready signal, so warm
/// attach completes within a few milliseconds instead of waking up every
/// 50 ms to probe for a socket file (issue #13). The 3 s ceiling is kept as
/// a hard upper bound — if the daemon panics before binding, `poll(2)`
/// times out and we surface a clear error.
pub fn spawn_server(session_name: &str, original_args: &[String]) -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let sock = socket_path(session_name);

    // Create a pipe(2). Parent keeps `read_fd`, hands `write_fd` to the
    // child via env. We deliberately do NOT mark `write_fd` CLOEXEC because
    // it must survive the child's exec — the child needs to inherit it,
    // close it after the bind succeeds, and that close on the write end is
    // what wakes the parent's poll().
    let mut fds = [0i32; 2];
    let pipe_ok = unsafe { libc::pipe(fds.as_mut_ptr()) } == 0;
    let (read_fd, write_fd) = if pipe_ok { (fds[0], fds[1]) } else { (-1, -1) };
    if pipe_ok {
        // FD_CLOEXEC on the read end so it doesn't leak into other children
        // we spawn later in this process.
        unsafe {
            let flags = libc::fcntl(read_fd, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(read_fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }

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

    if pipe_ok {
        cmd.env(READY_FD_ENV, write_fd.to_string());
    }

    let captured_write_fd = if pipe_ok { write_fd } else { -1 };
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            // Strip CLOEXEC from the write end so it survives `exec(2)`.
            // Std `Command` sets CLOEXEC on every parent fd by default.
            if captured_write_fd >= 0 {
                let flags = libc::fcntl(captured_write_fd, libc::F_GETFD);
                if flags >= 0 {
                    libc::fcntl(captured_write_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                }
            }
            Ok(())
        });
    }

    let _child = cmd.spawn()?;

    // Parent no longer needs the write end — closing it means an early
    // crash in the child (before bind) collapses the pipe and `poll`
    // returns POLLHUP immediately rather than blocking the full 3 s.
    if pipe_ok {
        unsafe {
            libc::close(write_fd);
        }
    }

    if pipe_ok {
        // Block until the daemon signals "bind succeeded" or 3 s elapses.
        let mut pfd = libc::pollfd {
            fd: read_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, 3000) };
        // Drain whatever byte is there; we don't care about its value, only
        // about the readiness edge.
        if rc > 0 && (pfd.revents & libc::POLLIN) != 0 {
            let mut buf = [0u8; 8];
            unsafe {
                libc::read(read_fd, buf.as_mut_ptr() as *mut _, buf.len());
                libc::close(read_fd);
            }
            if is_alive(&sock) {
                return Ok(sock);
            }
        } else {
            unsafe { libc::close(read_fd) };
        }
        // Pipe path failed (POLLHUP without write, or timeout). Fall through
        // to the legacy polling loop so we never leave the user without an
        // error message.
    }

    // Fallback: the legacy 50 ms polling loop. Triggered only when the pipe
    // pre-exec dance fails (rare — locked-down sandboxes that block fcntl)
    // or when the daemon crashed before binding.
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
