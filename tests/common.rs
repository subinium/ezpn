//! Shared helpers for ezpn integration tests.
//!
//! These tests spawn the real `ezpn` binary with a unique per-test session
//! name + temp `XDG_RUNTIME_DIR` so they can run in parallel on shared CI
//! without clobbering each other's sockets. They speak the wire protocol
//! directly (no terminal emulation) — that's why no `expectrl` dependency.
//!
//! The harness is intentionally small: enough to assert daemon lifecycle
//! (spawn → live → graceful shutdown) and reader-thread isolation. Richer
//! interactive scenarios are M2 follow-ups.

#![allow(dead_code)]

use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Build a unique session name per call. Combines the test name, the test
/// process PID, and an atomic counter so concurrent `cargo test` jobs never
/// collide on socket paths.
pub fn unique_session_name(prefix: &str) -> String {
    let n = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{}-{n}", std::process::id())
}

/// Path to the just-built `ezpn` binary. Cargo sets `CARGO_BIN_EXE_<name>`
/// for integration tests pointing at the freshly compiled artifact, which
/// is what we want — never a stale `~/.cargo/bin/ezpn`.
pub fn ezpn_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ezpn"))
}

/// Compute the per-session socket path the daemon will bind. Mirrors
/// `session::socket_path` so test code stays decoupled from daemon
/// internals — if the production scheme changes, the test fails noisily
/// rather than silently looking in the wrong place.
pub fn socket_path(runtime_dir: &Path, session_name: &str) -> PathBuf {
    runtime_dir.join(format!("ezpn-session-{session_name}.sock"))
}

/// Spawn the daemon in detached mode (`ezpn -d -S <name>`) under an
/// isolated `XDG_RUNTIME_DIR` and wait up to 3s for the socket to appear.
/// Returns a `DaemonHandle` that kills + cleans up on drop, so a panic in
/// the test body never leaves a zombie behind.
pub fn spawn_test_daemon(prefix: &str) -> DaemonHandle {
    let session = unique_session_name(prefix);
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = dir.path().to_path_buf();

    // `ezpn --server <name>` is the daemon entrypoint used internally by
    // `session::spawn_server`. It binds the per-session socket and runs the
    // event loop without ever needing a TTY (so it's safe for CI).
    let mut child = Command::new(ezpn_bin())
        .args(["--server", &session])
        .env("XDG_RUNTIME_DIR", &runtime)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ezpn --server");

    let sock = socket_path(&runtime, &session);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if sock.exists() {
            return DaemonHandle {
                child,
                _runtime_dir: dir,
                runtime,
                session,
                sock,
            };
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Daemon never bound — make sure the child gets reaped before we panic
    // so clippy's "spawned process must be wait()ed" lint stays satisfied
    // and we don't leak a process via an aborted test.
    let _ = child.kill();
    let _ = child.wait();
    panic!("daemon socket {} never appeared within 3s", sock.display());
}

/// Owns a running daemon for the lifetime of a test. Drop handler kills
/// the process so failed tests can't leave background daemons running.
pub struct DaemonHandle {
    child: Child,
    _runtime_dir: tempfile::TempDir, // RAII — deletes the dir on drop
    pub runtime: PathBuf,
    pub session: String,
    pub sock: PathBuf,
}

impl DaemonHandle {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Send SIGTERM and wait up to `timeout` for the process to exit.
    /// Returns true on clean exit, false on timeout (test should fail).
    pub fn shutdown_with_sigterm(&mut self, timeout: Duration) -> bool {
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

impl Drop for DaemonHandle {
    fn drop(&mut self) {
        // Best-effort: SIGTERM, brief grace, then SIGKILL if still alive.
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ── Minimal protocol helpers (subset that doesn't depend on `crate::protocol`) ──
// We keep these self-contained rather than importing crate internals so the
// integration tests catch wire-format regressions even if the internal API drifts.

pub const C_HELLO: u8 = 0x07;
pub const C_PING: u8 = 0x05;
pub const C_DETACH: u8 = 0x02;
pub const S_HELLO_OK: u8 = 0x85;
pub const S_HELLO_ERR: u8 = 0x86;
pub const S_PONG: u8 = 0x84;

pub fn write_msg(stream: &mut impl Write, tag: u8, payload: &[u8]) -> std::io::Result<()> {
    let len = (payload.len() as u32).to_be_bytes();
    stream.write_all(&[tag])?;
    stream.write_all(&len)?;
    if !payload.is_empty() {
        stream.write_all(payload)?;
    }
    stream.flush()
}

pub fn read_msg(stream: &mut impl Read) -> std::io::Result<(u8, Vec<u8>)> {
    let mut tag = [0u8; 1];
    stream.read_exact(&mut tag)?;
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    if len > 0 {
        stream.read_exact(&mut payload)?;
    }
    Ok((tag[0], payload))
}

/// Connect to the daemon and complete the v1 Hello handshake. Returns the
/// negotiated capability bitfield from `S_HELLO_OK` so the caller can assert
/// on it. Panics with a descriptive message if anything fails — this is for
/// tests, not production code.
pub fn connect_and_hello(sock: &Path) -> (UnixStream, u32) {
    let mut stream = UnixStream::connect(sock).expect("connect to daemon socket");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set timeout");

    let hello = r#"{"version":1,"capabilities":7,"client":"integration-test"}"#;
    write_msg(&mut stream, C_HELLO, hello.as_bytes()).expect("send C_HELLO");

    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let (tag, body) = read_msg(&mut reader).expect("read hello reply");
    assert_eq!(
        tag,
        S_HELLO_OK,
        "expected S_HELLO_OK, got 0x{tag:02x} body={}",
        String::from_utf8_lossy(&body)
    );
    // Parse `"capabilities":N` out of the JSON without pulling serde into
    // the test crate. We control both ends, so a regex-free byte search is
    // fine; if it ever breaks the assertion message will say so loudly.
    let body_str = String::from_utf8_lossy(&body);
    let caps = parse_caps(&body_str).unwrap_or(0);
    (stream, caps)
}

fn parse_caps(json: &str) -> Option<u32> {
    let needle = "\"capabilities\":";
    let i = json.find(needle)? + needle.len();
    let tail = &json[i..];
    let end = tail
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(tail.len());
    tail[..end].parse().ok()
}
