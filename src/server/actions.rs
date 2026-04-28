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

/// Translate a [`crate::keymap::Action`] into the corresponding
/// command-palette [`crate::commands::Command`], dispatching it through
/// [`execute_command`]. Keymap-only actions that don't have a palette
/// equivalent (mode toggles, copy mode, etc.) mutate the input-mode state
/// machine and are therefore handled by `input_modes::dispatch_keymap_action`
/// — *this* helper is the data-mutation shim only.
///
/// Returns the same `Result<Option<String>, String>` shape as
/// `execute_command` so the dispatcher can pipe success messages and
/// errors into the status-bar flash channel uniformly.
#[allow(clippy::too_many_arguments)]
pub(super) fn execute_action(
    action: &crate::keymap::Action,
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
    buffers: &mut crate::buffers::BufferStore,
) -> Result<Option<String>, String> {
    use crate::commands::{Command, Dir as CmdDir};
    use crate::keymap::{Action, Dir as KmDir};

    let to_cmd_dir = |d: KmDir| match d {
        KmDir::Up => CmdDir::Up,
        KmDir::Down => CmdDir::Down,
        KmDir::Left => CmdDir::Left,
        KmDir::Right => CmdDir::Right,
    };

    // Convert the keymap action into a command-palette command when
    // there's a 1:1 equivalent. Actions without an equivalent (mode
    // toggles, copy mode, equalize, named buffers) are handled inline
    // below and return early.
    let cmd: Option<Command> = match action {
        Action::SplitWindowH => Some(Command::SplitHorizontal),
        Action::SplitWindowV => Some(Command::SplitVertical),
        Action::KillPane => Some(Command::KillPane),
        Action::KillWindow => Some(Command::KillWindow),
        Action::NewWindow { name } => Some(Command::NewWindow { name: name.clone() }),
        Action::RenameWindow => {
            // No fixed argument — mode change handled by input_modes.
            return Ok(None);
        }
        Action::SelectPane { dir } => Some(Command::SelectPane {
            dir: to_cmd_dir(*dir),
        }),
        Action::ResizePane { dir, amount } => Some(Command::ResizePane {
            dir: to_cmd_dir(*dir),
            amount: *amount,
        }),
        Action::SwapPane { up } => Some(Command::SwapPane { up: *up }),
        Action::SelectLayout { name } => Some(Command::SelectLayout { name: name.clone() }),
        Action::SetOption { key, value } => Some(Command::SetOption {
            key: key.clone(),
            value: value.clone(),
        }),
        Action::DisplayMessage { text } => Some(Command::DisplayMessage { text: text.clone() }),
        // Inline handlers for non-command-palette actions:
        Action::Equalize => {
            layout.equalize();
            crate::resize_all(panes, layout, tw, th, settings);
            update.mark_all(layout);
            update.border_dirty = true;
            return Ok(None);
        }
        Action::ToggleBroadcast => {
            *broadcast = !*broadcast;
            update.full_redraw = true;
            update.status_dirty = true;
            return Ok(None);
        }
        Action::ToggleSettings => {
            settings.toggle();
            update.full_redraw = true;
            return Ok(None);
        }
        Action::ReloadConfig => {
            settings.reload_request = true;
            return Ok(None);
        }
        Action::SetBuffer { name, value } => {
            match buffers.set(name.clone(), value.clone()) {
                Ok(()) => return Ok(Some(format!("set-buffer {name}"))),
                Err(e) => return Err(e.to_string()),
            }
        }
        Action::PasteBuffer { name } => {
            let entry = match name {
                Some(n) => buffers.get(n.as_str()),
                None => buffers.default_buffer(),
            };
            if let Some(buf) = entry {
                if let Some(pane) = panes.get_mut(active) {
                    if pane.is_alive() {
                        pane.write_bytes(buf.text.as_bytes());
                    }
                }
                return Ok(None);
            }
            return Err(format!(
                "paste-buffer: no buffer `{}`",
                name.as_deref().unwrap_or("")
            ));
        }
        Action::ListBuffers => {
            return Ok(Some(format!("buffers: {} stored", buffers.len())));
        }
        // Mode/state actions — input_modes handles these directly.
        Action::CopyMode
        | Action::Cancel
        | Action::BeginSelection
        | Action::CopySelectionAndCancel
        | Action::CommandPrompt
        | Action::DetachSession
        | Action::KillSession
        | Action::SelectWindow { .. }
        | Action::NextWindow
        | Action::PreviousWindow => return Ok(None),
    };

    if let Some(cmd) = cmd {
        let rendered = render_command(&cmd);
        return execute_command(
            &rendered,
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
    Ok(None)
}

/// Render a `Command` back into the string form `commands::parse` accepts,
/// so the keymap dispatcher can re-use the existing palette parser/dispatch
/// path without duplicating every match arm.
fn render_command(cmd: &crate::commands::Command) -> String {
    use crate::commands::{Command, Dir};
    let dir_flag = |d: Dir| match d {
        Dir::Up => "-U",
        Dir::Down => "-D",
        Dir::Left => "-L",
        Dir::Right => "-R",
    };
    match cmd {
        Command::SplitHorizontal => "split-window -h".into(),
        Command::SplitVertical => "split-window -v".into(),
        Command::KillPane => "kill-pane".into(),
        Command::KillWindow => "kill-window".into(),
        Command::NewWindow { name } => match name {
            Some(n) => format!("new-window -n {n}"),
            None => "new-window".into(),
        },
        Command::RenameWindow { name } => format!("rename-window {name}"),
        Command::SelectPane { dir } => format!("select-pane {}", dir_flag(*dir)),
        Command::ResizePane { dir, amount } => {
            format!("resize-pane {} {}", dir_flag(*dir), amount)
        }
        Command::SwapPane { up } => {
            if *up {
                "swap-pane -U".into()
            } else {
                "swap-pane -D".into()
            }
        }
        Command::SelectLayout { name } => format!("select-layout {name}"),
        Command::SetOption { key, value } => format!("set-option {key} {value}"),
        Command::DisplayMessage { text } => format!("display-message {text}"),
    }
}
