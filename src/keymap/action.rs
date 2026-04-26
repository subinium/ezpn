//! SPEC 09 — shared Action vocabulary.
//!
//! One `Action` enum is the canonical surface used by the legacy `:` palette,
//! the SPEC 10 fuzzy palette, and (post-refactor) the keymap dispatcher in
//! `daemon/keys.rs`. Today the palette parses commands inline (see
//! `daemon/dispatch.rs::execute_command`); this module gives both consumers
//! the same vocabulary so the SPEC 09 [keymap.*] tables and the SPEC 10
//! palette can route through one ingress.
//!
//! v0.11 scope: the Action types below cover every existing palette command
//! plus the SPEC 09 vocabulary additions (`select-pane DIR`, `enter-mode`,
//! `detach`, etc.). The full TOML keymap loader is deferred — this module
//! lands the parser + types so palette/keymap consumers compile against a
//! stable surface. `#![allow(dead_code)]` reflects that wiring; consumers
//! land in follow-up PRs.

#![allow(dead_code)]

use std::fmt;

use crate::layout::Direction;

/// One executable action. Built from `parse()` over a chord-derived string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Split the active pane.
    SplitWindow(Direction),
    /// Create a new tab.
    NewTab,
    /// Cycle to next tab.
    NextTab,
    /// Cycle to previous tab.
    PrevTab,
    /// Close the active pane.
    KillPane,
    /// Close the active tab.
    CloseTab,
    /// Rename the active tab.
    RenameTab(String),
    /// Apply a layout DSL spec.
    SelectLayout(String),
    /// Equalise pane sizes.
    Equalize,
    /// Toggle zoom on the active pane.
    Zoom,
    /// Toggle broadcast mode.
    Broadcast,
    /// SPEC 09 — focus a pane in the given direction.
    SelectPane(SelectDirection),
    /// SPEC 09 — enter a non-Normal input mode.
    EnterMode(EnterModeKind),
    /// SPEC 09 — leave the current non-Normal mode.
    LeaveMode,
    /// SPEC 09 — detach all clients.
    Detach,
    /// SPEC 09 — kill the whole session.
    KillSession,
    /// SPEC 09 — reload config from disk.
    ReloadConfig,
    /// SPEC 09 — toggle a chrome element by name.
    Toggle(ToggleTarget),
    /// SPEC 10 — open the fuzzy command palette.
    CommandPalette,
}

/// Direction argument for `select-pane DIR`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SelectDirection {
    Up,
    Down,
    Left,
    Right,
    Next,
    Prev,
    Last,
}

/// Mode argument for `enter-mode MODE`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EnterModeKind {
    Prefix,
    Copy,
    Resize,
    PaneSelect,
    Help,
    CommandPalette,
}

/// Target of `toggle X`. SPEC 11's `toggle hints` and existing `toggle status-bar`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ToggleTarget {
    StatusBar,
    Hints,
}

#[derive(Clone, Debug)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

