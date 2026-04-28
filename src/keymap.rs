//! User-defined keymap (issue #84).
//!
//! Keybindings live in `[keymap.<table>]` tables in `~/.config/ezpn/config.toml`
//! or per-project `.ezpn.toml`. Three tables are supported in v1: `prefix`
//! (active after `Ctrl+B`), `normal` (always-on), `copy_mode`. Defaults ship
//! in `assets/default-keymap.toml`; the user's table merges on top of the
//! defaults, and `clear = true` per-table drops every default for that
//! table.
//!
//! The action vocabulary is a frozen v1 surface — see [`Action`]. A rename
//! is a breaking change post-1.0; additions are always allowed.
//!
//! # Integration TODO (server.rs follow-up)
//!
//! The data model + parser live here; the dispatcher in `server.rs` is the
//! parent's follow-up. To wire this in:
//!
//! 1. At server boot, `let keymap = config::load_keymap()?` (returns
//!    [`Keymap`] with defaults + user overrides merged).
//! 2. Where the existing prefix-mode `match key.code { ... }` lives, look
//!    up `keymap.lookup(KeymapTable::Prefix, &chord)` and dispatch the
//!    returned [`Action`] through `commands::Command` execution.
//! 3. For `KeymapTable::Normal` chord matching, evaluate the lookup
//!    *before* the per-mode key handlers so the user can override builtins.
//! 4. On hot-reload (`Ctrl+B r` / SIGHUP), call `config::load_keymap()` and
//!    swap the in-memory `Keymap`.
//! 5. Unknown action names are rejected at *load time* with line/column
//!    pointing into the offending TOML — the daemon refuses to start.
//!
//! # Compatibility
//!
//! Key syntax is a strict subset of crossterm's `KeyCode`/`KeyModifiers` so
//! incoming `KeyEvent`s can be matched without translation:
//!
//! - Modifiers: `C-` (Ctrl), `M-` (Alt/Meta), `S-` (Shift). May be combined:
//!   `C-S-x`, `C-M-Right`. Order is conventionally Ctrl→Alt→Shift; the
//!   parser accepts any order.
//! - Named keys: `Enter`, `Escape`/`Esc`, `Tab`, `Backspace`, `Delete`,
//!   `Home`, `End`, `PageUp`, `PageDown`, `Up`, `Down`, `Left`, `Right`,
//!   `Insert`, `Space`, `F1`-`F12`.
//! - Single character: literal key (`a`, `0`, `?`). Case-significant only
//!   when paired with `S-`; unmodified `A` and `a` are normalized to lower
//!   case to match crossterm's reporting.
//!
//! # Action vocabulary
//!
//! See [`Action`]. The set is intentionally aligned 1:1 with the
//! command-palette parser in `commands.rs` so users never need to learn two
//! vocabularies.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;

// ─── Action vocabulary (FROZEN v1) ─────────────────────────────────────

/// Frozen v1 action vocabulary. Renames are a breaking change post-1.0.
/// Additions are always allowed.
///
/// Most actions correspond 1:1 to a `commands::Command` variant; a few are
/// keymap-only (mode toggles, palette open) that the command-palette
/// parser doesn't expose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    // ── pane lifecycle ──
    /// Split active pane into two columns.
    SplitWindowH,
    /// Split active pane into two rows.
    SplitWindowV,
    /// Close active pane (with confirmation in the UI layer).
    KillPane,

    // ── window/tab lifecycle ──
    /// Open a new tab; if `name` set, label it.
    NewWindow { name: Option<String> },
    /// Rename the current tab. The dispatcher prompts for the new name.
    RenameWindow,
    /// Close the current tab.
    KillWindow,
    /// `select-window N` — focus tab by zero-based index.
    SelectWindow { index: usize },
    /// Focus the next tab (wraps).
    NextWindow,
    /// Focus the previous tab (wraps).
    PreviousWindow,

    // ── pane navigation ──
    /// Focus an adjacent pane.
    SelectPane { dir: Dir },
    /// Resize active pane by `amount` cells in the given direction.
    ResizePane { dir: Dir, amount: u16 },
    /// Swap active pane with its tree-order neighbour.
    SwapPane { up: bool },
    /// Equalize all pane sizes in the current tab.
    Equalize,

    // ── layout ──
    /// Apply a layout preset by name (`ide`, `dev`, `1:1`, etc.).
    SelectLayout { name: String },

    // ── session lifecycle ──
    /// Detach the current client.
    DetachSession,
    /// Tear down the session (after confirmation in the UI layer).
    KillSession,

    // ── modes ──
    /// Enter copy mode.
    CopyMode,
    /// Cancel current selection / leave a transient mode.
    Cancel,
    /// Begin selection (copy mode).
    BeginSelection,
    /// Yank selection and exit copy mode.
    CopySelectionAndCancel,

    // ── meta ──
    /// Re-read `config.toml` and apply changes that don't need a restart.
    ReloadConfig,
    /// Open the command palette in the UI.
    CommandPrompt,
    /// Toggle the settings panel.
    ToggleSettings,
    /// Toggle the broadcast-input flag.
    ToggleBroadcast,
    /// Flash a message in the status bar. Used by `display-message`.
    DisplayMessage { text: String },
    /// `set-option KEY VALUE` — session-scoped option mutation.
    SetOption { key: String, value: String },

    // ── named copy buffers (#91) ──
    /// `set-buffer NAME VALUE` — write `value` to the named buffer slot.
    /// Empty `name` ("") writes the default buffer.
    SetBuffer { name: String, value: String },
    /// `paste-buffer [NAME]` — emit the named buffer's contents into the
    /// active pane. `None` reads the default buffer.
    PasteBuffer { name: Option<String> },
    /// `list-buffers` — open the named-buffer browser.
    ListBuffers,
}

