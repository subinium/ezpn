//! Event dispatch: routes raw `crossterm::Event` values into the right
//! handler (key / mouse / paste / focus) and exposes the command-palette
//! parser used when the user enters `:foo`.
//!
//! The chunky key handler lives in [`super::keys`] — see issue #24 for why
//! it's a separate file (single ~625-line match statement that the spec's
//! 500-line target can't accommodate without a function rewrite, which is
//! explicitly out of scope for this Tidy First refactor).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyEventKind, MouseButton, MouseEventKind};

use crate::config;
use crate::layout::{Direction, Layout, Rect};
use crate::pane::Pane;
use crate::render::{self, BorderCache};
use crate::settings::{Settings, SettingsAction};

use super::state::{DragState, InputMode, TabAction, TextSelection};
use crate::app::state::RenderUpdate;

/// Process a single crossterm Event (shared between direct and server modes).
#[allow(clippy::too_many_arguments, unused_variables)]
pub(crate) fn process_event(
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
) {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            super::keys::process_key(
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
            );
        }
        Event::Mouse(mouse) => {
            if let Some(ref cache) = border_cache {
                // [perf:hot] clone here: `BorderCache::inner()` returns &Rect
                // and `process_mouse` needs an owned `&Rect` slot to satisfy
                // the borrow checker (the cache itself is also passed in).
                // TODO(perf): make `Rect: Copy` or pass cache.inner() by
                // reference once the lifetime tangle is unwound — would
                // remove a per-event 16-byte copy on every mouse motion.
                let inner = cache.inner().clone();
                process_mouse(
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

/// Execute a command from the command palette.
#[allow(clippy::too_many_arguments)]
pub(super) fn execute_command(
    cmd: &str,
    layout: &mut Layout,
    panes: &mut HashMap<usize, Pane>,
    active: &mut usize,
    settings: &mut Settings,
    update: &mut RenderUpdate,
    default_shell: &str,
    tw: u16,
    th: u16,
    scrollback: usize,
    zoomed_pane: &mut Option<usize>,
    broadcast: &mut bool,
    tab_action: &mut TabAction,
) {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    match parts.first().copied() {
        Some("split-window") | Some("split") => {
            let dir = if parts.get(1) == Some(&"-v") || parts.get(1) == Some(&"v") {
                Direction::Vertical
            } else {
                Direction::Horizontal
            };
            let _ = crate::app::lifecycle::do_split(
                layout,
                panes,
                *active,
                dir,
                default_shell,
                tw,
                th,
                settings,
                scrollback,
            );
            update.mark_all(layout);
            update.border_dirty = true;
        }
        Some("new-window") | Some("new-tab") => {
            *tab_action = TabAction::NewTab;
        }
        Some("next-window") | Some("next-tab") => {
            *tab_action = TabAction::NextTab;
        }
        Some("prev-window") | Some("prev-tab") | Some("previous-window") => {
            *tab_action = TabAction::PrevTab;
        }
        Some("kill-pane") | Some("close-pane") => {
            let target = *active;
            crate::app::lifecycle::close_pane(layout, panes, active, target);
            crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
            update.mark_all(layout);
            update.border_dirty = true;
        }
        Some("kill-window") | Some("close-tab") => {
            *tab_action = TabAction::CloseTab;
        }
        Some("rename-window") | Some("rename-tab") => {
            if let Some(name) = parts.get(1..).map(|s| s.join(" ")) {
                if !name.is_empty() {
                    *tab_action = TabAction::Rename(name);
                }
            }
        }
        Some("select-layout") | Some("layout") => {
            if let Some(spec) = parts.get(1) {
                if let Ok(new_layout) = Layout::from_spec(spec) {
                    if let Ok(new_panes) = crate::app::lifecycle::spawn_layout_panes(
                        &new_layout,
                        HashMap::new(),
                        default_shell,
                        tw,
                        th,
                        settings,
                        scrollback,
                    ) {
                        crate::app::lifecycle::kill_all_panes(panes);
                        *layout = new_layout;
                        *panes = new_panes;
                        *active = *layout.pane_ids().first().unwrap_or(&0);
                        update.mark_all(layout);
                        update.border_dirty = true;
                    }
                }
            }
        }
        Some("equalize") | Some("even") => {
            layout.equalize();
            crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
            update.mark_all(layout);
            update.border_dirty = true;
        }
        Some("zoom") => {
            if zoomed_pane.is_some() {
                *zoomed_pane = None;
                crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
            } else {
                *zoomed_pane = Some(*active);
                crate::app::render_ctl::resize_zoomed_pane(panes, *active, tw, th, settings);
            }
            update.mark_all(layout);
            update.border_dirty = true;
        }
        Some("broadcast") => {
            *broadcast = !*broadcast;
            update.full_redraw = true;
        }
        _ => {
            // Unknown command — silently ignore
        }
    }
    update.full_redraw = true;
}

/// Process a mouse event.
#[allow(clippy::too_many_arguments, unused_variables)]
pub(crate) fn process_mouse(
    mouse: crossterm::event::MouseEvent,
    _mode: &mut InputMode,
    layout: &mut Layout,
    panes: &mut HashMap<usize, Pane>,
    active: &mut usize,
    settings: &mut Settings,
    update: &mut RenderUpdate,
    drag: &mut Option<DragState>,
    zoomed_pane: &mut Option<usize>,
    last_click: &mut Option<(Instant, u16, u16)>,
    broadcast: &mut bool,
    selection_anchor: &mut Option<(usize, u16, u16)>,
    text_selection: &mut Option<TextSelection>,
    default_shell: &str,
    tw: u16,
    th: u16,
    scrollback: usize,
    border_cache: &BorderCache,
    inner: &Rect,
    tab_action: &mut TabAction,
    tab_names: &[(usize, String, bool)],
) {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // Tab bar: single click = switch tab, double click = rename tab
            if tab_names.len() > 1 {
                let tab_y = render::tab_bar_y(th, settings.show_status_bar);
                if mouse.row == tab_y {
                    if let Some(idx) = render::tab_bar_hit(mouse.column, tab_names, tw) {
                        let now = Instant::now();
                        let is_double = last_click
                            .map(|(t, lx, ly)| {
                                now.duration_since(t) < Duration::from_millis(400)
                                    && lx == mouse.column
                                    && ly == mouse.row
                            })
                            .unwrap_or(false);
                        *last_click = Some((now, mouse.column, mouse.row));

                        if is_double {
                            // Double-click on tab → rename mode
                            // First switch to that tab if not active
                            if idx != tab_names.iter().position(|(_, _, a)| *a).unwrap_or(0) {
                                *tab_action = TabAction::GoToTab(idx);
                            }
                            // Enter rename mode — sentinel will be pre-filled by main loop
                            *_mode = InputMode::RenameTab {
                                buffer: "\0".to_string(),
                            };
                            update.full_redraw = true;
                        } else {
                            *tab_action = TabAction::GoToTab(idx);
                        }
                        return;
                    }
                }
            }

            if settings.visible {
                let prev_border = settings.border_style;
                let prev_status = settings.show_status_bar;
                let prev_tab_bar = settings.show_tab_bar;
                let action = settings.handle_click(mouse.column, mouse.row, tw, th);
                if action == SettingsAction::BroadcastToggle {
                    *broadcast = !*broadcast;
                }
                if settings.border_style != prev_border {
                    update.full_redraw = true;
                }
                if settings.show_status_bar != prev_status || settings.show_tab_bar != prev_tab_bar
                {
                    crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
                    update.border_dirty = true;
                    update.mark_all(layout);
                }
                if action == SettingsAction::Changed {
                    if let Err(e) = config::save_settings(settings) {
                        eprintln!("warning: failed to save settings: {e}");
                    }
                }
                if action == SettingsAction::Changed
                    || action == SettingsAction::Close
                    || action == SettingsAction::BroadcastToggle
                {
                    update.full_redraw = true;
                }
            } else if let Some(action) =
                render::title_button_hit(mouse.column, mouse.row, layout, inner)
            {
                match action {
                    render::TitleAction::Close(pid) => {
                        crate::app::lifecycle::close_pane(layout, panes, active, pid);
                        crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
                    }
                    render::TitleAction::SplitH(pid) => {
                        let _ = crate::app::lifecycle::do_split(
                            layout,
                            panes,
                            pid,
                            Direction::Vertical,
                            default_shell,
                            tw,
                            th,
                            settings,
                            scrollback,
                        );
                    }
                    render::TitleAction::SplitV(pid) => {
                        let _ = crate::app::lifecycle::do_split(
                            layout,
                            panes,
                            pid,
                            Direction::Horizontal,
                            default_shell,
                            tw,
                            th,
                            settings,
                            scrollback,
                        );
                    }
                }
                update.mark_all(layout);
                update.border_dirty = true;
            } else if let Some(hit) = layout.find_separator_at(mouse.column, mouse.row, inner) {
                *drag = Some(DragState::from_hit(hit));
                update.full_redraw = true;
            } else if let Some(pid) = layout.find_at(mouse.column, mouse.row, inner) {
                let now = Instant::now();
                let is_double = last_click
                    .map(|(t, lx, ly)| {
                        now.duration_since(t) < Duration::from_millis(400)
                            && lx == mouse.column
                            && ly == mouse.row
                    })
                    .unwrap_or(false);
                *last_click = Some((now, mouse.column, mouse.row));

                if is_double && panes.contains_key(&pid) {
                    if zoomed_pane.is_some() {
                        *zoomed_pane = None;
                        crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
                    } else {
                        *zoomed_pane = Some(pid);
                        crate::app::render_ctl::resize_zoomed_pane(panes, pid, tw, th, settings);
                    }
                    *active = pid;
                    update.mark_all(layout);
                    update.border_dirty = true;
                } else if pid != *active && panes.contains_key(&pid) {
                    *active = pid;
                    update.full_redraw = true;
                }
                if !is_double {
                    if let Some(pane) = panes.get_mut(&pid) {
                        if pane.wants_mouse() {
                            if let Some(rect) = border_cache.pane_rects().get(&pid) {
                                let rel_col = mouse.column.saturating_sub(rect.x);
                                let rel_row = mouse.row.saturating_sub(rect.y);
                                pane.send_mouse_event(0, rel_col, rel_row, false);
                            }
                        } else if pid == *active {
                            if let Some(rect) = border_cache.pane_rects().get(&pid) {
                                let rel_col = mouse.column.saturating_sub(rect.x);
                                let rel_row = mouse.row.saturating_sub(rect.y);
                                *selection_anchor = Some((pid, rel_col, rel_row));
                                if text_selection.is_some() {
                                    *text_selection = None;
                                    update.dirty_panes.insert(pid);
                                }
                            }
                        }
                    }
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(ref ds) = drag {
                let new_ratio = ds.calc_ratio(mouse.column, mouse.row);
                layout.set_ratio_at_path(&ds.path, new_ratio);
                crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            } else if let Some((pid, anchor_col, anchor_row)) = *selection_anchor {
                if let Some(rect) = border_cache.pane_rects().get(&pid) {
                    let rel_col = mouse
                        .column
                        .saturating_sub(rect.x)
                        .min(rect.w.saturating_sub(1));
                    let rel_row = mouse
                        .row
                        .saturating_sub(rect.y)
                        .min(rect.h.saturating_sub(1));
                    *text_selection = Some(TextSelection {
                        pane_id: pid,
                        start_row: anchor_row,
                        start_col: anchor_col,
                        end_row: rel_row,
                        end_col: rel_col,
                    });
                    update.dirty_panes.insert(pid);
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if drag.take().is_some() {
                crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            } else if let Some(ref sel) = text_selection {
                // Copy selected text to clipboard via OSC 52
                // Note: in server mode, the OSC 52 goes through the output buffer to the client
                if let Some(pane) = panes.get_mut(&sel.pane_id) {
                    pane.sync_scrollback();
                    let text = crate::app::lifecycle::extract_selected_text(
                        pane.screen(),
                        sel.pane_id,
                        sel.start_row,
                        sel.start_col,
                        sel.end_row,
                        sel.end_col,
                    );
                    pane.reset_scrollback_view();
                    if !text.is_empty() {
                        let encoded = crate::app::lifecycle::base64_encode(text.as_bytes());
                        let osc = format!("\x1b]52;c;{}\x07", encoded);
                        pane.osc52_pending.push(osc.into_bytes());
                    }
                }
                let pid = sel.pane_id;
                *text_selection = None;
                *selection_anchor = None;
                update.dirty_panes.insert(pid);
            } else {
                *selection_anchor = None;
                if let Some(pane) = panes.get_mut(active) {
                    if pane.wants_mouse() {
                        if let Some(rect) = border_cache.pane_rects().get(active) {
                            let rel_col = mouse.column.saturating_sub(rect.x);
                            let rel_row = mouse.row.saturating_sub(rect.y);
                            pane.send_mouse_event(0, rel_col, rel_row, true);
                        }
                    }
                }
            }
        }
        MouseEventKind::ScrollUp => {
            let target = layout
                .find_at(mouse.column, mouse.row, inner)
                .unwrap_or(*active);
            if let Some(pane) = panes.get_mut(&target) {
                if pane.is_alive() {
                    if pane.wants_mouse() {
                        if let Some(rect) = border_cache.pane_rects().get(&target) {
                            let rel_col = mouse.column.saturating_sub(rect.x);
                            let rel_row = mouse.row.saturating_sub(rect.y);
                            for _ in 0..3 {
                                pane.send_mouse_scroll(true, rel_col, rel_row);
                            }
                        }
                    } else {
                        pane.scroll_up(3);
                        update.dirty_panes.insert(target);
                    }
                }
            }
        }
        MouseEventKind::ScrollDown => {
            let target = layout
                .find_at(mouse.column, mouse.row, inner)
                .unwrap_or(*active);
            if let Some(pane) = panes.get_mut(&target) {
                if pane.is_alive() {
                    if pane.wants_mouse() {
                        if let Some(rect) = border_cache.pane_rects().get(&target) {
                            let rel_col = mouse.column.saturating_sub(rect.x);
                            let rel_row = mouse.row.saturating_sub(rect.y);
                            for _ in 0..3 {
                                pane.send_mouse_scroll(false, rel_col, rel_row);
                            }
                        }
                    } else {
                        pane.scroll_down(3);
                        update.dirty_panes.insert(target);
                    }
                }
            }
        }
        _ => {}
    }
}
