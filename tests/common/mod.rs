//! Bounded, isolated real-process helpers. No access to the user's sessions.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

pub const SOCKET_DIR_ENV: &str = "EZPN_TEST_SOCKET_DIR";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(8);
const CAPTURE_LIMIT: usize = 4 * 1024 * 1024;
pub type Capture = Arc<Mutex<Vec<u8>>>;

pub fn ezpn_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ezpn"))
}

pub fn wait_for<F, T>(label: &str, timeout: Duration, mut f: F) -> Result<T, String>
where
    F: FnMut() -> Option<T>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = f() {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(format!("{label} timed out after {timeout:?}"));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

pub fn wait_for_path(path: &Path, timeout: Duration) -> Result<(), String> {
    wait_for(&format!("path {}", path.display()), timeout, || {
        path.exists().then_some(())
    })
}

pub fn captured(capture: &Capture) -> String {
    String::from_utf8_lossy(&capture.lock().unwrap()).into_owned()
}

pub fn wait_for_output(capture: &Capture, needle: &str, timeout: Duration) -> Result<(), String> {
    wait_for(&format!("output {needle:?}"), timeout, || {
        captured(capture).contains(needle).then_some(())
    })
    .map_err(|error| format!("{error}; captured: {:?}", captured(capture)))
}

fn append(sink: &Capture, bytes: &[u8]) {
    let mut sink = sink.lock().unwrap();
    sink.extend_from_slice(bytes);
    let excess = sink.len().saturating_sub(CAPTURE_LIMIT);
    sink.drain(..excess);
}

pub fn spawn_capture<R: Read + Send + 'static>(
    mut reader: R,
    sink: Capture,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => append(&sink, &buf[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    })
}

pub struct TestEnv {
    pub temp: TempDir,
}

impl TestEnv {
    pub fn new() -> Self {
        // macOS's default TMPDIR can exceed sockaddr_un.sun_path's limit.
        let temp = tempfile::Builder::new()
            .prefix("ezpn-it-")
            .tempdir_in("/tmp")
            .unwrap();
        let env = Self { temp };
        for (_, path) in env.variables() {
            std::fs::create_dir_all(&path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        env
    }
    pub fn root(&self) -> &Path {
        self.temp.path()
    }
    pub fn socket_dir(&self) -> PathBuf {
        self.root().join("run")
    }
    pub fn snapshot_dir(&self) -> PathBuf {
        self.root().join("data/ezpn/sessions")
    }
    pub fn session_socket(&self, name: &str) -> PathBuf {
        self.socket_dir().join(format!("ezpn-session-{name}.sock"))
    }
    pub fn variables(&self) -> Vec<(&'static str, PathBuf)> {
        vec![
            ("HOME", self.root().join("home")),
            ("XDG_CONFIG_HOME", self.root().join("config")),
            ("XDG_DATA_HOME", self.root().join("data")),
            ("XDG_STATE_HOME", self.root().join("state")),
            ("XDG_CACHE_HOME", self.root().join("cache")),
            ("XDG_RUNTIME_DIR", self.socket_dir()),
            (SOCKET_DIR_ENV, self.socket_dir()),
        ]
    }
    pub fn command(&self) -> Command {
        self.command_for(ezpn_binary())
    }
    pub fn command_for(&self, executable: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut cmd = Command::new(executable);
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .env("LC_ALL", "C")
            .envs(self.variables())
            .current_dir(self.root())
            .stdin(Stdio::null());
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", profile);
        }
        cmd
    }
}

pub fn run_bounded(cmd: &mut Command) -> Output {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = Arc::new(Mutex::new(Vec::new()));
    let stderr = Arc::new(Mutex::new(Vec::new()));
    let out_reader = spawn_capture(child.stdout.take().unwrap(), stdout.clone());
    let err_reader = spawn_capture(child.stderr.take().unwrap(), stderr.clone());
    let result = wait_for("CLI exit", DEFAULT_TIMEOUT, || child.try_wait().unwrap());
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let status = result.unwrap_or_else(|error| panic!("{error}: {}", captured(&stderr)));
    wait_for("CLI output EOF", DEFAULT_TIMEOUT, || {
        (out_reader.is_finished() && err_reader.is_finished()).then_some(())
    })
    .expect("CLI descendants kept stdout/stderr open");
    out_reader.join().unwrap();
    err_reader.join().unwrap();
    let stdout = stdout.lock().unwrap().clone();
    let stderr = stderr.lock().unwrap().clone();
    Output {
        status,
        stdout,
        stderr,
    }
}

pub struct DaemonHandle {
    pub session: String,
    pub socket: PathBuf,
    pub stdout: Capture,
    pub stderr: Capture,
    child: Child,
}

impl DaemonHandle {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
    pub fn assert_alive(&mut self) {
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "daemon exited: {}",
            captured(&self.stderr)
        );
    }
    pub fn wait_exit(&mut self) -> std::process::ExitStatus {
        wait_for("daemon exit", DEFAULT_TIMEOUT, || {
            self.child.try_wait().unwrap()
        })
        .unwrap_or_else(|error| panic!("{error}: {}", captured(&self.stderr)))
    }
    pub fn signal(&self, signal: i32) {
        // SAFETY: this handle owns the unreaped child, so its PID cannot be reused.
        assert_eq!(unsafe { libc::kill(self.pid() as i32, signal) }, 0);
    }
    pub fn shutdown(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            // Cleanup is independent of whether the handshake under test works.
            unsafe {
                libc::kill(self.pid() as i32, libc::SIGTERM);
            }
            if wait_for("daemon cleanup", Duration::from_secs(2), || {
                self.child.try_wait().ok().flatten()
            })
            .is_err()
            {
                let _ = self.child.kill();
            }
        }
        let _ = self.child.wait();
    }
}
impl Drop for DaemonHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn spawn_daemon(env: &TestEnv, session: &str) -> DaemonHandle {
    spawn_daemon_args(env, session, &["1", "1"])
}

