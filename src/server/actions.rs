//! Layout-mutating handlers shared by the prefix-mode keymap, the
//! command palette, and the IPC dispatcher.
//!
//! Split out per #60 so the action surface lives next to `commands.rs`
//! instead of being buried inside the 2.8K-line `server.rs`. The
//! functions here are intentionally pure with respect to the input
//! state machine — they take `&mut Layout`, `&mut HashMap<usize, Pane>`,
//! `&mut RenderUpdate`, etc., and never touch `InputMode` directly.

use std::collections::HashMap;

use crate::layout::{Direction, Layout, NavDir};
use crate::pane::Pane;
use crate::settings::Settings;

use super::{RenderUpdate, TabAction};

/// Execute a command from the command palette.
///
/// Parses `cmd` into a [`crate::commands::Command`] and dispatches it to the
/// existing action helpers. Returns `Ok(Some(text))` for commands that
/// produce a one-line message (e.g. `display-message`), `Ok(None)` for
/// silent successes, and `Err(text)` when input could not be parsed or
/// executed — the caller pipes the error back into the status-bar flash
/// channel so typos surface visibly instead of being swallowed (#58).
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
) -> Result<Option<String>, String> {
    use crate::commands::{self, Command, Dir, ParseError};

    // Empty input is a no-op (closes the prompt without complaint).
    let parsed = match commands::parse(cmd) {
        Ok(c) => c,
        Err(ParseError::Empty) => {
            update.full_redraw = true;
            return Ok(None);
        }
        Err(e) => {
            update.full_redraw = true;
            return Err(e.to_string());
        }
    };

    let dir_to_nav = |d: Dir| match d {
        Dir::Up => NavDir::Up,
        Dir::Down => NavDir::Down,
        Dir::Left => NavDir::Left,
        Dir::Right => NavDir::Right,
    };

    let mut flash: Option<String> = None;

    match parsed {
        Command::SplitHorizontal => {
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
        Command::SplitVertical => {
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
        Command::KillPane => {
            let target = *active;
            crate::close_pane(layout, panes, active, target);
            crate::resize_all(panes, layout, tw, th, settings);
            update.mark_all(layout);
            update.border_dirty = true;
        }
        Command::KillWindow => {
            *tab_action = TabAction::CloseTab;
        }
        Command::NewWindow { name } => {
            *tab_action = TabAction::NewTab;
            // tmux's `-n NAME` is acknowledged but not auto-applied: the
            // current TabAction enum only carries one action per frame, so
            // CloseTab/NewTab take precedence. The user can chain a
            // `:rename-window <NAME>` immediately after for the same effect.
            if let Some(n) = name {
                if !n.is_empty() {
                    flash = Some(format!("new-window: name `{n}` (rename pending)"));
                }
            }
        }
        Command::RenameWindow { name } => {
            *tab_action = TabAction::Rename(name);
        }
        Command::SelectPane { dir } => {
            let inner = crate::make_inner(tw, th, settings.show_status_bar);
            if let Some(next) = layout.navigate(*active, dir_to_nav(dir), &inner) {
                *active = next;
                update.full_redraw = true;
            }
        }
        Command::ResizePane { dir, amount } => {
            // Match the `R`-mode keybinding: 0.05 of the parent split per
            // cell is approximate but keeps semantics consistent with the
            // prefix resize hotkeys.
            let delta = 0.05 * amount.max(1) as f32;
            if layout.resize_pane(*active, dir_to_nav(dir), delta) {
                crate::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
        }
        Command::SwapPane { up } => {
            let other = if up {
                layout.prev_pane(*active)
            } else {
                layout.next_pane(*active)
            };
            if other != *active {
                layout.swap_panes(*active, other);
                update.mark_all(layout);
                update.border_dirty = true;
            }
        }
        Command::SelectLayout { name } => {
            let new_layout = Layout::from_spec(&name).map_err(|e| format!("select-layout: {e}"))?;
            let new_panes = crate::spawn_layout_panes(
                &new_layout,
                HashMap::new(),
                default_shell,
                tw,
                th,
                settings,
                scrollback,
            )
            .map_err(|e| format!("select-layout: spawn failed: {e}"))?;
            crate::kill_all_panes(panes);
            *layout = new_layout;
            *panes = new_panes;
            *active = *layout.pane_ids().first().unwrap_or(&0);
            update.mark_all(layout);
            update.border_dirty = true;
        }
        Command::SetOption { key, value } => match key.as_str() {
            "status" | "status-bar" | "show-status-bar" => {
                let on = parse_bool_opt(&value)
                    .ok_or_else(|| format!("set-option {key}: expected on/off, got `{value}`"))?;
                settings.show_status_bar = on;
                crate::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            "tab-bar" | "show-tab-bar" => {
                let on = parse_bool_opt(&value)
                    .ok_or_else(|| format!("set-option {key}: expected on/off, got `{value}`"))?;
                settings.show_tab_bar = on;
                crate::resize_all(panes, layout, tw, th, settings);
                update.mark_all(layout);
                update.border_dirty = true;
            }
            "broadcast" => {
                let on = parse_bool_opt(&value)
                    .ok_or_else(|| format!("set-option {key}: expected on/off, got `{value}`"))?;
                *broadcast = on;
                update.full_redraw = true;
            }
            other => {
                return Err(format!("set-option: unknown key `{other}`"));
            }
        },
        Command::DisplayMessage { text } => {
            flash = Some(text);
        }
    }

    // `zoomed_pane` is part of this function's borrow set so the compiler
    // sees it as touched even though parity-floor commands don't mutate it.
    let _ = zoomed_pane;

    update.full_redraw = true;
    Ok(flash)
}

/// Parse an on/off flag for `set-option`. Accepts the spellings tmux uses
/// in its config files plus a couple of common aliases.
pub(super) fn parse_bool_opt(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => Some(true),
        "off" | "false" | "0" | "no" => Some(false),
        _ => None,
    }
}
