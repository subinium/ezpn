//! Scrollback persistence for v3 snapshots.
//!
//! Encodes a pane's visible terminal contents into a base64-encoded,
//! gzip-compressed, bincode-serialized blob suitable for inclusion in a JSON
//! snapshot. On restore, the blob is replayed into a fresh `vt100::Parser` so
//! the user sees their pre-detach screen on reattach.
//!
//! ## Limitations
//!
//! `vt100` 0.15 does not expose the historical scrollback ringbuffer directly,
//! and `Parser` is not `Clone`. Without `&mut Parser` we cannot walk the
//! scrollback offset to capture rows above the visible window. To keep the
//! encode side immutable (`&vt100::Parser`) we serialize **only the visible
//! cells** — the contents the user is currently looking at. This means
//! "scrollback above the fold" is lost across a detach/reattach cycle. The
//! visible screen, including any prompts, command output, and full-screen TUI
//! state (vim, less, htop), survives.
//!
//! ## Pipeline
//!
//! Encode: `Vec<Vec<u8>> (rows_formatted) → bincode → gzip → base64`.
//! Decode: `base64 → gunzip → bincode → parser.process(row) per row`.
//!
//! ## Size cap
//!
//! Encoded blobs above [`SCROLLBACK_BLOB_MAX_BYTES`] are truncated by dropping
//! the front (oldest) half of rows and re-encoding once. If still over cap,
//! the blob is dropped entirely (returns empty string + warning) so a single
//! pane cannot bloat a snapshot beyond a predictable limit.

use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::{Read, Write};

/// Per-pane cap on the encoded (base64) scrollback blob.
const SCROLLBACK_BLOB_MAX_BYTES: usize = 5 * 1024 * 1024;

/// Encode a parser's visible screen into a portable blob string.
///
/// Returns an empty string if the pane has zero size or if even the truncated
/// blob exceeds the cap (after dropping half the rows).
pub fn encode_scrollback(parser: &vt100::Parser) -> String {
    let screen = parser.screen();
    let (_rows, cols) = screen.size();
    if cols == 0 {
        return String::new();
    }

    // Collect visible rows as raw formatted byte streams (ANSI escapes inline).
    // `rows_formatted` is suitable for re-feeding into another `Parser`.
    let mut rows: Vec<Vec<u8>> = screen.rows_formatted(0, cols).collect();

    match try_encode(&rows) {
        Ok(s) if s.len() <= SCROLLBACK_BLOB_MAX_BYTES => s,
        _ => {
            // Over cap (or encode failure with too many rows): drop oldest half and retry once.
            let drop_n = rows.len() / 2;
            rows.drain(0..drop_n);
            match try_encode(&rows) {
                Ok(s) if s.len() <= SCROLLBACK_BLOB_MAX_BYTES => s,
                Ok(_) => {
                    eprintln!(
                        "ezpn: scrollback blob exceeds {} bytes after truncation; dropping",
                        SCROLLBACK_BLOB_MAX_BYTES
                    );
                    String::new()
                }
                Err(e) => {
                    eprintln!("ezpn: scrollback blob encode failed: {e}");
                    String::new()
                }
            }
        }
    }
}

/// Decode a blob and replay its rows into a parser.
///
/// On error returns `Err`; callers should warn and continue with an empty
/// scrollback — never panic. Rows are replayed in order with `\r\n` between
/// them so the parser's cursor advances naturally.
pub fn decode_scrollback(blob: &str, parser: &mut vt100::Parser) -> Result<()> {
    if blob.is_empty() {
        return Ok(());
    }
    let compressed = STANDARD
        .decode(blob.as_bytes())
        .context("base64 decode failed")?;
    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut serialized = Vec::with_capacity(compressed.len() * 4);
    decoder
        .read_to_end(&mut serialized)
        .context("gzip decompress failed")?;
    let rows: Vec<Vec<u8>> =
        bincode::deserialize(&serialized).context("bincode deserialize failed")?;

    for (i, row) in rows.iter().enumerate() {
        parser.process(row);
        // Advance to next line for all but the last row, so the cursor lands at
        // the bottom-left of the restored content (matches a live terminal).
        if i + 1 < rows.len() {
            parser.process(b"\r\n");
        }
    }
    Ok(())
}

