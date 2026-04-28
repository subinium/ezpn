//! In-server frame composition.
//!
//! [`render_frame_to_buf`] glues the lower-level `crate::render` helpers
//! to the server's per-frame state — input mode, tab bar, flash
//! overlay, zoom — and writes a complete frame into a reusable byte
//! buffer. The buffer is then broadcast to every attached client by
//! `connection::run`.
//!
//! Split out per #60 so the rendering pipeline lives in one place
//! instead of being interleaved with the input-mode state machine.

use std::collections::{HashMap, HashSet};

use crossterm::{cursor, queue, terminal};

use crate::layout::Layout;
use crate::pane::Pane;
use crate::render::{self, BorderCache};
use crate::settings::Settings;

use super::input_modes::InputMode;

// Re-export so phase 2b can `use server::render_glue::PaletteOverlayState`
// without reaching into `render::`.
pub(super) use crate::render::PaletteOverlayState;

/// Render a full frame to a byte buffer (instead of stdout).
///
/// Backwards-compatible entry point — phase 2b will migrate the call site to
/// [`render_frame_to_buf_with_palette`] to enable the fuzzy command palette
/// overlay (#86). This shim forwards with `palette_overlay = None`, which
/// keeps the legacy `: <buffer>` text-input fallback active.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_frame_to_buf(
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
    flash_message: Option<&str>,
    palette_overlay: Option<&PaletteOverlayState<'_>>,
    osc52_confirm: Option<&super::Osc52ConfirmState>,
) -> anyhow::Result<()> {
    render_frame_to_buf_with_palette(
        buf,
        panes,
        layout,
        active,
        settings,
        tw,
        th,
        dragging,
        border_cache,
        dirty_panes,
        full_redraw,
        mode,
        broadcast,
        selection,
        selection_chars,
        zoomed_pane,
        default_shell,
        tab_names,
        flash_message,
        palette_overlay,
    )?;
    // OSC 52 confirm prompt sits above everything else (modal).
    if let Some(state) = osc52_confirm {
        let palette = Some(&settings.resolved_palette);
        render::draw_osc52_confirm_overlay(
            buf,
            state.pane_id,
            state.byte_count,
            palette,
            tw,
            th,
        )?;
    }
    Ok(())
}

/// Themed + palette-overlay-aware variant of [`render_frame_to_buf`].
///
/// `palette_overlay` (#86): when `Some`, replaces the default
/// `InputMode::CommandPalette` text-input fallback with the 8-row fuzzy
/// palette overlay. Phase 2b populates this from the live `FuzzyIndex` +
/// selection cursor; until then, callers pass `None` and the overlay falls
/// back to the legacy `: <buffer>` prompt.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
pub(super) fn render_frame_to_buf_with_palette(
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
    flash_message: Option<&str>,
    palette_overlay: Option<&PaletteOverlayState<'_>>,
) -> anyhow::Result<()> {
    // Theme palette (#85). Borrow the pre-resolved palette every frame —
    // the renderer never re-resolves; `Settings::set_theme` did the work.
    let palette = Some(&settings.resolved_palette);
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
        InputMode::CommandPalette { .. } => "PALETTE",
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
                palette,
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
            palette,
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
                palette,
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
        render::draw_tab_bar(buf, tw, th, tab_names, settings.show_status_bar, palette)?;
    }

    // Overlays
    if matches!(mode, InputMode::HelpOverlay) {
        render::draw_help_overlay(buf, tw, th)?;
    }
    if matches!(mode, InputMode::PaneSelect) {
        let inner = crate::make_inner(tw, th, settings.show_status_bar);
        render::draw_pane_numbers(buf, layout, &inner)?;
    }

    // Text input overlay — drawn LAST so it's on top of status bar.
    // Flash messages share the same row but only render when no text-input
    // mode is active, so the prompt always wins over an in-flight flash.
    match mode {
        InputMode::RenameTab { buffer } => {
            render::draw_text_input(buf, tw, th, "Rename tab: ", buffer)?;
        }
        InputMode::CommandPalette { buffer } => {
            // #86: when phase 2b populates a `PaletteOverlayState` with the
            // live `FuzzyIndex` results, render the 8-row overlay. Otherwise
            // (current callers) fall back to the legacy `: <buffer>` prompt
            // so the renderer keeps working before the wiring lands.
            if let Some(state) = palette_overlay {
                render::draw_palette_overlay(buf, state, tw, th, palette)?;
            } else {
                render::draw_text_input(buf, tw, th, ":", buffer)?;
            }
        }
        _ => {
            // Flash messages share the status-bar row; only render when no
            // text-input mode is active so the prompt always wins (#58).
            if let Some(text) = flash_message {
                render::draw_flash_message(buf, tw, th, text)?;
            }
        }
    }

    // Ensure cursor is hidden at the end — prevents blinking on status/tab bar
    queue!(buf, cursor::Hide)?;

    Ok(())
}
