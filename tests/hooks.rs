//! SPEC 08 — Hooks system end-to-end coverage.
//!
//! Spawns a real `ezpn --server` daemon under a custom XDG_CONFIG_HOME
//! pointing at a tempdir whose `ezpn/config.toml` declares a single
//! `client-attached` hook that touches a sentinel file. Then attaches a
//! client (legacy C_RESIZE) and asserts the sentinel appears.
//!
//! Doesn't reuse `spawn_test_daemon` because that helper hard-codes a
//! minimal env; the config path needs to be redirected per-test.

mod common;

use common::*;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Locate the just-built `ezpn` binary the same way `common::ezpn_bin`
/// does — the macro is in scope for integration tests.
fn ezpn_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ezpn"))
}

/// Spawn `ezpn --server <name>` with both `XDG_RUNTIME_DIR` (per-session
/// socket) AND `XDG_CONFIG_HOME` (per-test config) redirected. Returns
/// owning handles that clean up on drop.
struct ConfiguredDaemon {
    child: Child,
    _runtime: tempfile::TempDir,
    _config: tempfile::TempDir,
    sock: PathBuf,
}

impl ConfiguredDaemon {
    fn spawn(prefix: &str, config_toml: &str) -> Self {
        let runtime = tempfile::tempdir().expect("runtime tempdir");
        let config = tempfile::tempdir().expect("config tempdir");
        let cfg_dir = config.path().join("ezpn");
        std::fs::create_dir_all(&cfg_dir).expect("mkdir ezpn config");
        std::fs::write(cfg_dir.join("config.toml"), config_toml).expect("write config");

        let session = unique_session_name(prefix);
        let child = Command::new(ezpn_path())
            .args(["--server", &session])
            .env("XDG_RUNTIME_DIR", runtime.path())
            .env("XDG_CONFIG_HOME", config.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ezpn --server");

        let sock = socket_path(runtime.path(), &session);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if sock.exists() {
                return Self {
                    child,
                    _runtime: runtime,
                    _config: config,
                    sock,
                };
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut child = child;
        let _ = child.kill();
        let _ = child.wait();
        panic!("daemon socket {} never appeared", sock.display());
    }

    fn sock(&self) -> &Path {
        &self.sock
    }
}

impl Drop for ConfiguredDaemon {
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

#[test]
fn client_attached_hook_fires() {
    // Sentinel lives outside both tempdirs so the daemon's drop can't
    // race it away. The `_<pid>` suffix isolates parallel runs.
    let sentinel_dir = tempfile::tempdir().expect("sentinel tempdir");
    let sentinel = sentinel_dir
        .path()
        .join(format!("client-attached-{}.flag", std::process::id()));
    let sentinel_str = sentinel.to_string_lossy();

    // argv-style command keeps shell quoting out of the picture; the
    // sentinel path can contain spaces from tempfile.
    let config = format!(
        r#"
[[hooks]]
event = "client-attached"
command = ["/usr/bin/touch", "{sentinel_str}"]
timeout_ms = 2000
"#
    );
    let daemon = ConfiguredDaemon::spawn("hk-att", &config);

    // Attach via legacy C_RESIZE (steal mode). Hello first so the
    // server registers our caps; resize triggers accept_client which
    // in turn fires the hook.
    let (mut stream, _) = connect_and_hello(daemon.sock());
    let mut payload = [0u8; 4];
    payload[1] = 80; // cols
    payload[3] = 24; // rows
    write_msg(&mut stream, C_RESIZE_TAG, &payload).expect("send C_RESIZE");

    // Hook runs in a worker thread + spawns /usr/bin/touch — wait a bit.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !sentinel.exists() {
        std::thread::sleep(Duration::from_millis(40));
    }
    assert!(
        sentinel.exists(),
        "client-attached hook must have touched {sentinel:?}"
    );
}

#[test]
fn invalid_hook_does_not_abort_daemon() {
    // Hook with an unknown event name MUST be ignored at load (warn line)
    // rather than killing daemon startup. Verified by spawning the
    // daemon and confirming the socket appears.
    let config = r#"
[[hooks]]
event = "definitely-not-a-real-event"
command = ["true"]
"#;
    let daemon = ConfiguredDaemon::spawn("hk-bad", config);
    // Reaching this point means the daemon spawned the socket within 3 s
    // — i.e. the bad hook didn't crash startup.
    let _ = UnixStream::connect(daemon.sock()).expect("daemon must accept connections");
}

const C_RESIZE_TAG: u8 = 0x03;
