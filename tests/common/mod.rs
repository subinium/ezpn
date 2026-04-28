//! Shared helpers for the ezpn integration test harness.
//!
//! These helpers spawn the real `ezpn` binary as a subprocess and talk to it
//! over a real Unix domain socket. To keep tests hermetic, every test is
//! expected to point the daemon at an isolated socket directory using the
//! `EZPN_TEST_SOCKET_DIR` environment variable. The matching wiring inside
//! `src/main.rs` (and `src/session.rs` / `src/ipc.rs`) is added in a
//! follow-up commit; until that lands, integration tests are marked
//! `#[ignore]` so the suite remains green.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Environment variable consumed by the daemon to redirect socket creation.
/// Must match the constant used inside `src/session.rs` once wired.
pub const SOCKET_DIR_ENV: &str = "EZPN_TEST_SOCKET_DIR";

/// Default poll interval for `wait_for`-style helpers.
pub const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Default deadline for waiting on daemon-side state changes.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Path to the `ezpn` binary built by Cargo for the current test target.
pub fn ezpn_binary() -> PathBuf {
    // assert_cmd resolves the binary the same way; we mirror its lookup so
    // helpers that don't use assert_cmd (raw `Command::new`) stay consistent.
    PathBuf::from(env!("CARGO_BIN_EXE_ezpn"))
}

/// Run a closure repeatedly until it returns `Some(value)` or the deadline
/// elapses. Returns `Err` with a description on timeout. Never sleeps the
/// thread for synchronization in the test bodies — wait through this helper.
pub fn wait_for<F, T>(label: &str, timeout: Duration, mut f: F) -> Result<T, String>
where
    F: FnMut() -> Option<T>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return Ok(v);
        }
        if Instant::now() >= deadline {
            return Err(format!("wait_for({}) timed out after {:?}", label, timeout));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Convenience wrapper around `wait_for` with the default timeout.
pub fn wait_for_default<F, T>(label: &str, f: F) -> Result<T, String>
where
    F: FnMut() -> Option<T>,
{
    wait_for(label, DEFAULT_TIMEOUT, f)
}

/// Wait until `path` exists on disk (e.g. a socket file appeared).
pub fn wait_for_path(path: &Path, timeout: Duration) -> Result<(), String> {
    wait_for(&format!("path exists: {}", path.display()), timeout, || {
        if path.exists() {
            Some(())
        } else {
            None
        }
    })
}

/// Wait for a substring to appear inside captured output.
pub fn wait_for_output(
    capture: &Arc<Mutex<Vec<u8>>>,
    needle: &str,
    timeout: Duration,
) -> Result<(), String> {
    wait_for(&format!("output contains {:?}", needle), timeout, || {
        let buf = capture.lock().ok()?;
        if twoway_contains(&buf, needle.as_bytes()) {
            Some(())
        } else {
            None
        }
    })
}

fn twoway_contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// A scratch directory dedicated to a single test. Holds the socket dir,
/// snapshot dir, and any other per-test fixtures. Drops cleanly.
pub struct TestEnv {
    pub temp: TempDir,
}

impl TestEnv {
    pub fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("ezpn-it-")
            .tempdir()
            .expect("create tempdir");
        // Pre-create canonical sub-paths so helpers can reference them eagerly.
        std::fs::create_dir_all(temp.path().join("sockets")).expect("mkdir sockets");
        std::fs::create_dir_all(temp.path().join("snapshots")).expect("mkdir snapshots");
        Self { temp }
    }

    pub fn root(&self) -> &Path {
        self.temp.path()
    }

    pub fn socket_dir(&self) -> PathBuf {
        self.temp.path().join("sockets")
    }

    pub fn snapshot_dir(&self) -> PathBuf {
        self.temp.path().join("snapshots")
    }

    /// Expected socket path for a session name, mirroring `session::socket_path`.
    pub fn session_socket(&self, name: &str) -> PathBuf {
        self.socket_dir()
            .join(format!("ezpn-session-{}.sock", name))
    }
}

/// RAII guard around a spawned daemon. Killing the underlying process on drop
/// is what keeps the test suite hermetic on panic.
pub struct DaemonHandle {
    pub session: String,
    pub socket: PathBuf,
    pub stdout: Arc<Mutex<Vec<u8>>>,
    pub stderr: Arc<Mutex<Vec<u8>>>,
    child: Option<Child>,
}