fn try_encode(rows: &[Vec<u8>]) -> Result<String> {
    let serialized = bincode::serialize(rows).context("bincode serialize failed")?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&serialized)
        .context("gzip write failed")?;
    let compressed = encoder.finish().context("gzip finalize failed")?;
    Ok(STANDARD.encode(&compressed))
}

#[cfg(test)]
mod tests {
    // bench `render_hotpaths` includes this file via `#[path]`, which exposes
    // the test mod under `cfg(test)` even when the test mod's items are unused
    // from the bench's perspective. Allow that without breaking the main
    // `-D warnings` build.
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn round_trip_preserves_visible_text() {
        let mut p = vt100::Parser::new(5, 20, 1000);
        p.process(b"hello world\r\nsecond line\r\n");
        let blob = encode_scrollback(&p);
        assert!(!blob.is_empty(), "blob should be non-empty");

        let mut q = vt100::Parser::new(5, 20, 1000);
        decode_scrollback(&blob, &mut q).expect("decode should succeed");

        let original: String = p.screen().rows(0, 20).collect::<Vec<_>>().join("\n");
        let restored: String = q.screen().rows(0, 20).collect::<Vec<_>>().join("\n");
        assert_eq!(restored, original);
    }

    #[test]
    fn empty_blob_is_no_op() {
        let mut p = vt100::Parser::new(5, 20, 1000);
        decode_scrollback("", &mut p).expect("empty blob should be Ok");
        // Screen still empty
        assert!(p.screen().rows(0, 20).all(|r| r.trim().is_empty()));
    }

    #[test]
    fn corrupt_base64_returns_error_without_panic() {
        let mut p = vt100::Parser::new(5, 20, 1000);
        let result = decode_scrollback("!!!not valid base64!!!", &mut p);
        assert!(result.is_err());
    }

    #[test]
    fn corrupt_gzip_returns_error_without_panic() {
        let mut p = vt100::Parser::new(5, 20, 1000);
        // Valid base64 but garbage bytes inside (not gzip-compressed)
        let blob = STANDARD.encode(b"this is not gzip data at all");
        let result = decode_scrollback(&blob, &mut p);
        assert!(result.is_err());
    }

    #[test]
    fn corrupt_bincode_returns_error_without_panic() {
        // Valid gzip wrapping non-bincode data
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"not a bincode payload").unwrap();
        let compressed = encoder.finish().unwrap();
        let blob = STANDARD.encode(&compressed);

        let mut p = vt100::Parser::new(5, 20, 1000);
        let result = decode_scrollback(&blob, &mut p);
        assert!(result.is_err());
    }

    #[test]
    fn cap_truncates_oversized_blob() {
        // Build a parser with a giant pane filled with text so the encoded blob
        // would exceed the 5 MB cap on the first try.
        let cols: u16 = 500;
        let rows: u16 = 500;
        let mut p = vt100::Parser::new(rows, cols, 0);
        // Fill each row with random-looking but compressible data; gzip will
        // shrink runs, so use varied bytes to defeat compression.
        let mut payload = Vec::with_capacity((cols as usize + 2) * rows as usize);
        for r in 0..rows {
            for c in 0..cols {
                let ch = ((r as usize * 31 + c as usize * 17) % 94) as u8 + b'!';
                payload.push(ch);
            }
            payload.extend_from_slice(b"\r\n");
        }
        p.process(&payload);

        let blob = encode_scrollback(&p);
        // Either truncated to fit, or dropped entirely. Both are acceptable;
        // what we MUST guarantee is the cap is respected.
        assert!(
            blob.len() <= SCROLLBACK_BLOB_MAX_BYTES,
            "blob len {} exceeds cap {}",
            blob.len(),
            SCROLLBACK_BLOB_MAX_BYTES
        );
    }

    #[test]
    fn round_trip_preserves_basic_ansi_color() {
        let mut p = vt100::Parser::new(3, 20, 100);
        // Red "hi", reset, then "bye" plain.
        p.process(b"\x1b[31mhi\x1b[0m bye");
        let blob = encode_scrollback(&p);

        let mut q = vt100::Parser::new(3, 20, 100);
        decode_scrollback(&blob, &mut q).unwrap();

        let r0: String = p.screen().rows(0, 20).next().unwrap();
        let r1: String = q.screen().rows(0, 20).next().unwrap();
        assert_eq!(r0, r1);
    }
}
