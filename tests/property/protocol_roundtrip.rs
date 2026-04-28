//! Property tests for `protocol.rs` (issue #94).
//!
//! Invariants verified:
//! 1. `write_msg → read_msg` round-trips for any `(tag, payload)` triple
//!    within the size limit.
//! 2. The decoder rejects malformed frames without panicking — both
//!    truncated headers and over-large payloads.
//! 3. `encode_resize → decode_resize` round-trips for all `(cols, rows)`.

#![allow(dead_code)]

#[path = "../../src/protocol.rs"]
mod protocol;

use proptest::prelude::*;
use std::io::Cursor;

use protocol::{decode_resize, encode_resize, read_msg, write_msg};

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        ..ProptestConfig::default()
    })]

    /// Any `(tag, payload)` triple round-trips through write_msg / read_msg.
    /// Cap payload at 64 KB to keep the test fast — the 16 MB limit is
    /// covered separately.
    #[test]
    fn frame_encode_decode_roundtrip(tag in any::<u8>(), payload in prop::collection::vec(any::<u8>(), 0..64 * 1024)) {
        let mut buf = Vec::new();
        write_msg(&mut buf, tag, &payload).expect("encode never fails for in-bounds payload");

        let mut cursor = Cursor::new(&buf);
        let (got_tag, got_payload) = read_msg(&mut cursor).expect("roundtrip read");
        prop_assert_eq!(got_tag, tag);
        prop_assert_eq!(got_payload, payload);
    }

    /// Decoder must reject truncated frames without panic. We feed it
    /// random byte streams shorter than 5 bytes (header) and assert it
    /// returns Err rather than crashing.
    #[test]
    fn truncated_header_returns_err(bytes in prop::collection::vec(any::<u8>(), 0..5)) {
        let mut cursor = Cursor::new(&bytes);
        let result = read_msg(&mut cursor);
        prop_assert!(result.is_err(), "truncated frame must be Err, not panic");
    }

    /// Decoder must reject frames whose declared length exceeds the
    /// MAX_PAYLOAD cap. We craft a tag + length header that claims a
    /// huge payload and verify it errors.
    #[test]
    fn oversized_length_returns_err(tag in any::<u8>()) {
        let len: u32 = 32 * 1024 * 1024; // > MAX_PAYLOAD (16 MB)
        let mut header = vec![tag];
        header.extend_from_slice(&len.to_be_bytes());
        let mut cursor = Cursor::new(&header);
        let result = read_msg(&mut cursor);
        prop_assert!(result.is_err(), "over-cap length must be Err");
    }

    /// Resize encode / decode round-trips for any (cols, rows).
    #[test]
    fn resize_roundtrip(cols in any::<u16>(), rows in any::<u16>()) {
        let bytes = encode_resize(cols, rows);
        let (got_cols, got_rows) = decode_resize(&bytes).expect("4 bytes always decodes");
        prop_assert_eq!(got_cols, cols);
        prop_assert_eq!(got_rows, rows);
    }

    /// Resize decode of fewer than 4 bytes must return None — never panic.
    #[test]
    fn resize_short_payload_returns_none(payload in prop::collection::vec(any::<u8>(), 0..4)) {
        prop_assert_eq!(decode_resize(&payload), None);
    }
}