impl Action {
    /// Stable wire name. Lower-case `kebab-case`. **Frozen.**
    pub fn kind(&self) -> &'static str {
        match self {
            Action::SplitWindowH => "split-window-h",
            Action::SplitWindowV => "split-window-v",
            Action::KillPane => "kill-pane",
            Action::NewWindow { .. } => "new-window",
            Action::RenameWindow => "rename-window",
            Action::KillWindow => "kill-window",
            Action::SelectWindow { .. } => "select-window",
            Action::NextWindow => "next-window",
            Action::PreviousWindow => "previous-window",
            Action::SelectPane { .. } => "select-pane",
            Action::ResizePane { .. } => "resize-pane",
            Action::SwapPane { .. } => "swap-pane",
            Action::Equalize => "equalize",
            Action::SelectLayout { .. } => "select-layout",
            Action::DetachSession => "detach-session",
            Action::KillSession => "kill-session",
            Action::CopyMode => "copy-mode",
            Action::Cancel => "cancel",
            Action::BeginSelection => "begin-selection",
            Action::CopySelectionAndCancel => "copy-selection-and-cancel",
            Action::ReloadConfig => "reload-config",
            Action::CommandPrompt => "command-prompt",
            Action::ToggleSettings => "toggle-settings",
            Action::ToggleBroadcast => "toggle-broadcast",
            Action::DisplayMessage { .. } => "display-message",
            Action::SetOption { .. } => "set-option",
            Action::SetBuffer { .. } => "set-buffer",
            Action::PasteBuffer { .. } => "paste-buffer",
            Action::ListBuffers => "list-buffers",
        }
    }

    /// Frozen list of action *kinds* (without arguments). Useful for help
    /// text and validation against the user's TOML.
    pub fn vocabulary() -> &'static [&'static str] {
        &[
            "split-window-h",
            "split-window-v",
            "kill-pane",
            "new-window",
            "rename-window",
            "kill-window",
            "select-window",
            "next-window",
            "previous-window",
            "select-pane",
            "resize-pane",
            "swap-pane",
            "equalize",
            "select-layout",
            "detach-session",
            "kill-session",
            "copy-mode",
            "cancel",
            "begin-selection",
            "copy-selection-and-cancel",
            "reload-config",
            "command-prompt",
            "toggle-settings",
            "toggle-broadcast",
            "display-message",
            "set-option",
            "set-buffer",
            "paste-buffer",
            "list-buffers",
        ]
    }
}

/// Direction shared with `commands::Dir`. We define our own to keep the
/// keymap module decoupled from the command-palette parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

impl Dir {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "up" | "U" | "u" => Some(Dir::Up),
            "down" | "D" | "d" => Some(Dir::Down),
            "left" | "L" | "l" => Some(Dir::Left),
            "right" | "R" | "r" => Some(Dir::Right),
            _ => None,
        }
    }
}

// ─── Action string parser ──────────────────────────────────────────────

