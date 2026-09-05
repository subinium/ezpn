//! Binary wire protocol for ezpn client-server communication.
//!
//! Wire format: `[u8 tag][u32 big-endian length][payload bytes]`
//!
//! # Version negotiation
//!
//! Every authenticated connection emits an [`S_VERSION`] frame first.
//! Negotiating clients reply with [`C_HELLO`] before attaching; v1 framed
//! attach requests and out-of-band ping/kill requests remain supported.
//! If the major version disagrees the server replies with
//! [`S_INCOMPAT`] and closes the connection. Minor-version mismatch is
//! tolerated: additive payload fields are forward-compatible.
//!
//! Tag space `0x10..=0x1F` is reserved for future negotiation extensions
//! (capability re-negotiation, auth challenges, etc.).

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

// ── Server → Client tags ──

/// Raw terminal output bytes (rendered frame).
pub const S_OUTPUT: u8 = 0x81;
/// Server acknowledges detach.
pub const S_DETACHED: u8 = 0x82;
/// Server is shutting down.
pub const S_EXIT: u8 = 0x83;
/// Pong response to C_PING.
pub const S_PONG: u8 = 0x84;

// ── Version negotiation tags (reserved range 0x10..=0x1F) ──

/// Server hello. Payload = JSON [`ServerHello`]. First frame on every
/// server-accepted connection.
pub const S_VERSION: u8 = 0x10;
/// Client hello. Payload = JSON [`ClientHello`]. Sent in response to
/// [`S_VERSION`] before any other client frame.
pub const C_HELLO: u8 = 0x11;
/// Server-emitted incompatibility notice. Payload = JSON [`IncompatNotice`].
/// Sent when the server cannot speak the client's protocol (major mismatch
/// or legacy client). Server closes the connection immediately after.
pub const S_INCOMPAT: u8 = 0x12;

/// Current wire protocol major version. Bumped only for breaking changes.
pub const PROTO_MAJOR: u16 = 1;
/// Current wire protocol minor version. Bumped for additive changes.
pub const PROTO_MINOR: u16 = 0;

/// Maximum message payload size (16 MB).
pub const MAX_PAYLOAD: usize = 16 * 1024 * 1024;
const READ_WINDOW: usize = 8 * 1024;
const GROWTH_WINDOW: usize = 64 * 1024;

/// A socket operation budget shared by every read/write in one exchange.
/// Socket timeouts alone restart after each partial transfer.
#[cfg(unix)]
pub struct DeadlineStream<'a> {
    stream: &'a std::os::unix::net::UnixStream,
    deadline: std::time::Instant,
}

#[cfg(unix)]
impl<'a> DeadlineStream<'a> {
    pub fn new(stream: &'a std::os::unix::net::UnixStream, timeout: std::time::Duration) -> Self {
        Self {
            stream,
            deadline: std::time::Instant::now() + timeout,
        }
    }

    fn remaining(&self) -> io::Result<std::time::Duration> {
        self.deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "socket operation deadline exceeded",
                )
            })
    }

    fn wait(&self, events: libc::c_short) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        loop {
            let timeout = self.remaining()?.as_millis().max(1).min(i32::MAX as u128) as i32;
            let mut poll = libc::pollfd {
                fd: self.stream.as_raw_fd(),
                events,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll, 1, timeout) };
            if ready > 0 {
                return Ok(());
            }
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
    }
}

#[cfg(unix)]
impl Read for DeadlineStream<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use std::os::fd::AsRawFd;
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            self.wait(libc::POLLIN)?;
            // Per-call nonblocking I/O preserves the other half's flags.
            // Repeated SO_RCVTIMEO updates fail with EINVAL on macOS after
            // peer close even while received bytes are still buffered.
            let n = unsafe {
                libc::recv(
                    self.stream.as_raw_fd(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            if n >= 0 {
                return Ok(n as usize);
            }
            let error = io::Error::last_os_error();
            if !matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) {
                return Err(error);
            }
        }
    }
}

