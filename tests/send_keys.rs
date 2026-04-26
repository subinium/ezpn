//! SPEC 06 — `ezpn-ctl send-keys` end-to-end coverage.
//!
//! Spawns a real `ezpn --server` daemon, connects to its JSON-IPC socket,
//! and walks through the load-bearing `send-keys` semantics:
//! - happy path: List → grab a pane id → SendKeys with a chord list
//! - target=current: omitting the pane id resolves to the active pane
//! - error path: pane-not-found
//! - error path: --literal forbids named keys
//!
//! Wire-format layer is exercised inline so wire drift surfaces here even
//! when the in-process serde tests stay green.

mod common;

use common::*;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Same scheme as `ipc::socket_path_for_pid` — duplicated here to keep the
/// test crate decoupled from internal modules.
fn ipc_socket_path(runtime: &Path, pid: u32) -> PathBuf {
    runtime.join(format!("ezpn-{pid}.sock"))
}

/// Wait up to 3 s for the IPC socket to appear (separate from the
/// per-session protocol socket that `spawn_test_daemon` already polls).
fn wait_for_ipc_socket(path: &Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if path.exists() && UnixStream::connect(path).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("ipc socket {} never appeared", path.display());
}

/// Send one IPC request (JSON line) and read one response (JSON line).
fn ipc_call(socket: &Path, request_json: &str) -> serde_json::Value {
    let mut stream = UnixStream::connect(socket).expect("connect ipc socket");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(stream, "{request_json}").expect("write ipc request");
    stream.flush().unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read ipc response");
    serde_json::from_str(line.trim()).expect("parse ipc response json")
}

#[test]
fn send_keys_to_existing_pane_succeeds() {
    let daemon = spawn_test_daemon("sk-ok");
    let ipc_sock = ipc_socket_path(&daemon.runtime, daemon.pid());
    wait_for_ipc_socket(&ipc_sock);

    // 1. List panes — daemon bootstraps with at least one.
    let listed = ipc_call(&ipc_sock, r#"{"cmd":"list"}"#);
    let panes = listed["panes"]
        .as_array()
        .expect("list returns panes array");
    assert!(!panes.is_empty(), "daemon must boot with at least one pane");
    let pane_id = panes[0]["id"].as_u64().expect("pane id is u64") as usize;

    // 2. SendKeys with a valid chord list — expect ok=true and a byte count.
    let req = format!(
        r#"{{"cmd":"send_keys","target":{{"kind":"id","value":{pane_id}}},"keys":["echo","Space","SENDKEYS_OK","Enter"],"literal":false}}"#
    );
    let resp = ipc_call(&ipc_sock, &req);
    assert_eq!(
        resp["ok"].as_bool(),
        Some(true),
        "send-keys must succeed: {resp}"
    );
    let msg = resp["message"].as_str().unwrap_or("");
    assert!(
        msg.starts_with("sent ") && msg.ends_with(" bytes"),
        "expected 'sent N bytes', got {msg:?}"
    );
}

#[test]
fn send_keys_target_current_resolves_active_pane() {
    let daemon = spawn_test_daemon("sk-cur");
    let ipc_sock = ipc_socket_path(&daemon.runtime, daemon.pid());
    wait_for_ipc_socket(&ipc_sock);

    let resp = ipc_call(
        &ipc_sock,
        r#"{"cmd":"send_keys","target":{"kind":"current"},"keys":["a"],"literal":false}"#,
    );
    assert_eq!(
        resp["ok"].as_bool(),
        Some(true),
        "current target must resolve: {resp}"
    );
}

#[test]
fn send_keys_unknown_pane_returns_structured_error() {
    let daemon = spawn_test_daemon("sk-bad");
    let ipc_sock = ipc_socket_path(&daemon.runtime, daemon.pid());
    wait_for_ipc_socket(&ipc_sock);

    let resp = ipc_call(
        &ipc_sock,
        r#"{"cmd":"send_keys","target":{"kind":"id","value":99999},"keys":["a"],"literal":false}"#,
    );
    assert_eq!(resp["ok"].as_bool(), Some(false));
    let err = resp["error"].as_str().unwrap_or("");
    assert!(
        err.contains("pane 99999 not found"),
        "expected 'pane 99999 not found', got {err:?}"
    );
}

#[test]
fn send_keys_literal_rejects_named_token() {
    // Short prefix because macOS sun_path is capped at ~104 bytes — the
    // tempdir prefix plus `ezpn-session-<prefix>-<pid>-<n>.sock` overflows
    // with longer names.
    let daemon = spawn_test_daemon("sk-lit");
    let ipc_sock = ipc_socket_path(&daemon.runtime, daemon.pid());
    wait_for_ipc_socket(&ipc_sock);

    let resp = ipc_call(
        &ipc_sock,
        r#"{"cmd":"send_keys","target":{"kind":"current"},"keys":["Enter"],"literal":true}"#,
    );
    assert_eq!(resp["ok"].as_bool(), Some(false));
    let err = resp["error"].as_str().unwrap_or("");
    assert!(
        err.contains("--literal forbids named keys"),
        "expected 'literal forbids named keys', got {err:?}"
    );
}

#[test]
fn send_keys_empty_payload_rejected() {
    let daemon = spawn_test_daemon("sk-emp");
    let ipc_sock = ipc_socket_path(&daemon.runtime, daemon.pid());
    wait_for_ipc_socket(&ipc_sock);

    let resp = ipc_call(
        &ipc_sock,
        r#"{"cmd":"send_keys","target":{"kind":"current"},"keys":[],"literal":false}"#,
    );
    assert_eq!(resp["ok"].as_bool(), Some(false));
    let err = resp["error"].as_str().unwrap_or("");
    assert!(err.contains("no keys"), "expected 'no keys', got {err:?}");
}
