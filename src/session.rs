//! Session naming, discovery, and server process spawning.

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::protocol;
use anyhow::Context;

/// Runtime directory for session sockets.
///
/// `EZPN_TEST_SOCKET_DIR` takes precedence over `XDG_RUNTIME_DIR` to allow
/// integration tests (#62) to redirect socket creation into a tempdir.
fn runtime_dir() -> PathBuf {
    if let Ok(test_dir) = std::env::var("EZPN_TEST_SOCKET_DIR") {
        return PathBuf::from(test_dir);
    }
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
    probe(path).is_ok()
}

fn probe(path: &std::path::Path) -> anyhow::Result<()> {
    probe_for_pid(path, None)
}

fn probe_for_pid(path: &std::path::Path, expected_pid: Option<u32>) -> anyhow::Result<()> {
    let stream = crate::socket_security::connect_with_timeout(path, Duration::from_millis(200))
        .context("connect probe")?;
    let peer = crate::socket_security::peer_uid(&stream)?;
    anyhow::ensure!(
        peer == unsafe { libc::getuid() },
        "probe peer uid mismatch: {peer}"
    );
    if let Some(expected) = expected_pid {
        anyhow::ensure!(
            crate::socket_security::peer_pid(&stream)? == expected,
            "socket belongs to a different server process"
        );
    }
    let mut io = protocol::DeadlineStream::new(&stream, Duration::from_millis(200));
    protocol::write_msg(&mut io, protocol::C_PING, &[]).context("write probe")?;
    let (mut tag, mut payload) =
        protocol::read_msg_limited(&mut io, 64 * 1024).context("read first probe reply")?;
    if tag == protocol::S_VERSION {
        serde_json::from_slice::<protocol::ServerHello>(&payload)?;
        (tag, payload) =
            protocol::read_msg_limited(&mut io, 64 * 1024).context("read pong after greeting")?;
    }
    anyhow::ensure!(
        tag == protocol::S_PONG && payload.is_empty(),
        "invalid probe response: {tag:#x}"
    );
    Ok(())
}

/// Auto-generate a session name from the current directory.
pub fn auto_name() -> String {
    let base = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "default".to_string());

    // Sanitize: only keep alphanumeric, dash, underscore, dot
    let mut base: String = base
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Leave room for collision suffixes and stay within the name contract.
    while base.len() > 48 {
        base.pop();
    }
    if base.is_empty() {
        base.push_str("default");
    }

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
    list_in(&dir)
}

fn list_in(dir: &std::path::Path) -> Vec<(String, PathBuf)> {
    let mut sessions = Vec::new();

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().into_owned();
            if let Some(name) = fname
                .strip_prefix("ezpn-session-")
                .and_then(|s| s.strip_suffix(".sock"))
            {
                if validate_name(name).is_err() {
                    continue;
                }
                let path = entry.path();
                if is_alive(&path) {
                    sessions.push((name.to_string(), path));
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
        if validate_name(n).is_err() {
            return None;
        }
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
    validate_name(session_name)?;
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

    let mut child = cmd.spawn()?;
    wait_for_server(&mut child, &sock, Duration::from_secs(3))?;
    Ok(sock)
}

fn wait_for_server(
    child: &mut std::process::Child,
    sock: &std::path::Path,
    timeout: Duration,
) -> anyhow::Result<()> {
    // Wait for the server to create its socket (up to 3 seconds)
    // Use is_alive() to confirm via C_PING without side effects
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("server exited before becoming ready: {status}");
        }
        if probe_for_pid(sock, Some(child.id())).is_ok() {
            if let Some(status) = child.try_wait()? {
                anyhow::bail!("server exited during startup: {status}");
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Give only the child we spawned a chance to release its PTYs, then
    // force termination and reap it. Never unlink a possibly live socket.
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let grace = std::time::Instant::now() + Duration::from_millis(500);
    while std::time::Instant::now() < grace {
        if child.try_wait()?.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if child.try_wait()?.is_none() {
        let _ = child.kill();
    }
    child.wait()?;
    anyhow::bail!(
        "server did not become ready within {} seconds; startup process stopped",
        timeout.as_secs_f64()
    )
}

pub fn validate_name(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.')),
        "session name must be 1-64 bytes containing only letters, numbers, '-', '_' or '.'"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::net::UnixListener;

    #[test]
    fn listing_unresponsive_listener_does_not_remove_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("ezpn-session-unresponsive.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        assert!(list_in(dir.path()).is_empty());
        assert!(socket.exists());
    }

    #[test]
    fn startup_failure_is_reaped_and_reported_before_timeout() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 17"])
            .spawn()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        let error = wait_for_server(
            &mut child,
            &dir.path().join("absent"),
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(error.to_string().contains("exited before"));
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn startup_timeout_stops_and_reaps_owned_child() {
        let mut child = Command::new("/bin/sleep").arg("10").spawn().unwrap();
        let dir = tempfile::tempdir().unwrap();
        assert!(wait_for_server(
            &mut child,
            &dir.path().join("absent"),
            Duration::from_millis(30)
        )
        .is_err());
        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn probe_accepts_version_greeting_before_pong() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("probe.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&protocol::server_hello()).unwrap();
            assert_eq!(protocol::read_msg(&mut stream).unwrap().0, protocol::C_PING);
            let _ = protocol::write_msg(&mut stream, protocol::S_PONG, &[]);
        });
        let alive = probe(&path);
        server.join().unwrap();
        alive.unwrap();
    }
}