/// Parse an action string from the keymap value side, e.g. `"select-pane right"`,
/// `"new-window -n logs"`, `"select-window 0"`.
pub fn parse_action(input: &str) -> Result<Action, ActionParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ActionParseError::Empty);
    }
    let (head, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((h, r)) => (h, r.trim()),
        None => (trimmed, ""),
    };
    match head {
        "split-window-h" => Ok(Action::SplitWindowH),
        "split-window-v" => Ok(Action::SplitWindowV),
        "kill-pane" => Ok(Action::KillPane),
        "kill-window" => Ok(Action::KillWindow),
        "new-window" => parse_new_window(rest),
        "rename-window" => Ok(Action::RenameWindow),
        "select-window" => {
            let idx: usize = rest
                .parse()
                .map_err(|_| ActionParseError::InvalidArgument {
                    action: "select-window",
                    arg: rest.to_string(),
                })?;
            Ok(Action::SelectWindow { index: idx })
        }
        "next-window" => Ok(Action::NextWindow),
        "previous-window" => Ok(Action::PreviousWindow),
        "select-pane" => {
            let dir = Dir::parse(rest).ok_or_else(|| ActionParseError::InvalidArgument {
                action: "select-pane",
                arg: rest.to_string(),
            })?;
            Ok(Action::SelectPane { dir })
        }
        "resize-pane" => parse_resize_pane(rest),
        "swap-pane" => match rest {
            "up" | "U" | "u" | "" => Ok(Action::SwapPane { up: true }),
            "down" | "D" | "d" => Ok(Action::SwapPane { up: false }),
            other => Err(ActionParseError::InvalidArgument {
                action: "swap-pane",
                arg: other.to_string(),
            }),
        },
        "equalize" => Ok(Action::Equalize),
        "select-layout" => {
            if rest.is_empty() {
                return Err(ActionParseError::MissingArgument {
                    action: "select-layout",
                    arg: "NAME",
                });
            }
            Ok(Action::SelectLayout {
                name: rest.to_string(),
            })
        }
        "detach-session" => Ok(Action::DetachSession),
        "kill-session" => Ok(Action::KillSession),
        "copy-mode" => Ok(Action::CopyMode),
        "cancel" => Ok(Action::Cancel),
        "begin-selection" => Ok(Action::BeginSelection),
        "copy-selection-and-cancel" => Ok(Action::CopySelectionAndCancel),
        "reload-config" => Ok(Action::ReloadConfig),
        "command-prompt" => Ok(Action::CommandPrompt),
        "toggle-settings" => Ok(Action::ToggleSettings),
        "toggle-broadcast" => Ok(Action::ToggleBroadcast),
        "display-message" => {
            if rest.is_empty() {
                return Err(ActionParseError::MissingArgument {
                    action: "display-message",
                    arg: "TEXT",
                });
            }
            Ok(Action::DisplayMessage {
                text: rest.to_string(),
            })
        }
        "set-option" => {
            let (k, v) =
                rest.split_once(char::is_whitespace)
                    .ok_or(ActionParseError::MissingArgument {
                        action: "set-option",
                        arg: "VALUE",
                    })?;
            Ok(Action::SetOption {
                key: k.to_string(),
                value: v.trim().to_string(),
            })
        }
        "set-buffer" => {
            if rest.is_empty() {
                return Err(ActionParseError::MissingArgument {
                    action: "set-buffer",
                    arg: "NAME",
                });
            }
            let (name, value) =
                rest.split_once(char::is_whitespace)
                    .ok_or(ActionParseError::MissingArgument {
                        action: "set-buffer",
                        arg: "VALUE",
                    })?;
            Ok(Action::SetBuffer {
                name: name.to_string(),
                value: value.trim().to_string(),
            })
        }
        "paste-buffer" => {
            let name = if rest.is_empty() {
                None
            } else {
                Some(rest.to_string())
            };
            Ok(Action::PasteBuffer { name })
        }
        "list-buffers" => Ok(Action::ListBuffers),
        other => Err(ActionParseError::UnknownAction(other.to_string())),
    }
}

fn parse_new_window(rest: &str) -> Result<Action, ActionParseError> {
    if rest.is_empty() {
        return Ok(Action::NewWindow { name: None });
    }
    // Accept `-n NAME` for parity with the palette.
    if let Some(stripped) = rest.strip_prefix("-n") {
        let name = stripped.trim();
        if name.is_empty() {
            return Err(ActionParseError::MissingArgument {
                action: "new-window",
                arg: "NAME",
            });
        }
        return Ok(Action::NewWindow {
            name: Some(name.to_string()),
        });
    }
    // Bare positional: take the rest as the name.
    Ok(Action::NewWindow {
        name: Some(rest.to_string()),
    })
}

fn parse_resize_pane(rest: &str) -> Result<Action, ActionParseError> {
    let (dir_part, amount_part) = match rest.split_once(char::is_whitespace) {
        Some((d, a)) => (d, a.trim()),
        None => (rest, ""),
    };
    let dir = Dir::parse(dir_part).ok_or_else(|| ActionParseError::InvalidArgument {
        action: "resize-pane",
        arg: dir_part.to_string(),
    })?;
    let amount: u16 = if amount_part.is_empty() {
        1
    } else {
        amount_part
            .parse()
            .map_err(|_| ActionParseError::InvalidArgument {
                action: "resize-pane",
                arg: amount_part.to_string(),
            })?
    };
    Ok(Action::ResizePane { dir, amount })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionParseError {
    Empty,
    UnknownAction(String),
    MissingArgument {
        action: &'static str,
        arg: &'static str,
    },
    InvalidArgument {
        action: &'static str,
        arg: String,
    },
}

impl fmt::Display for ActionParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActionParseError::Empty => f.write_str("empty action"),
            ActionParseError::UnknownAction(name) => write!(f, "unknown action: {name}"),
            ActionParseError::MissingArgument { action, arg } => {
                write!(f, "{action}: missing argument <{arg}>")
            }
            ActionParseError::InvalidArgument { action, arg } => {
                write!(f, "{action}: invalid argument: {arg}")
            }
        }
    }
}

