//! KeySpec grammar — text → raw PTY bytes.
//!
//! Per SPEC 06 §4.2 (`docs/spec/v0.10.0/06-send-keys-api.md`) the grammar is:
//!
//! ```text
//! KeySpec  ← Chord (WS Chord)*
//! Chord    ← (Modifier '-')* Atom
//! Modifier ← 'C' / 'M' / 'S'
//! Atom     ← Named / Char+
//! Named    ← Enter | Tab | Esc | Space | Backspace | Delete
//!          | Up | Down | Left | Right
//!          | Home | End | PageUp | PageDown
//!          | F1 .. F12
//! ```
//!
//! The wire format carries `keys: Vec<String>` — one element per chord
//! token, matching CLI argv after `--`. This is a deliberate deviation
//! from the SPEC's draft single-string example: a vector eliminates
//! escape ambiguity for multi-char literal arguments like `'echo hi'`.
//!
//! Untouched-by-SPEC quirks worth knowing:
//! * `Enter` emits `\r` (0x0D) — what an unmodified terminal sends, not `\n`.
//! * `Backspace` emits `\x7f` (DEL) — the xterm default.
//! * Modifier+Named is intentionally narrow: only `S-Tab`, `M-Tab`, and
//!   `M-<arrow>` style "Alt-prefixed Named" sequences. Other combinations
//!   return a structured `ParseError`; users can fall back to `--literal`.
//! * `is_named_key` lets the dispatch layer enforce SPEC §4.5's
//!   "literal forbids named keys" rule without re-parsing.

use std::fmt;

/// Structured parse failure. Display string is what `IpcResponse::error`
/// surfaces to `ezpn-ctl`, so it is end-user-facing.
#[derive(Debug, Clone)]
pub struct ParseError {
    msg: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for ParseError {}

/// Compile a CLI-style chord token list into the byte sequence that
/// pressing those keys interactively would deliver to the PTY.
///
/// Tokens are concatenated with no separator between them, so
/// `["echo", "Space", "hi", "Enter"]` yields `b"echo hi\r"` (Space is the
/// Named key, not a separator).
pub fn compile_to_bytes(tokens: &[String]) -> Result<Vec<u8>, ParseError> {
    if tokens.is_empty() {
        return Err(err("no keys to send"));
    }
    let mut out = Vec::new();
    for tok in tokens {
        compile_token(tok, &mut out)?;
    }
    Ok(out)
}

/// True iff `token` would compile as a Named key (Enter, Tab, F5, …).
/// Used by the dispatch layer to enforce the literal-mode guard.
pub fn is_named_key(token: &str) -> bool {
    lookup_named(token).is_some()
}

fn compile_token(token: &str, out: &mut Vec<u8>) -> Result<(), ParseError> {
    if token.is_empty() {
        return Err(err("empty chord"));
    }
    if token.contains('\n') {
        return Err(err(
            "newline in non-literal token; use 'Enter' or --literal",
        ));
    }
    if let Some((mods, atom)) = try_split_chord(token)? {
        emit_chord(&mods, atom, out)?;
        return Ok(());
    }
    if let Some(bytes) = lookup_named(token) {
        out.extend_from_slice(bytes);
        return Ok(());
    }
    // Multi-char or non-named single-char: treat as literal UTF-8 text.
    out.extend_from_slice(token.as_bytes());
    Ok(())
}

const NAMED_KEYS: &[(&str, &[u8])] = &[
    ("Enter", b"\r"),
    ("Tab", b"\t"),
    ("Esc", b"\x1b"),
    ("Space", b" "),
    ("Backspace", b"\x7f"),
    ("Delete", b"\x1b[3~"),
    ("Up", b"\x1b[A"),
    ("Down", b"\x1b[B"),
    ("Left", b"\x1b[D"),
    ("Right", b"\x1b[C"),
    ("Home", b"\x1b[H"),
    ("End", b"\x1b[F"),
    ("PageUp", b"\x1b[5~"),
    ("PageDown", b"\x1b[6~"),
    ("F1", b"\x1bOP"),
    ("F2", b"\x1bOQ"),
    ("F3", b"\x1bOR"),
    ("F4", b"\x1bOS"),
    ("F5", b"\x1b[15~"),
    ("F6", b"\x1b[17~"),
    ("F7", b"\x1b[18~"),
    ("F8", b"\x1b[19~"),
    ("F9", b"\x1b[20~"),
    ("F10", b"\x1b[21~"),
    ("F11", b"\x1b[23~"),
    ("F12", b"\x1b[24~"),
];

fn lookup_named(name: &str) -> Option<&'static [u8]> {
    NAMED_KEYS.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)
}

