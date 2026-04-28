//! Input-mode state machine + per-event dispatchers.
//!
//! Owns the [`InputMode`] enum (Normal / Prefix / CopyMode /
//! QuitConfirm / CloseConfirm / CloseTabConfirm / ResizeMode /
//! PaneSelect / HelpOverlay / RenameTab / CommandPalette) and the three
//! crossterm event handlers (`process_event` -> `process_key` /
//! `process_mouse`). Split from the monolithic `server.rs` per #60 so
//! the state machine has a single home and the rest of the server
//! tree can pull it in via `super::input_modes::*`.
//!
//! Helpers from the crate root (`do_split`, `resize_all`, `close_pane`,
//! `make_inner`, `replace_pane`, `extract_selected_text`, `base64_encode`)
//! are reached through `crate::*` exactly as before — the move only
//! changes the `super::*` path semantics, never which symbols resolve.

use std::collections::HashMap;
use std::time::Instant;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::layout::{Direction, Layout, NavDir, Rect, SepHit};
use crate::pane::{Pane, PaneLaunch};
use crate::render::BorderCache;
use crate::settings::{Settings, SettingsAction};

use super::{actions, RenderUpdate};

/// Server-runtime knobs threaded into `process_event` (#79 / #84 / #86).
///
/// These live behind a single struct so the caller's arg list doesn't
/// balloon further every time a new subsystem is wired up. The fields
/// are mutable references so the dispatcher can short-circuit input on
/// an OSC 52 confirm prompt, hot-swap the keymap on reload, and rebuild
/// the fuzzy palette index when entering CommandPalette mode.
#[allow(clippy::struct_field_names)]
pub(super) struct RuntimeCtx<'a> {
    pub keymap: &'a crate::keymap::Keymap,
    pub osc52_confirm: &'a mut Option<super::Osc52ConfirmState>,
    pub fuzzy_index: &'a mut Option<crate::fuzzy::FuzzyIndex>,
    pub palette_query: &'a mut String,
    pub palette_selected: &'a mut usize,
    pub history: &'a mut crate::fuzzy::History,
    pub session_name: &'a str,
}

/// Input state machine for prefix key support.
#[allow(dead_code)]
pub(super) enum InputMode {
    Normal,
    Prefix {
        entered_at: Instant,
    },
    CopyMode(crate::copy_mode::CopyModeState),
    QuitConfirm,
    CloseConfirm,
    CloseTabConfirm,
    ResizeMode,
    PaneSelect,
    HelpOverlay,
    /// Tab rename: typing a new name for the current tab.
    RenameTab {
        buffer: String,
    },
    /// Command palette: typing a command to execute.
    CommandPalette {
        buffer: String,
    },
}

/// Tab action requested by the key handler. The main loop handles the switch.
pub(crate) enum TabAction {
    None,
    NewTab,
    NextTab,
    PrevTab,
    GoToTab(usize),
    CloseTab,
    Rename(String),
    KillSession,
}

/// Text selection state for copy-on-drag.
#[derive(Clone)]
pub(super) struct TextSelection {
    pub(super) pane_id: usize,
    pub(super) start_row: u16,
    pub(super) start_col: u16,
    pub(super) end_row: u16,
    pub(super) end_col: u16,
}

impl TextSelection {
    pub(super) fn normalized(&self) -> (u16, u16, u16, u16) {
        if self.start_row < self.end_row
            || (self.start_row == self.end_row && self.start_col <= self.end_col)
        {
            (self.start_row, self.start_col, self.end_row, self.end_col)
        } else {
            (self.end_row, self.end_col, self.start_row, self.start_col)
        }
    }
}

pub(super) struct DragState {
    path: Vec<bool>,
    direction: Direction,
    area: Rect,
}

impl DragState {
    pub(super) fn from_hit(hit: SepHit) -> Self {
        Self {
            path: hit.path,
            direction: hit.direction,
            area: hit.area,
        }
    }

    pub(super) fn path(&self) -> &[bool] {
        &self.path
    }

    pub(super) fn calc_ratio(&self, mx: u16, my: u16) -> f32 {
        match self.direction {
            Direction::Horizontal => {
                let usable = self.area.w.saturating_sub(1) as f32;
                if usable <= 0.0 {
                    return 0.5;
                }
                ((mx as f32 - self.area.x as f32) / usable).clamp(0.1, 0.9)
            }
            Direction::Vertical => {
                let usable = self.area.h.saturating_sub(1) as f32;
                if usable <= 0.0 {
                    return 0.5;
                }
                ((my as f32 - self.area.y as f32) / usable).clamp(0.1, 0.9)
            }
        }
    }
}

