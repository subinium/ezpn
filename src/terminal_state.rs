//! Per-pane terminal state machine — DECSET bits, Kitty keyboard stack,
//! OSC 7 reported cwd, OSC 52 clipboard policy, and the active theme
//! palette consulted when answering OSC 4/10/11/12 colour queries.
//!
//! Owned by [`crate::pane::Pane`]. All multiplexer-side decisions about
//! how to encode keys, paste, and focus events for a given pane consult
//! this struct rather than scanning vt100's screen each frame.
//!
//! Designed for issues #74–#79 (terminal-protocol foundations). The
//! struct deliberately does NOT include the DECSET 2026 sync bit —
//! that's owned by issue #73 (`Pane::in_sync`), and DECSET ?1049
//! (alternate screen) which vt100 already tracks internally.

use std::path::PathBuf;
use std::time::Instant;

// ─── DECSET bits ────────────────────────────────────────────

/// Mouse-reporting protocol the child has requested via `?1000/?1002/?1003`,
/// independent of the encoding (`?1006` SGR or legacy).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseProtocol {
    /// No mouse reporting (default).
    #[default]
    Off,
    /// `?1000` — press/release only.
    X10,
    /// `?1002` — press/release + drag motion while button held.
    Btn,
    /// `?1003` — press/release + any motion (with or without buttons).
    Any,
}

/// Wire encoding the child wants for mouse events.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseEncoding {
    /// X10 6-byte encoding (`ESC [ M` + 3 bytes).
    #[default]
    X10,
    /// SGR encoding (`ESC [ < … M/m`), enabled by `?1006`.
    Sgr,
}

/// Combined mouse mode: protocol + encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseMode {
    pub protocol: MouseProtocol,
    pub encoding: MouseEncoding,
}

impl MouseMode {
    pub fn is_off(&self) -> bool {
        matches!(self.protocol, MouseProtocol::Off)
    }
}

// ─── Kitty keyboard stack (#74) ─────────────────────────────

/// Bit flags for the kitty keyboard protocol, levels 0–3.
///
/// Only the low 5 bits are defined by the spec; ezpn stores the raw value
/// (no bitflags crate to keep deps minimal). See
/// <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KittyKbdFlags(pub u8);

impl KittyKbdFlags {
    pub const DISAMBIGUATE: u8 = 0b00001;
    pub const REPORT_EVENTS: u8 = 0b00010;
    pub const REPORT_ALTERNATES: u8 = 0b00100;
    pub const REPORT_ALL_AS_ESCAPES: u8 = 0b01000;
    pub const REPORT_ASSOCIATED_TEXT: u8 = 0b10000;
    pub const ALL: u8 = 0b11111;

    pub fn bits(self) -> u8 {
        self.0 & Self::ALL
    }
}

/// Push/pop/set semantics from the spec:
/// - `CSI > flags u` — push `flags` on the stack (becomes new top).
/// - `CSI = flags ; mode u` — modify top entry (mode 1 = set, 2 = OR, 3 = AND-NOT).
/// - `CSI < N u` — pop `N` entries (default 1).
/// - `CSI ? u` — query current top; multiplexer responds with `CSI ? <flags> u`.
///
/// Stack is initially empty (legacy behaviour). The "active" flags are the top
/// of the stack, or `0` if empty.
#[derive(Clone, Debug, Default)]
pub struct KittyKbdStack {
    entries: Vec<KittyKbdFlags>,
}

impl KittyKbdStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Top of stack, or 0 if empty.
    pub fn active(&self) -> KittyKbdFlags {
        self.entries.last().copied().unwrap_or_default()
    }

    pub fn push(&mut self, flags: KittyKbdFlags) {
        // Cap depth defensively — apps sometimes push without ever popping.
        // 32 is generous; kitty's reference impl uses a similar bound.
        const MAX_DEPTH: usize = 32;
        if self.entries.len() >= MAX_DEPTH {
            self.entries.remove(0);
        }
        self.entries.push(flags);
    }

    /// Pop `n` entries (default 1 if `n == 0`). Saturates at empty.
    pub fn pop(&mut self, n: usize) {
        let n = n.max(1);
        for _ in 0..n {
            if self.entries.pop().is_none() {
                break;
            }
        }
    }

    /// `CSI = flags ; mode u` — apply to top entry, push new entry if empty.
    /// `mode`: 1 = set, 2 = OR (enable bits), 3 = AND-NOT (disable bits).
    /// Modes outside 1..=3 are ignored.
    pub fn modify_top(&mut self, flags: KittyKbdFlags, mode: u8) {
        if self.entries.is_empty() {
            self.entries.push(KittyKbdFlags(0));
        }
        let top = self.entries.last_mut().unwrap();
        match mode {
            1 => *top = flags,
            2 => *top = KittyKbdFlags(top.0 | flags.0),
            3 => *top = KittyKbdFlags(top.0 & !flags.0),
            _ => {}
        }
    }

    pub fn depth(&self) -> usize {
        self.entries.len()
    }
}