#[cfg(unix)]
impl Write for DeadlineStream<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        use std::os::fd::AsRawFd;
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            self.wait(libc::POLLOUT)?;
            let n = unsafe {
                libc::send(
                    self.stream.as_raw_fd(),
                    buf.as_ptr().cast(),
                    buf.len(),
                    libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                )
            };
            if n >= 0 {
                return Ok(n as usize);
            }
            let error = io::Error::last_os_error();
            if !matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) {
                return Err(error);
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// Idle connections may wait indefinitely, but a started frame must finish.
#[cfg(unix)]
pub fn read_socket_msg(stream: &std::os::unix::net::UnixStream) -> io::Result<(u8, Vec<u8>)> {
    let mut first = [0];
    (&*stream).read_exact(&mut first)?;
    let deadline = DeadlineStream::new(stream, std::time::Duration::from_secs(2));
    read_msg(&mut first.as_slice().chain(deadline))
}

/// Clamp dimensions before allocating terminal grids, not in the wire codec.
pub fn normalize_size(cols: u16, rows: u16) -> (u16, u16) {
    (cols.clamp(1, 1000), rows.clamp(1, 500))
}

/// Write a length-prefixed framed message.
pub fn write_msg(w: &mut impl Write, tag: u8, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("payload too large: {} bytes", payload.len()),
        ));
    }
    let len = (payload.len() as u32).to_be_bytes();
    w.write_all(&[tag, len[0], len[1], len[2], len[3]])?;
    if !payload.is_empty() {
        w.write_all(payload)?;
    }
    w.flush()
}

/// Read a length-prefixed framed message. Returns `(tag, payload)`.
#[inline]
pub fn read_msg(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    read_msg_limited(r, MAX_PAYLOAD)
}

/// Limit control-plane frames without changing the v1 data-plane cap.
#[inline]
pub fn read_msg_limited(r: &mut impl Read, limit: usize) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 5];
    r.read_exact(&mut header)?;
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > limit.min(MAX_PAYLOAD) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message too large: {} bytes", len),
        ));
    }
    // A small advertised frame reserves at most one bounded control-size
    // buffer. Large advertised frames still need payload progress to grow.
    if len <= GROWTH_WINDOW {
        let mut payload = vec![0u8; len];
        r.read_exact(&mut payload)?;
        return Ok((header[0], payload));
    }
    Ok((header[0], read_large_payload(r, len)?))
}

#[cold]
fn read_large_payload(r: &mut impl Read, len: usize) -> io::Result<Vec<u8>> {
    // A header alone reserves at most 8 KiB. Only after that prefix arrives
    // may capacity grow, always at most 64 KiB beyond completed reads.
    let mut payload = vec![0u8; READ_WINDOW];
    r.read_exact(&mut payload)?;
    while payload.len() < len {
        let received = payload.len();
        let next_len = (received + GROWTH_WINDOW).min(len);
        // Avoid Vec's geometric capacity growth getting ahead of received data.
        payload.reserve_exact(next_len - received);
        payload.resize(next_len, 0);
        r.read_exact(&mut payload[received..])?;
    }
    Ok(payload)
}

/// Encode a terminal resize as 4 bytes: `[cols_hi][cols_lo][rows_hi][rows_lo]`.
pub fn encode_resize(cols: u16, rows: u16) -> [u8; 4] {
    let c = cols.to_be_bytes();
    let r = rows.to_be_bytes();
    [c[0], c[1], r[0], r[1]]
}

/// Decode a terminal resize from 4 bytes.
pub fn decode_resize(payload: &[u8]) -> Option<(u16, u16)> {
    if payload.len() != 4 {
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

// ── Version handshake payloads ──

/// Payload of [`S_VERSION`] (first server frame on every connection).
///
/// Forward-compat rule: clients MUST tolerate unknown additive fields, and
/// servers MUST tolerate missing optional fields when parsing future
/// extensions. Required fields below MUST never be removed without a
/// [`PROTO_MAJOR`] bump.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServerHello {
    pub proto_major: u16,
    pub proto_minor: u16,
    /// Human-readable build identifier, e.g. `"ezpn 0.12.0 (rev abc1234)"`.
    pub build: String,
}

/// Payload of [`C_HELLO`] (client's reply to [`S_VERSION`]).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClientHello {
    pub proto_major: u16,
    pub proto_minor: u16,
    /// Human-readable client build identifier.
    pub client_build: String,
    /// Capability flags the client understands. The server may opt to
    /// stream extra payload formats only when a known flag is present.
    pub supported_features: Vec<String>,
}

