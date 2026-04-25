//! Binary wire protocol for ezpn client-server communication.
//!
//! Wire format: `[u8 tag][u32 big-endian length][payload bytes]`

use std::io::{self, Read, Write};

// ── Client → Server tags ──

/// Crossterm event serialized as JSON.
pub const C_EVENT: u8 = 0x01;
/// Client wants to detach.
pub const C_DETACH: u8 = 0x02;
/// Terminal resize: payload = `[u16 cols BE][u16 rows BE]`.
pub const C_RESIZE: u8 = 0x03;
/// Kill the server (sent by `ezpn kill`).
pub const C_KILL: u8 = 0x04;
/// Lightweight liveness probe (sent by `ezpn ls`). No side effects.
pub const C_PING: u8 = 0x05;
/// Client attach with mode. Payload = JSON `AttachRequest`.
pub const C_ATTACH: u8 = 0x06;
/// Client capability/version handshake. Payload = JSON `HelloMessage`.
/// Optional but strongly recommended — without it the server assumes a
/// "v0" capability-less client (legacy behaviour). On unknown major
/// version, the server replies `S_HELLO_ERR` and closes.
pub const C_HELLO: u8 = 0x07;

// ── Server → Client tags ──

/// Raw terminal output bytes (rendered frame).
pub const S_OUTPUT: u8 = 0x81;
/// Server acknowledges detach.
pub const S_DETACHED: u8 = 0x82;
/// Server is shutting down.
pub const S_EXIT: u8 = 0x83;
/// Pong response to C_PING.
pub const S_PONG: u8 = 0x84;
/// Server accepts the handshake. Payload = JSON `HelloOk`.
pub const S_HELLO_OK: u8 = 0x85;
/// Server rejects the handshake (version mismatch, malformed payload).
/// Payload = JSON `HelloErr`. Connection is closed after sending.
pub const S_HELLO_ERR: u8 = 0x86;

/// Wire-protocol major version. Bump on any backwards-incompatible
/// change to message tags or framing semantics. `S_HELLO_OK` carries
/// the server's version so the client can refuse a mismatch up-front
/// rather than silently misparsing later messages.
pub const PROTOCOL_VERSION: u32 = 1;

/// Capability bits the daemon currently supports. Sent in `S_HELLO_OK`
/// and intersected with the client's bits to determine what features
/// the rest of the session may use (true-color sequences, focus events,
/// etc.). New bits MUST keep their meaning forever — once shipped the
/// bit number is load-bearing.
pub const SERVER_CAPABILITIES: u32 = CAP_KITTY_KEYBOARD | CAP_FOCUS_EVENTS | CAP_TRUE_COLOR;

pub const CAP_KITTY_KEYBOARD: u32 = 0x0001;
pub const CAP_FOCUS_EVENTS: u32 = 0x0002;
pub const CAP_TRUE_COLOR: u32 = 0x0004;
/// Reserved for #14 (scrollback persistence). Not yet emitted by the daemon.
#[allow(dead_code)]
pub const CAP_SCROLLBACK_PERSIST: u32 = 0x0008;

/// Maximum message payload size (16 MB).
const MAX_PAYLOAD: usize = 16 * 1024 * 1024;

/// Write a length-prefixed framed message.
pub fn write_msg(w: &mut impl Write, tag: u8, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("payload too large: {} bytes", payload.len()),
        ));
    }
    let len = (payload.len() as u32).to_be_bytes();
    w.write_all(&[tag])?;
    w.write_all(&len)?;
    if !payload.is_empty() {
        w.write_all(payload)?;
    }
    w.flush()
}

/// Read a length-prefixed framed message. Returns `(tag, payload)`.
pub fn read_msg(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut tag = [0u8; 1];
    r.read_exact(&mut tag)?;
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message too large: {} bytes", len),
        ));
    }
    let mut payload = vec![0u8; len];
    if len > 0 {
        r.read_exact(&mut payload)?;
    }
    Ok((tag[0], payload))
}

/// Encode a terminal resize as 4 bytes: `[cols_hi][cols_lo][rows_hi][rows_lo]`.
pub fn encode_resize(cols: u16, rows: u16) -> [u8; 4] {
    let c = cols.to_be_bytes();
    let r = rows.to_be_bytes();
    [c[0], c[1], r[0], r[1]]
}

/// Decode a terminal resize from 4 bytes.
pub fn decode_resize(payload: &[u8]) -> Option<(u16, u16)> {
    if payload.len() < 4 {
        return None;
    }
    let cols = u16::from_be_bytes([payload[0], payload[1]]);
    let rows = u16::from_be_bytes([payload[2], payload[3]]);
    Some((cols, rows))
}

/// How a client wants to attach to a session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachMode {
    /// Default: detach any existing client (legacy behavior).
    #[default]
    Steal,
    /// Shared session: all clients can send input and see output.
    Shared,
    /// Read-only: client can only observe, no input forwarded.
    Readonly,
}