fn try_split_chord(token: &str) -> Result<Option<(Vec<char>, &str)>, ParseError> {
    if !token.contains('-') {
        return Ok(None);
    }
    let parts: Vec<&str> = token.split('-').collect();
    if parts.len() < 2 {
        return Ok(None);
    }
    // First segment must look like a modifier; otherwise the token is just
    // a literal that happens to contain '-' (e.g. "long-name").
    if !is_modifier(parts[0]) {
        return Ok(None);
    }
    let mut mods = Vec::with_capacity(parts.len() - 1);
    for &m in &parts[..parts.len() - 1] {
        if m.is_empty() {
            return Err(err("malformed chord (empty modifier)"));
        }
        if !is_modifier(m) {
            return Err(err(format!("unknown modifier '{m}'")));
        }
        mods.push(m.chars().next().expect("non-empty mod"));
    }
    let atom = *parts.last().expect("parts.len() >= 2");
    if atom.is_empty() {
        return Err(err("incomplete chord (no atom)"));
    }
    Ok(Some((mods, atom)))
}

fn is_modifier(s: &str) -> bool {
    matches!(s, "C" | "M" | "S")
}

fn emit_chord(mods: &[char], atom: &str, out: &mut Vec<u8>) -> Result<(), ParseError> {
    let has_alt = mods.contains(&'M');
    let has_ctrl = mods.contains(&'C');
    let has_shift = mods.contains(&'S');

    if let Some(named_bytes) = lookup_named(atom) {
        match (atom, has_shift, has_ctrl, has_alt) {
            ("Tab", true, false, false) => {
                // xterm shifted Tab (Back-Tab).
                out.extend_from_slice(b"\x1b[Z");
            }
            (_, false, false, true) => {
                // Alt-prefixed Named (e.g. M-Up): ESC then the Named bytes.
                out.push(0x1b);
                out.extend_from_slice(named_bytes);
            }
            (_, false, false, false) => out.extend_from_slice(named_bytes),
            _ => {
                return Err(err(format!("unsupported modifier on named key '{atom}'")));
            }
        }
        return Ok(());
    }

    let mut chars = atom.chars();
    let c = chars.next().ok_or_else(|| err("empty chord atom"))?;
    if chars.next().is_some() {
        return Err(err(format!(
            "modifier prefix valid on single-char atom or Named only; got '{atom}'"
        )));
    }
    if !c.is_ascii() {
        return Err(err(format!("non-ASCII atom in chord: '{c}'")));
    }
    let mut byte = c as u8;
    if has_shift {
        byte = byte.to_ascii_uppercase();
    }
    if has_ctrl {
        byte &= 0x1f;
    }
    if has_alt {
        out.push(0x1b);
    }
    out.push(byte);
    Ok(())
}

