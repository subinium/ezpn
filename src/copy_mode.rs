//! Copy mode: vi-style text navigation, selection, search, and clipboard copy.
//!
//! Entered via `Ctrl+B [`. Provides cursor-based navigation through scrollback,
//! visual selection (v/V), search (//?), and yank (y) to OSC 52 clipboard.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthStr;

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

/// SPEC 13 — search engine selectable by `[copy_mode] search` config.
/// `Substring` is the v0.9 default (case-insensitive substring); `Regex`
/// uses the `regex` crate with smart-case (lowercase query → `(?i)`).
///
/// `Regex` lands here unwired — the `[copy_mode] search` config plumbing
/// and Ctrl+R toggle binding ship in a follow-up. The `#[allow(dead_code)]`
/// on the variant keeps clippy `-D warnings` happy until then.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SearchEngine {
    #[default]
    Substring,
    #[allow(dead_code)]
    Regex,
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
    /// SPEC 13 — search backend. Toggleable via `Ctrl+R` while in
    /// `Phase::Search` (binding lands with the full SPEC 13 keymap PR).
    pub search_engine: SearchEngine,
    /// Compile cache for the regex backend, keyed by the post-smart-case
    /// pattern string. Avoids re-compiling on every keystroke during
    /// incremental search and bounds compile-time cost (a pathological
    /// pattern like `a{1000000}` only pays the size_limit penalty once).
    cached_regex: Option<(String, regex::Regex)>,
}

impl CopyModeState {
    pub fn new(pane_rows: u16, pane_cols: u16) -> Self {
        Self::new_with_engine(pane_rows, pane_cols, SearchEngine::default())
    }

