//! Frame composition for the daemon.
//!
//! `render_frame_to_buf` is the single entry point that turns the live
//! daemon state into a contiguous byte buffer ready to broadcast to every
//! attached client. All callers go through this function so the
//! smallest-client policy can guarantee that every viewer sees the same
//! frame.

use std::collections::{HashMap, HashSet};

use crossterm::{cursor, queue, terminal};

use crate::layout::Layout;
use crate::pane::Pane;
use crate::render::{self, BorderCache};
use crate::settings::Settings;

use super::state::InputMode;

/// Render a full frame to a byte buffer (instead of stdout).
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_frame_to_buf(
    buf: &mut Vec<u8>,
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
    mode: &InputMode,
    broadcast: bool,
    selection: render::PaneSelection,
    selection_chars: usize,
    zoomed_pane: Option<usize>,
    default_shell: &str,
    tab_names: &[(usize, String, bool)],
) -> anyhow::Result<()> {
    let mode_label = match mode {
        InputMode::Prefix { .. } => "PREFIX",
        InputMode::CopyMode(ref cm) => cm.mode_label(),
        InputMode::QuitConfirm => "KILL SESSION? y/n",
        InputMode::CloseConfirm => "CLOSE PANE? y/n",
        InputMode::CloseTabConfirm => "CLOSE TAB? y/n",
        InputMode::ResizeMode => "RESIZE",
        InputMode::PaneSelect => "SELECT",
        InputMode::HelpOverlay => "",
        InputMode::RenameTab { .. } => "RENAME",
        InputMode::CommandPalette { .. } => ":",
        InputMode::Normal if broadcast => "BROADCAST",
        InputMode::Normal => "",
    };

    if let Some(zpid) = zoomed_pane {
        queue!(buf, terminal::BeginSynchronizedUpdate)?;
        let pane_order = border_cache.pane_order();
        let pane_idx = pane_order.iter().position(|&id| id == zpid).unwrap_or(0);
        let label = panes
            .get(&zpid)
            .map(|p| p.launch_label(default_shell))
            .unwrap_or_default();
        if let Some(pane) = panes.get(&zpid) {
            render::render_zoomed_pane(
                buf,
                pane,
                pane_idx,
                &label,
                settings.border_style,
                tw,
                th,
                settings.show_status_bar,
                &settings.theme,
            )?;
        }
        if settings.show_status_bar {
            let zoom_label = if mode_label.is_empty() {
                "ZOOM"
            } else {
                mode_label
            };
            let pane_name = panes.get(&zpid).and_then(|p| p.name()).unwrap_or("");
            render::draw_status_bar_full(
                buf,
                tw,
                th,
                pane_idx,
                pane_order.len(),
                zoom_label,
                pane_name,
                0,
                &settings.theme,
            )?;
        }
        queue!(buf, terminal::EndSynchronizedUpdate)?;
    } else {
        queue!(buf, terminal::BeginSynchronizedUpdate)?;
        render::render_panes(
            buf,
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
        let is_text_input = matches!(
            mode,
            InputMode::RenameTab { .. } | InputMode::CommandPalette { .. }
        );
        // Status bar (skip if text input mode will draw over it)
        if !is_text_input
            && settings.show_status_bar
            && (!mode_label.is_empty() || selection_chars > 0)
        {
            let pane_order = border_cache.pane_order();
            let active_idx = pane_order.iter().position(|&id| id == active).unwrap_or(0);
            let pane_name = panes.get(&active).and_then(|p| p.name()).unwrap_or("");
            render::draw_status_bar_full(
                buf,
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
            settings.render_overlay(buf, tw, th, broadcast)?;
            queue!(buf, cursor::Hide)?;
        }
        queue!(buf, terminal::EndSynchronizedUpdate)?;
    }

    // Tab bar (only when multiple tabs exist and show_tab_bar is enabled)
    if tab_names.len() > 1 && settings.show_tab_bar {
        render::draw_tab_bar(
            buf,
            tw,
            th,
            tab_names,
            settings.show_status_bar,
            &settings.theme,
        )?;
    }

    // Overlays
    if matches!(mode, InputMode::HelpOverlay) {
        render::draw_help_overlay(buf, tw, th, &settings.theme)?;
    }
    if matches!(mode, InputMode::PaneSelect) {
        let inner = crate::app::render_ctl::make_inner(tw, th, settings.show_status_bar);
        render::draw_pane_numbers(buf, layout, &inner, &settings.theme)?;
    }

    // Text input overlay — drawn LAST so it's on top of status bar
    match mode {
        InputMode::RenameTab { buffer } => {
            render::draw_text_input(buf, tw, th, "Rename tab: ", buffer, &settings.theme)?;
        }
        InputMode::CommandPalette { buffer } => {
            render::draw_text_input(buf, tw, th, ":", buffer, &settings.theme)?;
        }
        _ => {}
    }

    // Ensure cursor is hidden at the end — prevents blinking on status/tab bar
    queue!(buf, cursor::Hide)?;

    Ok(())
}