impl std::error::Error for ActionParseError {}

// ─── Key chord parser ──────────────────────────────────────────────────

/// A parsed keystroke — modifiers + a [`KeyCode`] subset.
///
/// We hash on `(KeyCode, KeyModifiers)` so lookup against an incoming
/// crossterm `KeyEvent` is one map probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl KeyChord {
    pub fn new(code: KeyCode, mods: KeyModifiers) -> Self {
        Self { code, mods }
    }

    /// Build a chord from an incoming `KeyEvent`. Strips modifiers we don't
    /// match on (super, hyper, num-pad markers) so the lookup doesn't care.
    pub fn from_event(ev: KeyEvent) -> Self {
        let mods = ev.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        // Normalize unmodified single-char keys to lower-case so `a` and
        // `A` only differ when SHIFT is explicit. Crossterm reports `A`
        // with SHIFT and `a` without on most platforms; we collapse those.
        let code = match ev.code {
            KeyCode::Char(c) if !mods.contains(KeyModifiers::SHIFT) => {
                KeyCode::Char(c.to_ascii_lowercase())
            }
            other => other,
        };
        Self { code, mods }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyParseError {
    Empty,
    UnknownNamedKey(String),
    InvalidModifier(String),
    DanglingModifier,
}

impl fmt::Display for KeyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyParseError::Empty => f.write_str("empty key spec"),
            KeyParseError::UnknownNamedKey(s) => write!(f, "unknown named key: {s}"),
            KeyParseError::InvalidModifier(s) => write!(f, "invalid modifier: {s}"),
            KeyParseError::DanglingModifier => f.write_str("modifier without a key"),
        }
    }
}

impl std::error::Error for KeyParseError {}

/// Parse a key spec like `C-Right`, `M-S-Tab`, `F2`, `?`, `Space`.
pub fn parse_chord(input: &str) -> Result<KeyChord, KeyParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(KeyParseError::Empty);
    }

    let mut mods = KeyModifiers::NONE;
    let mut rest = trimmed;
    loop {
        // Modifiers are `C-`, `M-`, `S-` (any order). Bail out as soon as
        // the next 2 chars don't look like a modifier prefix.
        let bytes = rest.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b'-' {
            match bytes[0] {
                b'C' | b'c' => {
                    mods |= KeyModifiers::CONTROL;
                    rest = &rest[2..];
                    continue;
                }
                b'M' | b'm' => {
                    mods |= KeyModifiers::ALT;
                    rest = &rest[2..];
                    continue;
                }
                b'S' | b's' => {
                    mods |= KeyModifiers::SHIFT;
                    rest = &rest[2..];
                    continue;
                }
                _ => {}
            }
        }
        break;
    }

    if rest.is_empty() {
        return Err(KeyParseError::DanglingModifier);
    }

    // F-keys.
    if let Some(num) = rest.strip_prefix(['F', 'f']) {
        if let Ok(n) = num.parse::<u8>() {
            if (1..=12).contains(&n) {
                return Ok(KeyChord::new(KeyCode::F(n), mods));
            }
        }
    }

    let code = match rest {
        "Enter" | "enter" | "Return" | "return" => KeyCode::Enter,
        "Esc" | "esc" | "Escape" | "escape" => KeyCode::Esc,
        "Tab" | "tab" => KeyCode::Tab,
        "BackTab" | "backtab" => KeyCode::BackTab,
        "Backspace" | "backspace" | "BS" => KeyCode::Backspace,
        "Delete" | "delete" | "Del" => KeyCode::Delete,
        "Insert" | "insert" | "Ins" => KeyCode::Insert,
        "Home" | "home" => KeyCode::Home,
        "End" | "end" => KeyCode::End,
        "PageUp" | "pageup" | "PgUp" => KeyCode::PageUp,
        "PageDown" | "pagedown" | "PgDn" | "PgDown" => KeyCode::PageDown,
        "Up" | "up" => KeyCode::Up,
        "Down" | "down" => KeyCode::Down,
        "Left" | "left" => KeyCode::Left,
        "Right" | "right" => KeyCode::Right,
        "Space" | "space" => KeyCode::Char(' '),
        // Single character.
        s if s.chars().count() == 1 => {
            let c = s.chars().next().unwrap();
            // If SHIFT was specified explicitly, keep the literal case
            // (parser-side we trust the user). Otherwise normalize lower.
            let c = if mods.contains(KeyModifiers::SHIFT) {
                c
            } else {
                c.to_ascii_lowercase()
            };
            KeyCode::Char(c)
        }
        other => return Err(KeyParseError::UnknownNamedKey(other.to_string())),
    };

    Ok(KeyChord { code, mods })
}

// ─── Tables and the Keymap container ───────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeymapTable {
    Prefix,
    Normal,
    CopyMode,
}

