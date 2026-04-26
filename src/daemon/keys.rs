//! Keyboard event handler.
//!
//! `process_key` is a single ~625-line dispatch tree because every mode
//! (Normal / Prefix / Resize / CopyMode / RenameTab / CommandPalette / etc.)
//! lives behind one entry point. The 500-line guideline from issue #24's
//! file-size budget can't be met without a function rewrite, which is
//! explicitly out of scope for the Tidy First refactor — see the spec
//! deviation notes in MAINTENANCE.md.

use std::collections::HashMap;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config;
use crate::layout::{Direction, Layout, NavDir};
use crate::pane::{Pane, PaneLaunch};
use crate::render::BorderCache;
use crate::settings::{Settings, SettingsAction};

use super::dispatch::execute_command;
use super::state::{InputMode, TabAction};
use crate::app::state::RenderUpdate;

/// Process a key event. This is the core input handler shared between modes.
#[allow(clippy::too_many_arguments, unused_variables)]
pub(crate) fn process_key(
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
) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

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
                crate::app::lifecycle::close_pane(layout, panes, active, target);
                crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
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
                update.full_redraw = true;
            }
            KeyCode::Backspace => {
                buffer.pop();
                update.full_redraw = true;
            }
            KeyCode::Enter => {
                let cmd = std::mem::take(buffer);
                *mode = InputMode::Normal;
                // Parse and execute command
                execute_command(
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
            }
            KeyCode::Esc => {
                *mode = InputMode::Normal;
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
                crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            KeyCode::Right | KeyCode::Char('l')
                if layout.resize_pane(*active, NavDir::Right, 0.05) =>
            {
                crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            KeyCode::Up | KeyCode::Char('k') if layout.resize_pane(*active, NavDir::Up, 0.05) => {
                crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            KeyCode::Down | KeyCode::Char('j')
                if layout.resize_pane(*active, NavDir::Down, 0.05) =>
            {
                crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
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
                    // OSC 52 clipboard copy
                    let encoded = crate::app::lifecycle::base64_encode(text.as_bytes());
                    let osc = format!("\x1b]52;c;{}\x07", encoded);
                    pane.osc52_pending.push(osc.into_bytes());
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
                let _ = crate::app::lifecycle::do_split(
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
                let _ = crate::app::lifecycle::do_split(
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
                let i = crate::app::render_ctl::make_inner(tw, th, settings.show_status_bar);
                if let Some(n) = layout.navigate(*active, NavDir::Left, &i) {
                    *active = n;
                }
            }
            KeyCode::Right => {
                let i = crate::app::render_ctl::make_inner(tw, th, settings.show_status_bar);
                if let Some(n) = layout.navigate(*active, NavDir::Right, &i) {
                    *active = n;
                }
            }
            KeyCode::Up => {
                let i = crate::app::render_ctl::make_inner(tw, th, settings.show_status_bar);
                if let Some(n) = layout.navigate(*active, NavDir::Up, &i) {
                    *active = n;
                }
            }
            KeyCode::Down => {
                let i = crate::app::render_ctl::make_inner(tw, th, settings.show_status_bar);
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
                crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
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
            // Toggle status bar
            KeyCode::Char('s') => {
                settings.show_status_bar = !settings.show_status_bar;
                crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            // Hot-reload config from ~/.config/ezpn/config.toml.
            // Picks up edits made externally (or by another ezpn instance) without
            // restarting the daemon. Does NOT touch shell/scrollback/prefix at runtime
            // since those are sampled at startup.
            KeyCode::Char('r') => {
                let cfg = config::load_config();
                let prev_status = settings.show_status_bar;
                let prev_tab_bar = settings.show_tab_bar;
                config::apply_config_to_settings(&cfg, settings);
                if settings.show_status_bar != prev_status || settings.show_tab_bar != prev_tab_bar
                {
                    crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
                    update.mark_all(layout);
                    update.border_dirty = true;
                }
                update.full_redraw = true;
            }
            // Zoom toggle
            KeyCode::Char('z') => {
                if zoomed_pane.is_some() {
                    *zoomed_pane = None;
                    crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
                    update.mark_all(layout);
                    update.border_dirty = true;
                } else {
                    *zoomed_pane = Some(*active);
                    crate::app::render_ctl::resize_zoomed_pane(panes, *active, tw, th, settings);
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
                crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
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
            // Previous tab (tmux p)
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
            crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
            update.border_dirty = true;
            update.mark_all(layout);
        }
        if action == SettingsAction::Changed {
            if let Err(e) = config::save_settings(settings) {
                eprintln!("warning: failed to save settings: {e}");
            }
        }
        update.full_redraw = true;
    } else if key.code == KeyCode::Char('d') && ctrl {
        let _ = crate::app::lifecycle::do_split(
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
        let _ = crate::app::lifecycle::do_split(
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
        crate::app::lifecycle::resize_all(panes, layout, tw, th, settings);
        update.mark_all(layout);
        update.border_dirty = true;
    } else if alt {
        let inner = crate::app::render_ctl::make_inner(tw, th, settings.show_status_bar);
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
                    // [perf:cold] clone here: respawning a dead pane on Enter
                    // happens at most once per shell exit per pane. Cloning a
                    // `PaneLaunch` (small enum, often `Shell` unit variant) is
                    // negligible compared to the fork+exec that follows.
                    p.launch().clone(),
                    p.name().map(String::from),
                    p.initial_shell().map(String::from),
                )
            })
            .unwrap_or((PaneLaunch::Shell, None, None));
        let eff_shell = pane_shell.as_deref().unwrap_or(default_shell);
        if crate::app::lifecycle::replace_pane(
            panes, layout, *active, launch, eff_shell, tw, th, settings, scrollback,
        )
        .is_ok()
        {
            if let Some(pane) = panes.get_mut(active) {
                pane.set_name(old_name);
                if let Some(ref s) = pane_shell {
                    // [perf:cold] clone here: pane-respawn path; clones the
                    // Option<String> shell override so it can be re-applied
                    // to the freshly spawned pane after `replace_pane`.
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