/// Parse one action string into an `Action`. The grammar is SPEC 09 §4.2's
/// table; aliases (`split` ⇔ `split-window`, `new-window` ⇔ `new-tab`, …)
/// are honoured 1:1 with the v0.9 palette.
pub fn parse(s: &str) -> Result<Action, ParseError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(ParseError("empty action".into()));
    }
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    let head = parts[0];
    let rest = &parts[1..];

    match head {
        "split-window" | "split" => {
            let dir = if rest.first() == Some(&"-v") || rest.first() == Some(&"v") {
                Direction::Vertical
            } else if rest.first() == Some(&"horizontal") {
                Direction::Horizontal
            } else if rest.first() == Some(&"vertical") {
                Direction::Vertical
            } else {
                Direction::Horizontal
            };
            Ok(Action::SplitWindow(dir))
        }
        "new-window" | "new-tab" => Ok(Action::NewTab),
        "next-window" | "next-tab" => Ok(Action::NextTab),
        "prev-window" | "prev-tab" | "previous-window" => Ok(Action::PrevTab),
        "kill-pane" | "close-pane" => Ok(Action::KillPane),
        "kill-window" | "close-tab" => Ok(Action::CloseTab),
        "rename-window" | "rename-tab" => {
            let name = rest.join(" ");
            if name.is_empty() {
                Err(ParseError("rename-tab requires a name".into()))
            } else {
                Ok(Action::RenameTab(name))
            }
        }
        "select-layout" | "layout" => {
            let spec = rest.join(" ");
            if spec.is_empty() {
                Err(ParseError("select-layout requires a spec".into()))
            } else {
                Ok(Action::SelectLayout(spec))
            }
        }
        "equalize" | "even" => Ok(Action::Equalize),
        "zoom" => Ok(Action::Zoom),
        "broadcast" => Ok(Action::Broadcast),
        "select-pane" => {
            let dir = match rest.first().copied() {
                Some("up") => SelectDirection::Up,
                Some("down") => SelectDirection::Down,
                Some("left") => SelectDirection::Left,
                Some("right") => SelectDirection::Right,
                Some("next") => SelectDirection::Next,
                Some("prev") | Some("previous") => SelectDirection::Prev,
                Some("last") => SelectDirection::Last,
                _ => {
                    return Err(ParseError(
                        "select-pane requires up|down|left|right|next|prev|last".into(),
                    ))
                }
            };
            Ok(Action::SelectPane(dir))
        }
        "enter-mode" => {
            let kind = match rest.first().copied() {
                Some("prefix") => EnterModeKind::Prefix,
                Some("copy") | Some("copy-mode") => EnterModeKind::Copy,
                Some("resize") => EnterModeKind::Resize,
                Some("pane-select") => EnterModeKind::PaneSelect,
                Some("help") => EnterModeKind::Help,
                Some("command-palette") | Some("palette") => EnterModeKind::CommandPalette,
                _ => return Err(ParseError("enter-mode requires a mode name".into())),
            };
            Ok(Action::EnterMode(kind))
        }
        "leave-mode" => Ok(Action::LeaveMode),
        "detach" => Ok(Action::Detach),
        "kill-session" => Ok(Action::KillSession),
        "reload-config" | "reload" => Ok(Action::ReloadConfig),
        "toggle" => {
            let kind = match rest.first().copied() {
                Some("status-bar") | Some("statusbar") => ToggleTarget::StatusBar,
                Some("hints") => ToggleTarget::Hints,
                _ => return Err(ParseError("toggle requires status-bar|hints".into())),
            };
            Ok(Action::Toggle(kind))
        }
        "command-palette" | "palette" => Ok(Action::CommandPalette),
        other => Err(ParseError(format!("unknown action: {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_split_aliases() {
        assert_eq!(
            parse("split").unwrap(),
            Action::SplitWindow(Direction::Horizontal)
        );
        assert_eq!(
            parse("split-window").unwrap(),
            Action::SplitWindow(Direction::Horizontal)
        );
        assert_eq!(
            parse("split -v").unwrap(),
            Action::SplitWindow(Direction::Vertical)
        );
        assert_eq!(
            parse("split vertical").unwrap(),
            Action::SplitWindow(Direction::Vertical)
        );
    }

    #[test]
    fn parses_tab_aliases() {
        assert_eq!(parse("new-tab").unwrap(), Action::NewTab);
        assert_eq!(parse("new-window").unwrap(), Action::NewTab);
        assert_eq!(parse("next-tab").unwrap(), Action::NextTab);
        assert_eq!(parse("prev-tab").unwrap(), Action::PrevTab);
        assert_eq!(parse("previous-window").unwrap(), Action::PrevTab);
    }

    #[test]
    fn parses_select_pane_directions() {
        assert_eq!(
            parse("select-pane left").unwrap(),
            Action::SelectPane(SelectDirection::Left)
        );
        assert_eq!(
            parse("select-pane next").unwrap(),
            Action::SelectPane(SelectDirection::Next)
        );
        assert!(parse("select-pane").is_err());
        assert!(parse("select-pane diagonal").is_err());
    }

    #[test]
    fn parses_enter_mode_variants() {
        assert_eq!(
            parse("enter-mode copy").unwrap(),
            Action::EnterMode(EnterModeKind::Copy)
        );
        assert_eq!(
            parse("enter-mode help").unwrap(),
            Action::EnterMode(EnterModeKind::Help)
        );
        assert!(parse("enter-mode").is_err());
        assert!(parse("enter-mode bogus").is_err());
    }

    #[test]
    fn parses_toggle_variants() {
        assert_eq!(
            parse("toggle hints").unwrap(),
            Action::Toggle(ToggleTarget::Hints)
        );
        assert_eq!(
            parse("toggle status-bar").unwrap(),
            Action::Toggle(ToggleTarget::StatusBar)
        );
    }

    #[test]
    fn parses_rename_with_multi_word_name() {
        assert_eq!(
            parse("rename-tab my pretty tab").unwrap(),
            Action::RenameTab("my pretty tab".to_string())
        );
        assert!(parse("rename-tab").is_err());
    }

    #[test]
    fn rejects_unknown_action() {
        let e = parse("teleport").unwrap_err();
        assert!(e.to_string().contains("unknown action"), "got: {e}");
    }

    #[test]
    fn rejects_empty_action() {
        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
    }
}