// ─── OSC 52 clipboard policy (#79) ──────────────────────────

/// What to do when a child writes `OSC 52 ; c ; <base64>` to set the clipboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Osc52SetPolicy {
    /// Pass through unchanged. Documented as insecure.
    Allow,
    /// Prompt the user once per pane; cache the decision.
    /// Until prompted-and-accepted, drop the sequence.
    #[default]
    Confirm,
    /// Drop silently, log at warn level.
    Deny,
}

/// What to do when a child writes `OSC 52 ; c ; ?` to read the clipboard.
/// Read is the dominant attack vector — default to `Deny`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Osc52GetPolicy {
    Allow,
    #[default]
    Deny,
}

/// Configurable clipboard settings (matches the `[clipboard]` config table).
#[derive(Clone, Copy, Debug)]
pub struct ClipboardPolicy {
    pub set: Osc52SetPolicy,
    pub get: Osc52GetPolicy,
    pub max_bytes: usize,
}

impl Default for ClipboardPolicy {
    fn default() -> Self {
        Self {
            set: Osc52SetPolicy::Confirm,
            get: Osc52GetPolicy::Deny,
            // 1 MiB hard cap — anything larger is almost certainly an
            // exhaustion attempt rather than a legitimate paste.
            max_bytes: 1024 * 1024,
        }
    }
}

/// Per-pane runtime decision cache for OSC 52. The first time a pane prompts
/// and the user answers, store the answer and skip the prompt for the rest
/// of the pane's lifetime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Osc52Decision {
    /// Not yet decided — `Confirm` policy still requires a prompt.
    #[default]
    Pending,
    /// User accepted; treat subsequent writes as `Allow` for this pane.
    Allowed,
    /// User rejected; treat subsequent writes as `Deny` for this pane.
    Denied,
}

// ─── OSC 4/10/11/12 colour palette (#77) ────────────────────

/// 24-bit colour. Used both for theme defaults and parsed OSC responses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Encode as the 4-digit-per-channel form xterm uses in OSC responses
    /// (`rgb:RRRR/GGGG/BBBB`). Each byte is duplicated to fill the 16-bit
    /// channel, matching what real terminals send.
    pub fn to_xterm_rgb_str(self) -> String {
        format!(
            "rgb:{:02x}{:02x}/{:02x}{:02x}/{:02x}{:02x}",
            self.r, self.r, self.g, self.g, self.b, self.b
        )
    }
}

/// The active theme's foreground / background / cursor colours plus the
/// 256-colour palette consulted when an app sends `OSC 4 ; N ; ?`.
///
/// `None` for any field disables interception for that query — ezpn passes
/// the request through to the host emulator unchanged so the host can
/// answer authoritatively.
#[derive(Clone, Debug)]
pub struct ThemePalette {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub cursor: Option<Rgb>,
    /// 256-colour ANSI palette. Indices 0–15 are the canonical 16, 16–231
    /// the 6×6×6 cube, 232–255 the greyscale ramp. `None` per-index means
    /// "let the host emulator answer".
    pub palette: [Option<Rgb>; 256],
}

impl Default for ThemePalette {
    fn default() -> Self {
        Self {
            fg: None,
            bg: None,
            cursor: None,
            palette: [None; 256],
        }
    }
}

impl ThemePalette {
    /// Whether ezpn should answer any OSC colour query at all. If every slot
    /// is `None` we fall through to the host emulator.
    pub fn is_active(&self) -> bool {
        self.fg.is_some()
            || self.bg.is_some()
            || self.cursor.is_some()
            || self.palette.iter().any(|c| c.is_some())
    }
}

// ─── Aggregate pane state ───────────────────────────────────

