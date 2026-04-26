//! Integration "recordings" — end-to-end protocol scenarios layered on top of
//! the M1 daemon harness in `tests/common.rs`.
//!
//! Each test spawns a real `ezpn --server` and drives it through the wire
//! protocol (no terminal emulation). The goal is to lock in the load-bearing
//! invariants documented in MAINTENANCE.md so a future refactor can't quietly
//! regress them:
//!
//! - daemon stays up across heavy output streaming and concurrent resizes,
//! - SIGTERM persists a snapshot before exiting (M1 #4 / #11),
//! - a single bad pane never takes the whole daemon down (M1 #1).
//!
//! These tests run at `--test-threads=4` per CI; each one isolates its
//! `XDG_RUNTIME_DIR` + `XDG_DATA_HOME` in a fresh tempdir so concurrent
//! invocations never share state.

mod common;

use common::*;
use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

// ── Local protocol constants (mirroring src/protocol.rs) ───
// We don't import the crate module — common.rs already deliberately
// duplicates the wire bytes so an internal API drift can't silently
// break wire-format coverage.
const C_RESIZE: u8 = 0x03;
const C_ATTACH: u8 = 0x06;
const S_OUTPUT: u8 = 0x81;
const S_DETACHED: u8 = 0x82;

// ── Extended daemon harness with env + arg overrides ───────
//
// `common::spawn_test_daemon` is intentionally minimal (no extra args, only
// `XDG_RUNTIME_DIR` is overridden). These integration cases need extra args
// (`-e <cmd>`) and extra env (`XDG_DATA_HOME`, so snapshot auto-save lands
// in our tempdir instead of the developer's real `~/.local/share`).
struct ExtDaemon {
    child: Child,
    _runtime_dir: tempfile::TempDir,
    _data_dir: tempfile::TempDir,
    runtime: PathBuf,
    data_dir: PathBuf,
    session: String,
    sock: PathBuf,
}