impl DaemonHandle {
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|c| c.id())
    }

    /// Try to terminate gracefully then force-kill if still alive.
    pub fn shutdown(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Best-effort: ask the daemon to kill itself via the CLI command
            // first, then fall back to SIGKILL through std::process::Child.
            let _ = Command::new(ezpn_binary())
                .arg("kill")
                .arg(&self.session)
                .env(SOCKET_DIR_ENV, parent_dir(&self.socket))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            // Give the daemon a beat to clean up its socket. We don't sleep
            // unconditionally — bail as soon as the socket is gone.
            let _ = wait_for("daemon socket removed", Duration::from_secs(2), || {
                if !self.socket.exists() {
                    Some(())
                } else {
                    None
                }
            });

            let _ = child.kill();
            let _ = child.wait();
        }
        // Clean up any stray socket file, just in case.
        let _ = std::fs::remove_file(&self.socket);
    }
}

impl Drop for DaemonHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn parent_dir(p: &Path) -> PathBuf {
    p.parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Spawn a fresh `ezpn` daemon for a given session inside `env`.
///
/// The daemon process is detached from a controlling terminal — exactly the
/// production code path. Returns once the socket is observable on disk so
/// callers don't race the bind.
///
/// NOTE: depends on `EZPN_TEST_SOCKET_DIR` being honored by the daemon. Until
/// that lands in `src/main.rs`, every caller should mark its test `#[ignore]`.
pub fn spawn_daemon(env: &TestEnv, session: &str) -> DaemonHandle {
    let socket_dir = env.socket_dir();
    std::fs::create_dir_all(&socket_dir).expect("mkdir socket dir");

    // We can't exec the interactive `ezpn` entrypoint directly: it enables
    // raw mode on stdin/stdout, which corrupts the test runner's terminal.
    // The internal `--server <name>` entrypoint runs the daemon body without
    // touching our TTY, which is exactly what we want here. Tests that need
    // an attached client connect via `attach_client` over the Unix socket.
    let mut cmd = Command::new(ezpn_binary());
    cmd.env(SOCKET_DIR_ENV, &socket_dir)
        // Force a known shell so PTY spawn is deterministic across CI hosts.
        .env("SHELL", "/bin/sh")
        // Prevent the binary from refusing to start because of a parent EZPN env.
        .env_remove("EZPN")
        .arg("--server")
        .arg(session)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("spawn ezpn daemon");

    let stdout_buf = Arc::new(Mutex::new(Vec::new()));
    let stderr_buf = Arc::new(Mutex::new(Vec::new()));

    if let Some(out) = child.stdout.take() {
        spawn_capture(out, Arc::clone(&stdout_buf));
    }
    if let Some(err) = child.stderr.take() {
        spawn_capture(err, Arc::clone(&stderr_buf));
    }

    let socket = socket_dir.join(format!("ezpn-session-{}.sock", session));

    let mut handle = DaemonHandle {
        session: session.to_string(),
        socket: socket.clone(),
        stdout: stdout_buf,
        stderr: stderr_buf,
        child: Some(child),
    };

    if let Err(e) = wait_for_path(&socket, Duration::from_secs(5)) {
        // Drop will tear down the child if spawn failed past the bind step.
        handle.shutdown();
        panic!("daemon never created socket {}: {}", socket.display(), e);
    }

    handle
}

fn spawn_capture<R: Read + Send + 'static>(mut r: R, sink: Arc<Mutex<Vec<u8>>>) {
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut sink) = sink.lock() {
                        sink.extend_from_slice(&buf[..n]);
                    }
                }
                Err(_) => break,
            }
        }
    });
}

/// Minimal in-process attach client. Connects to a daemon's session socket,
/// sends `C_RESIZE` to negotiate a virtual terminal size, then exposes
/// readers/writers for the framed protocol.
///
/// This is deliberately *not* `client::run`: that path enables raw mode on
/// stdin/stdout, which would corrupt the test runner's terminal.
pub struct AttachClient {
    pub stream: UnixStream,
    pub reader: BufReader<UnixStream>,
    pub cols: u16,
    pub rows: u16,
    output: Arc<Mutex<Vec<u8>>>,
}

impl AttachClient {
    pub fn output(&self) -> Arc<Mutex<Vec<u8>>> {
        Arc::clone(&self.output)
    }

    /// Send a raw key-event JSON payload to the daemon.
    pub fn send_event(&mut self, json: &[u8]) -> std::io::Result<()> {
        // Tag matches `protocol::C_EVENT` (0x01); spelled out here to keep
        // the helper independent of internal modules.
        write_msg(&mut self.stream, 0x01, json)
    }

