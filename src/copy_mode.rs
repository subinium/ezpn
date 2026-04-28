//! Copy mode: vi-style text navigation, selection, search, and clipboard copy.
//!
//! Entered via `Ctrl+B [`. Provides cursor-based navigation through scrollback,
//! visual selection (v/V), search (//?), and yank (y) to OSC 52 clipboard.
//!
//! ## Yank pipeline (#91, #92)
//! `handle_key` returns [`CopyAction::CopyAndExit(text)`] — the actual
//! side-effects live with the caller (server/input_modes.rs and
//! server/mouse.rs, both off-limits for this slice). [`yank_to_buffer`]
//! is the helper they call to:
//!
//! 1. Push the text into the default slot of the [`crate::buffers::BufferStore`].
//! 2. Try the system clipboard via [`crate::clipboard::copy`] — Wayland,
//!    X11, then macOS, all gated on tool availability.
//! 3. Return a [`YankReport`] so the caller can decide whether to also
//!    emit OSC 52 (the historical fallback).
//!
//! The caller still owns the OSC 52 push because the OSC sequence has
//! to flow through the per-pane output buffer (`pane.osc52_pending`)
//! to reach the attached client — that plumbing lives in `server/`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::buffers::{BufferStore, SetError};
use crate::clipboard::{self, ClipboardError};

/// Copy mode sub-phase.
pub enum Phase {
    /// Navigating with cursor, no selection.
    Navigate,
    /// Visual character selection.
    VisualChar { anchor_row: u16, anchor_col: u16 },
    /// Visual line selection.
    VisualLine { anchor_row: u16 },
    /// Search input.
    Search { forward: bool, query: String },
}

/// Full copy mode state.
pub struct CopyModeState {
    pub phase: Phase,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub pane_rows: u16,
    pub pane_cols: u16,
    pub last_search: Option<String>,
    pub last_search_forward: bool,
    pub search_matches: Vec<(u16, u16, u16)>, // (row, col, len)
    pub current_match_idx: Option<usize>,
}

impl CopyModeState {
    pub fn new(pane_rows: u16, pane_cols: u16) -> Self {
        Self {
            phase: Phase::Navigate,
            cursor_row: pane_rows.saturating_sub(1),
            cursor_col: 0,
            pane_rows,
            pane_cols,
            last_search: None,
            last_search_forward: true,
            search_matches: Vec::new(),
            current_match_idx: None,
        }
    }

    /// Get the current selection range for rendering, if any.
    /// Returns (start_row, start_col, end_row, end_col).
    pub fn selection(&self) -> Option<(u16, u16, u16, u16)> {
        match &self.phase {
            Phase::VisualChar {
                anchor_row,
                anchor_col,
            } => Some(normalize(
                *anchor_row,
                *anchor_col,
                self.cursor_row,
                self.cursor_col,
            )),
            Phase::VisualLine { anchor_row } => {
                let (sr, er) = if *anchor_row <= self.cursor_row {
                    (*anchor_row, self.cursor_row)
                } else {
                    (self.cursor_row, *anchor_row)
                };
                Some((sr, 0, er, self.pane_cols.saturating_sub(1)))
            }
            _ => None,
        }
    }

    /// Mode label for the status bar.
    pub fn mode_label(&self) -> &str {
        match &self.phase {
            Phase::Navigate => "COPY",
            Phase::VisualChar { .. } => "VISUAL",
            Phase::VisualLine { .. } => "V-LINE",
            Phase::Search { .. } => "SEARCH",
        }
    }

    /// Search query for display (if in search phase).
    #[allow(dead_code)]
    pub fn search_prompt(&self) -> Option<String> {
        match &self.phase {
            Phase::Search { forward, query } => {
                let prefix = if *forward { "/" } else { "?" };
                Some(format!("{}{}", prefix, query))
            }
            _ => None,
        }
    }
}

/// Result of handling a key in copy mode.
pub enum CopyAction {
    /// Stay in copy mode, pane needs redraw.
    Redraw,
    /// Copy text to clipboard and exit copy mode.
    CopyAndExit(String),
    /// Exit copy mode without copying.
    Exit,
    /// No state change.
    None,
}