/// Payload of [`S_INCOMPAT`].
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IncompatNotice {
    /// `"<major>.<minor>"` of the server.
    pub server_proto: String,
    /// `"<major>.<minor>"` of the client (best-effort; may be `"unknown"`
    /// if the client never sent a [`C_HELLO`], e.g. legacy v0.5 clients).
    pub client_proto: String,
    /// Human-readable explanation suitable for direct display to the user.
    pub message: String,
}

/// Capability strings the current client advertises in [`ClientHello`].
///
/// Server code uses these to gate optional output formats. Adding a new
/// flag is additive and does NOT require a [`PROTO_MINOR`] bump unless the
/// flag is mandatory.
pub const CLIENT_FEATURES: &[&str] = &["scrollback-v3", "kitty-kbd-stack", "osc-52-confirm"];

/// Build the canonical `build` string for [`ServerHello`].
///
/// NOTE: the git revision is currently not captured at build time (no
/// `build.rs`). Callers who want the rev should pass it explicitly.
/// See issue #57 for the follow-up to wire `git rev-parse --short HEAD`
/// through a build script.
pub fn build_string(rev: Option<&str>) -> String {
    match rev {
        Some(sha) if !sha.is_empty() => {
            format!("ezpn {} (rev {})", env!("CARGO_PKG_VERSION"), sha)
        }
        _ => format!("ezpn {} (rev unknown)", env!("CARGO_PKG_VERSION")),
    }
}

/// Encode the canonical [`S_VERSION`] frame the server sends as its first
/// message. Returns the framed bytes ready to be written to the socket.
///
/// The server-side `accept` loop should call this immediately after
/// `listener.accept()` succeeds and before reading anything from the
/// client.
#[allow(dead_code)] // wired by the server-side commit follow-up to #57
pub fn server_hello() -> Vec<u8> {
    let hello = ServerHello {
        proto_major: PROTO_MAJOR,
        proto_minor: PROTO_MINOR,
        build: build_string(None),
    };
    let json = serde_json::to_vec(&hello).expect("ServerHello serialization is infallible");
    let mut buf = Vec::with_capacity(5 + json.len());
    write_msg(&mut buf, S_VERSION, &json).expect("writing to Vec<u8> never fails");
    buf
}

/// Outcome of inspecting the first byte received on a server-side socket
/// after `S_VERSION` has been written.
///
/// We classify pre-handshake bytes into three buckets so the server can
/// emit a friendly [`S_INCOMPAT`] for legacy v0.5 clients that don't
/// understand version negotiation and just dump an `AttachRequest` JSON
/// blob immediately.
///
/// Variants are `#[allow(dead_code)]` because the matching server-side
/// dispatch lands in a follow-up commit (the `accept` loop in
/// `src/server.rs` is owned by another agent in the v0.12 split).
#[allow(dead_code)] // wired by the server-side commit follow-up to #57
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstByteKind {
    /// Looks like a tag in the negotiation/normal range (`0x00..=0x20`).
    /// Server should proceed with `read_msg` as normal.
    Tag,
    /// Looks like raw JSON (`{`, `[`, whitespace) — almost certainly a
    /// legacy client that skipped the handshake. Server should reply with
    /// a friendly [`S_INCOMPAT`] and close.
    LegacyJson,
    /// Anything else — treat as a protocol violation and close.
    Unknown,
}

/// Heuristic: classify the first byte received on a freshly-accepted
/// connection. The current tag space tops out at `0x20`; legacy v0.5
/// clients send `AttachRequest` JSON, whose first byte is always `{`
/// (`0x7b`) — well above the tag range. We also treat `[` and ASCII
/// whitespace as legacy markers because Serde happily consumes leading
/// whitespace.
#[allow(dead_code)] // wired by the server-side commit follow-up to #57
pub fn classify_first_byte(b: u8) -> FirstByteKind {
    match b {
        b' ' | b'\t' | b'\r' | b'\n' => FirstByteKind::LegacyJson,
        0x00..=0x20 => FirstByteKind::Tag,
        b'{' | b'[' => FirstByteKind::LegacyJson,
        _ => FirstByteKind::Unknown,
    }
}

