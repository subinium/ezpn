//! Optional ANSI frame differ for the renderer's bounded output vocabulary.
//!
//! This is not a general terminal proxy. Unknown controls, oversized screens,
//! and incomplete frames pass through unchanged. After bypass, only a full
//! redraw establishes a new baseline. Callers must force that redraw on attach,
//! resize, or lost output. Share a baseline only between clients receiving
//! identical frames at identical terminal dimensions.

use crate::vt100;

const MAX_MODEL_BYTES: u64 = 1024 * 1024;
const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
const SYNC_END: &[u8] = b"\x1b[?2026l";

#[derive(Default)]
pub struct AnsiFrameDiffer {
    parser: Option<vt100::Parser>,
}

impl AnsiFrameDiffer {
    /// Full redraws and bypasses preserve original bytes and side effects.
    /// Sparse diffs are wrapped in one synchronized-update block.
    pub fn encode(&mut self, frame: &[u8], cols: u16, rows: u16, full_redraw: bool) -> Vec<u8> {
        // Budget both grids plus row metadata before allocation, not overall RSS.
        let estimate =
            u64::from(rows) * u64::from(cols) * std::mem::size_of::<vt100::Cell>() as u64 * 2
                + u64::from(rows) * 128;
        if cols == 0 || rows == 0 || estimate > MAX_MODEL_BYTES || !supported_frame(frame) {
            self.parser = None;
            return frame.to_vec();
        }
        if self
            .parser
            .as_ref()
            .is_some_and(|p| p.screen().size() != (rows, cols))
        {
            self.parser = None;
        }
        if full_redraw {
            let mut parser = vt100::Parser::new(rows, cols, 0);
            parser.process(frame);
            self.parser = Some(parser);
            return frame.to_vec();
        }
        let Some(parser) = &mut self.parser else {
            return frame.to_vec();
        };
        let previous = parser.screen().clone();
        parser.process(frame);
        let delta = parser.screen().state_diff(&previous);
        if delta.is_empty() {
            return Vec::new();
        }
        if delta.len() + SYNC_BEGIN.len() + SYNC_END.len() >= frame.len() {
            return frame.to_vec();
        }
        let mut output = Vec::with_capacity(delta.len() + SYNC_BEGIN.len() + SYNC_END.len());
        output.extend_from_slice(SYNC_BEGIN);
        output.extend_from_slice(&delta);
        output.extend_from_slice(SYNC_END);
        output
    }
}

/// Only accept complete UTF-8 text and the CSI vocabulary emitted by render.rs.
/// OSC, DCS, alternate-screen, cursor-shape, and unknown SGR must not be lost.
fn supported_frame(frame: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(frame) else {
        return false;
    };
    for ch in text.chars() {
        if ch.is_control() && ch != '\x1b' {
            return false;
        }
    }
    let mut index = 0;
    while index < frame.len() {
        match frame[index] {
            0x1b => {
                if frame.get(index + 1) != Some(&b'[') {
                    return false;
                }
                let start = index + 2;
                let mut end = start;
                while frame.get(end).is_some_and(|b| (0x20..=0x3f).contains(b)) {
                    end += 1;
                }
                let Some(&command) = frame.get(end) else {
                    return false;
                };
                let params = &frame[start..end];
                let supported = match command {
                    b'H' | b'f' | b'J' | b'K' => {
                        params.iter().all(|b| b.is_ascii_digit() || *b == b';')
                    }
                    b'h' | b'l' => params == b"?25" || params == b"?2026",
                    b'm' => supported_sgr(params),
                    _ => false,
                };
                if !supported {
                    return false;
                }
                index = end + 1;
            }
            0..=0x1f | 0x7f => return false,
            _ => {
                let start = index;
                while frame.get(index).is_some_and(|b| *b >= 0x20 && *b != 0x7f) {
                    index += 1;
                }
                let run = &text[start..index];
                // vt100 0.16.2 appends zero-width scalars only while a cell's
                // UTF-8 payload is below 18 bytes. UI labels can exceed it.
                let mut cell_bytes = 0;
                for ch in run.chars() {
                    if unicode_width::UnicodeWidthChar::width(ch) == Some(0) {
                        if cell_bytes == 0 || cell_bytes >= 18 {
                            return false;
                        }
                        cell_bytes += ch.len_utf8();
                    } else {
                        cell_bytes = ch.len_utf8();
                    }
                }
                // Measure only printed text, not escape bytes or CSI parameters.
                // vt100 is cell/codepoint based, not a grapheme-cluster terminal.
                let scalar_width: usize = run
                    .chars()
                    .map(|ch| unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0))
                    .sum();
                if unicode_width::UnicodeWidthStr::width(run) != scalar_width {
                    return false;
                }
            }
        }
    }
    true
}

