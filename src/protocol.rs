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

// ── Server → Client tags ──

/// Raw terminal output bytes (rendered frame).
pub const S_OUTPUT: u8 = 0x81;
/// Server acknowledges detach.
pub const S_DETACHED: u8 = 0x82;
/// Server is shutting down.
pub const S_EXIT: u8 = 0x83;
/// Pong response to C_PING.
pub const S_PONG: u8 = 0x84;

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