/// Process a single crossterm Event (shared between direct and server modes).
#[allow(clippy::too_many_arguments, unused_variables)]
pub(super) fn process_event(
    event: Event,
    mode: &mut InputMode,
    layout: &mut Layout,
    panes: &mut HashMap<usize, Pane>,
    active: &mut usize,
    settings: &mut Settings,
    update: &mut RenderUpdate,
    drag: &mut Option<DragState>,
    zoomed_pane: &mut Option<usize>,
    last_click: &mut Option<(Instant, u16, u16)>,
    broadcast: &mut bool,
    last_active: &mut usize,
    selection_anchor: &mut Option<(usize, u16, u16)>,
    text_selection: &mut Option<TextSelection>,
    default_shell: &str,
    tw: u16,
    th: u16,
    scrollback: usize,
    border_cache: &Option<BorderCache>,
    detach_requested: &mut bool,
    tab_action: &mut TabAction,
    tab_names: &[(usize, String, bool)],
    prefix_key: char,
    flash_message: &mut Option<(String, Instant)>,
    buffers: &mut crate::buffers::BufferStore,
    clipboard_copy_argv: Option<&[String]>,
    ctx: &mut RuntimeCtx<'_>,
) {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            process_key(
                key,
                mode,
                layout,
                panes,
                active,
                settings,
                update,
                zoomed_pane,
                broadcast,
                last_active,
                default_shell,
                tw,
                th,
                scrollback,
                border_cache,
                detach_requested,
                tab_action,
                prefix_key,
                flash_message,
                buffers,
                clipboard_copy_argv,
                ctx,
            );
        }
        Event::Mouse(mouse) => {
            if let Some(ref cache) = border_cache {
                let inner = cache.inner().clone();
                super::mouse::process_mouse(
                    mouse,
                    mode,
                    layout,
                    panes,
                    active,
                    settings,
                    update,
                    drag,
                    zoomed_pane,
                    last_click,
                    broadcast,
                    selection_anchor,
                    text_selection,
                    default_shell,
                    tw,
                    th,
                    scrollback,
                    cache,
                    &inner,
                    tab_action,
                    tab_names,
                );
            }
        }
        Event::Resize(w, h) => {
            // Handled separately via C_RESIZE message
            let _ = (w, h);
        }
        Event::FocusGained => {
            // Forward focus to active pane (only if it requested focus events)
            if let Some(pane) = panes.get_mut(active) {
                if pane.is_alive() && pane.wants_focus() {
                    pane.write_bytes(b"\x1b[I");
                }
            }
        }
        Event::FocusLost => {
            if let Some(pane) = panes.get_mut(active) {
                if pane.is_alive() && pane.wants_focus() {
                    pane.write_bytes(b"\x1b[O");
                }
            }
        }
        Event::Paste(text) => {
            // Forward paste to active pane, with bracketed paste wrapping if enabled
            if let Some(pane) = panes.get_mut(active) {
                if pane.is_alive() {
                    if pane.bracketed_paste() {
                        pane.write_bytes(b"\x1b[200~");
                        pane.write_bytes(text.as_bytes());
                        pane.write_bytes(b"\x1b[201~");
                    } else {
                        pane.write_bytes(text.as_bytes());
                    }
                }
            }
        }
        _ => {}
    }
}