    /// Constructor used by callers that read `[copy_mode] search` from
    /// config and want to override the default substring engine.
    pub fn new_with_engine(pane_rows: u16, pane_cols: u16, search_engine: SearchEngine) -> Self {
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
            search_engine,
            cached_regex: None,
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

    let pane_rows = state.pane_rows;
    let pane_cols = state.pane_cols;
    let engine = state.search_engine;
    let matches = match engine {
        SearchEngine::Substring => find_substring(&query, screen, pane_rows, pane_cols),
        SearchEngine::Regex => find_regex(
            &query,
            screen,
            pane_rows,
            pane_cols,
            &mut state.cached_regex,
        ),
    };

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

/// Build `(row_text, col_map)` for one row. `col_map[byte_index]` is the
/// terminal column of the cell that contributed that byte. Wide-char
/// continuation cells are skipped, matching the v0.9 behaviour.
fn build_row_text(screen: &vt100::Screen, row: u16, cols: u16) -> (String, Vec<u16>) {
    let mut row_text = String::new();
    let mut col_map: Vec<u16> = Vec::new();
    for c in 0..cols {
        if let Some(cell) = screen.cell(row, c) {
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
    (row_text, col_map)
}

fn find_substring(
    query: &str,
    screen: &vt100::Screen,
    rows: u16,
    cols: u16,
) -> Vec<(u16, u16, u16)> {
    let mut matches = Vec::new();
    let lower_query = query.to_lowercase();
    for r in 0..rows {
        let (row_text, col_map) = build_row_text(screen, r, cols);
        let lower_text = row_text.to_lowercase();
        let mut start = 0;
        while let Some(pos) = lower_text[start..].find(&lower_query) {
            let byte_pos = start + pos;
            if byte_pos < col_map.len() {
                let col = col_map[byte_pos];
                // Issue #15: highlight length must be display width (cells),
                // not byte length. "🔍" is 4 bytes but 2 cells; "café" is
                // 5 bytes but 4 cells. The previous `lower_query.len()`
                // over-highlighted by 50–200% on emoji / wide-char queries
                // and bled into adjacent cells.
                let match_end = (byte_pos + lower_query.len()).min(row_text.len());
                let display_len = row_text[byte_pos..match_end].width() as u16;
                matches.push((r, col, display_len));
            }
            start = byte_pos + 1;
        }
    }
    matches
}

/// Smart-case judgement matching ripgrep behaviour: only literal characters
/// outside of escape sequences and inside `[…]` character classes count.
/// Examples:
/// - `\D`       → no literal upper (escape eats `D`) → smart-case fires
/// - `[A-Z]+`   → literal `A`/`Z` inside class → user meant uppercase
/// - `\u{0041}` → escape eats `u`, then `{0041}` has no upper letter →
///   smart-case fires (acceptable false positive)
/// - `error`    → no upper → smart-case
/// - `Error`    → literal `E` → case-sensitive
///
/// The previous heuristic (`chars().any(is_uppercase)`) misclassified
/// patterns like `\D` or `\S` as uppercase-bearing, silently wrapping
/// `(?i)` around them — meaningless for shorthand classes but a real
/// behaviour change for `[A-Z]+` (which would match lowercase too).
fn has_literal_uppercase(pattern: &str) -> bool {
    let mut in_class = false;
    let mut escaped = false;
    for c in pattern.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if !in_class && c == '[' {
            in_class = true;
            continue;
        }
        if in_class && c == ']' {
            in_class = false;
            continue;
        }
        if c.is_uppercase() {
            return true;
        }
    }
    false
}

/// SPEC 13 — regex search using the `regex` crate. Smart-case: a query
/// with no literal uppercase letter (per `has_literal_uppercase`) gets
/// `(?i)` prepended (matches vim/ripgrep convention). Invalid patterns
/// produce **zero** matches rather than a panic — the search prompt stays
/// open so the user can edit.
///
/// Compile-time cost is bounded via `RegexBuilder::size_limit` /
/// `dfa_size_limit` (1 MiB each). A pathological pattern like `a{100000}`
/// fails at `build()` and returns no matches instead of stalling the
/// daemon main loop. The compiled regex is cached by post-smart-case
/// pattern so incremental search (one keystroke = one extra char) does
/// not recompile when the user is just typing past a character.
///
/// Search remains line-scoped, walking left-to-right matching the
/// substring engine's footprint.
fn find_regex(
    query: &str,
    screen: &vt100::Screen,
    rows: u16,
    cols: u16,
    cache: &mut Option<(String, regex::Regex)>,
) -> Vec<(u16, u16, u16)> {
    let pattern = if has_literal_uppercase(query) {
        query.to_string()
    } else {
        format!("(?i){query}")
    };
    let need_recompile = cache.as_ref().is_none_or(|(p, _)| p != &pattern);
    if need_recompile {
        match regex::RegexBuilder::new(&pattern)
            .size_limit(1 << 20)
            .dfa_size_limit(1 << 20)
            .build()
        {
            Ok(re) => *cache = Some((pattern, re)),
            Err(_) => {
                *cache = None;
                return Vec::new();
            }
        }
    }
    let re = match cache.as_ref() {
        Some((_, re)) => re,
        None => return Vec::new(),
    };
    let mut matches = Vec::new();
    for r in 0..rows {
        let (row_text, col_map) = build_row_text(screen, r, cols);
        for m in re.find_iter(&row_text) {
            let byte_pos = m.start();
            if byte_pos < col_map.len() {
                let col = col_map[byte_pos];
                // Same display-width fix as the substring path.
                let match_end = m.end().min(row_text.len());
                let display_len = row_text[byte_pos..match_end].width() as u16;
                matches.push((r, col, display_len));
            }
        }
    }
    matches
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

#[cfg(test)]
mod tests {
    use super::*;

    fn screen_with(rows: u16, cols: u16, lines: &[&str]) -> vt100::Screen {
        let mut p = vt100::Parser::new(rows, cols, 0);
        for (i, line) in lines.iter().enumerate() {
            p.process(line.as_bytes());
            if i < lines.len() - 1 {
                p.process(b"\r\n");
            }
        }
        p.screen().clone()
    }

    #[test]
    fn substring_matches_lower_query_against_uppercase() {
        let screen = screen_with(3, 80, &["ERROR 404", "warning ERR-12", "ok"]);
        let m = find_substring("err", &screen, 3, 80);
        // Case-insensitive: matches both "ERROR" (row 0) and "ERR" (row 1).
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].0, 0);
        assert_eq!(m[1].0, 1);
    }

    #[test]
    fn regex_matches_anchored_pattern() {
        let screen = screen_with(3, 80, &["ERROR 404", "warning ERR-12", "ok"]);
        let mut cache = None;
        // Anchored regex — only line starting with "ERR".
        let m = find_regex("^ERR", &screen, 3, 80, &mut cache);
        assert_eq!(
            m.len(),
            1,
            "anchored ^ERR must match exactly one row, got {m:?}"
        );
        assert_eq!(m[0].0, 0);
    }

    #[test]
    fn regex_smart_case_lowercase_query_matches_uppercase() {
        let screen = screen_with(2, 80, &["ERROR 404", "ok"]);
        let mut cache = None;
        // Lowercase query → smart-case kicks in (?i) prefix.
        let m = find_regex("error", &screen, 2, 80, &mut cache);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn regex_uppercase_in_query_disables_smart_case() {
        let screen = screen_with(2, 80, &["ERROR 404", "error ok"]);
        let mut cache = None;
        // Uppercase E in query → no (?i) prefix → matches only "ERROR".
        let m = find_regex("ERROR", &screen, 2, 80, &mut cache);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].0, 0);
    }

    #[test]
    fn regex_invalid_pattern_returns_empty() {
        let screen = screen_with(1, 80, &["abc"]);
        let mut cache = None;
        // Unclosed character class — must not panic.
        let m = find_regex("[unclosed", &screen, 1, 80, &mut cache);
        assert!(m.is_empty());
    }

    #[test]
    fn regex_finds_multiple_per_line_walking_left_to_right() {
        let screen = screen_with(1, 80, &["ERR-1 ERR-2 ERR-3"]);
        let mut cache = None;
        let m = find_regex(r"ERR-\d", &screen, 1, 80, &mut cache);
        assert_eq!(m.len(), 3);
        for (r, _, _) in &m {
            assert_eq!(*r, 0);
        }
        assert!(m[0].1 < m[1].1 && m[1].1 < m[2].1);
    }

    #[test]
    fn smart_case_skips_escape_shorthand_classes() {
        // \D, \S, \W carry no literal uppercase letter — smart-case should
        // still fire (the previous heuristic counted the escape's letter
        // and silently disabled smart-case).
        assert!(!has_literal_uppercase(r"\D"));
        assert!(!has_literal_uppercase(r"\S"));
        assert!(!has_literal_uppercase(r"\w"));
        assert!(!has_literal_uppercase("error"));
        // [A-Z]+ — the user clearly meant uppercase. Inside the class the
        // literal A and Z trip case-sensitive mode.
        assert!(has_literal_uppercase("[A-Z]+"));
        // Mixed: outside-class literal upper still wins.
        assert!(has_literal_uppercase("Error"));
        // Unicode escape: opaque to the heuristic — acceptable false negative.
        assert!(!has_literal_uppercase(r"\u{0041}"));
    }

    #[test]
    fn regex_charclass_uppercase_disables_smart_case() {
        let screen = screen_with(2, 80, &["ABC", "abc"]);
        let mut cache = None;
        // [A-Z]+ — smart-case must NOT add (?i), so only the uppercase row
        // matches.
        let m = find_regex("[A-Z]+", &screen, 2, 80, &mut cache);
        assert_eq!(m.len(), 1, "charclass-uppercase should be case-sensitive");
        assert_eq!(m[0].0, 0);
    }

    #[test]
    fn regex_compile_cache_is_reused_across_calls() {
        let screen = screen_with(2, 80, &["abc def", "abc def"]);
        let mut cache = None;
        // First call compiles into the cache.
        let _ = find_regex("abc", &screen, 2, 80, &mut cache);
        let pattern_cached = cache.as_ref().map(|(p, _)| p.clone());
        assert_eq!(pattern_cached.as_deref(), Some("(?i)abc"));
        // Second call with same query: must NOT replace the cache entry
        // (the same `(?i)abc` pattern stays).
        let _ = find_regex("abc", &screen, 2, 80, &mut cache);
        assert_eq!(
            cache.as_ref().map(|(p, _)| p.as_str()),
            Some("(?i)abc"),
            "same query must reuse cached compile"
        );
        // Different query: cache replaced.
        let _ = find_regex("def", &screen, 2, 80, &mut cache);
        assert_eq!(cache.as_ref().map(|(p, _)| p.as_str()), Some("(?i)def"));
    }

    #[test]
    fn regex_pathological_pattern_is_rejected_by_size_limit() {
        // A pattern that would explode the compiled program size hits the
        // size_limit and falls into the empty-matches arm rather than
        // stalling the daemon main loop. Use a repetition so wide that it
        // exceeds the 1 MiB program cap without taking forever to detect.
        let screen = screen_with(1, 80, &["aaaaaaaaaaaaaaaaaaaa"]);
        let mut cache = None;
        let huge = format!("a{{0,{n}}}", n = 1_000_000);
        let m = find_regex(&huge, &screen, 1, 80, &mut cache);
        assert!(
            m.is_empty(),
            "size_limit must reject the pattern (got {m:?})"
        );
        assert!(cache.is_none(), "failed compile must clear the cache");
    }

    #[test]
    fn search_engine_default_is_substring() {
        assert_eq!(SearchEngine::default(), SearchEngine::Substring);
    }

    #[test]
    fn copy_mode_state_new_with_engine_picks_correct_backend() {
        let state = CopyModeState::new_with_engine(24, 80, SearchEngine::Regex);
        assert_eq!(state.search_engine, SearchEngine::Regex);
        let default_state = CopyModeState::new(24, 80);
        assert_eq!(default_state.search_engine, SearchEngine::Substring);
    }
}