impl KeymapTable {
    pub fn name(self) -> &'static str {
        match self {
            KeymapTable::Prefix => "prefix",
            KeymapTable::Normal => "normal",
            KeymapTable::CopyMode => "copy_mode",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "prefix" => Some(KeymapTable::Prefix),
            "normal" => Some(KeymapTable::Normal),
            "copy_mode" | "copy-mode" => Some(KeymapTable::CopyMode),
            _ => None,
        }
    }

    pub fn all() -> &'static [KeymapTable] {
        &[
            KeymapTable::Prefix,
            KeymapTable::Normal,
            KeymapTable::CopyMode,
        ]
    }
}

/// Resolved keymap. The dispatcher in `server.rs` calls
/// [`Keymap::lookup`] with the active table + an incoming chord.
///
/// We store tables in a `BTreeMap` (keyed by `KeymapTable`) so iteration
/// order is deterministic for tests, and the inner `chord -> action` map in
/// a `HashMap` because `crossterm::KeyCode` does not implement `Ord`.
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    tables: BTreeMap<KeymapTable, HashMap<KeyChord, Action>>,
}

impl Keymap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a chord in the given table. `None` means "fall through to
    /// the built-in handler" — callers must not error on miss.
    pub fn lookup(&self, table: KeymapTable, chord: &KeyChord) -> Option<&Action> {
        self.tables.get(&table)?.get(chord)
    }

    /// Number of bindings registered in the given table.
    pub fn len(&self, table: KeymapTable) -> usize {
        self.tables.get(&table).map(HashMap::len).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.tables.values().all(HashMap::is_empty)
    }

    fn table_mut(&mut self, t: KeymapTable) -> &mut HashMap<KeyChord, Action> {
        self.tables.entry(t).or_default()
    }

    /// Bind a parsed chord → action, replacing any existing entry for the
    /// same chord in the same table.
    pub fn bind(&mut self, table: KeymapTable, chord: KeyChord, action: Action) {
        self.table_mut(table).insert(chord, action);
    }

    /// Drop every binding in a table. Driven by `clear = true` in TOML.
    pub fn clear(&mut self, table: KeymapTable) {
        if let Some(t) = self.tables.get_mut(&table) {
            t.clear();
        }
    }
}

// ─── TOML parsing ──────────────────────────────────────────────────────

/// Errors with line/column metadata so the daemon can refuse to start with
/// a structured message pointing at the offending TOML row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeymapLoadError {
    pub table: String,
    pub key: String,
    pub message: String,
}

impl fmt::Display for KeymapLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[keymap.{}] {key}: {msg}",
            self.table,
            key = self.key,
            msg = self.message
        )
    }
}

impl std::error::Error for KeymapLoadError {}

/// Apply a single TOML table (e.g. the value of `[keymap.prefix]`) to the
/// keymap. Returns the first hard error encountered. Soft warnings (e.g.
/// shadowed default) are passed back as `Vec<String>`.
pub fn apply_table(
    keymap: &mut Keymap,
    table: KeymapTable,
    raw: &toml::Table,
) -> Result<Vec<String>, KeymapLoadError> {
    let mut warnings = Vec::new();
    // The synthetic `clear` flag is consumed first — it nukes the table
    // before applying any bindings from this layer.
    if let Some(v) = raw.get("clear") {
        match v.as_bool() {
            Some(true) => keymap.clear(table),
            Some(false) => {}
            None => {
                return Err(KeymapLoadError {
                    table: table.name().to_string(),
                    key: "clear".to_string(),
                    message: "must be a boolean".to_string(),
                });
            }
        }
    }

    for (key_str, value) in raw {
        if key_str == "clear" {
            continue;
        }
        let action_str = match value.as_str() {
            Some(s) => s,
            None => {
                return Err(KeymapLoadError {
                    table: table.name().to_string(),
                    key: key_str.clone(),
                    message: "value must be a string action".to_string(),
                });
            }
        };
        let chord = parse_chord(key_str).map_err(|e| KeymapLoadError {
            table: table.name().to_string(),
            key: key_str.clone(),
            message: format!("invalid key: {e}"),
        })?;
        let action = parse_action(action_str).map_err(|e| KeymapLoadError {
            table: table.name().to_string(),
            key: key_str.clone(),
            message: format!("invalid action: {e}"),
        })?;
        if keymap.lookup(table, &chord).is_some() {
            warnings.push(format!(
                "[keymap.{}] {key_str} overrides previous binding",
                table.name()
            ));
        }
        keymap.bind(table, chord, action);
    }
    Ok(warnings)
}

