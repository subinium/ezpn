//! SPEC 07 — Event subscription stream end-to-end coverage.
//!
//! Drives a real `ezpn --server` daemon via the binary protocol:
//! 1. C_HELLO → assert `CAP_EVENT_STREAM` is advertised.
//! 2. C_SUBSCRIBE {topics:[client]} → assert `S_SUBSCRIBE_OK` arrives.
//! 3. Open a *second* connection that attaches via `C_RESIZE` (legacy
//!    steal-mode attach, simplest happy path), then detaches.
//! 4. Subscriber should receive `client.attached` then `client.detached`
//!    `S_EVENT` frames.
//!
//! The event payload is parsed inline to keep this crate decoupled from
//! the daemon module tree.

mod common;

use common::*;
use std::io::{BufReader, Read};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

const C_SUBSCRIBE: u8 = 0x08;
const S_SUBSCRIBE_OK: u8 = 0x87;
const S_EVENT: u8 = 0x88;
const CAP_EVENT_STREAM: u32 = 0x0010;

/// Hello with the full capability mask — required because
/// `connect_and_hello` only requests the legacy caps (0x07) and the
/// daemon returns the *intersection*, so CAP_EVENT_STREAM is masked out
/// of the negotiated bits unless the client explicitly asks for it.
fn connect_and_hello_with_event_stream(sock: &Path) -> (UnixStream, u32) {
    let mut stream = UnixStream::connect(sock).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set timeout");
    let hello = r#"{"version":1,"capabilities":23,"client":"events-integration-test"}"#;
    write_msg(&mut stream, C_HELLO, hello.as_bytes()).expect("send C_HELLO");
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let (tag, body) = read_msg(&mut reader).expect("read S_HELLO_OK");
    assert_eq!(tag, S_HELLO_OK, "expected S_HELLO_OK, got 0x{tag:02x}");
    let body_str = String::from_utf8_lossy(&body);
    let needle = "\"capabilities\":";
    let start = body_str.find(needle).expect("caps in hello ok") + needle.len();
    let tail = &body_str[start..];
    let end = tail
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(tail.len());
    let caps: u32 = tail[..end].parse().expect("caps u32");
    (stream, caps)
}

#[test]
fn hello_advertises_cap_event_stream() {
    let daemon = spawn_test_daemon("ev-cap");
    let (_stream, caps) = connect_and_hello_with_event_stream(&daemon.sock);
    assert!(
        caps & CAP_EVENT_STREAM != 0,
        "S_HELLO_OK must intersect CAP_EVENT_STREAM when client requests it (got 0x{:04x})",
        caps
    );
}

#[test]
fn subscribe_returns_subscribe_ok() {
    let daemon = spawn_test_daemon("ev-sub");
    let (mut stream, _) = connect_and_hello(&daemon.sock);

    let req = br#"{"topics":["client"]}"#;
    write_msg(&mut stream, C_SUBSCRIBE, req).unwrap();

    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let (tag, body) = read_msg(&mut reader).expect("read S_SUBSCRIBE_OK");
    assert_eq!(tag, S_SUBSCRIBE_OK, "expected 0x87, got 0x{tag:02x}");

    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        json["subscriber_id"].as_u64().is_some(),
        "ack must carry subscriber_id: {json}"
    );
    assert_eq!(json["topics"], serde_json::json!(["client"]));
}

#[test]
fn subscribe_receives_client_attached_and_detached() {
    let daemon = spawn_test_daemon("ev-cli");

    // ── Subscriber connection ─────────────────────────────────────
    let (sub_stream, _) = connect_and_hello(&daemon.sock);
    let mut sub_writer = sub_stream.try_clone().unwrap();
    write_msg(&mut sub_writer, C_SUBSCRIBE, br#"{"topics":["client"]}"#).unwrap();
    let mut sub_reader = BufReader::new(sub_stream.try_clone().unwrap());
    sub_stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let (ack_tag, _) = read_msg(&mut sub_reader).expect("ack");
    assert_eq!(ack_tag, S_SUBSCRIBE_OK);

    // ── Drive a real attach + detach on a second connection ──────
    // Use legacy C_RESIZE attach (steal mode) — simplest path that does
    // not require us to negotiate AttachRequest JSON in this test.
    let (mut attach_stream, _) = connect_and_hello(&daemon.sock);
    // C_RESIZE payload: 4 bytes [cols_hi cols_lo rows_hi rows_lo].
    let mut resize_payload = [0u8; 4];
    resize_payload[0] = 0;
    resize_payload[1] = 80;
    resize_payload[2] = 0;
    resize_payload[3] = 24;
    write_msg(
        &mut attach_stream,
        0x03, /* C_RESIZE */
        &resize_payload,
    )
    .unwrap();
    // Give the daemon a moment to register the attach + emit the event.
    std::thread::sleep(Duration::from_millis(150));
    // Trigger detach by closing the attach socket.
    drop(attach_stream);
    // Daemon detects disconnect on next loop iteration; allow time for the
    // event to flow through the per-subscriber bounded channel.
    std::thread::sleep(Duration::from_millis(300));

    // ── Read events until we see attached + detached ─────────────
    let mut saw_attached = false;
    let mut saw_detached = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !(saw_attached && saw_detached) && std::time::Instant::now() < deadline {
        match read_msg(&mut sub_reader) {
            Ok((tag, body)) if tag == S_EVENT => {
                let json: serde_json::Value =
                    serde_json::from_slice(&body).expect("event must be JSON");
                let event_type = json["type"].as_str().unwrap_or("");
                match event_type {
                    "client.attached" => saw_attached = true,
                    "client.detached" => saw_detached = true,
                    _ => {} // other client events (e.g. our own subscriber) — ignore
                }
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    assert!(saw_attached, "subscriber must see client.attached");
    assert!(saw_detached, "subscriber must see client.detached");
}

#[test]
fn subscribe_with_empty_topics_is_rejected() {
    let daemon = spawn_test_daemon("ev-empty");
    let (mut stream, _) = connect_and_hello(&daemon.sock);
    write_msg(&mut stream, C_SUBSCRIBE, br#"{"topics":[]}"#).unwrap();

    // Daemon replies S_HELLO_ERR (reused error tag for handshake-class
    // failures) and closes; either we read that frame OR the socket EOFs.
    let mut buf = [0u8; 1];
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    // Just confirm the daemon does NOT send S_SUBSCRIBE_OK.
    if let Ok(n) = stream.read(&mut buf) {
        if n == 0 {
            return; // socket closed — acceptable
        }
        assert_ne!(buf[0], S_SUBSCRIBE_OK, "empty topics must not be acked");
    }
}