/// Handle a key event in copy mode.
pub fn handle_key(
    key: KeyEvent,
    state: &mut CopyModeState,
    screen: &vt100::Screen,
    scroll_up: &mut dyn FnMut(usize),
    scroll_down: &mut dyn FnMut(usize),
) -> CopyAction {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // Search input sub-mode
    if let Phase::Search {
        forward,
        ref mut query,
    } = &mut state.phase
    {
        let fwd = *forward;
        match key.code {
            KeyCode::Enter => {
                state.last_search = Some(query.clone());
                state.last_search_forward = fwd;
                execute_search(state, screen);
                state.phase = Phase::Navigate;
                return CopyAction::Redraw;
            }
            KeyCode::Esc => {
                state.search_matches.clear();
                state.current_match_idx = None;
                state.phase = Phase::Navigate;
                return CopyAction::Redraw;
            }
            KeyCode::Backspace => {
                query.pop();
                execute_search(state, screen);
                return CopyAction::Redraw;
            }
            KeyCode::Char(c) if !ctrl => {
                query.push(c);
                execute_search(state, screen);
                return CopyAction::Redraw;
            }
            _ => return CopyAction::None,
        }
    }

    match key.code {
        // Navigation
        KeyCode::Char('h') | KeyCode::Left => {
            state.cursor_col = state.cursor_col.saturating_sub(1);
            CopyAction::Redraw
        }
        KeyCode::Char('l') | KeyCode::Right => {
            if state.cursor_col + 1 < state.pane_cols {
                state.cursor_col += 1;
            }
            CopyAction::Redraw
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if state.cursor_row + 1 < state.pane_rows {
                state.cursor_row += 1;
            } else {
                scroll_down(1);
            }
            CopyAction::Redraw
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if state.cursor_row > 0 {
                state.cursor_row -= 1;
            } else {
                scroll_up(1);
            }
            CopyAction::Redraw
        }

        // Word movement
        KeyCode::Char('w') => {
            move_word_forward(state, screen);
            CopyAction::Redraw
        }
        KeyCode::Char('b') => {
            move_word_backward(state, screen);
            CopyAction::Redraw
        }

        // Line movement
        KeyCode::Char('0') => {
            state.cursor_col = 0;
            CopyAction::Redraw
        }
        KeyCode::Char('$') => {
            state.cursor_col = line_end(state, screen);
            CopyAction::Redraw
        }
        KeyCode::Char('^') => {
            state.cursor_col = first_non_blank(state, screen);
            CopyAction::Redraw
        }

        // Page movement
        KeyCode::Char('g') => {
            scroll_up(usize::MAX);
            state.cursor_row = 0;
            state.cursor_col = 0;
            CopyAction::Redraw
        }
        KeyCode::Char('G') => {
            scroll_down(usize::MAX);
            state.cursor_row = state.pane_rows.saturating_sub(1);
            state.cursor_col = 0;
            CopyAction::Redraw
        }
        KeyCode::Char('u') if ctrl => {
            let half = (state.pane_rows / 2) as usize;
            scroll_up(half);
            CopyAction::Redraw
        }
        KeyCode::Char('d') if ctrl => {
            let half = (state.pane_rows / 2) as usize;
            scroll_down(half);
            CopyAction::Redraw
        }
        KeyCode::PageUp => {
            let page = state.pane_rows as usize;
            scroll_up(page);
            CopyAction::Redraw
        }
        KeyCode::PageDown => {
            let page = state.pane_rows as usize;
            scroll_down(page);
            CopyAction::Redraw
        }

        // Viewport positions
        KeyCode::Char('H') => {
            state.cursor_row = 0;
            CopyAction::Redraw
        }
        KeyCode::Char('M') => {
            state.cursor_row = state.pane_rows / 2;
            CopyAction::Redraw
        }
        KeyCode::Char('L') => {
            state.cursor_row = state.pane_rows.saturating_sub(1);
            CopyAction::Redraw
        }

        // Selection
        KeyCode::Char('v') => {
            state.phase = match state.phase {
                Phase::VisualChar { .. } => Phase::Navigate,
                _ => Phase::VisualChar {
                    anchor_row: state.cursor_row,
                    anchor_col: state.cursor_col,
                },
            };
            CopyAction::Redraw
        }
        KeyCode::Char('V') => {
            state.phase = match state.phase {
                Phase::VisualLine { .. } => Phase::Navigate,
                _ => Phase::VisualLine {
                    anchor_row: state.cursor_row,
                },
            };
            CopyAction::Redraw
        }
        KeyCode::Char(' ') if matches!(state.phase, Phase::Navigate) => {
            state.phase = Phase::VisualChar {
                anchor_row: state.cursor_row,
                anchor_col: state.cursor_col,
            };
            CopyAction::Redraw
        }

        // Yank / copy
        KeyCode::Char('y') | KeyCode::Enter => {
            if let Some(text) = extract_selection(state, screen) {
                CopyAction::CopyAndExit(text)
            } else {
                CopyAction::None
            }
        }

        // Search
        KeyCode::Char('/') => {
            state.phase = Phase::Search {
                forward: true,
                query: String::new(),
            };
            CopyAction::Redraw
        }
        KeyCode::Char('?') => {
            state.phase = Phase::Search {
                forward: false,
                query: String::new(),
            };
            CopyAction::Redraw
        }
        KeyCode::Char('n') => {
            if state.last_search.is_some() {
                jump_to_match(state, state.last_search_forward);
            }
            CopyAction::Redraw
        }
        KeyCode::Char('N') => {
            if state.last_search.is_some() {
                jump_to_match(state, !state.last_search_forward);
            }
            CopyAction::Redraw
        }

        // Exit
        KeyCode::Char('q') | KeyCode::Esc => CopyAction::Exit,

        _ => CopyAction::None,
    }
}