/// Build an [`S_INCOMPAT`] frame for a major-version mismatch. The
/// `message` follows the spec'd UX template:
/// `client v<X.Y> cannot attach to server v<A.B> — restart the daemon
/// with 'ezpn kill <name>' to upgrade.`
#[allow(dead_code)] // wired by the server-side commit follow-up to #57
pub fn incompat_for_major_mismatch(client: &ClientHello, session_name: &str) -> Vec<u8> {
    let server_proto = format!("{}.{}", PROTO_MAJOR, PROTO_MINOR);
    let client_proto = format!("{}.{}", client.proto_major, client.proto_minor);
    let message = format!(
        "client v{} cannot attach to server v{} \u{2014} restart the daemon with 'ezpn kill {}' to upgrade.",
        client_proto, server_proto, session_name
    );
    let notice = IncompatNotice {
        server_proto,
        client_proto,
        message,
    };
    encode_incompat(&notice)
}

/// Build an [`S_INCOMPAT`] frame for a legacy v0.5 client that didn't
/// send a [`C_HELLO`].
#[allow(dead_code)] // wired by the server-side commit follow-up to #57
pub fn incompat_for_legacy_client(session_name: &str) -> Vec<u8> {
    let server_proto = format!("{}.{}", PROTO_MAJOR, PROTO_MINOR);
    let message = format!(
        "legacy client detected (no version handshake) \u{2014} restart the daemon with 'ezpn kill {}' to upgrade.",
        session_name
    );
    let notice = IncompatNotice {
        server_proto,
        client_proto: "unknown".to_string(),
        message,
    };
    encode_incompat(&notice)
}

#[allow(dead_code)] // helper used by `incompat_for_*` (wired in a follow-up)
fn encode_incompat(notice: &IncompatNotice) -> Vec<u8> {
    let json = serde_json::to_vec(notice).expect("IncompatNotice serialization is infallible");
    let mut buf = Vec::with_capacity(5 + json.len());
    write_msg(&mut buf, S_INCOMPAT, &json).expect("writing to Vec<u8> never fails");
    buf
}

/// Result of the client-side handshake.
#[derive(Debug)]
pub enum HandshakeOutcome {
    /// Server and client are compatible; carries the parsed [`ServerHello`]
    /// for the caller to log or expose via `ezpn ls`.
    Ok(ServerHello),
    /// Server replied with [`S_INCOMPAT`] (or our heuristic detected one).
    /// Caller should print [`IncompatNotice::message`] and exit non-zero.
    Incompat(IncompatNotice),
}

/// Drive the client-side version handshake on a freshly-connected stream.
///
/// Reads the first frame (must be [`S_VERSION`]), then writes a
/// [`C_HELLO`] advertising [`PROTO_MAJOR`] / [`PROTO_MINOR`] and
/// [`CLIENT_FEATURES`]. An initial [`S_INCOMPAT`] is returned immediately.
/// There is no v1 hello acknowledgement; callers must also handle a later
/// [`S_INCOMPAT`] in their normal message loop.
///
/// A major-version mismatch where the server's major is greater than
/// ours is reported as [`HandshakeOutcome::Incompat`] synthesized
/// locally — the server's [`S_INCOMPAT`] frame is preferred when present
/// because it carries the canonical message string.
pub fn client_handshake<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
) -> io::Result<HandshakeOutcome> {
    let (tag, payload) = read_msg(reader)?;
    match tag {
        S_VERSION => {
            let server: ServerHello = serde_json::from_slice(&payload).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("malformed S_VERSION payload: {}", e),
                )
            })?;

            // Major mismatch: don't bother sending C_HELLO — the server
            // either can't parse it or will refuse us anyway.
            if server.proto_major != PROTO_MAJOR {
                return Ok(HandshakeOutcome::Incompat(IncompatNotice {
                    server_proto: format!("{}.{}", server.proto_major, server.proto_minor),
                    client_proto: format!("{}.{}", PROTO_MAJOR, PROTO_MINOR),
                    message: format!(
                        "client v{}.{} cannot attach to server v{}.{} \u{2014} reinstall the matching ezpn binary or restart the daemon.",
                        PROTO_MAJOR, PROTO_MINOR, server.proto_major, server.proto_minor
                    ),
                }));
            }

            // Minor-version mismatch (additive, forward-compat) is fine.
            let hello = ClientHello {
                proto_major: PROTO_MAJOR,
                proto_minor: PROTO_MINOR,
                client_build: build_string(None),
                supported_features: CLIENT_FEATURES.iter().map(|s| s.to_string()).collect(),
            };
            let json = serde_json::to_vec(&hello).map_err(io::Error::other)?;
            write_msg(writer, C_HELLO, &json)?;
            Ok(HandshakeOutcome::Ok(server))
        }
        S_INCOMPAT => {
            let notice: IncompatNotice = serde_json::from_slice(&payload).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("malformed S_INCOMPAT payload: {}", e),
                )
            })?;
            Ok(HandshakeOutcome::Incompat(notice))
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "expected S_VERSION (0x10) or S_INCOMPAT (0x12) as first frame, got 0x{:02x}",
                other
            ),
        )),
    }
}