/// Aggregate per-pane terminal state. Owned by [`crate::pane::Pane`].
///
/// **Not included** (intentionally):
/// - `?1049` alternate-screen — vt100's `Screen::alternate_screen()` is
///   authoritative.
/// - `?2026` synchronised output — issue #73 owns `Pane::in_sync`.
#[derive(Clone, Debug, Default)]
pub struct PaneTerminalState {
    /// `?2004` bracketed paste mode.
    pub bracketed_paste: bool,
    /// `?1004` focus reporting mode.
    pub focus_reporting: bool,
    /// `?1000/?1002/?1003` + `?1006` mouse mode.
    pub mouse_mode: MouseMode,
    /// Kitty keyboard protocol flag stack (#74).
    pub kitty_kbd: KittyKbdStack,
    /// Most recent OSC 7 reported cwd from the shell, with a timestamp so
    /// stale values can fall back to procfs polling (#75).
    pub reported_cwd: Option<(PathBuf, Instant)>,
    /// Per-pane OSC 52 decision cache (#79).
    pub osc52_decision: Osc52Decision,
    /// Pending pane-scoped OSC 52 set-clipboard prompts awaiting user
    /// confirmation. Each entry is the **decoded** payload (not the raw
    /// `OSC 52 ; c ; …` envelope) so the prompt can show byte counts and
    /// the multiplexer can re-emit the canonical envelope on accept.
    pub osc52_pending_confirm: Vec<Vec<u8>>,
}

impl PaneTerminalState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset state when a pane slot is reused for a freshly spawned shell.
    /// Prevents the state of the previous occupant from leaking into the
    /// new process — see #78 acceptance criteria.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_stack_push_pop_top() {
        let mut s = KittyKbdStack::new();
        assert_eq!(s.active().bits(), 0);

        s.push(KittyKbdFlags(0b00001));
        assert_eq!(s.active().bits(), 0b00001);

        s.push(KittyKbdFlags(0b01111));
        assert_eq!(s.active().bits(), 0b01111);

        s.pop(1);
        assert_eq!(s.active().bits(), 0b00001);

        s.pop(5); // saturates
        assert_eq!(s.depth(), 0);
        assert_eq!(s.active().bits(), 0);
    }

    #[test]
    fn kitty_stack_modify_top() {
        let mut s = KittyKbdStack::new();
        // mode=1 set on empty stack: pushes a slot first
        s.modify_top(KittyKbdFlags(0b00101), 1);
        assert_eq!(s.active().bits(), 0b00101);

        // mode=2 OR
        s.modify_top(KittyKbdFlags(0b01000), 2);
        assert_eq!(s.active().bits(), 0b01101);

        // mode=3 AND-NOT
        s.modify_top(KittyKbdFlags(0b00100), 3);
        assert_eq!(s.active().bits(), 0b01001);

        // unknown mode is a no-op
        let before = s.active().bits();
        s.modify_top(KittyKbdFlags(0xff), 99);
        assert_eq!(s.active().bits(), before);
    }

    #[test]
    fn kitty_stack_capped_depth() {
        let mut s = KittyKbdStack::new();
        for i in 0..40 {
            s.push(KittyKbdFlags(i as u8));
        }
        // Capped at 32 — oldest entries dropped.
        assert!(s.depth() <= 32);
        assert_eq!(s.active().bits(), 39 & KittyKbdFlags::ALL);
    }

    #[test]
    fn rgb_xterm_format_roundtrip() {
        let c = Rgb::new(0x12, 0xab, 0xff);
        // xterm replicates each byte to fill the 4-hex-digit channel.
        assert_eq!(c.to_xterm_rgb_str(), "rgb:1212/abab/ffff");
    }

    #[test]
    fn theme_palette_inactive_when_empty() {
        let p = ThemePalette::default();
        assert!(!p.is_active());
    }

    #[test]
    fn theme_palette_active_when_any_field_set() {
        let mut p = ThemePalette::default();
        p.fg = Some(Rgb::new(255, 255, 255));
        assert!(p.is_active());
    }

    #[test]
    fn clipboard_policy_defaults_secure() {
        let p = ClipboardPolicy::default();
        assert_eq!(p.set, Osc52SetPolicy::Confirm);
        assert_eq!(p.get, Osc52GetPolicy::Deny);
        assert_eq!(p.max_bytes, 1024 * 1024);
    }

    #[test]
    fn pane_state_reset_clears_everything() {
        let mut s = PaneTerminalState::new();
        s.bracketed_paste = true;
        s.focus_reporting = true;
        s.mouse_mode = MouseMode {
            protocol: MouseProtocol::Btn,
            encoding: MouseEncoding::Sgr,
        };
        s.kitty_kbd.push(KittyKbdFlags(0b11111));
        s.osc52_decision = Osc52Decision::Allowed;

        s.reset();

        assert!(!s.bracketed_paste);
        assert!(!s.focus_reporting);
        assert!(s.mouse_mode.is_off());
        assert_eq!(s.kitty_kbd.depth(), 0);
        assert_eq!(s.osc52_decision, Osc52Decision::Pending);
    }
}