pub fn spawn_daemon_args(env: &TestEnv, session: &str, args: &[&str]) -> DaemonHandle {
    let mut child = env
        .command()
        .args(["--server", session])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = Arc::new(Mutex::new(Vec::new()));
    let stderr = Arc::new(Mutex::new(Vec::new()));
    spawn_capture(child.stdout.take().unwrap(), stdout.clone());
    spawn_capture(child.stderr.take().unwrap(), stderr.clone());
    let mut daemon = DaemonHandle {
        session: session.into(),
        socket: env.session_socket(session),
        stdout,
        stderr,
        child,
    };
    wait_for("daemon startup", DEFAULT_TIMEOUT, || {
        daemon.assert_alive();
        daemon.socket.exists().then_some(())
    })
    .unwrap_or_else(|error| panic!("{error}: {}", captured(&daemon.stderr)));
    daemon.assert_alive();
    daemon
}

pub fn write_msg(w: &mut impl Write, tag: u8, payload: &[u8]) -> std::io::Result<()> {
    w.write_all(&[tag])?;
    w.write_all(&(payload.len() as u32).to_be_bytes())?;
    w.write_all(payload)?;
    w.flush()
}
pub fn read_msg(r: &mut impl Read) -> std::io::Result<(u8, Vec<u8>)> {
    let mut header = [0; 5];
    r.read_exact(&mut header)?;
    let len = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
    if len > 16 * 1024 * 1024 {
        return Err(std::io::Error::other("oversized test frame"));
    }
    let mut payload = vec![0; len];
    r.read_exact(&mut payload)?;
    Ok((header[0], payload))
}
pub fn connect(daemon: &DaemonHandle) -> UnixStream {
    let stream = UnixStream::connect(&daemon.socket).unwrap();
    stream.set_read_timeout(Some(DEFAULT_TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(DEFAULT_TIMEOUT)).unwrap();
    stream
}
pub fn hello(stream: &mut UnixStream, major_override: Option<u64>) -> serde_json::Value {
    let (tag, payload) = read_msg(stream).expect("server version handshake");
    assert_eq!(tag, 0x10, "expected S_VERSION");
    let version: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    let response = serde_json::json!({
        "proto_major": major_override.unwrap_or(version["proto_major"].as_u64().unwrap()),
        "proto_minor": version["proto_minor"], "client_build": "integration-test", "supported_features": []
    });
    write_msg(stream, 0x11, &serde_json::to_vec(&response).unwrap()).unwrap();
    version
}

pub struct AttachClient {
    pub stream: UnixStream,
    output: Capture,
}
impl AttachClient {
    pub fn output(&self) -> Capture {
        self.output.clone()
    }
    pub fn send_resize(&mut self, cols: u16, rows: u16) -> std::io::Result<()> {
        let mut payload = Vec::from(cols.to_be_bytes());
        payload.extend_from_slice(&rows.to_be_bytes());
        write_msg(&mut self.stream, 0x03, &payload)
    }
    pub fn send_detach(&mut self) -> std::io::Result<()> {
        write_msg(&mut self.stream, 0x02, &[])
    }
}
impl Drop for AttachClient {
    fn drop(&mut self) {
        // Close the collector clone too; dropping only the writer leaves it attached.
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}
pub fn attach_client(daemon: &DaemonHandle, cols: u16, rows: u16) -> AttachClient {
    attach_with_mode(daemon, cols, rows, "steal")
}
pub fn attach_with_mode(daemon: &DaemonHandle, cols: u16, rows: u16, mode: &str) -> AttachClient {
    let mut stream = connect(daemon);
    hello(&mut stream, None);
    let request = serde_json::json!({"cols": cols, "rows": rows, "mode": mode});
    write_msg(&mut stream, 0x06, &serde_json::to_vec(&request).unwrap()).unwrap();
    // Idle timeouts corrupt partial read_exact frames. Drop shuts down all clones.
    stream.set_read_timeout(None).unwrap();
    let mut reader = stream.try_clone().unwrap();
    let output = Arc::new(Mutex::new(Vec::new()));
    let sink = output.clone();
    thread::spawn(move || {
        while let Ok((tag, payload)) = read_msg(&mut reader) {
            match tag {
                0x81 => append(&sink, &payload),
                0x82 | 0x83 => break,
                _ => (),
            }
        }
    });
    AttachClient { stream, output }
}
pub fn type_text(client: &mut AttachClient, text: &str) -> std::io::Result<()> {
    let event = crossterm::event::Event::Paste(text.into());
    write_msg(&mut client.stream, 0x01, &serde_json::to_vec(&event)?)
}
pub fn ls(env: &TestEnv) -> String {
    let output = run_bounded(env.command().arg("ls"));
    assert!(output.status.success(), "ls failed: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}
pub fn kill_session(env: &TestEnv, session: &str) -> Output {
    run_bounded(env.command().args(["kill", session]))
}

pub fn ipc(daemon: &DaemonHandle, request: serde_json::Value) -> serde_json::Value {
    let socket = daemon
        .socket
        .parent()
        .unwrap()
        .join(format!("ezpn-{}.sock", daemon.pid()));
    let mut stream = UnixStream::connect(socket).unwrap_or_else(|error| {
        panic!(
            "IPC connection: {error}; daemon stderr: {}",
            captured(&daemon.stderr)
        )
    });
    stream.set_read_timeout(Some(DEFAULT_TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(DEFAULT_TIMEOUT)).unwrap();
    writeln!(stream, "{request}").unwrap();
    let mut line = String::new();
    BufReader::new(stream)
        .take(16 * 1024 * 1024)
        .read_line(&mut line)
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

pub fn ctl(env: &TestEnv, daemon: &DaemonHandle, args: &[&str]) -> Output {
    let mut command = env.command_for(env!("CARGO_BIN_EXE_ezpn-ctl"));
    command
        .args(["--pid", &daemon.pid().to_string()])
        .args(args);
    run_bounded(&mut command)
}