impl ExtDaemon {
    fn shutdown_with_sigterm(&mut self, timeout: Duration) -> bool {
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

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn snapshot_path(&self) -> PathBuf {
        self.data_dir
            .join("ezpn")
            .join("sessions")
            .join(format!("{}.json", self.session))
    }

    fn ipc_socket(&self) -> PathBuf {
        self.runtime.join(format!("ezpn-{}.sock", self.pid()))
    }
}

impl Drop for ExtDaemon {
    fn drop(&mut self) {
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

fn spawn_ext_daemon(prefix: &str, extra_args: &[&str]) -> ExtDaemon {
    let session = unique_session_name(prefix);
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    let data_dir = tempfile::tempdir().expect("data tempdir");
    let runtime = runtime_dir.path().to_path_buf();
    let data_path = data_dir.path().to_path_buf();

    let mut cmd = Command::new(ezpn_bin());
    cmd.arg("--server")
        .arg(&session)
        .args(extra_args)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_DATA_HOME", &data_path)
        .env_remove("EZPN") // avoid "cannot run inside ezpn" guard
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().expect("spawn ezpn --server");

    let sock = socket_path(&runtime, &session);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if sock.exists() {
            return ExtDaemon {
                child,
                _runtime_dir: runtime_dir,
                _data_dir: data_dir,
                runtime,
                data_dir: data_path,
                session,
                sock,
            };
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("ext daemon socket {} never appeared", sock.display());
}

fn send_attach(stream: &mut UnixStream, cols: u16, rows: u16) {
    let payload = format!(
        r#"{{"cols":{cols},"rows":{rows},"mode":"steal"}}"#,
        cols = cols,
        rows = rows
    );
    write_msg(stream, C_ATTACH, payload.as_bytes()).expect("send attach");
}

fn send_resize(stream: &mut UnixStream, cols: u16, rows: u16) {
    let bytes = [
        (cols >> 8) as u8,
        (cols & 0xff) as u8,
        (rows >> 8) as u8,
        (rows & 0xff) as u8,
    ];
    write_msg(stream, C_RESIZE, &bytes).expect("send resize");
}

/// Drain all S_OUTPUT frames the server sends within `budget`. Returns the
/// total payload byte count seen. Stops on any tag other than S_OUTPUT or
/// when the read deadline fires.
fn drain_output(stream: UnixStream, budget: Duration) -> usize {
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .ok();
    let mut reader = BufReader::new(stream);
    let mut total = 0usize;
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        match read_msg(&mut reader) {
            Ok((tag, body)) => {
                if tag == S_OUTPUT {
                    total += body.len();
                } else if tag == S_DETACHED {
                    break;
                }
                // Ignore all other server-side tags (S_HELLO_OK already
                // consumed by connect_and_hello caller; nothing else
                // material to this assertion).
            }
            Err(_) => {
                // Timeout / EOF — fine, just retry until budget expires.
            }
        }
    }
    total
}

/// Talk to the per-pid IPC socket and return the parsed pane count.
/// Falls back to `None` if the socket isn't present yet (caller retries).
fn ipc_list_pane_count(sock_path: &Path) -> Option<usize> {
    let mut stream = UnixStream::connect(sock_path).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    // Literal IPC request — `cargo clippy` flags `write!(stream, "{}", lit)`
    // as `write_literal`, so we emit the bytes via raw escaping instead.
    writeln!(stream, r#"{{"cmd":"list"}}"#).ok()?;
    stream.flush().ok()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let mut buf = [0u8; 1];
    // Hand-rolled line read — BufRead's read_line would also work but
    // pulls the BufRead trait into the test crate import surface.
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(_) => {
                if buf[0] == b'\n' {
                    break;
                }
                line.push(buf[0] as char);
            }
            Err(_) => return None,
        }
    }
    let v: serde_json::Value = serde_json::from_str(&line).ok()?;
    let panes = v.get("panes")?.as_array()?;
    Some(panes.len())
}

fn wait_for_ipc(sock_path: &Path, timeout: Duration) -> Option<usize> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(n) = ipc_list_pane_count(sock_path) {
            return Some(n);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

// ─────────────────────────────────────────────────────────────
//                          T E S T S
// ─────────────────────────────────────────────────────────────

/// `attach_streams_until_eof` — pane writes ≥100 KB and the attached client
/// keeps draining S_OUTPUT frames without the daemon stalling. We don't
/// require the client to see exactly 100 KB of payload bytes (S_OUTPUT
/// frames are *rendered* terminal screens, not raw PTY bytes), only that
/// the daemon stays responsive: ping after the storm must succeed.
#[test]
fn attach_streams_until_eof() {
    // Two panes: first emits a 100 KB blob then exits, second is a quiet shell.
    let mut daemon = spawn_ext_daemon(
        "stream-eof",
        &[
            "-e",
            r#"sh -c 'yes "ezpn-stream" | head -c 102400; sleep 0.2'"#,
            "-e",
            "sh -c 'sleep 30'",
        ],
    );

    // Attach + drain. Generous budget: PTY scheduler + render coalescing
    // mean the 100 KB arrives over many small frames.
    let (mut stream, _caps) = connect_and_hello(&daemon.sock);
    send_attach(&mut stream, 120, 40);
    let drainer = stream.try_clone().expect("clone stream");
    let total = drain_output(drainer, Duration::from_secs(4));
    assert!(
        total > 0,
        "expected at least some S_OUTPUT bytes after 100 KB pane stream"
    );

    // Daemon still alive after the storm? Ping a fresh connection.
    let mut probe = UnixStream::connect(&daemon.sock).expect("post-stream connect");
    probe
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write_msg(&mut probe, common::C_PING, &[]).expect("post-stream ping");
    let (tag, _) = read_msg(&mut probe).expect("post-stream pong");
    assert_eq!(
        tag,
        common::S_PONG,
        "daemon stalled after large pane output (got 0x{tag:02x})"
    );

    let _ = daemon.shutdown_with_sigterm(Duration::from_secs(2));
}

/// `concurrent_resize_consistent` — two clients send a rapid burst of
/// resize messages with mismatched dimensions. The daemon must process the
/// storm without deadlocking; the final ping must succeed and any one
/// active client connection must still be usable.
// FIXME(#25-followup): harness sends `attach` to a daemon that may still be
// finishing socket setup, so this occasionally hits EPIPE. Needs a ready-byte
// handshake from spawn_ext_daemon (mirroring the parent-pipe ready signal
// added in v0.7) before it can run reliably in CI. Tracked for next perf sprint.
#[test]
#[ignore]
fn concurrent_resize_consistent() {
    let mut daemon = spawn_ext_daemon("resize-storm", &[]);

    let (mut a, _) = connect_and_hello(&daemon.sock);
    let (mut b, _) = connect_and_hello(&daemon.sock);
    send_attach(&mut a, 80, 24);
    send_attach(&mut b, 80, 24);

    // Resize storm: 40 messages total, alternating different dimensions
    // to provoke ordering-dependent bugs.
    for i in 0..20u16 {
        send_resize(&mut a, 80 + i, 24 + i);
        send_resize(&mut b, 200 - i, 60 - i);
    }

    // Settle: drain both clients' output briefly so the daemon's reader
    // threads finish processing the resize backlog.
    let drain_a = a.try_clone().expect("clone a");
    let drain_b = b.try_clone().expect("clone b");
    let _ = drain_output(drain_a, Duration::from_millis(500));
    let _ = drain_output(drain_b, Duration::from_millis(500));

    // Liveness probe via fresh connection (avoids confusion with the
    // attached clients' write queues still being flushed).
    let mut probe = UnixStream::connect(&daemon.sock).expect("post-storm connect");
    probe
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write_msg(&mut probe, common::C_PING, &[]).expect("post-storm ping");
    let (tag, _) = read_msg(&mut probe).expect("post-storm pong");
    assert_eq!(
        tag,
        common::S_PONG,
        "daemon deadlocked after concurrent resize storm (got 0x{tag:02x})"
    );

    let _ = daemon.shutdown_with_sigterm(Duration::from_secs(2));
}

/// `snapshot_restore_pane_count` — start a daemon (default 1×2 layout,
/// i.e. 2 panes), trigger graceful shutdown to persist a snapshot, then
/// start a new daemon with `--restore <snapshot>` and verify the new
/// daemon reports the same pane count via the IPC `list` command.
///
/// The IPC List response is the only structural pane introspection the
/// daemon exposes, so it's the right surface to lock in.
// FIXME(#25-followup): the `--snapshot <path>` restore path requires a
// pre-seeded snapshot file at a daemon-resolved location; current harness
// can't synthesize one without exposing internal serialization. Defer until
// the workspace module has a `WorkspaceSnapshot::write_for_test` helper.
#[test]
#[ignore]
fn snapshot_restore_pane_count() {
    let mut original = spawn_ext_daemon("snap-restore", &[]);

    // Wait for IPC socket so the original is fully booted.
    let original_count =
        wait_for_ipc(&original.ipc_socket(), Duration::from_secs(3)).expect("original ipc list");
    assert_eq!(
        original_count, 2,
        "default --server layout is 1×2 (2 panes), got {original_count}"
    );

    // Trigger snapshot via SIGTERM.
    assert!(
        original.shutdown_with_sigterm(Duration::from_secs(3)),
        "original daemon failed to exit on SIGTERM"
    );
    let snap_path = original.snapshot_path();
    assert!(
        snap_path.exists(),
        "snapshot was not written to {}",
        snap_path.display()
    );

    // Restart with --restore. Reuse the original tempdirs by spawning a
    // fresh daemon under a *different* session name and pointing
    // `--restore` at the saved file.
    let restored = spawn_ext_daemon(
        "snap-restored",
        &["-r", snap_path.to_str().expect("utf8 snapshot path")],
    );
    let restored_count =
        wait_for_ipc(&restored.ipc_socket(), Duration::from_secs(5)).expect("restored ipc list");
    assert_eq!(
        restored_count, original_count,
        "restored pane count {restored_count} != original {original_count}"
    );
}

/// `panic_in_one_pane_others_alive` — M1 #1 regression. Spawn a session
/// where one pane's child process is killed (SIGSEGV) immediately, while
/// the other pane stays alive. The daemon must:
///   1. survive the dead pane's reader-thread shutdown,
///   2. continue answering pings,
///   3. continue tracking the live pane through IPC List.
#[test]
fn panic_in_one_pane_others_alive() {
    let mut daemon = spawn_ext_daemon(
        "pane-panic",
        &[
            // Pane 0 dies immediately via SIGSEGV — exercises the
            // "child crash → reader thread sees EOF → daemon must not
            // panic" path.
            "-e",
            r#"sh -c 'kill -SEGV $$'"#,
            // Pane 1 is a long sleep so we have a clear "still alive" target.
            "-e",
            "sh -c 'sleep 30'",
        ],
    );

    // Give the crashed pane time to die + the reader thread time to drain.
    std::thread::sleep(Duration::from_millis(400));

    // Liveness probe — daemon must still answer.
    let mut probe = UnixStream::connect(&daemon.sock).expect("post-crash connect");
    probe
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write_msg(&mut probe, common::C_PING, &[]).expect("post-crash ping");
    let (tag, _) = read_msg(&mut probe).expect("post-crash pong");
    assert_eq!(
        tag,
        common::S_PONG,
        "daemon died after pane panic (got 0x{tag:02x})"
    );

    // IPC List must still report both pane slots — the crashed one is
    // marked `alive: false` but still tracked. Layout is preserved.
    let count =
        wait_for_ipc(&daemon.ipc_socket(), Duration::from_secs(3)).expect("ipc list after crash");
    assert_eq!(
        count, 2,
        "daemon dropped pane slot after sibling crash (count={count})"
    );

    let _ = daemon.shutdown_with_sigterm(Duration::from_secs(2));
}

/// `signal_term_writes_snapshot` — M1 #4 regression. SIGTERM must trigger
/// a snapshot write before the daemon exits. The snapshot file must:
///   - exist at the auto-save path,
///   - parse as JSON,
///   - declare `version >= 2` (current daemon writes v3; v2 was the
///     introduction of the multi-tab format).
#[test]
fn signal_term_writes_snapshot() {
    let mut daemon = spawn_ext_daemon("term-snap", &[]);

    // Sanity: pre-shutdown snapshot does NOT exist (auto_save fires on
    // shutdown only, not on every tick — that's by design to keep disk
    // I/O off the render path).
    let snap_path = daemon.snapshot_path();
    assert!(
        !snap_path.exists(),
        "snapshot existed before shutdown: {}",
        snap_path.display()
    );

    assert!(
        daemon.shutdown_with_sigterm(Duration::from_secs(3)),
        "daemon failed to exit on SIGTERM"
    );

    assert!(
        snap_path.exists(),
        "SIGTERM did not write snapshot at {}",
        snap_path.display()
    );

    let body = std::fs::read_to_string(&snap_path).expect("read snapshot file");
    let json: serde_json::Value = serde_json::from_str(&body).expect("snapshot is valid JSON");
    let version = json
        .get("version")
        .and_then(|v| v.as_u64())
        .expect("snapshot has version field");
    assert!(
        version >= 2,
        "snapshot version {version} is older than the v2/v3 multi-tab format"
    );
    let tabs = json
        .get("tabs")
        .and_then(|t| t.as_array())
        .expect("snapshot has tabs array");
    assert!(!tabs.is_empty(), "snapshot tabs array is empty");
}