fn supported_sgr(params: &[u8]) -> bool {
    let Ok(params) = std::str::from_utf8(params) else {
        return false;
    };
    let mut parts = params.split(';');
    while let Some(part) = parts.next() {
        let Ok(code) = (if part.is_empty() { "0" } else { part }).parse::<u16>() else {
            return false;
        };
        match code {
            0
            | 1
            | 3
            | 4
            | 7
            | 22
            | 23
            | 24
            | 27
            | 30..=37
            | 39
            | 40..=47
            | 49
            | 90..=97
            | 100..=107 => {}
            38 | 48 => {
                let count = match parts.next() {
                    Some("5") => 1,
                    Some("2") => 3,
                    _ => return false,
                };
                for _ in 0..count {
                    if parts.next().and_then(|s| s.parse::<u8>().ok()).is_none() {
                        return false;
                    }
                }
            }
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full(text: &str) -> Vec<u8> {
        format!("\x1b[?2026h\x1b[0m\x1b[2J\x1b[1;1H{text}\x1b[?25l\x1b[?2026l").into_bytes()
    }

    #[test]
    fn reliability_diff_replays_unicode_attributes_and_cursor() {
        let mut differ = AnsiFrameDiffer::default();
        let mut expected = vt100::Parser::new(4, 40, 0);
        let mut actual = vt100::Parser::new(4, 40, 0);
        let first = full("\x1b[1;38;2;10;20;30m한e\u{301}🙂 tail\x1b[0m");
        assert_eq!(differ.encode(&first, 40, 4, true), first);
        expected.process(&first);
        actual.process(&first);
        for text in [
            "\x1b[3;31m한e\u{301}🙂 changed\x1b[0m",
            "short",
            "\x1b[4;7mCJK 字\x1b[0m",
        ] {
            let frame = full(text);
            expected.process(&frame);
            actual.process(&differ.encode(&frame, 40, 4, false));
            assert_eq!(
                actual.screen().state_formatted(),
                expected.screen().state_formatted()
            );
        }
        let cursor = b"\x1b[3;10H\x1b[?25h";
        expected.process(cursor);
        actual.process(&differ.encode(cursor, 40, 4, false));
        assert_eq!(
            actual.screen().cursor_position(),
            expected.screen().cursor_position()
        );
        assert_eq!(
            actual.screen().hide_cursor(),
            expected.screen().hide_cursor()
        );
    }

    #[test]
    fn reliability_unchanged_frame_is_noop_and_sparse_change_is_smaller() {
        let mut differ = AnsiFrameDiffer::default();
        let first = full(&"a".repeat(200));
        differ.encode(&first, 80, 24, true);
        assert!(differ.encode(&first, 80, 24, false).is_empty());
        let changed = full(&format!("b{}", "a".repeat(199)));
        let delta = differ.encode(&changed, 80, 24, false);
        assert!(delta.len() < changed.len());
        assert!(delta.starts_with(SYNC_BEGIN) && delta.ends_with(SYNC_END));
    }

    #[test]
    fn reliability_unknown_sequences_bypass_until_full_redraw() {
        for unknown in [
            b"\x1b[3 q".as_slice(),
            b"\x1b]52;c;dGVzdA==\x07",
            b"\x1b[?1049h",
            b"\x1bPpayload\x1b\\",
            b"\x1b[9mstrike",
            b"\x1b[",
            b"\xff",
            "👩\u{200d}💻".as_bytes(),
            "♥\u{fe0f}".as_bytes(),
            "e\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}".as_bytes(),
        ] {
            let mut differ = AnsiFrameDiffer::default();
            let first = full("hello");
            differ.encode(&first, 80, 24, true);
            assert_eq!(differ.encode(unknown, 80, 24, false), unknown);
            assert_eq!(differ.encode(&first, 80, 24, false), first);
            assert_eq!(differ.encode(&first, 80, 24, true), first);
            assert!(differ.encode(&first, 80, 24, false).is_empty());
        }
    }

    #[test]
    fn reliability_resize_attach_and_extreme_sizes_require_raw_redraw() {
        let mut differ = AnsiFrameDiffer::default();
        let first = full("hello");
        differ.encode(&first, 80, 24, true);
        assert_eq!(
            differ.encode(&first, 80, 24, true),
            first,
            "attach cannot receive a no-op"
        );
        assert_eq!(differ.encode(&first, 40, 12, false), first);
        assert_eq!(differ.encode(&first, 40, 12, false), first);
        for (cols, rows) in [(0, 24), (80, 0), (u16::MAX, u16::MAX)] {
            assert_eq!(differ.encode(&first, cols, rows, true), first);
            assert!(differ.parser.is_none());
        }
    }

    #[test]
    fn reliability_sgr_validation_is_lossless_not_permissive() {
        assert!(supported_sgr(b"1;38;2;255;0;12;48;5;200;0"));
        for params in [
            b"2".as_slice(),
            b"5",
            b"9",
            b"38;2;256;0;0",
            b"38;5",
            b"4:3",
            b"999999",
        ] {
            assert!(!supported_sgr(params));
        }
    }
}