// ── Helpers ──

fn normalize(sr: u16, sc: u16, er: u16, ec: u16) -> (u16, u16, u16, u16) {
    if sr < er || (sr == er && sc <= ec) {
        (sr, sc, er, ec)
    } else {
        (er, ec, sr, sc)
    }
}

fn cell_char(screen: &vt100::Screen, r: u16, c: u16) -> char {
    screen
        .cell(r, c)
        .map(|cell| {
            let s = cell.contents();
            s.chars().next().unwrap_or(' ')
        })
        .unwrap_or(' ')
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn line_end(state: &CopyModeState, screen: &vt100::Screen) -> u16 {
    let mut last = 0u16;
    for c in 0..state.pane_cols {
        let ch = cell_char(screen, state.cursor_row, c);
        if ch != ' ' {
            last = c;
        }
    }
    last
}

fn first_non_blank(state: &CopyModeState, screen: &vt100::Screen) -> u16 {
    for c in 0..state.pane_cols {
        let ch = cell_char(screen, state.cursor_row, c);
        if ch != ' ' {
            return c;
        }
    }
    0
}

fn move_word_forward(state: &mut CopyModeState, screen: &vt100::Screen) {
    let mut r = state.cursor_row;
    let mut c = state.cursor_col;
    let start_word = is_word_char(cell_char(screen, r, c));

    // Skip current word class
    loop {
        c += 1;
        if c >= state.pane_cols {
            c = 0;
            r += 1;
            if r >= state.pane_rows {
                return;
            }
        }
        let ch = cell_char(screen, r, c);
        if is_word_char(ch) != start_word || ch == ' ' {
            break;
        }
    }
    // Skip whitespace
    loop {
        let ch = cell_char(screen, r, c);
        if ch != ' ' {
            break;
        }
        c += 1;
        if c >= state.pane_cols {
            c = 0;
            r += 1;
            if r >= state.pane_rows {
                return;
            }
        }
    }
    state.cursor_row = r;
    state.cursor_col = c;
}

fn move_word_backward(state: &mut CopyModeState, screen: &vt100::Screen) {
    let mut r = state.cursor_row;
    let mut c = state.cursor_col;

    // Move back one
    if c == 0 {
        if r == 0 {
            return;
        }
        r -= 1;
        c = state.pane_cols - 1;
    } else {
        c -= 1;
    }

    // Skip whitespace
    loop {
        let ch = cell_char(screen, r, c);
        if ch != ' ' {
            break;
        }
        if c == 0 {
            if r == 0 {
                state.cursor_row = 0;
                state.cursor_col = 0;
                return;
            }
            r -= 1;
            c = state.pane_cols - 1;
        } else {
            c -= 1;
        }
    }

    // Skip current word class backward
    let target_word = is_word_char(cell_char(screen, r, c));
    loop {
        if c == 0 {
            break;
        }
        let prev = cell_char(screen, r, c - 1);
        if is_word_char(prev) != target_word || prev == ' ' {
            break;
        }
        c -= 1;
    }

    state.cursor_row = r;
    state.cursor_col = c;
}

fn extract_selection(state: &CopyModeState, screen: &vt100::Screen) -> Option<String> {
    let (sr, sc, er, ec) = state.selection()?;
    let mut text = String::new();
    for r in sr..=er {
        let c_start = if r == sr { sc } else { 0 };
        let c_end = if r == er { ec } else { state.pane_cols - 1 };
        let mut row_text = String::new();
        for c in c_start..=c_end {
            if let Some(cell) = screen.cell(r, c) {
                if cell.is_wide_continuation() {
                    continue;
                }
                let s = cell.contents();
                if s.is_empty() {
                    row_text.push(' ');
                } else {
                    row_text.push_str(&s);
                }
            }
        }
        text.push_str(row_text.trim_end());
        if r < er {
            text.push('\n');
        }
    }
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn execute_search(state: &mut CopyModeState, screen: &vt100::Screen) {
    let query = match &state.phase {
        Phase::Search { query, .. } => query.clone(),
        _ => state.last_search.clone().unwrap_or_default(),
    };
    if query.is_empty() {
        state.search_matches.clear();
        state.current_match_idx = None;
        return;
    }

    let mut matches = Vec::new();
    let lower_query = query.to_lowercase();

    for r in 0..state.pane_rows {
        let mut row_text = String::new();
        let mut col_map: Vec<u16> = Vec::new();
        for c in 0..state.pane_cols {
            if let Some(cell) = screen.cell(r, c) {
                if cell.is_wide_continuation() {
                    continue;
                }
                let start_byte = row_text.len();
                let s = cell.contents();
                if s.is_empty() {
                    row_text.push(' ');
                } else {
                    row_text.push_str(&s);
                }
                for _ in start_byte..row_text.len() {
                    col_map.push(c);
                }
            }
        }

        let lower_text = row_text.to_lowercase();
        let mut start = 0;
        while let Some(pos) = lower_text[start..].find(&lower_query) {
            let byte_pos = start + pos;
            if byte_pos < col_map.len() {
                let col = col_map[byte_pos];
                let display_len = lower_query.len().min(col_map.len() - byte_pos) as u16;
                matches.push((r, col, display_len));
            }
            start = byte_pos + 1;
        }
    }

    state.search_matches = matches;

    // Jump to nearest match after cursor
    if !state.search_matches.is_empty() {
        let cursor = (state.cursor_row, state.cursor_col);
        let forward = match &state.phase {
            Phase::Search { forward, .. } => *forward,
            _ => state.last_search_forward,
        };
        state.current_match_idx = if forward {
            state
                .search_matches
                .iter()
                .position(|(r, c, _)| (*r, *c) > cursor)
                .or(Some(0))
        } else {
            state
                .search_matches
                .iter()
                .rposition(|(r, c, _)| (*r, *c) < cursor)
                .or(Some(state.search_matches.len() - 1))
        };

        if let Some(idx) = state.current_match_idx {
            let (r, c, _) = state.search_matches[idx];
            state.cursor_row = r;
            state.cursor_col = c;
        }
    } else {
        state.current_match_idx = None;
    }
}

fn jump_to_match(state: &mut CopyModeState, forward: bool) {
    if state.search_matches.is_empty() {
        return;
    }
    let total = state.search_matches.len();
    let idx = state.current_match_idx.unwrap_or(0);
    let next = if forward {
        (idx + 1) % total
    } else if idx == 0 {
        total - 1
    } else {
        idx - 1
    };
    state.current_match_idx = Some(next);
    let (r, c, _) = state.search_matches[next];
    state.cursor_row = r;
    state.cursor_col = c;
}

// ─── Yank pipeline (#91, #92) ──────────────────────────────
//
// `yank_to_buffer` and `YankReport` are deferred-integration surface:
// the call site lives in `server/input_modes.rs` (off-limits for this
// slice). `#[allow(dead_code)]` keeps cargo quiet without resorting to
// a module-wide silencer that would also hide drift in the existing
// copy-mode key handler.

/// Outcome of [`yank_to_buffer`]. The caller decides whether to also
/// emit OSC 52 — if `clipboard` is `Err(_)` (and *especially* if it is
/// `Err(NoCommand)`), OSC 52 is the historical fallback and should
/// fire. If a real clipboard tool ran successfully, OSC 52 is still
/// useful for forwarding through SSH-attached terminals; the policy is
/// caller-defined.
#[allow(dead_code)]
#[derive(Debug)]
pub struct YankReport {
    /// Result of pushing the text into the default buffer slot. Always
    /// `Ok(())` for payloads ≤ 16 MiB; `Err(SetError::TooLarge)`
    /// otherwise. The buffer push happens *before* the clipboard exec
    /// so the user can still paste-buffer the yank even when the host
    /// has no graphical clipboard.
    pub buffer: Result<(), SetError>,
    /// Result of [`crate::clipboard::copy`]. `Ok(label)` carries the
    /// program name (`"wl-copy"` / `"override(my-tool)"` / …) so the
    /// caller can log it once per session. `Err(_)` means the caller
    /// should fall back to OSC 52.
    pub clipboard: Result<String, ClipboardError>,
}

#[allow(dead_code)]
impl YankReport {
    /// True iff at least one of the two paths succeeded. Used by the
    /// status-bar flash to decide between a "yanked" and a "copy
    /// failed" message.
    pub fn any_success(&self) -> bool {
        self.buffer.is_ok() || self.clipboard.is_ok()
    }
}

/// Push a yanked selection through both the named-buffer store and the
/// system-clipboard fallback chain.
///
/// `buffers` is the long-lived [`BufferStore`] held by the server.
/// `copy_command_override` mirrors the parsed
/// `EzpnConfig::clipboard_copy_command` slice — `None` means
/// auto-detect (same as `Some(&[])`).
///
/// Returns a [`YankReport`] so the caller can chain OSC 52 fall-back
/// and a status-bar flash.
#[allow(dead_code)]
pub fn yank_to_buffer(
    text: &str,
    buffers: &mut BufferStore,
    copy_command_override: Option<&[String]>,
) -> YankReport {
    let buffer = buffers.set(BufferStore::DEFAULT_NAME, text.to_string());
    let clipboard = clipboard::copy(text, copy_command_override);
    YankReport { buffer, clipboard }
}

#[cfg(test)]
mod yank_tests {
    use super::*;

    #[test]
    fn yank_pushes_text_into_default_buffer() {
        let mut store = BufferStore::new();
        let report = yank_to_buffer("hello world", &mut store, None);
        assert!(report.buffer.is_ok());
        assert_eq!(store.default_buffer().unwrap().text, "hello world");
    }

    #[test]
    fn yank_uses_override_argv_for_clipboard() {
        // Use a non-existent program so the clipboard call deterministically
        // fails — the buffer push must still succeed and the report must
        // carry the spawn error.
        let mut store = BufferStore::new();
        let argv = vec!["this-binary-does-not-exist-zzz".to_string()];
        let report = yank_to_buffer("payload", &mut store, Some(&argv));
        assert!(report.buffer.is_ok());
        assert!(matches!(
            report.clipboard,
            Err(ClipboardError::Spawn { .. })
        ));
        // Buffer succeeded → caller's flash message should still say
        // "yanked".
        assert!(report.any_success());
    }

    #[test]
    fn yank_oversize_payload_fails_buffer_but_attempts_clipboard() {
        let mut store = BufferStore::new();
        let big = "x".repeat(BufferStore::MAX_BYTES + 1);
        let argv = vec!["this-binary-does-not-exist-zzz".to_string()];
        let report = yank_to_buffer(&big, &mut store, Some(&argv));
        assert!(matches!(report.buffer, Err(SetError::TooLarge { .. })));
        // Both paths failed — caller logs and falls back to OSC 52
        // (which the OSC 52 hard cap from #79 will then also reject).
        assert!(!report.any_success());
    }
}