/// Process a key event. This is the core input handler shared between modes.
#[allow(clippy::too_many_arguments, unused_variables)]
pub(super) fn process_key(
    key: KeyEvent,
    mode: &mut InputMode,
    layout: &mut Layout,
    panes: &mut HashMap<usize, Pane>,
    active: &mut usize,
    settings: &mut Settings,
    update: &mut RenderUpdate,
    zoomed_pane: &mut Option<usize>,
    broadcast: &mut bool,
    last_active: &mut usize,
    default_shell: &str,
    tw: u16,
    th: u16,
    scrollback: usize,
    border_cache: &Option<BorderCache>,
    detach_requested: &mut bool,
    tab_action: &mut TabAction,
    prefix_key: char,
    flash_message: &mut Option<(String, Instant)>,
    buffers: &mut crate::buffers::BufferStore,
    clipboard_copy_argv: Option<&[String]>,
    ctx: &mut RuntimeCtx<'_>,
) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    // ── OSC 52 confirm prompt (#79) ──
    // Modal: while a payload is queued, all keys route here. y/n
    // resolve the prompt; Esc re-queues. Status bar shows "OSC52".
    if let Some(state) = ctx.osc52_confirm.as_mut() {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(pane) = panes.get_mut(&state.pane_id) {
                    pane.set_osc52_decision(crate::terminal_state::Osc52Decision::Allowed);
                    // Drain queued payloads into the forward queue so
                    // they reach attached clients on the next iteration.
                    pane.osc52_pending.append(&mut state.queued_payloads);
                }
                *ctx.osc52_confirm = None;
                update.full_redraw = true;
                update.status_dirty = true;
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                if let Some(pane) = panes.get_mut(&state.pane_id) {
                    pane.set_osc52_decision(crate::terminal_state::Osc52Decision::Denied);
                }
                state.queued_payloads.clear();
                *ctx.osc52_confirm = None;
                update.full_redraw = true;
                update.status_dirty = true;
            }
            KeyCode::Esc => {
                // Re-queue: drop the prompt but leave decision Pending so
                // the next decoded payload from the same pane will surface
                // a fresh prompt. The current queued payloads are pushed
                // back onto the pane's confirm queue so we don't lose them.
                if let Some(pane) = panes.get_mut(&state.pane_id) {
                    let payloads = std::mem::take(&mut state.queued_payloads);
                    pane.requeue_osc52_pending_confirm(payloads);
                }
                *ctx.osc52_confirm = None;
                update.full_redraw = true;
                update.status_dirty = true;
            }
            _ => {}
        }
        return;
    }

    // ── Keymap dispatch (#84) ──
    // Look up the user-bound chord BEFORE the hardcoded match so users
    // can override builtins. On miss we fall through to the legacy
    // per-mode handler below — keeps back-compat with the existing
    // hotkey set even when the user supplies a partial keymap.
    let chord = crate::keymap::KeyChord::from_event(key);
    let table = match mode {
        InputMode::Prefix { .. } => Some(crate::keymap::KeymapTable::Prefix),
        InputMode::CopyMode(_) => Some(crate::keymap::KeymapTable::CopyMode),
        InputMode::Normal if !settings.visible => Some(crate::keymap::KeymapTable::Normal),
        _ => None,
    };
    if let Some(t) = table {
        if let Some(action) = ctx.keymap.lookup(t, &chord).cloned() {
            // Pre-mode side effects: a few keymap actions toggle modes
            // rather than mutating data, so handle those before the
            // execute_action shim. The shim returns Ok(None) for these.
            match &action {
                crate::keymap::Action::CommandPrompt => {
                    enter_command_palette(mode, ctx, panes, layout, settings, update);
                    if matches!(t, crate::keymap::KeymapTable::Prefix) {
                        // Leave prefix mode after dispatching.
                    }
                    return;
                }
                crate::keymap::Action::CopyMode => {
                    if let Some(pane) = panes.get(active) {
                        let (rows, cols) = pane.screen().size();
                        *mode =
                            InputMode::CopyMode(crate::copy_mode::CopyModeState::new(rows, cols));
                        update.full_redraw = true;
                    }
                    return;
                }
                crate::keymap::Action::DetachSession => {
                    *detach_requested = true;
                    *mode = InputMode::Normal;
                    return;
                }
                crate::keymap::Action::KillSession => {
                    *mode = InputMode::QuitConfirm;
                    update.full_redraw = true;
                    return;
                }
                crate::keymap::Action::RenameWindow => {
                    *mode = InputMode::RenameTab {
                        buffer: "\0".to_string(),
                    };
                    return;
                }
                crate::keymap::Action::NextWindow => {
                    *tab_action = TabAction::NextTab;
                    *mode = InputMode::Normal;
                    return;
                }
                crate::keymap::Action::PreviousWindow => {
                    *tab_action = TabAction::PrevTab;
                    *mode = InputMode::Normal;
                    return;
                }
                crate::keymap::Action::SelectWindow { index } => {
                    *tab_action = TabAction::GoToTab(*index);
                    *mode = InputMode::Normal;
                    return;
                }
                _ => {}
            }
            // Data-mutating actions go through the shared
            // execute_action shim so palette + keymap stay in sync.
            let result = super::actions::execute_action(
                &action,
                layout,
                panes,
                active,
                settings,
                update,
                default_shell,
                tw,
                th,
                scrollback,
                zoomed_pane,
                broadcast,
                tab_action,
                buffers,
            );
            match result {
                Ok(Some(text)) => *flash_message = Some((text, Instant::now())),
                Ok(None) => {}
                Err(text) => *flash_message = Some((text, Instant::now())),
            }
            // Exit prefix mode after dispatch (matches built-in
            // behaviour where `Ctrl+B X` returns to Normal).
            if matches!(t, crate::keymap::KeymapTable::Prefix) {
                *mode = InputMode::Normal;
            }
            return;
        }
    }

    // ── Quit confirmation ──
    if matches!(mode, InputMode::QuitConfirm) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                // Kill entire session (all tabs)
                *tab_action = TabAction::KillSession;
            }
            _ => {
                *mode = InputMode::Normal;
                update.full_redraw = true;
            }
        }
        return;
    }

    // ── Close tab (window) confirmation ──
    if matches!(mode, InputMode::CloseTabConfirm) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                *tab_action = TabAction::CloseTab;
                *mode = InputMode::Normal;
            }
            _ => {
                *mode = InputMode::Normal;
                update.full_redraw = true;
            }
        }
        return;
    }

    // ── Close pane confirmation ──
    if matches!(mode, InputMode::CloseConfirm) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                let target = *active;
                crate::close_pane(layout, panes, active, target);
                crate::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
                *mode = InputMode::Normal;
            }
            _ => {
                *mode = InputMode::Normal;
                update.full_redraw = true;
            }
        }
        return;
    }

    // ── Help overlay ──
    if matches!(mode, InputMode::HelpOverlay) {
        *mode = InputMode::Normal;
        update.full_redraw = true;
        return;
    }

    // ── Pane select ──
    if matches!(mode, InputMode::PaneSelect) {
        let ids = layout.pane_ids();
        if let KeyCode::Char(c @ '0'..='9') = key.code {
            let idx = match c {
                '1'..='9' => c as usize - '1' as usize,
                '0' => 9,
                _ => unreachable!(),
            };
            if let Some(&target) = ids.get(idx) {
                if panes.contains_key(&target) {
                    *active = target;
                }
            }
        }
        *mode = InputMode::Normal;
        update.full_redraw = true;
        return;
    }

    // ── Rename tab mode ──
    if let InputMode::RenameTab { buffer } = mode {
        match key.code {
            KeyCode::Char(c) if !ctrl => {
                buffer.push(c);
                update.full_redraw = true;
            }
            KeyCode::Backspace => {
                buffer.pop();
                update.full_redraw = true;
            }
            KeyCode::Enter => {
                if !buffer.is_empty() {
                    *tab_action = TabAction::Rename(std::mem::take(buffer));
                }
                *mode = InputMode::Normal;
                update.full_redraw = true;
            }
            KeyCode::Esc => {
                *mode = InputMode::Normal;
                update.full_redraw = true;
            }
            _ => {}
        }
        return;
    }

    // ── Command palette mode ──
    if let InputMode::CommandPalette { buffer } = mode {
        match key.code {
            KeyCode::Char(c) if !ctrl => {
                buffer.push(c);
                ctx.palette_query.push(c);
                *ctx.palette_selected = 0;
                update.full_redraw = true;
            }
            KeyCode::Backspace => {
                buffer.pop();
                ctx.palette_query.pop();
                *ctx.palette_selected = 0;
                update.full_redraw = true;
            }
            KeyCode::Up => {
                *ctx.palette_selected = ctx.palette_selected.saturating_sub(1);
                update.full_redraw = true;
            }
            KeyCode::Down => {
                *ctx.palette_selected = ctx.palette_selected.saturating_add(1);
                update.full_redraw = true;
            }
            KeyCode::Tab => {
                // Completion: replace query with the selected match's payload.
                if let Some(idx) = ctx.fuzzy_index.as_mut() {
                    let matches = idx.search(ctx.palette_query.as_str(), 6);
                    if let Some(m) = matches.get(*ctx.palette_selected) {
                        if let Some(entry) = idx.entries().get(m.index) {
                            *ctx.palette_query = entry.payload.clone();
                            *buffer = entry.payload.clone();
                            *ctx.palette_selected = 0;
                            update.full_redraw = true;
                        }
                    }
                }
            }
            KeyCode::Enter => {
                let cmd = std::mem::take(buffer);
                *mode = InputMode::Normal;
                ctx.palette_query.clear();
                *ctx.palette_selected = 0;
                *ctx.fuzzy_index = None;
                if !cmd.trim().is_empty() {
                    ctx.history.push(cmd.clone());
                }
                // Parse and execute. Successful commands may produce a
                // flash payload (e.g. `:display-message`); errors are
                // surfaced as a 2-second status-bar flash so typos never
                // fail silently (#58).
                let result = actions::execute_command(
                    &cmd,
                    layout,
                    panes,
                    active,
                    settings,
                    update,
                    default_shell,
                    tw,
                    th,
                    scrollback,
                    zoomed_pane,
                    broadcast,
                    tab_action,
                );
                match result {
                    Ok(Some(text)) => {
                        *flash_message = Some((text, Instant::now()));
                    }
                    Ok(None) => {}
                    Err(text) => {
                        *flash_message = Some((text, Instant::now()));
                    }
                }
            }
            KeyCode::Esc => {
                *mode = InputMode::Normal;
                ctx.palette_query.clear();
                *ctx.palette_selected = 0;
                *ctx.fuzzy_index = None;
                update.full_redraw = true;
            }
            _ => {}
        }
        return;
    }

    // ── Resize mode ──
    if matches!(mode, InputMode::ResizeMode) {
        match key.code {
            KeyCode::Left | KeyCode::Char('h')
                if layout.resize_pane(*active, NavDir::Left, 0.05) =>
            {
                crate::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            KeyCode::Right | KeyCode::Char('l')
                if layout.resize_pane(*active, NavDir::Right, 0.05) =>
            {
                crate::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            KeyCode::Up | KeyCode::Char('k') if layout.resize_pane(*active, NavDir::Up, 0.05) => {
                crate::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            KeyCode::Down | KeyCode::Char('j')
                if layout.resize_pane(*active, NavDir::Down, 0.05) =>
            {
                crate::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            KeyCode::Char('q') | KeyCode::Esc => {
                *mode = InputMode::Normal;
                update.full_redraw = true;
            }
            _ => {}
        }
        return;
    }

    // ── Copy mode (vi keys, selection, search) ──
    if let InputMode::CopyMode(ref mut cm_state) = mode {
        if let Some(pane) = panes.get_mut(active) {
            // Handle scrolling first (before screen access)
            match key.code {
                KeyCode::Char('k') | KeyCode::Up if cm_state.cursor_row == 0 => {
                    pane.scroll_up(1);
                }
                KeyCode::Char('j') | KeyCode::Down
                    if cm_state.cursor_row >= cm_state.pane_rows.saturating_sub(1) =>
                {
                    pane.scroll_down(1);
                }
                KeyCode::Char('g') if !ctrl => {
                    pane.scroll_up(usize::MAX);
                }
                KeyCode::Char('G') => {
                    pane.snap_to_bottom();
                }
                KeyCode::Char('u') if ctrl => {
                    pane.scroll_up((cm_state.pane_rows / 2) as usize);
                }
                KeyCode::Char('d') if ctrl => {
                    pane.scroll_down((cm_state.pane_rows / 2) as usize);
                }
                KeyCode::PageUp => {
                    pane.scroll_up(cm_state.pane_rows as usize);
                }
                KeyCode::PageDown => {
                    pane.scroll_down(cm_state.pane_rows as usize);
                }
                _ => {}
            }

            // Process key through copy mode state machine
            pane.sync_scrollback();
            let action = crate::copy_mode::handle_key(
                key,
                cm_state,
                pane.screen(),
                &mut |_| {}, // scrolling handled above
                &mut |_| {},
            );
            pane.reset_scrollback_view();

            match action {
                crate::copy_mode::CopyAction::CopyAndExit(text) => {
                    // Push the yank through the buffer store + system
                    // clipboard fallback chain (#91, #92). OSC 52 is the
                    // last-resort fallback for when no clipboard tool is
                    // wired up — e.g. SSH with no host clipboard daemon.
                    let report =
                        crate::copy_mode::yank_to_buffer(&text, buffers, clipboard_copy_argv);
                    match &report.clipboard {
                        Ok(label) => {
                            tracing::debug!(
                                sink = "buffer + clipboard",
                                program = %label,
                                bytes = text.len(),
                                "copy-and-exit yanked",
                            );
                        }
                        Err(err) => {
                            // Clipboard fallback failed — emit OSC 52 so
                            // the host terminal can still receive the
                            // yank.
                            let encoded = crate::base64_encode(text.as_bytes());
                            let osc = format!("\x1b]52;c;{}\x07", encoded);
                            pane.osc52_pending.push(osc.into_bytes());
                            tracing::debug!(
                                sink = "buffer + osc52",
                                bytes = text.len(),
                                clipboard_error = %err,
                                "copy-and-exit yanked (osc52 fallback)",
                            );
                        }
                    }
                    pane.snap_to_bottom();
                    *mode = InputMode::Normal;
                }
                crate::copy_mode::CopyAction::Exit => {
                    pane.snap_to_bottom();
                    *mode = InputMode::Normal;
                }
                _ => {}
            }
            update.dirty_panes.insert(*active);
        }
        return;
    }

    // ── Prefix mode ──
    if matches!(mode, InputMode::Prefix { .. }) {
        update.full_redraw = true;
        let mut next_mode = InputMode::Normal;
        match key.code {
            // Split
            KeyCode::Char('%') => {
                let _ = crate::do_split(
                    layout,
                    panes,
                    *active,
                    Direction::Horizontal,
                    default_shell,
                    tw,
                    th,
                    settings,
                    scrollback,
                );
                update.mark_all(layout);
                update.border_dirty = true;
            }
            KeyCode::Char('"') => {
                let _ = crate::do_split(
                    layout,
                    panes,
                    *active,
                    Direction::Vertical,
                    default_shell,
                    tw,
                    th,
                    settings,
                    scrollback,
                );
                update.mark_all(layout);
                update.border_dirty = true;
            }
            // Navigate
            KeyCode::Char('o') => {
                *active = layout.next_pane(*active);
            }
            KeyCode::Left => {
                let i = crate::make_inner(tw, th, settings.show_status_bar);
                if let Some(n) = layout.navigate(*active, NavDir::Left, &i) {
                    *active = n;
                }
            }
            KeyCode::Right => {
                let i = crate::make_inner(tw, th, settings.show_status_bar);
                if let Some(n) = layout.navigate(*active, NavDir::Right, &i) {
                    *active = n;
                }
            }
            KeyCode::Up => {
                let i = crate::make_inner(tw, th, settings.show_status_bar);
                if let Some(n) = layout.navigate(*active, NavDir::Up, &i) {
                    *active = n;
                }
            }
            KeyCode::Down => {
                let i = crate::make_inner(tw, th, settings.show_status_bar);
                if let Some(n) = layout.navigate(*active, NavDir::Down, &i) {
                    *active = n;
                }
            }
            // Close pane (with confirmation, tmux-style)
            KeyCode::Char('x') => {
                next_mode = InputMode::CloseConfirm;
            }
            // Equalize
            KeyCode::Char('E') => {
                layout.equalize();
                crate::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            // Scroll mode
            KeyCode::Char('[') => {
                // Enter copy mode — need pane dimensions
                if let Some(pane) = panes.get(active) {
                    let screen = pane.screen();
                    let (rows, cols) = screen.size();
                    next_mode =
                        InputMode::CopyMode(crate::copy_mode::CopyModeState::new(rows, cols));
                }
            }
            // Detach (tmux d)
            KeyCode::Char('d') => {
                *detach_requested = true;
            }
            // Reload config (#64) — flag is consumed by the signal-polling
            // block at the top of the main loop, so SIGHUP and Ctrl+B r
            // share one reload path.
            KeyCode::Char('r') => {
                settings.reload_request = true;
            }
            // Toggle status bar
            KeyCode::Char('s') => {
                settings.show_status_bar = !settings.show_status_bar;
                crate::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            // Zoom toggle
            KeyCode::Char('z') => {
                if zoomed_pane.is_some() {
                    *zoomed_pane = None;
                    crate::resize_all(panes, layout, tw, th, settings);
                    update.mark_all(layout);
                    update.border_dirty = true;
                } else {
                    *zoomed_pane = Some(*active);
                    crate::resize_zoomed_pane(panes, *active, tw, th, settings);
                }
            }
            // Resize mode
            KeyCode::Char('R') => {
                next_mode = InputMode::ResizeMode;
            }
            // Pane select
            KeyCode::Char('q') => {
                next_mode = InputMode::PaneSelect;
            }
            // Help
            KeyCode::Char('?') => {
                next_mode = InputMode::HelpOverlay;
            }
            // Swap
            KeyCode::Char('{') => {
                let prev = layout.prev_pane(*active);
                if prev != *active {
                    layout.swap_panes(*active, prev);
                    update.mark_all(layout);
                    update.border_dirty = true;
                }
            }
            KeyCode::Char('}') => {
                let next = layout.next_pane(*active);
                if next != *active {
                    layout.swap_panes(*active, next);
                    update.mark_all(layout);
                    update.border_dirty = true;
                }
            }
            // Broadcast toggle
            KeyCode::Char('B') => {
                *broadcast = !*broadcast;
                update.full_redraw = true;
            }
            // Last pane
            KeyCode::Char(';') if panes.contains_key(last_active) => {
                *active = *last_active;
                update.full_redraw = true;
            }
            // Equalize (space)
            KeyCode::Char(' ') => {
                layout.equalize();
                crate::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            // New tab (tmux c = new window)
            KeyCode::Char('c') => {
                *tab_action = TabAction::NewTab;
            }
            // Next tab (tmux n)
            KeyCode::Char('n') => {
                *tab_action = TabAction::NextTab;
            }
            // Previous tab (tmux p) or command palette opener.
            // The keymap (#84) gives users a way to rebind this; without
            // a binding, default behaviour is the legacy "previous tab".
            // The fuzzy palette is reachable via prefix `:` (also tmux's
            // command-prompt key) — see the `:` arm below.
            KeyCode::Char('p') => {
                *tab_action = TabAction::PrevTab;
            }
            // Close tab (with confirmation)
            KeyCode::Char('&') => {
                next_mode = InputMode::CloseTabConfirm;
            }
            // Rename tab (tmux ,) — pre-fill with current tab name
            KeyCode::Char(',') => {
                // tab_name is not accessible here directly, use empty for now
                // The actual pre-fill happens in the render where the prompt shows current name
                next_mode = InputMode::RenameTab {
                    buffer: "\0".to_string(), // sentinel: will be pre-filled by main loop
                };
            }
            // Command palette (tmux :)
            KeyCode::Char(':') => {
                build_palette_index(ctx, panes, layout);
                next_mode = InputMode::CommandPalette {
                    buffer: String::new(),
                };
            }
            // Tab jump by number (tmux 0-9 for windows)
            KeyCode::Char(digit @ '0'..='9') => {
                let idx = if digit == '0' {
                    9
                } else {
                    (digit as usize) - ('1' as usize)
                };
                *tab_action = TabAction::GoToTab(idx);
            }
            _ => {}
        }
        *mode = next_mode;
        return;
    }

    // ── Normal mode ──
    if key.code == KeyCode::Char(prefix_key) && ctrl {
        *mode = InputMode::Prefix {
            entered_at: Instant::now(),
        };
        update.full_redraw = true;
    } else if (key.code == KeyCode::Char('g') && ctrl) || key.code == KeyCode::F(1) {
        settings.toggle();
        update.full_redraw = true;
    } else if ctrl
        && (key.code == KeyCode::Char('\\')
            || key.code == KeyCode::Char('q')
            || key.code == KeyCode::Char('w'))
    {
        // Confirm before killing session
        *mode = InputMode::QuitConfirm;
        update.full_redraw = true;
    } else if settings.visible {
        let prev_border = settings.border_style;
        let prev_status = settings.show_status_bar;
        let prev_tab_bar = settings.show_tab_bar;
        let action = settings.handle_key(key);
        if action == SettingsAction::BroadcastToggle {
            *broadcast = !*broadcast;
        }
        if settings.border_style != prev_border {
            update.full_redraw = true;
        }
        if settings.show_status_bar != prev_status || settings.show_tab_bar != prev_tab_bar {
            crate::resize_all(panes, layout, tw, th, settings);
            update.border_dirty = true;
            update.mark_all(layout);
        }
        update.full_redraw = true;
    } else if key.code == KeyCode::Char('d') && ctrl {
        let _ = crate::do_split(
            layout,
            panes,
            *active,
            Direction::Horizontal,
            default_shell,
            tw,
            th,
            settings,
            scrollback,
        );
        update.mark_all(layout);
        update.border_dirty = true;
    } else if key.code == KeyCode::Char('e') && ctrl {
        let _ = crate::do_split(
            layout,
            panes,
            *active,
            Direction::Vertical,
            default_shell,
            tw,
            th,
            settings,
            scrollback,
        );
        update.mark_all(layout);
        update.border_dirty = true;
    } else if ctrl && (key.code == KeyCode::Char(']') || key.code == KeyCode::Char('n')) {
        *active = layout.next_pane(*active);
        update.full_redraw = true;
    } else if key.code == KeyCode::F(2) {
        layout.equalize();
        crate::resize_all(panes, layout, tw, th, settings);
        update.mark_all(layout);
        update.border_dirty = true;
    } else if alt {
        let inner = crate::make_inner(tw, th, settings.show_status_bar);
        let nav = match key.code {
            KeyCode::Left => Some(NavDir::Left),
            KeyCode::Right => Some(NavDir::Right),
            KeyCode::Up => Some(NavDir::Up),
            KeyCode::Down => Some(NavDir::Down),
            _ => None,
        };
        if let Some(dir) = nav {
            if let Some(next) = layout.navigate(*active, dir, &inner) {
                *active = next;
                update.full_redraw = true;
            }
        } else if *broadcast {
            for pane in panes.values_mut() {
                if pane.is_alive() {
                    pane.write_key(key);
                }
            }
        } else if let Some(pane) = panes.get_mut(active) {
            if pane.is_alive() {
                pane.write_key(key);
            }
        }
    } else if key.code == KeyCode::Enter && panes.get(active).is_some_and(|p| !p.is_alive()) {
        let (launch, old_name, pane_shell) = panes
            .get(active)
            .map(|p| {
                (
                    p.launch().clone(),
                    p.name().map(String::from),
                    p.initial_shell().map(String::from),
                )
            })
            .unwrap_or((PaneLaunch::Shell, None, None));
        let eff_shell = pane_shell.as_deref().unwrap_or(default_shell);
        if crate::replace_pane(
            panes, layout, *active, launch, eff_shell, tw, th, settings, scrollback,
        )
        .is_ok()
        {
            if let Some(pane) = panes.get_mut(active) {
                pane.set_name(old_name);
                if let Some(ref s) = pane_shell {
                    pane.set_initial_shell(Some(s.clone()));
                }
            }
        }
        update.dirty_panes.insert(*active);
    } else if *broadcast {
        for pane in panes.values_mut() {
            if pane.is_alive() {
                pane.write_key(key);
            }
        }
    } else if let Some(pane) = panes.get_mut(active) {
        if pane.is_alive() {
            pane.write_key(key);
        }
    }
}

/// Build the fuzzy palette candidate set (#86) and stash it in the
/// runtime context. Sources: action vocabulary, recent history, current
/// pane / tab list. Called when entering CommandPalette mode.
fn build_palette_index(ctx: &mut RuntimeCtx<'_>, panes: &HashMap<usize, Pane>, layout: &Layout) {
    use crate::fuzzy::{Entry, EntryKind, FuzzyIndex};
    let mut entries: Vec<Entry> = Vec::new();
    // Action / command vocabulary (frozen v1).
    for kind in crate::keymap::Action::vocabulary() {
        entries.push(Entry::new(EntryKind::Command, *kind));
    }
    // Recent history.
    for e in ctx.history.as_entries() {
        entries.push(e);
    }
    // Live pane list.
    for pid in layout.pane_ids() {
        let label = panes
            .get(&pid)
            .and_then(|p| p.name())
            .map(String::from)
            .unwrap_or_else(|| format!("pane {pid}"));
        entries.push(
            Entry::new(EntryKind::Pane, format!("pane: {label}"))
                .with_payload(format!("select-pane {pid}")),
        );
    }
    // Active session tag — keeps the fuzzy "@session" pattern usable.
    entries.push(
        Entry::new(EntryKind::Session, format!("@{}", ctx.session_name))
            .with_payload("display-message current".to_string()),
    );
    *ctx.fuzzy_index = Some(FuzzyIndex::new(entries));
    ctx.palette_query.clear();
    *ctx.palette_selected = 0;
}

/// Enter CommandPalette mode from a non-prefix dispatch path (e.g. user
/// keymap binding). Mirrors the `KeyCode::Char(':')` arm in the
/// prefix-mode handler.
fn enter_command_palette(
    mode: &mut InputMode,
    ctx: &mut RuntimeCtx<'_>,
    panes: &HashMap<usize, Pane>,
    layout: &Layout,
    _settings: &Settings,
    update: &mut RenderUpdate,
) {
    build_palette_index(ctx, panes, layout);
    *mode = InputMode::CommandPalette {
        buffer: String::new(),
    };
    update.full_redraw = true;
}
