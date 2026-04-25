//! End-to-end daemon lifecycle tests covering the M1 stability work:
//! - C_HELLO version negotiation (#10)
//! - Graceful shutdown via SIGTERM with snapshot save (#11)
//! - Liveness probe (`C_PING` / `S_PONG`)
//!
//! These tests spawn the real `ezpn` binary; they exercise the wire
//! protocol over a real Unix socket, not a mock. That's the only way
//! to catch protocol regressions.

mod common;

use common::*;
use std::time::Duration;

#[test]
fn daemon_responds_to_ping() {
    let daemon = spawn_test_daemon("ping");
    let mut stream =
        std::os::unix::net::UnixStream::connect(&daemon.sock).expect("connect for ping");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write_msg(&mut stream, C_PING, &[]).expect("send C_PING");
    let (tag, _) = read_msg(&mut stream).expect("read pong");
    assert_eq!(tag, S_PONG, "expected S_PONG, got 0x{tag:02x}");
}

#[test]
fn hello_handshake_succeeds_for_v1() {
    let daemon = spawn_test_daemon("hello-ok");
    let (_stream, caps) = connect_and_hello(&daemon.sock);
    // Server caps include kitty kbd + focus events + true color = 0x07.
    // Client requested 0x07 too, so intersection should be 0x07.
    assert_eq!(caps, 0x07, "negotiated caps should be 0x07, got 0x{caps:02x}");
}

#[test]
fn hello_handshake_rejects_wrong_major() {
    let daemon = spawn_test_daemon("hello-bad-version");
    let mut stream =
        std::os::unix::net::UnixStream::connect(&daemon.sock).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    // version=999 is unknown to the server → expect S_HELLO_ERR
    let bad_hello = r#"{"version":999,"capabilities":0,"client":"future-test"}"#;
    write_msg(&mut stream, C_HELLO, bad_hello.as_bytes()).expect("send bad hello");
    let (tag, body) = read_msg(&mut stream).expect("read reply");
    assert_eq!(
        tag, S_HELLO_ERR,
        "expected S_HELLO_ERR for major mismatch, got 0x{tag:02x}"
    );
    let s = String::from_utf8_lossy(&body);
    assert!(
        s.contains("mismatch") || s.contains("upgrade"),
        "error reason should mention mismatch/upgrade, got: {s}"
    );
}

#[test]
fn sigterm_triggers_graceful_shutdown() {
    let mut daemon = spawn_test_daemon("sigterm");
    // Confirm daemon is alive via ping first
    {
        let mut stream =
            std::os::unix::net::UnixStream::connect(&daemon.sock).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write_msg(&mut stream, C_PING, &[]).expect("ping");
        let (tag, _) = read_msg(&mut stream).expect("pong");
        assert_eq!(tag, S_PONG);
    }
    let exited = daemon.shutdown_with_sigterm(Duration::from_secs(3));
    assert!(exited, "daemon failed to exit within 3s of SIGTERM");
    // After graceful shutdown, the socket file should be gone (cleanup ran).
    assert!(
        !daemon.sock.exists(),
        "socket file still present after graceful shutdown: {}",
        daemon.sock.display()
    );
}