/// Apply a top-level `[keymap]` TOML table: each sub-table is one of the
/// known [`KeymapTable`] names. Unknown sub-tables produce a hard error.
pub fn apply_keymap_section(
    keymap: &mut Keymap,
    raw: &toml::Table,
) -> Result<Vec<String>, KeymapLoadError> {
    let mut warnings = Vec::new();
    for (sub_name, sub_value) in raw {
        let table = KeymapTable::from_str(sub_name).ok_or_else(|| KeymapLoadError {
            table: sub_name.clone(),
            key: String::new(),
            message: format!(
                "unknown keymap table (valid: {})",
                KeymapTable::all()
                    .iter()
                    .map(|t| t.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        })?;
        let sub_table = sub_value.as_table().ok_or_else(|| KeymapLoadError {
            table: sub_name.clone(),
            key: String::new(),
            message: "must be a table".to_string(),
        })?;
        warnings.extend(apply_table(keymap, table, sub_table)?);
    }
    Ok(warnings)
}

// ─── Defaults ──────────────────────────────────────────────────────────

/// The shipped default keymap, embedded at compile time. Deserialized once
/// via [`load_defaults`] and merged under user config.
pub const DEFAULT_KEYMAP_TOML: &str = include_str!("../assets/default-keymap.toml");

/// Build the default keymap from the embedded asset. Panics on malformed
/// asset content — that's a build-time bug, not a runtime condition.
pub fn load_defaults() -> Keymap {
    let raw: toml::Table = toml::from_str(DEFAULT_KEYMAP_TOML)
        .expect("default-keymap.toml must parse — this is a build error, not runtime");
    let inner = raw
        .get("keymap")
        .and_then(toml::Value::as_table)
        .expect("default-keymap.toml must contain [keymap.*] tables");
    let mut keymap = Keymap::new();
    apply_keymap_section(&mut keymap, inner)
        .expect("default-keymap.toml must validate — this is a build error, not runtime");
    keymap
}

// ─── TOML wire schema (re-exported for config.rs) ──────────────────────

/// Top-level `[keymap.<table>]` capture used by [`crate::config`] and
/// [`crate::project`]. `flatten` would be nice but breaks `toml`'s type
/// inference here, so we just take the inner table.
#[derive(Debug, Default, Deserialize)]
pub struct RawKeymapSection {
    #[serde(flatten)]
    pub tables: BTreeMap<String, toml::Table>,
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(code: KeyCode, mods: KeyModifiers) -> KeyChord {
        KeyChord::new(code, mods)
    }

    // ─── Action parser ─────────────────────────────────────────────────

    #[test]
    fn parses_simple_actions() {
        assert_eq!(parse_action("kill-pane"), Ok(Action::KillPane));
        assert_eq!(parse_action("equalize"), Ok(Action::Equalize));
        assert_eq!(parse_action("detach-session"), Ok(Action::DetachSession));
        assert_eq!(parse_action("reload-config"), Ok(Action::ReloadConfig));
    }

    #[test]
    fn parses_actions_with_args() {
        assert_eq!(
            parse_action("select-window 0"),
            Ok(Action::SelectWindow { index: 0 })
        );
        assert_eq!(
            parse_action("select-pane right"),
            Ok(Action::SelectPane { dir: Dir::Right })
        );
        assert_eq!(
            parse_action("resize-pane left 5"),
            Ok(Action::ResizePane {
                dir: Dir::Left,
                amount: 5
            })
        );
        assert_eq!(
            parse_action("new-window -n logs"),
            Ok(Action::NewWindow {
                name: Some("logs".to_string()),
            })
        );
        assert_eq!(
            parse_action("display-message hello world"),
            Ok(Action::DisplayMessage {
                text: "hello world".to_string(),
            })
        );
        assert_eq!(
            parse_action("set-option border heavy"),
            Ok(Action::SetOption {
                key: "border".to_string(),
                value: "heavy".to_string(),
            })
        );
    }

    #[test]
    fn unknown_action_is_structured_error() {
        let err = parse_action("frobnicate").unwrap_err();
        assert_eq!(err, ActionParseError::UnknownAction("frobnicate".into()));
    }

    #[test]
    fn select_window_rejects_non_integer() {
        let err = parse_action("select-window foo").unwrap_err();
        assert!(matches!(err, ActionParseError::InvalidArgument { .. }));
    }

    #[test]
    fn parses_named_buffer_actions() {
        // set-buffer NAME VALUE — name and value separated by whitespace.
        assert_eq!(
            parse_action("set-buffer foo hello world"),
            Ok(Action::SetBuffer {
                name: "foo".to_string(),
                value: "hello world".to_string(),
            })
        );
        // paste-buffer with explicit name.
        assert_eq!(
            parse_action("paste-buffer foo"),
            Ok(Action::PasteBuffer {
                name: Some("foo".to_string()),
            })
        );
        // paste-buffer with no name → default buffer.
        assert_eq!(
            parse_action("paste-buffer"),
            Ok(Action::PasteBuffer { name: None })
        );
        // list-buffers takes no arguments.
        assert_eq!(parse_action("list-buffers"), Ok(Action::ListBuffers));
    }

    #[test]
    fn buffer_actions_kind_strings_are_stable() {
        // The `kind()` string is the wire / display name — frozen v1.
        assert_eq!(
            Action::SetBuffer {
                name: "n".into(),
                value: "v".into()
            }
            .kind(),
            "set-buffer",
        );
        assert_eq!(
            Action::PasteBuffer { name: None }.kind(),
            "paste-buffer",
        );
        assert_eq!(
            Action::PasteBuffer {
                name: Some("foo".into())
            }
            .kind(),
            "paste-buffer",
        );
        assert_eq!(Action::ListBuffers.kind(), "list-buffers");
    }

    #[test]
    fn set_buffer_requires_name_and_value() {
        let err = parse_action("set-buffer").unwrap_err();
        assert!(
            matches!(
                err,
                ActionParseError::MissingArgument {
                    action: "set-buffer",
                    arg: "NAME"
                }
            ),
            "expected missing NAME, got {err:?}",
        );
        let err = parse_action("set-buffer onlyname").unwrap_err();
        assert!(
            matches!(
                err,
                ActionParseError::MissingArgument {
                    action: "set-buffer",
                    arg: "VALUE"
                }
            ),
            "expected missing VALUE, got {err:?}",
        );
    }

    #[test]
    fn vocabulary_includes_every_action_kind() {
        // Every variant of `Action` should appear in `vocabulary()`. We
        // build one Action per variant via parse_action and assert the
        // .kind() shows up.
        let probes = [
            "split-window-h",
            "split-window-v",
            "kill-pane",
            "new-window",
            "rename-window",
            "kill-window",
            "select-window 0",
            "next-window",
            "previous-window",
            "select-pane right",
            "resize-pane up",
            "swap-pane",
            "equalize",
            "select-layout ide",
            "detach-session",
            "kill-session",
            "copy-mode",
            "cancel",
            "begin-selection",
            "copy-selection-and-cancel",
            "reload-config",
            "command-prompt",
            "toggle-settings",
            "toggle-broadcast",
            "display-message hi",
            "set-option a b",
            "set-buffer name value",
            "paste-buffer",
            "list-buffers",
        ];
        for p in probes {
            let action = parse_action(p).unwrap_or_else(|e| panic!("{p:?}: {e}"));
            assert!(
                Action::vocabulary().contains(&action.kind()),
                "kind {} missing from vocabulary",
                action.kind()
            );
        }
    }

    // ─── Key chord parser ─────────────────────────────────────────────

    #[test]
    fn parses_single_chars() {
        assert_eq!(
            parse_chord("d"),
            Ok(chord(KeyCode::Char('d'), KeyModifiers::NONE))
        );
        assert_eq!(
            parse_chord("?"),
            Ok(chord(KeyCode::Char('?'), KeyModifiers::NONE))
        );
        // Unmodified `A` normalizes to `a`.
        assert_eq!(
            parse_chord("A"),
            Ok(chord(KeyCode::Char('a'), KeyModifiers::NONE))
        );
    }

    #[test]
    fn parses_modified_chords() {
        assert_eq!(
            parse_chord("C-Right"),
            Ok(chord(KeyCode::Right, KeyModifiers::CONTROL))
        );
        assert_eq!(
            parse_chord("M-x"),
            Ok(chord(KeyCode::Char('x'), KeyModifiers::ALT))
        );
        assert_eq!(
            parse_chord("S-Tab"),
            Ok(chord(KeyCode::Tab, KeyModifiers::SHIFT))
        );
        // Multiple modifiers, any order.
        assert_eq!(
            parse_chord("C-M-Right"),
            Ok(chord(
                KeyCode::Right,
                KeyModifiers::CONTROL | KeyModifiers::ALT
            ))
        );
        assert_eq!(
            parse_chord("M-C-Right"),
            Ok(chord(
                KeyCode::Right,
                KeyModifiers::CONTROL | KeyModifiers::ALT
            ))
        );
    }

    #[test]
    fn parses_named_keys() {
        assert_eq!(
            parse_chord("F2"),
            Ok(chord(KeyCode::F(2), KeyModifiers::NONE))
        );
        assert_eq!(
            parse_chord("Enter"),
            Ok(chord(KeyCode::Enter, KeyModifiers::NONE))
        );
        assert_eq!(
            parse_chord("Esc"),
            Ok(chord(KeyCode::Esc, KeyModifiers::NONE))
        );
        assert_eq!(
            parse_chord("Space"),
            Ok(chord(KeyCode::Char(' '), KeyModifiers::NONE))
        );
        assert_eq!(
            parse_chord("PageUp"),
            Ok(chord(KeyCode::PageUp, KeyModifiers::NONE))
        );
    }

    #[test]
    fn rejects_dangling_modifier() {
        assert_eq!(
            parse_chord("C-").unwrap_err(),
            KeyParseError::DanglingModifier
        );
    }

    #[test]
    fn rejects_unknown_named_key() {
        let err = parse_chord("Banana").unwrap_err();
        assert!(matches!(err, KeyParseError::UnknownNamedKey(_)));
    }

    #[test]
    fn from_event_strips_irrelevant_modifiers_and_normalizes_case() {
        let ev = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE);
        let c = KeyChord::from_event(ev);
        assert_eq!(c.code, KeyCode::Char('a'));
        assert!(c.mods.is_empty());

        let ev = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT);
        let c = KeyChord::from_event(ev);
        assert_eq!(c.code, KeyCode::Char('A'));
        assert_eq!(c.mods, KeyModifiers::SHIFT);
    }

    // ─── Keymap container ──────────────────────────────────────────────

    #[test]
    fn lookup_returns_bound_action() {
        let mut km = Keymap::new();
        km.bind(
            KeymapTable::Prefix,
            parse_chord("d").unwrap(),
            Action::DetachSession,
        );
        let hit = km.lookup(KeymapTable::Prefix, &parse_chord("d").unwrap());
        assert_eq!(hit, Some(&Action::DetachSession));
        // Wrong table → miss.
        assert!(km
            .lookup(KeymapTable::Normal, &parse_chord("d").unwrap())
            .is_none());
    }

    #[test]
    fn clear_drops_bindings_for_table_only() {
        let mut km = Keymap::new();
        km.bind(
            KeymapTable::Prefix,
            parse_chord("d").unwrap(),
            Action::DetachSession,
        );
        km.bind(
            KeymapTable::Normal,
            parse_chord("F2").unwrap(),
            Action::Equalize,
        );
        km.clear(KeymapTable::Prefix);
        assert_eq!(km.len(KeymapTable::Prefix), 0);
        assert_eq!(km.len(KeymapTable::Normal), 1);
    }

    // ─── TOML application ──────────────────────────────────────────────

    #[test]
    fn apply_table_binds_and_clears() {
        let mut km = Keymap::new();
        km.bind(
            KeymapTable::Prefix,
            parse_chord("d").unwrap(),
            Action::DetachSession,
        );
        let raw: toml::Table = toml::from_str(
            r#"
            clear = true
            "d" = "kill-window"
            "%" = "split-window-h"
            "#,
        )
        .unwrap();
        apply_table(&mut km, KeymapTable::Prefix, &raw).unwrap();
        // `clear = true` removed the default DetachSession; user's d→kill-window
        // is the new binding.
        let hit = km.lookup(KeymapTable::Prefix, &parse_chord("d").unwrap());
        assert_eq!(hit, Some(&Action::KillWindow));
    }

    #[test]
    fn apply_table_rejects_unknown_action_with_structured_error() {
        let mut km = Keymap::new();
        let raw: toml::Table = toml::from_str(r#""d" = "frobnicate""#).unwrap();
        let err = apply_table(&mut km, KeymapTable::Prefix, &raw).unwrap_err();
        assert_eq!(err.table, "prefix");
        assert_eq!(err.key, "d");
        assert!(err.message.contains("unknown action"));
    }

    #[test]
    fn apply_table_rejects_invalid_key_spec() {
        let mut km = Keymap::new();
        let raw: toml::Table = toml::from_str(r#""C-" = "kill-pane""#).unwrap();
        let err = apply_table(&mut km, KeymapTable::Prefix, &raw).unwrap_err();
        assert!(err.message.contains("invalid key"), "{err}");
    }

    #[test]
    fn apply_keymap_section_rejects_unknown_table() {
        let mut km = Keymap::new();
        let raw: toml::Table = toml::from_str(
            r#"
            [mystery]
            "d" = "kill-pane"
            "#,
        )
        .unwrap();
        let err = apply_keymap_section(&mut km, &raw).unwrap_err();
        assert_eq!(err.table, "mystery");
        assert!(err.message.contains("unknown keymap table"));
    }

    #[test]
    fn defaults_load_without_panic_and_have_bindings() {
        let km = load_defaults();
        assert!(!km.is_empty(), "defaults must contain at least one binding");
        assert!(
            km.len(KeymapTable::Prefix) > 0,
            "defaults must have prefix bindings"
        );
    }

    #[test]
    fn user_clear_drops_defaults_full_path() {
        // The acceptance criterion: `[keymap.prefix] clear = true` drops
        // every default in that table.
        let mut km = load_defaults();
        let before = km.len(KeymapTable::Prefix);
        assert!(before > 0);
        let raw: toml::Table = toml::from_str(
            r#"
            [prefix]
            clear = true
            "x" = "kill-pane"
            "#,
        )
        .unwrap();
        apply_keymap_section(&mut km, &raw).unwrap();
        // Only the user's single binding remains.
        assert_eq!(km.len(KeymapTable::Prefix), 1);
    }

    #[test]
    fn user_can_remap_prefix_d_to_kill_window() {
        // Direct expression of the issue's first acceptance criterion.
        let mut km = load_defaults();
        let raw: toml::Table = toml::from_str(
            r#"
            [prefix]
            "d" = "kill-window"
            "#,
        )
        .unwrap();
        apply_keymap_section(&mut km, &raw).unwrap();
        let hit = km.lookup(KeymapTable::Prefix, &parse_chord("d").unwrap());
        assert_eq!(hit, Some(&Action::KillWindow));
    }
}