/// Parse a [`C_HELLO`] payload. Returned for use by the server (in a
/// follow-up commit).
#[allow(dead_code)] // wired by the server-side commit follow-up to #57
pub fn parse_client_hello(payload: &[u8]) -> io::Result<ClientHello> {
    serde_json::from_slice(payload).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("malformed C_HELLO payload: {}", e),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_payload_windows_preserve_boundaries_and_following_frames() {
        for size in [
            0,
            1,
            READ_WINDOW - 1,
            READ_WINDOW,
            READ_WINDOW + 1,
            READ_WINDOW * 8 + 1,
            READ_WINDOW + GROWTH_WINDOW - 1,
            READ_WINDOW + GROWTH_WINDOW,
            READ_WINDOW + GROWTH_WINDOW + 1,
            READ_WINDOW + GROWTH_WINDOW * 2 + 1,
        ] {
            let payload: Vec<u8> = (0..size).map(|index| index as u8).collect();
            let mut wire = Vec::new();
            write_msg(&mut wire, S_OUTPUT, &payload).unwrap();
            write_msg(&mut wire, S_EXIT, &[]).unwrap();
            let mut reader = std::io::Cursor::new(wire);
            assert_eq!(read_msg(&mut reader).unwrap(), (S_OUTPUT, payload));
            assert_eq!(read_msg(&mut reader).unwrap(), (S_EXIT, Vec::new()));
        }
    }

    #[test]
    fn header_only_large_frame_requests_at_most_one_initialized_window() {
        struct HeaderOnly {
            header: std::io::Cursor<[u8; 5]>,
            payload_requested: usize,
        }
        impl Read for HeaderOnly {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if self.header.position() < 5 {
                    return self.header.read(buffer);
                }
                self.payload_requested = buffer.len();
                assert!(buffer.len() <= READ_WINDOW);
                assert!(buffer.iter().all(|byte| *byte == 0));
                Ok(0)
            }
        }
        let length = (MAX_PAYLOAD as u32).to_be_bytes();
        let mut reader = HeaderOnly {
            header: std::io::Cursor::new([S_OUTPUT, length[0], length[1], length[2], length[3]]),
            payload_requested: 0,
        };
        assert_eq!(
            read_msg(&mut reader).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(reader.payload_requested, READ_WINDOW);
    }

    #[test]
    fn header_only_small_frame_allocation_is_bounded_to_64k() {
        struct HeaderOnly {
            header: std::io::Cursor<[u8; 5]>,
            requested: usize,
        }
        impl Read for HeaderOnly {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                if self.header.position() < 5 {
                    return self.header.read(bytes);
                }
                self.requested = bytes.len();
                assert!(bytes.len() <= GROWTH_WINDOW);
                assert!(bytes.iter().all(|byte| *byte == 0));
                Ok(0)
            }
        }
        let length = (GROWTH_WINDOW as u32).to_be_bytes();
        let mut reader = HeaderOnly {
            header: std::io::Cursor::new([S_OUTPUT, length[0], length[1], length[2], length[3]]),
            requested: 0,
        };
        assert_eq!(
            read_msg(&mut reader).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(reader.requested, GROWTH_WINDOW);
    }

    #[test]
    fn truncated_small_and_large_payloads_are_not_accepted() {
        for size in [1, READ_WINDOW, READ_WINDOW + 1, READ_WINDOW * 2 + 1] {
            let mut wire = Vec::new();
            write_msg(&mut wire, S_OUTPUT, &vec![b'x'; size]).unwrap();
            wire.pop();
            assert_eq!(
                read_msg(&mut wire.as_slice()).unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof
            );
        }
    }

    #[test]
    fn large_frame_growth_requires_payload_progress_and_stays_bounded() {
        struct PrefixOnly {
            header: std::io::Cursor<[u8; 5]>,
            delivered: usize,
            requests: Vec<usize>,
        }
        impl Read for PrefixOnly {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if self.header.position() < 5 {
                    return self.header.read(buffer);
                }
                self.requests.push(buffer.len());
                assert!(buffer.iter().all(|byte| *byte == 0));
                if self.delivered == 0 {
                    assert!(buffer.len() <= READ_WINDOW);
                    buffer.fill(b'x');
                    self.delivered += buffer.len();
                    Ok(buffer.len())
                } else {
                    assert!(buffer.len() <= GROWTH_WINDOW);
                    Ok(0)
                }
            }
        }
        let length = (MAX_PAYLOAD as u32).to_be_bytes();
        let mut reader = PrefixOnly {
            header: std::io::Cursor::new([S_OUTPUT, length[0], length[1], length[2], length[3]]),
            delivered: 0,
            requests: Vec::new(),
        };
        assert_eq!(
            read_msg(&mut reader).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(reader.requests, [READ_WINDOW, GROWTH_WINDOW]);
        assert_eq!(reader.delivered, READ_WINDOW);
    }

    #[test]
    fn per_call_limit_rejects_payload_before_reading_body() {
        let wire = [S_OUTPUT, 0, 0, 0, 9];
        assert_eq!(
            read_msg_limited(&mut wire.as_slice(), 8)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        let wire = [S_OUTPUT, 1, 0, 0, 1];
        assert_eq!(
            read_msg_limited(&mut wire.as_slice(), usize::MAX)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn framed_header_is_batched_and_handles_short_writes() {
        #[derive(Default)]
        struct Writer {
            bytes: Vec<u8>,
            requested: Vec<usize>,
        }
        impl Write for Writer {
            fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
                self.requested.push(buffer.len());
                let count = buffer.len().min(2);
                self.bytes.extend_from_slice(&buffer[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut writer = Writer::default();
        write_msg(&mut writer, S_OUTPUT, b"abc").unwrap();
        assert_eq!(writer.requested[0], 5);
        assert_eq!(writer.bytes, [S_OUTPUT, 0, 0, 0, 3, b'a', b'b', b'c']);
    }

    #[cfg(unix)]
    #[test]
    fn deadline_reader_drains_frames_after_peer_close() {
        let (client, mut server) = std::os::unix::net::UnixStream::pair().unwrap();
        write_msg(&mut server, S_OUTPUT, b"last output").unwrap();
        write_msg(&mut server, S_EXIT, &[]).unwrap();
        drop(server);
        let mut io = DeadlineStream::new(&client, std::time::Duration::from_secs(1));
        assert_eq!(
            read_msg(&mut io).unwrap(),
            (S_OUTPUT, b"last output".to_vec())
        );
        assert_eq!(read_msg(&mut io).unwrap().0, S_EXIT);
        assert_eq!(
            read_msg(&mut io).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[cfg(unix)]
    #[test]
    fn socket_deadline_is_not_reset_by_partial_reads() {
        use std::time::{Duration, Instant};
        let (client, mut server) = std::os::unix::net::UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let frame = [S_OUTPUT, 0, 0, 0, 20];
            server.write_all(&frame).unwrap();
            for _ in 0..20 {
                if server.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        });
        let start = Instant::now();
        assert!(read_msg(&mut DeadlineStream::new(&client, Duration::from_millis(50))).is_err());
        assert!(start.elapsed() < Duration::from_millis(250));
        drop(client);
        worker.join().unwrap();
    }

    #[test]
    fn resize_rejects_trailing_bytes() {
        assert_eq!(decode_resize(&[0, 80, 0, 24, 1]), None);
    }

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

    // ── version handshake tests ──

    #[test]
    fn s_version_frame_round_trip() {
        let buf = server_hello();
        // First byte is the tag.
        assert_eq!(buf[0], S_VERSION);
        let (tag, payload) = read_msg(&mut buf.as_slice()).unwrap();
        assert_eq!(tag, S_VERSION);
        let parsed: ServerHello = serde_json::from_slice(&payload).unwrap();
        assert_eq!(parsed.proto_major, PROTO_MAJOR);
        assert_eq!(parsed.proto_minor, PROTO_MINOR);
        assert!(parsed.build.starts_with("ezpn "));
    }

    #[test]
    fn server_hello_tag_is_in_reserved_range() {
        // Spec reserves 0x10..=0x1F for negotiation tags.
        assert!((0x10..=0x1F).contains(&S_VERSION));
        assert!((0x10..=0x1F).contains(&C_HELLO));
        assert!((0x10..=0x1F).contains(&S_INCOMPAT));
    }

    #[test]
    fn c_hello_payload_round_trip() {
        let hello = ClientHello {
            proto_major: 1,
            proto_minor: 0,
            client_build: "ezpn 0.12.0 (rev test)".into(),
            supported_features: vec![
                "scrollback-v3".into(),
                "kitty-kbd-stack".into(),
                "osc-52-confirm".into(),
            ],
        };
        let json = serde_json::to_vec(&hello).unwrap();
        let parsed = parse_client_hello(&json).unwrap();
        assert_eq!(parsed, hello);
    }

    #[test]
    fn c_hello_tolerates_unknown_additive_fields() {
        // Forward-compat: a future client might add fields. The server
        // must still accept the hello as long as required fields exist.
        let json = br#"{
            "proto_major": 1,
            "proto_minor": 7,
            "client_build": "ezpn 99.0.0 (rev future)",
            "supported_features": ["scrollback-v3"],
            "future_field": {"nested": true}
        }"#;
        let parsed = parse_client_hello(json).unwrap();
        assert_eq!(parsed.proto_minor, 7);
        assert_eq!(parsed.supported_features, vec!["scrollback-v3".to_string()]);
    }

    #[test]
    fn major_mismatch_emits_incompat() {
        let client = ClientHello {
            proto_major: PROTO_MAJOR + 1,
            proto_minor: 0,
            client_build: "ezpn 1.0.0".into(),
            supported_features: vec![],
        };
        let frame = incompat_for_major_mismatch(&client, "myproj");
        assert_eq!(frame[0], S_INCOMPAT);
        let (tag, payload) = read_msg(&mut frame.as_slice()).unwrap();
        assert_eq!(tag, S_INCOMPAT);
        let notice: IncompatNotice = serde_json::from_slice(&payload).unwrap();
        assert_eq!(
            notice.server_proto,
            format!("{}.{}", PROTO_MAJOR, PROTO_MINOR)
        );
        assert_eq!(notice.client_proto, format!("{}.0", PROTO_MAJOR + 1));
        assert!(notice.message.contains("ezpn kill myproj"));
        assert!(notice.message.contains("cannot attach"));
    }

    #[test]
    fn minor_mismatch_is_tolerated_by_client_handshake() {
        // Server pretends to be (PROTO_MAJOR, PROTO_MINOR + 1).
        let server = ServerHello {
            proto_major: PROTO_MAJOR,
            proto_minor: PROTO_MINOR + 1,
            build: format!("ezpn {} (rev future)", env!("CARGO_PKG_VERSION")),
        };
        let mut server_to_client: Vec<u8> = Vec::new();
        let json = serde_json::to_vec(&server).unwrap();
        write_msg(&mut server_to_client, S_VERSION, &json).unwrap();

        let mut client_to_server: Vec<u8> = Vec::new();
        let outcome =
            client_handshake(&mut server_to_client.as_slice(), &mut client_to_server).unwrap();

        match outcome {
            HandshakeOutcome::Ok(parsed) => {
                assert_eq!(parsed.proto_minor, PROTO_MINOR + 1);
            }
            HandshakeOutcome::Incompat(n) => {
                panic!("minor mismatch must NOT be reported as incompat: {:?}", n);
            }
        }

        // Client must have written a C_HELLO frame in response.
        let (tag, payload) = read_msg(&mut client_to_server.as_slice()).unwrap();
        assert_eq!(tag, C_HELLO);
        let hello = parse_client_hello(&payload).unwrap();
        assert_eq!(hello.proto_major, PROTO_MAJOR);
        assert_eq!(hello.proto_minor, PROTO_MINOR);
        assert!(hello
            .supported_features
            .contains(&"scrollback-v3".to_string()));
    }

    #[test]
    fn major_mismatch_short_circuits_client_handshake() {
        // Server claims major = PROTO_MAJOR + 1 — client must NOT send
        // C_HELLO and must report Incompat.
        let server = ServerHello {
            proto_major: PROTO_MAJOR + 1,
            proto_minor: 0,
            build: "ezpn future".into(),
        };
        let mut server_to_client: Vec<u8> = Vec::new();
        let json = serde_json::to_vec(&server).unwrap();
        write_msg(&mut server_to_client, S_VERSION, &json).unwrap();

        let mut client_to_server: Vec<u8> = Vec::new();
        let outcome =
            client_handshake(&mut server_to_client.as_slice(), &mut client_to_server).unwrap();

        assert!(
            client_to_server.is_empty(),
            "client must not send C_HELLO on major mismatch"
        );
        match outcome {
            HandshakeOutcome::Incompat(notice) => {
                assert_eq!(notice.server_proto, format!("{}.0", PROTO_MAJOR + 1));
                assert_eq!(
                    notice.client_proto,
                    format!("{}.{}", PROTO_MAJOR, PROTO_MINOR)
                );
            }
            HandshakeOutcome::Ok(_) => panic!("expected Incompat for major mismatch"),
        }
    }

    #[test]
    fn server_pushed_incompat_is_surfaced() {
        // Server skips S_VERSION (e.g. detected legacy first byte from a
        // different connection in spec — here we just simulate a server
        // that pushes S_INCOMPAT outright).
        let frame = incompat_for_legacy_client("demo");
        let mut client_to_server: Vec<u8> = Vec::new();
        let outcome = client_handshake(&mut frame.as_slice(), &mut client_to_server).unwrap();
        match outcome {
            HandshakeOutcome::Incompat(notice) => {
                assert!(notice.message.contains("legacy client detected"));
                assert!(notice.message.contains("ezpn kill demo"));
            }
            HandshakeOutcome::Ok(_) => panic!("expected Incompat from server-pushed S_INCOMPAT"),
        }
    }

    #[test]
    fn legacy_first_byte_classification() {
        // Real tags fall in 0x00..=0x20. Any current/future negotiation
        // tag is also in that range (we reserved 0x10..=0x1F).
        assert_eq!(classify_first_byte(C_EVENT), FirstByteKind::Tag);
        assert_eq!(classify_first_byte(C_RESIZE), FirstByteKind::Tag);
        assert_eq!(classify_first_byte(C_ATTACH), FirstByteKind::Tag);
        assert_eq!(classify_first_byte(S_VERSION), FirstByteKind::Tag);
        assert_eq!(classify_first_byte(C_HELLO), FirstByteKind::Tag);

        // Legacy v0.5 client dumps `AttachRequest` JSON — first byte `{`.
        assert_eq!(classify_first_byte(b'{'), FirstByteKind::LegacyJson);
        // JSON arrays would also be a legacy marker.
        assert_eq!(classify_first_byte(b'['), FirstByteKind::LegacyJson);
        assert_eq!(classify_first_byte(b' '), FirstByteKind::LegacyJson);
        assert_eq!(classify_first_byte(b'\n'), FirstByteKind::LegacyJson);

        // Anything else: protocol violation.
        assert_eq!(classify_first_byte(b'A'), FirstByteKind::Unknown);
        assert_eq!(classify_first_byte(0xFF), FirstByteKind::Unknown);
    }

    #[test]
    fn build_string_with_and_without_rev() {
        let with_rev = build_string(Some("abc1234"));
        assert!(with_rev.contains("rev abc1234"));
        assert!(with_rev.contains(env!("CARGO_PKG_VERSION")));
        let no_rev = build_string(None);
        assert!(no_rev.contains("rev unknown"));
        let empty_rev = build_string(Some(""));
        assert!(empty_rev.contains("rev unknown"));
    }

    #[test]
    fn unknown_first_frame_is_an_error() {
        // Server speaks gibberish — client must fail loudly.
        let mut bad: Vec<u8> = Vec::new();
        write_msg(&mut bad, S_OUTPUT, b"raw bytes").unwrap();
        let mut sink: Vec<u8> = Vec::new();
        let err = client_handshake(&mut bad.as_slice(), &mut sink).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