    /// Send a resize. Tag matches `protocol::C_RESIZE` (0x03).
    pub fn send_resize(&mut self, cols: u16, rows: u16) -> std::io::Result<()> {
        let payload = encode_resize(cols, rows);
        write_msg(&mut self.stream, 0x03, &payload)
    }

    /// Send detach. Tag matches `protocol::C_DETACH` (0x02).
    pub fn send_detach(&mut self) -> std::io::Result<()> {
        write_msg(&mut self.stream, 0x02, &[])
    }
}

/// Connect to an existing daemon's socket and perform the steal-mode handshake.
pub fn attach_client(daemon: &DaemonHandle, cols: u16, rows: u16) -> AttachClient {
    let stream = UnixStream::connect(&daemon.socket).expect("connect to daemon");
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .expect("set read timeout");

    let mut writer = stream.try_clone().expect("clone stream");
    let resize = encode_resize(cols, rows);
    write_msg(&mut writer, 0x03 /* C_RESIZE */, &resize).expect("send initial resize");

    let reader_stream = stream.try_clone().expect("clone reader");
    let output = Arc::new(Mutex::new(Vec::new()));
    spawn_frame_collector(reader_stream, Arc::clone(&output));

    AttachClient {
        stream: writer,
        reader: BufReader::new(stream),
        cols,
        rows,
        output,
    }
}

/// Send `bytes` as a synthetic crossterm `Event::Paste`. Useful for shoving
/// text into a daemon's pane when we don't want to hand-craft KeyEvents.
pub fn type_text(client: &mut AttachClient, text: &str) -> std::io::Result<()> {
    // Wire format: a JSON-serialized `crossterm::event::Event::Paste(String)`.
    let json = format!("{{\"Paste\":{}}}", serde_json_string(text));
    client.send_event(json.as_bytes())
}

fn serde_json_string(s: &str) -> String {
    // Minimal JSON string escape — avoids pulling serde_json into the helper.
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ── Wire helpers (mirror `src/protocol.rs` so common/ stays standalone) ──

fn write_msg<W: Write>(w: &mut W, tag: u8, payload: &[u8]) -> std::io::Result<()> {
    let len = (payload.len() as u32).to_be_bytes();
    w.write_all(&[tag])?;
    w.write_all(&len)?;
    if !payload.is_empty() {
        w.write_all(payload)?;
    }
    w.flush()
}

fn read_msg<R: Read>(r: &mut R) -> std::io::Result<(u8, Vec<u8>)> {
    let mut tag = [0u8; 1];
    r.read_exact(&mut tag)?;
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    if len > 0 {
        r.read_exact(&mut payload)?;
    }
    Ok((tag[0], payload))
}

fn encode_resize(cols: u16, rows: u16) -> [u8; 4] {
    let c = cols.to_be_bytes();
    let r = rows.to_be_bytes();
    [c[0], c[1], r[0], r[1]]
}

fn spawn_frame_collector(stream: UnixStream, sink: Arc<Mutex<Vec<u8>>>) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        loop {
            match read_msg(&mut reader) {
                Ok((0x81 /* S_OUTPUT */, payload)) => {
                    if let Ok(mut sink) = sink.lock() {
                        sink.extend_from_slice(&payload);
                    }
                }
                Ok((0x83 /* S_EXIT */, _)) => break,
                Ok((0x82 /* S_DETACHED */, _)) => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
}

/// Run `ezpn ls` against the test socket dir and return the captured stdout.
pub fn ls(env: &TestEnv) -> String {
    let out = Command::new(ezpn_binary())
        .env(SOCKET_DIR_ENV, env.socket_dir())
        .arg("ls")
        .output()
        .expect("run ezpn ls");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run `ezpn kill <session>` against the test socket dir.
pub fn kill_session(env: &TestEnv, session: &str) -> std::process::Output {
    Command::new(ezpn_binary())
        .env(SOCKET_DIR_ENV, env.socket_dir())
        .arg("kill")
        .arg(session)
        .output()
        .expect("run ezpn kill")
}

/// Read a `BufRead` line-by-line until the predicate hits or the deadline lapses.
pub fn drain_until<R: BufRead>(
    reader: &mut R,
    pred: impl Fn(&str) -> bool,
    timeout: Duration,
) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    let mut acc = String::new();
    while Instant::now() < deadline {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return Err("eof before predicate matched".to_string()),
            Ok(_) => {
                acc.push_str(&line);
                if pred(&line) {
                    return Ok(acc);
                }
            }
            Err(_) => break,
        }
    }
    Err(format!("drain_until timed out after {:?}", timeout))
}