fn err(msg: impl Into<String>) -> ParseError {
    ParseError { msg: msg.into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> String {
        s.to_string()
    }

    fn compile(strs: &[&str]) -> Result<Vec<u8>, ParseError> {
        let toks: Vec<String> = strs.iter().map(|s| t(s)).collect();
        compile_to_bytes(&toks)
    }

    #[test]
    fn literal_single_char() {
        assert_eq!(compile(&["a"]).unwrap(), b"a");
    }

    #[test]
    fn ctrl_letter_collapses_to_control_code() {
        assert_eq!(compile(&["C-a"]).unwrap(), &[0x01]);
        assert_eq!(compile(&["C-c"]).unwrap(), &[0x03]);
        assert_eq!(compile(&["C-l"]).unwrap(), &[0x0c]);
    }

    #[test]
    fn ctrl_alt_letter_uses_esc_prefix() {
        // SPEC §4.2 row: C-M-x → 0x1b 0x18
        assert_eq!(compile(&["C-M-x"]).unwrap(), &[0x1b, 0x18]);
    }

    #[test]
    fn named_keys_round_trip_table() {
        // Spot-check every documented Named key.
        let pairs: &[(&str, &[u8])] = &[
            ("Enter", b"\r"),
            ("Tab", b"\t"),
            ("Esc", b"\x1b"),
            ("Space", b" "),
            ("Backspace", b"\x7f"),
            ("Delete", b"\x1b[3~"),
            ("Up", b"\x1b[A"),
            ("Down", b"\x1b[B"),
            ("Left", b"\x1b[D"),
            ("Right", b"\x1b[C"),
            ("Home", b"\x1b[H"),
            ("End", b"\x1b[F"),
            ("PageUp", b"\x1b[5~"),
            ("PageDown", b"\x1b[6~"),
            ("F1", b"\x1bOP"),
            ("F4", b"\x1bOS"),
            ("F5", b"\x1b[15~"),
            ("F12", b"\x1b[24~"),
        ];
        for (name, want) in pairs {
            let got = compile(&[name]).unwrap();
            assert_eq!(&got, want, "mismatch on Named '{name}'");
        }
    }

    #[test]
    fn multi_token_concat_no_separator() {
        // SPEC §4.4: 'echo' Space 'hi' Enter → b"echo hi\r"
        assert_eq!(
            compile(&["echo", "Space", "hi", "Enter"]).unwrap(),
            b"echo hi\r"
        );
    }

    #[test]
    fn multi_char_token_is_literal() {
        assert_eq!(compile(&["echo hi"]).unwrap(), b"echo hi");
    }

    #[test]
    fn shift_tab_is_back_tab() {
        assert_eq!(compile(&["S-Tab"]).unwrap(), b"\x1b[Z");
    }

    #[test]
    fn alt_named_uses_esc_prefix() {
        let got = compile(&["M-Up"]).unwrap();
        assert_eq!(got, b"\x1b\x1b[A", "M-Up = ESC + Up");
    }

    #[test]
    fn token_with_dash_but_no_modifier_is_literal() {
        // "long-name" must NOT be parsed as chord — first segment "long" is
        // not a Modifier, so the entire token is literal text.
        assert_eq!(compile(&["long-name"]).unwrap(), b"long-name");
    }

    #[test]
    fn rejects_unknown_modifier() {
        // First segment 'X' is not a Modifier and the token contains '-',
        // so this falls through to literal text — no error, just bytes.
        // The actual reject case is when a *valid* modifier is followed by
        // an unknown one mid-token. Asserted in the next test.
        assert_eq!(compile(&["X-a"]).unwrap(), b"X-a");
    }

    #[test]
    fn rejects_unknown_modifier_after_valid_one() {
        let e = compile(&["C-X-y"]).unwrap_err();
        assert!(e.to_string().contains("unknown modifier 'X'"), "got: {e}");
    }

    #[test]
    fn rejects_empty_modifier_segment() {
        let e = compile(&["C--a"]).unwrap_err();
        assert!(e.to_string().contains("empty modifier"), "got: {e}");
    }

    #[test]
    fn rejects_chord_without_atom() {
        let e = compile(&["C-"]).unwrap_err();
        assert!(e.to_string().contains("incomplete chord"), "got: {e}");
    }

    #[test]
    fn rejects_empty_token() {
        let e = compile(&[""]).unwrap_err();
        assert!(e.to_string().contains("empty chord"), "got: {e}");
    }

    #[test]
    fn rejects_newline_in_token() {
        let e = compile(&["abc\ndef"]).unwrap_err();
        assert!(e.to_string().contains("newline"), "got: {e}");
    }

    #[test]
    fn rejects_empty_input() {
        let toks: Vec<String> = vec![];
        let e = compile_to_bytes(&toks).unwrap_err();
        assert!(e.to_string().contains("no keys"), "got: {e}");
    }

    #[test]
    fn is_named_key_recognises_table_entries() {
        assert!(is_named_key("Enter"));
        assert!(is_named_key("F12"));
        assert!(!is_named_key("enter")); // case-sensitive
        assert!(!is_named_key("hello"));
    }

    #[test]
    fn rejects_unsupported_named_modifier_combo() {
        // C-Enter is ambiguous across terminals; we surface a clear error
        // rather than emit an arbitrary encoding.
        let e = compile(&["C-Enter"]).unwrap_err();
        assert!(
            e.to_string().contains("unsupported modifier on named key"),
            "got: {e}"
        );
    }
}