/// Attach request sent by C_ATTACH.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AttachRequest {
    pub cols: u16,
    pub rows: u16,
    pub mode: AttachMode,
}

/// First message a v0.6+ client sends. Everything else (C_ATTACH /
/// C_RESIZE) follows after the server confirms with `S_HELLO_OK`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HelloMessage {
    /// Major protocol version. Mismatch with `PROTOCOL_VERSION` is
    /// fatal — the server rejects with `S_HELLO_ERR`.
    pub version: u32,
    /// Bitfield of `CAP_*` constants the client supports / wants enabled.
    pub capabilities: u32,
    /// Free-form client identifier ("ezpn 0.6.0", "ezpn-ctl 0.6.0"…).
    /// Used for logging only — never load-bearing.
    pub client: String,
}

/// Response payload for `S_HELLO_OK`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HelloOk {
    pub version: u32,
    /// Intersection of client + server caps. Both sides agree to use
    /// only these features for the rest of the session.
    pub capabilities: u32,
    pub server: String,
}

/// Response payload for `S_HELLO_ERR` — connection is closed after this.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HelloErr {
    pub reason: String,
    /// Server's preferred version, so the client can hint at the upgrade.
    pub server_version: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_request_round_trip() {
        let req = AttachRequest {
            cols: 120,
            rows: 40,
            mode: AttachMode::Shared,
        };
        let json = serde_json::to_vec(&req).unwrap();
        let decoded: AttachRequest = serde_json::from_slice(&json).unwrap();
        assert_eq!(decoded.cols, 120);
        assert_eq!(decoded.rows, 40);
        assert_eq!(decoded.mode, AttachMode::Shared);
    }

    #[test]
    fn attach_mode_default_is_steal() {
        assert_eq!(AttachMode::default(), AttachMode::Steal);
    }

    #[test]
    fn resize_encode_decode() {
        let encoded = encode_resize(200, 50);
        let (cols, rows) = decode_resize(&encoded).unwrap();
        assert_eq!(cols, 200);
        assert_eq!(rows, 50);
    }

    #[test]
    fn framed_message_round_trip() {
        let mut buf: Vec<u8> = Vec::new();
        write_msg(&mut buf, C_EVENT, b"hello").unwrap();
        let (tag, payload) = read_msg(&mut buf.as_slice()).unwrap();
        assert_eq!(tag, C_EVENT);
        assert_eq!(payload, b"hello");
    }

    #[test]
    fn hello_message_round_trip() {
        let hello = HelloMessage {
            version: PROTOCOL_VERSION,
            capabilities: CAP_KITTY_KEYBOARD | CAP_TRUE_COLOR,
            client: "ezpn-test".to_string(),
        };
        let json = serde_json::to_vec(&hello).unwrap();
        let decoded: HelloMessage = serde_json::from_slice(&json).unwrap();
        assert_eq!(decoded.version, PROTOCOL_VERSION);
        assert_eq!(
            decoded.capabilities & CAP_KITTY_KEYBOARD,
            CAP_KITTY_KEYBOARD
        );
        assert_eq!(decoded.client, "ezpn-test");
    }

    #[test]
    fn hello_ok_carries_intersection() {
        let server_caps = SERVER_CAPABILITIES;
        let client_caps = CAP_KITTY_KEYBOARD | CAP_SCROLLBACK_PERSIST; // unknown bit set
        let agreed = server_caps & client_caps;
        // We agree on what BOTH sides know. Client requesting a future bit
        // (SCROLLBACK_PERSIST) must not magically enable it server-side.
        assert_eq!(agreed, CAP_KITTY_KEYBOARD);
    }

    #[test]
    fn hello_err_includes_server_version() {
        let err = HelloErr {
            reason: "client/server major mismatch".to_string(),
            server_version: PROTOCOL_VERSION,
        };
        let json = serde_json::to_vec(&err).unwrap();
        let decoded: HelloErr = serde_json::from_slice(&json).unwrap();
        assert_eq!(decoded.server_version, PROTOCOL_VERSION);
        assert!(decoded.reason.contains("mismatch"));
    }

    #[test]
    fn hello_tags_distinct_from_existing() {
        // Defensive: catch accidental tag collisions when constants are added.
        let tags = [
            C_EVENT, C_DETACH, C_RESIZE, C_KILL, C_PING, C_ATTACH, C_HELLO,
        ];
        let mut sorted = tags.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), tags.len(), "client tag collision");

        let stags = [
            S_OUTPUT,
            S_DETACHED,
            S_EXIT,
            S_PONG,
            S_HELLO_OK,
            S_HELLO_ERR,
        ];
        let mut sorted_s = stags.to_vec();
        sorted_s.sort_unstable();
        sorted_s.dedup();
        assert_eq!(sorted_s.len(), stags.len(), "server tag collision");
    }
}
