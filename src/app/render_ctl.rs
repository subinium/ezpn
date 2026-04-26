//! Frame composition + dirty-set bookkeeping for the foreground loop.
//!
//! Wraps `render::render_panes` so the event loop can stay focused on
//! input handling. Two concerns share this file because they're always
//! used together:
//! 1. Coordinate math ([`make_inner`], [`zoomed_content_size`],
//!    [`resize_zoomed_pane`]) — terminal-size → pane-content geometry.
//! 2. Dirty-set helpers ([`collect_render_targets`],
//!    [`sync_render_targets`], [`reset_render_targets`]) — pick which
//!    panes get re-rendered this tick and snapshot their scrollback.
//!
//! Splitting math vs dirty-set into separate files would force callers
//! to import both anyway, so they live together until one side outgrows
//! ~200 lines.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};

use crossterm::{cursor, queue, terminal};

use crate::app::lifecycle::extract_selected_text;
use crate::layout::{Layout, Rect};
use crate::pane::Pane;
use crate::render::{self, BorderCache};
use crate::settings::Settings;

pub(crate) fn make_inner(tw: u16, th: u16, show_status_bar: bool) -> Rect {
    let sh = if show_status_bar { 1u16 } else { 0 };
    Rect {
        x: 1,
        y: 1,
        w: tw.saturating_sub(2),
        h: th.saturating_sub(sh + 2),
    }
}

pub(crate) fn zoomed_content_size(tw: u16, th: u16, show_status_bar: bool) -> (u16, u16) {
    let sh = if show_status_bar { 1u16 } else { 0 };
    (tw.saturating_sub(2), th.saturating_sub(sh + 2))
}

pub(crate) fn resize_zoomed_pane(
    panes: &mut HashMap<usize, Pane>,
    pane_id: usize,
    tw: u16,
    th: u16,
    settings: &Settings,
) {
    let (cols, rows) = zoomed_content_size(tw, th, settings.show_status_bar);
    if let Some(pane) = panes.get_mut(&pane_id) {
        pane.resize(cols, rows);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_frame(
    stdout: &mut io::Stdout,
    panes: &HashMap<usize, Pane>,
    layout: &Layout,
    active: usize,
    settings: &Settings,
    tw: u16,
    th: u16,
    dragging: bool,
    border_cache: &BorderCache,
    dirty_panes: &HashSet<usize>,
    full_redraw: bool,
    mode_label: &str,
    selection: render::PaneSelection,
    selection_chars: usize,
    broadcast: bool,
) -> anyhow::Result<()> {
    queue!(stdout, terminal::BeginSynchronizedUpdate)?;
    render::render_panes(
        stdout,
        panes,
        layout,
        active,
        settings.border_style,
        settings.show_status_bar,
        tw,
        th,
        dragging,
        border_cache,
        dirty_panes,
        full_redraw,
        selection,
        broadcast,
        &settings.theme,
    )?;
    // Mode-aware status bar (render over the default one if we have a mode)
    if settings.show_status_bar && (!mode_label.is_empty() || selection_chars > 0) {
        let pane_order = border_cache.pane_order();
        let active_idx = pane_order.iter().position(|&id| id == active).unwrap_or(0);
        let pane_name = panes.get(&active).and_then(|p| p.name()).unwrap_or("");
        render::draw_status_bar_full(
            stdout,
            tw,
            th,
            active_idx,
            pane_order.len(),
            mode_label,
            pane_name,
            selection_chars,
            &settings.theme,
        )?;
    }
    if settings.visible {
        settings.render_overlay(stdout, tw, th, broadcast)?;
        queue!(stdout, cursor::Hide)?; // no blinking cursor over modal
    }
    queue!(stdout, terminal::EndSynchronizedUpdate)?;
    stdout.flush()?;
    Ok(())
}

pub(crate) fn collect_render_targets(
    panes: &HashMap<usize, Pane>,
    dirty_panes: &HashSet<usize>,
    full_redraw: bool,
    zoomed_pane: Option<usize>,
    extra_pane: Option<usize>,
) -> Vec<usize> {
    let mut targets = if let Some(pid) = zoomed_pane {
        let mut out = Vec::with_capacity(1 + usize::from(extra_pane.is_some()));
        if panes.contains_key(&pid) {
            out.push(pid);
        }
        out
    } else if full_redraw {
        panes.keys().copied().collect::<Vec<_>>()
    } else {
        dirty_panes
            .iter()
            .copied()
            .filter(|pid| panes.contains_key(pid))
            .collect::<Vec<_>>()
    };

    if let Some(pid) = extra_pane {
        if panes.contains_key(&pid) && !targets.contains(&pid) {
            targets.push(pid);
        }
    }

    targets
}

pub(crate) fn sync_render_targets(panes: &mut HashMap<usize, Pane>, targets: &[usize]) {
    for pid in targets {
        if let Some(pane) = panes.get_mut(pid) {
            pane.sync_scrollback();
        }
    }
}

pub(crate) fn reset_render_targets(panes: &mut HashMap<usize, Pane>, targets: &[usize]) {
    for pid in targets {
        if let Some(pane) = panes.get_mut(pid) {
            pane.reset_scrollback_view();
        }
    }
}

pub(crate) fn selection_char_count_from_synced(
    panes: &HashMap<usize, Pane>,
    selection: render::PaneSelection,
) -> usize {
    selection
        .and_then(|(pane_id, sr, sc, er, ec)| {
            panes.get(&pane_id).map(|pane| {
                let text = extract_selected_text(pane.screen(), pane_id, sr, sc, er, ec);
                text.chars().count()
            })
        })
        .unwrap_or(0)
}
