//! Mouse-event handling for the server input pipeline.
//!
//! Split off from `input_modes.rs` per the #60 LOC budget; the keyboard
//! state machine and the mouse handler are independent enough that
//! pulling the mouse path out keeps both files focused. The handler
//! still drives the same `InputMode` / `TabAction` types defined in
//! `input_modes`, so the two modules form a logical pair through
//! `super::input_modes::*` imports.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

use crate::layout::{Direction, Layout, Rect};
use crate::pane::Pane;
use crate::render::{self, BorderCache};
use crate::settings::{Settings, SettingsAction};

use super::input_modes::{DragState, InputMode, TabAction, TextSelection};
use super::RenderUpdate;

/// Process a mouse event.
#[allow(clippy::too_many_arguments, unused_variables)]
pub(super) fn process_mouse(
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
    if !matches!(_mode, InputMode::Normal) && !settings.visible {
        return;
    }
    let zoom_rects = zoomed_pane.map(|id| {
        HashMap::from([(
            id,
            crate::bootstrap::terminal_content_area(tw, th, settings),
        )])
    });
    let rects = zoom_rects
        .as_ref()
        .unwrap_or_else(|| border_cache.pane_rects());
    let hit_pane = rects
        .iter()
        .find(|(_, r)| contains(r, mouse.column, mouse.row))
        .map(|(&id, _)| id);
    let zoom_layout = zoomed_pane.map(|id| Layout::singleton(id, layout.next_id));
    let title_action = render::title_button_hit(
        mouse.column,
        mouse.row,
        zoom_layout.as_ref().unwrap_or(layout),
        inner,
    );
    let on_tab_bar = settings.show_tab_bar
        && tab_names.len() > 1
        && mouse.row == render::tab_bar_y(th, settings.show_status_bar);
    if !settings.visible
        && !on_tab_bar
        && drag.is_none()
        && selection_anchor.is_none()
        && !mouse.modifiers.contains(KeyModifiers::SHIFT)
    {
        if let Some(id) = hit_pane {
            if let Some(pane) = panes.get_mut(&id) {
                if pane.wants_mouse() && !mouse.modifiers.contains(KeyModifiers::SHIFT) {
                    if matches!(mouse.kind, MouseEventKind::Down(_)) {
                        *active = id;
                        update.full_redraw = true;
                    }
                    let rect = &rects[&id];
                    let mut relative = mouse;
                    relative.column -= rect.x;
                    relative.row -= rect.y;
                    pane.forward_mouse_event(relative);
                    return;
                }
            }
        }
    }
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // Tab bar: single click = switch tab, double click = rename tab
            if !settings.visible && settings.show_tab_bar && tab_names.len() > 1 {
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
                    crate::resize_all(panes, layout, tw, th, settings);
                    update.border_dirty = true;
                    update.mark_all(layout);
                }
                if action == SettingsAction::Changed
                    || action == SettingsAction::Close
                    || action == SettingsAction::BroadcastToggle
                {
                    update.full_redraw = true;
                }
            } else if let Some(action) = title_action {
                match action {
                    render::TitleAction::Close(pid) => {
                        *active = pid;
                        *_mode = InputMode::CloseConfirm;
                    }
                    render::TitleAction::SplitH(pid) => {
                        let _ = crate::do_split(
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
                        let _ = crate::do_split(
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
            } else if let Some(hit) = if zoomed_pane.is_none() {
                layout.find_separator_at(mouse.column, mouse.row, inner)
            } else {
                None
            } {
                *drag = Some(DragState::from_hit(hit));
                update.full_redraw = true;
            } else if let Some(pid) = hit_pane {
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
                        crate::resize_all(panes, layout, tw, th, settings);
                    } else {
                        *zoomed_pane = Some(pid);
                        crate::resize_zoomed_pane(panes, pid, tw, th, settings);
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
                        if pane.wants_mouse() && !mouse.modifiers.contains(KeyModifiers::SHIFT) {
                            if let Some(rect) = rects.get(&pid) {
                                let rel_col = mouse.column.saturating_sub(rect.x);
                                let rel_row = mouse.row.saturating_sub(rect.y);
                                pane.send_mouse_event(0, rel_col, rel_row, false);
                            }
                        } else if pid == *active {
                            if let Some(rect) = rects.get(&pid) {
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
                layout.set_ratio_at_path(ds.path(), new_ratio);
                crate::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            } else if let Some((pid, anchor_col, anchor_row)) = *selection_anchor {
                if let Some(rect) = rects.get(&pid) {
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
                crate::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            } else if let Some(ref sel) = text_selection {
                // Copy selected text to clipboard via OSC 52
                // Note: in server mode, the OSC 52 goes through the output buffer to the client
                if let Some(pane) = panes.get_mut(&sel.pane_id) {
                    pane.sync_scrollback();
                    let text = crate::extract_selected_text(
                        pane.screen(),
                        sel.pane_id,
                        sel.start_row,
                        sel.start_col,
                        sel.end_row,
                        sel.end_col,
                    );
                    pane.reset_scrollback_view();
                    if !text.is_empty() {
                        let encoded = crate::base64_encode(text.as_bytes());
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
                    if pane.wants_mouse() && !mouse.modifiers.contains(KeyModifiers::SHIFT) {
                        if let Some(rect) = rects.get(active) {
                            let rel_col = mouse.column.saturating_sub(rect.x);
                            let rel_row = mouse.row.saturating_sub(rect.y);
                            pane.send_mouse_event(0, rel_col, rel_row, true);
                        }
                    }
                }
            }
        }
        MouseEventKind::ScrollUp => {
            let target = hit_pane.unwrap_or(*active);
            if let Some(pane) = panes.get_mut(&target) {
                if pane.is_alive() {
                    if pane.wants_mouse() && !mouse.modifiers.contains(KeyModifiers::SHIFT) {
                        if let Some(rect) = rects.get(&target) {
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
            let target = hit_pane.unwrap_or(*active);
            if let Some(pane) = panes.get_mut(&target) {
                if pane.is_alive() {
                    if pane.wants_mouse() && !mouse.modifiers.contains(KeyModifiers::SHIFT) {
                        if let Some(rect) = rects.get(&target) {
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

fn contains(rect: &Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x - rect.x < rect.w && y >= rect.y && y - rect.y < rect.h
}
