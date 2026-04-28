use crate::hooks::{Hook, HookParseError, RawHook};
use crate::keymap::{apply_keymap_section, load_defaults, Keymap, KeymapLoadError};
use crate::render::BorderStyle;
use crate::terminal_state::{ClipboardPolicy, Osc52GetPolicy, Osc52SetPolicy};
use crate::theme::Theme;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Eviction policy for the byte-budget scrollback cap (#67).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollbackEviction {
    /// Evict the oldest line first. Default. Matches user mental model
    /// (FIFO timeline preserved minus the head).
    OldestLine,
    /// Evict the largest line first. Useful when the timeline matters
    /// more than rare giant lines (compiler errors, log dumps).
    LargestLine,
}

impl Default for ScrollbackEviction {
    fn default() -> Self {
        ScrollbackEviction::OldestLine
    }
}

impl ScrollbackEviction {
    pub fn as_str(self) -> &'static str {
        match self {
            ScrollbackEviction::OldestLine => "oldest_line",
            ScrollbackEviction::LargestLine => "largest_line",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "oldest_line" => Some(ScrollbackEviction::OldestLine),
            "largest_line" => Some(ScrollbackEviction::LargestLine),
            _ => None,
        }
    }
}

/// Runtime config merged from: defaults < config file < CLI args.
pub struct EzpnConfig {
    pub border: BorderStyle,
    pub shell: String,
    pub scrollback: usize,
    /// Byte-budget cap on the per-pane scrollback shim (#67).
    /// `0` disables the byte cap; the line cap (`scrollback`) still applies.
    /// Default: 32 MiB.
    pub scrollback_bytes: usize,
    /// Eviction policy applied when `scrollback_bytes` is exceeded.
    pub scrollback_eviction: ScrollbackEviction,
    pub show_status_bar: bool,
    pub show_tab_bar: bool,
    /// Prefix key character (default: 'b' for Ctrl+B).
    pub prefix_key: char,
    /// OSC 52 clipboard policy applied to every pane on spawn (#79).
    /// Defaults to `Confirm` for set, `Deny` for read, 1 MiB hard cap —
    /// see [`ClipboardPolicy::default`].
    pub clipboard: ClipboardPolicy,
    /// Optional override for the system-clipboard fallback chain (#92).
    /// Empty = auto-detect (`wl-copy` / `xclip` / `xsel` / `pbcopy`,
    /// in that order). When non-empty, the slice is invoked verbatim:
    /// `argv[0]` is the program, the rest are args. The yank path
    /// pipes the selected text into the child's stdin and falls back
    /// to OSC 52 on any error.
    pub clipboard_copy_command: Vec<String>,
    /// Reserved companion to `clipboard_copy_command`. Paste from the
    /// system clipboard is emulator-driven today; the field is parsed
    /// and stored so the schema is forwards-compatible with the eventual
    /// paste support (out of scope for #92).
    pub clipboard_paste_command: Vec<String>,
    /// Resolved colour palette (#85). Defaults to the built-in
    /// `ezpn-dark` theme. Replaced when the user sets `[theme] name = "..."`
    /// or supplies a full inline `[theme]` section.
    pub theme: Theme,
    /// Status-bar segment layout (#87). Empty vectors mean "use default
    /// hardcoded layout"; the renderer decides what to render based on
    /// segment kind plus current input mode.
    pub status_bar: StatusBarConfig,
    /// Whether the daemon should persist per-pane scrollback into the
    /// session snapshot (#69). Off by default: the v3 schema is
    /// additive, so leaving this `false` keeps every existing
    /// integration working and the on-disk JSON byte-compatible with
    /// v2 — turning it on costs a `[global]` opt-in plus optionally a
    /// per-pane `[[pane]] persist_scrollback = true` override (project
    /// `.ezpn.toml`).
    pub persist_scrollback: bool,
}

impl Default for EzpnConfig {
    fn default() -> Self {
        Self {
            border: BorderStyle::Rounded,
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
            scrollback: 10_000,
            scrollback_bytes: DEFAULT_SCROLLBACK_BYTES,
            scrollback_eviction: ScrollbackEviction::default(),
            show_status_bar: true,
            show_tab_bar: true,
            prefix_key: 'b',
            clipboard: ClipboardPolicy::default(),
            clipboard_copy_command: Vec::new(),
            clipboard_paste_command: Vec::new(),
            theme: Theme::default_theme(),
            status_bar: StatusBarConfig::default(),
            persist_scrollback: false,
        }
    }
}

// ─── Status-bar declarative segments (#87) ────────────────
//
// Schema (TOML):
//   [status_bar]
//   left  = ["{session}", "{tab_count}", "{mode}"]
//   right = ["{key_hints}", "{time}"]
//
//   [status_bar.segments.key_hints]
//   type = "key_hints"
//   mode = "auto"      # "auto" | "normal" | "prefix" | "copy_mode" | "search"
//   max_width = 60
//   keys = [
//       { key = "C-b d", label = "detach" },
//       { key = "C-b c", label = "new tab" },
//   ]

/// Side of the status bar a segment list anchors to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusBarSide {
    Left,
    Right,
}

/// Built-in mode tag for `key_hints` segments. `Auto` means "follow the
/// renderer's current input mode"; the four explicit values render that
/// mode's hints regardless of state — useful for static cheatsheets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HintMode {
    #[default]
    Auto,
    Normal,
    Prefix,
    CopyMode,
    Search,
}

impl HintMode {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "normal" => Some(Self::Normal),
            "prefix" => Some(Self::Prefix),
            "copy_mode" => Some(Self::CopyMode),
            "search" => Some(Self::Search),
            _ => None,
        }
    }
}

/// One key-hint pair as declared in TOML.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyHint {
    pub key: String,
    pub label: String,
}

/// One status-bar segment. A segment is referenced from `left`/`right` by
/// its placeholder name (e.g. `{key_hints}`); the kind drives rendering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SegmentKind {
    /// Built-in placeholders rendered by the status-bar pipeline. Names:
    /// `session`, `tab_count`, `mode`, `time`. The renderer owns the format.
    Builtin(String),
    /// Declarative key-hint table. Renderer truncates by `max_width` and
    /// drops right-most entries when the terminal narrows.
    KeyHints {
        mode: HintMode,
        max_width: u16,
        keys: Vec<KeyHint>,
    },
    /// Free-form literal string. Lets users add custom labels without
    /// editing source.
    Literal(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusBarSegment {
    pub name: String,
    pub kind: SegmentKind,
}

/// Resolved status-bar config. Both vecs preserve TOML declaration order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusBarConfig {
    /// Ordered list of placeholder names anchored to the left edge.
    pub left: Vec<String>,
    /// Ordered list of placeholder names anchored to the right edge.
    pub right: Vec<String>,
    /// Lookup table: placeholder name -> resolved segment.
    pub segments: Vec<StatusBarSegment>,
}

impl StatusBarConfig {
    /// Look up a segment by placeholder name (e.g. `key_hints` for
    /// `"{key_hints}"`). Returns the first match.
    pub fn segment(&self, name: &str) -> Option<&StatusBarSegment> {
        self.segments.iter().find(|s| s.name == name)
    }
}

const SCROLLBACK_MAX: usize = 100_000;
/// Default byte-budget for scrollback (#67): 32 MiB.
pub const DEFAULT_SCROLLBACK_BYTES: usize = 32 * 1024 * 1024;
/// Hard ceiling on `scrollback_bytes` to keep a misconfigured value from
/// crashing the daemon: 4 GiB. Anything past this is silently clamped.
pub const SCROLLBACK_BYTES_MAX: usize = 4 * 1024 * 1024 * 1024;
const KNOWN_GLOBAL_KEYS: &[&str] = &[
    "border",
    "shell",
    "scrollback",
    "scrollback_bytes",
    "scrollback_eviction",
    "status_bar",
    "tab_bar",
    "persist_scrollback",
];
const KNOWN_KEYS_KEYS: &[&str] = &["prefix"];
const KNOWN_CLIPBOARD_KEYS: &[&str] = &[
    "osc52_set",
    "osc52_get",
    "osc52_max_bytes",
    "copy_command",
    "paste_command",
];
const KNOWN_THEME_KEYS: &[&str] = &[
    "name",
    "fg",
    "bg",
    "border",
    "border_active",
    "status_bg",
    "status_fg",
    "tab_active_bg",
    "tab_active_fg",
    "tab_inactive_fg",
    "selection",
    "search_match",
    "broadcast_indicator",
    "copy_mode_indicator",
];
const KNOWN_STATUS_BAR_KEYS: &[&str] = &["left", "right", "segments"];
const KNOWN_TOP_LEVEL: &[&str] = &[
    "global",
    "keys",
    "hooks",
    "keymap",
    "clipboard",
    "theme",
    "status_bar",
];

// ─── Schema (TOML format, v0.12+) ──────────────────────────

#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    global: Option<toml::Table>,
    #[serde(default)]
    keys: Option<toml::Table>,
    /// `[[hooks]]` array-of-tables. See `crate::hooks` for the wire schema
    /// and security guarantees (issue #83).
    #[serde(default)]
    hooks: Vec<RawHook>,
    /// `[keymap.<table>]` user keybindings. See `crate::keymap` for the
    /// schema and the frozen v1 action vocabulary (issue #84).
    #[serde(default)]
    keymap: Option<toml::Table>,
    /// `[clipboard]` table for OSC 52 paste-injection guard policy (issue #79).
    #[serde(default)]
    clipboard: Option<toml::Table>,
    /// `[theme]` table — either `name = "..."` to pick a built-in or the
    /// full inline palette (issue #85).
    #[serde(default)]
    theme: Option<toml::Table>,
    /// `[status_bar]` table with `left` / `right` segment lists and a
    /// `[status_bar.segments.<name>]` definition per declared placeholder
    /// (issue #87).
    #[serde(default)]
    status_bar: Option<toml::Table>,
}

#[derive(Debug, Default, Deserialize)]
struct GlobalSection {
    border: Option<String>,
    shell: Option<String>,
    scrollback: Option<i64>,
    /// Accepts either an integer byte count (`33554432`) or a humansize
    /// string (`"32M"`, `"512K"`, `"2G"`). `0` disables the cap.
    scrollback_bytes: Option<toml::Value>,
    scrollback_eviction: Option<String>,
    status_bar: Option<bool>,
    tab_bar: Option<bool>,
    /// Opt-in flag for snapshot scrollback persistence (#69). Defaults
    /// to `false` to keep snapshots small and v2-compatible. Per-pane
    /// `[[pane]] persist_scrollback = true` in `.ezpn.toml` overrides
    /// this when set.
    persist_scrollback: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct KeysSection {
    prefix: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ClipboardSection {
    osc52_set: Option<Osc52SetPolicy>,
    osc52_get: Option<Osc52GetPolicy>,
    osc52_max_bytes: Option<i64>,
    /// `copy_command = ["wl-copy"]` — explicit override for the system
    /// clipboard fallback chain (#92). Empty array == omit the key ==
    /// auto-detect.
    copy_command: Option<Vec<String>>,
    /// `paste_command = ["wl-paste"]` — companion to `copy_command`,
    /// stored for forward-compatibility (paste is out of scope today).
    paste_command: Option<Vec<String>>,
}

// ─── Public entry point ────────────────────────────────────

/// Load config from `~/.config/ezpn/config.toml` (or `$XDG_CONFIG_HOME/ezpn/config.toml`).
///
/// Modern format (v0.12+):
/// ```toml
/// [global]
/// border = "rounded"
/// shell = "/bin/zsh"
/// scrollback = 10000
/// status_bar = true
/// tab_bar = true
///
/// [keys]
/// prefix = "b"
/// ```
///
/// Legacy flat `key = value` format (v0.5.x) is still accepted with a
/// deprecation warning printed to stderr; this fallback will be removed in
/// a future release.
pub fn load_config() -> EzpnConfig {
    let path = match config_path() {
        Some(p) => p,
        None => return EzpnConfig::default(),
    };
    let contents = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return EzpnConfig::default(),
    };
    parse_config(&contents, Some(&path))
}

/// Load and validate the `[[hooks]]` array from `~/.config/ezpn/config.toml`.
/// Returns the parsed hooks plus any structured errors from individual
/// entries; the caller decides whether to treat parse errors as fatal
/// (load-time refusal) or to drop the offending entry. Today we drop and
/// log; the dispatcher's `replace()` is atomic so partial sets are safe.
///
/// This intentionally returns `Vec<Hook>` rather than a `HookExecutor` —
/// the executor is constructed by `server::run` once from the merged set
/// of (global config) ∪ (project `.ezpn.toml`) hooks.
pub fn load_hooks() -> Vec<Hook> {
    let path = match config_path() {
        Some(p) => p,
        None => return Vec::new(),
    };
    let contents = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    parse_hooks(&contents, Some(&path))
}

fn parse_hooks(contents: &str, source: Option<&Path>) -> Vec<Hook> {
    if contents.trim().is_empty() {
        return Vec::new();
    }
    let label = source
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<config>".into());
    let raw: RawConfig = match toml::from_str(contents) {
        Ok(r) => r,
        Err(e) => {
            let (line, col) = error_line_col(&e, contents);
            eprintln!(
                "config: {label}: malformed [[hooks]] at line {line}, column {col}: {msg}",
                msg = e.message()
            );
            return Vec::new();
        }
    };
    let mut out = Vec::with_capacity(raw.hooks.len());
    for (i, raw_hook) in raw.hooks.into_iter().enumerate() {
        match Hook::from_raw(raw_hook) {
            Ok(h) => out.push(h),
            Err(e) => emit_hook_error(&label, i, &e),
        }
    }
    out
}

fn emit_hook_error(label: &str, index: usize, err: &HookParseError) {
    eprintln!("config: {label}: [[hooks]][{index}] rejected: {err} — dropping entry");
}

/// Load the user's keymap, layered on top of the embedded defaults.
///
/// Returns `Err` if the user's `[keymap.<table>]` section contains an
/// invalid binding — the daemon should refuse to start and surface the
/// error verbatim (it includes the offending table + key).
pub fn load_keymap() -> Result<Keymap, KeymapLoadError> {
    let mut km = load_defaults();
    let path = match config_path() {
        Some(p) => p,
        None => return Ok(km),
    };
    let contents = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Ok(km),
    };
    apply_keymap_overrides(&mut km, &contents)?;
    Ok(km)
}

fn apply_keymap_overrides(km: &mut Keymap, contents: &str) -> Result<(), KeymapLoadError> {
    if !has_toml_table_header(contents) {
        return Ok(());
    }
    let raw: RawConfig = match toml::from_str(contents) {
        Ok(r) => r,
        // Soft-fail on top-level TOML errors — the main `parse_config`
        // already surfaced them.
        Err(_) => return Ok(()),
    };
    if let Some(km_tbl) = raw.keymap {
        for warning in apply_keymap_section(km, &km_tbl)? {
            eprintln!("config: {warning}");
        }
    }
    Ok(())
}

/// Parse a config string into an `EzpnConfig`. Public for testing.
fn parse_config(contents: &str, source: Option<&Path>) -> EzpnConfig {
    let mut config = EzpnConfig::default();
    let label = source
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<config>".into());

    // Detect schema. A [section] header => modern; otherwise legacy flat.
    if has_toml_table_header(contents) {
        match toml::from_str::<RawConfig>(contents) {
            Ok(raw) => apply_raw(&mut config, raw, contents, &label),
            Err(e) => {
                let (line, col) = error_line_col(&e, contents);
                eprintln!(
                    "config: malformed TOML in {label} at line {line}, column {col}: {msg}",
                    msg = e.message()
                );
            }
        }
    } else if !contents.trim().is_empty() {
        eprintln!(
            "config: {label} uses the legacy flat `key = value` format; \
             please migrate to the [global] / [keys] schema. \
             Legacy support will be removed in a future release."
        );
        apply_legacy_flat(&mut config, contents, &label);
    }

    config
}

// ─── Modern TOML application ───────────────────────────────

fn apply_raw(config: &mut EzpnConfig, raw: RawConfig, source: &str, label: &str) {
    // Warn about unknown top-level keys. RawConfig only captures `global`
    // and `keys`; we can't tell from RawConfig alone, so re-parse as a flat
    // toml::Table to enumerate top-level keys.
    if let Ok(top) = toml::from_str::<toml::Table>(source) {
        for key in top.keys() {
            if !KNOWN_TOP_LEVEL.contains(&key.as_str()) {
                let (line, col) = locate_key(source, &[key]);
                warn_unknown(label, key, line, col, "top-level");
            }
        }
    }

    if let Some(global_tbl) = raw.global {
        // Detect unknown keys in [global] before strict deserialization.
        for key in global_tbl.keys() {
            if !KNOWN_GLOBAL_KEYS.contains(&key.as_str()) {
                let (line, col) = locate_key(source, &["global", key]);
                warn_unknown(label, key, line, col, "[global]");
            }
        }
        match global_tbl.try_into::<GlobalSection>() {
            Ok(g) => apply_global(config, g, label),
            Err(e) => {
                eprintln!("config: invalid [global] in {label}: {e}");
            }
        }
    }

    if let Some(keys_tbl) = raw.keys {
        for key in keys_tbl.keys() {
            if !KNOWN_KEYS_KEYS.contains(&key.as_str()) {
                let (line, col) = locate_key(source, &["keys", key]);
                warn_unknown(label, key, line, col, "[keys]");
            }
        }
        match keys_tbl.try_into::<KeysSection>() {
            Ok(k) => apply_keys(config, k, label),
            Err(e) => {
                eprintln!("config: invalid [keys] in {label}: {e}");
            }
        }
    }

    if let Some(clip_tbl) = raw.clipboard {
        for key in clip_tbl.keys() {
            if !KNOWN_CLIPBOARD_KEYS.contains(&key.as_str()) {
                let (line, col) = locate_key(source, &["clipboard", key]);
                warn_unknown(label, key, line, col, "[clipboard]");
            }
        }
        match clip_tbl.try_into::<ClipboardSection>() {
            Ok(c) => apply_clipboard(config, c, label),
            Err(e) => {
                eprintln!("config: invalid [clipboard] in {label}: {e}");
            }
        }
    }

    if let Some(theme_tbl) = raw.theme {
        apply_theme(config, theme_tbl, source, label);
    }

    if let Some(sb_tbl) = raw.status_bar {
        for key in sb_tbl.keys() {
            if !KNOWN_STATUS_BAR_KEYS.contains(&key.as_str()) {
                let (line, col) = locate_key(source, &["status_bar", key]);
                warn_unknown(label, key, line, col, "[status_bar]");
            }
        }
        apply_status_bar(config, sb_tbl, label);
    }
}

// ─── [theme] (#85) ────────────────────────────────────────

fn apply_theme(config: &mut EzpnConfig, tbl: toml::Table, source: &str, label: &str) {
    // Two valid shapes:
    //   1. `[theme] name = "ezpn-dark"` — load a built-in by name.
    //   2. Full inline palette — every field present, parsed verbatim.
    let only_name = tbl.len() == 1 && tbl.contains_key("name");
    let has_name_only_no_fields =
        tbl.contains_key("name") && tbl.keys().all(|k| matches!(k.as_str(), "name"));
    if only_name || has_name_only_no_fields {
        let raw_name = match tbl.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => {
                eprintln!("config: {label}: [theme] name must be a string, ignoring");
                return;
            }
        };
        match Theme::builtin(&raw_name) {
            Some(t) => config.theme = t,
            None => eprintln!(
                "config: {label}: unknown theme '{raw_name}' — built-ins are {known:?}",
                known = Theme::builtin_names()
            ),
        }
        return;
    }

    // Inline palette path: validate keys, then re-serialize as a `[theme]`
    // doc and hand to the parser.
    for key in tbl.keys() {
        if !KNOWN_THEME_KEYS.contains(&key.as_str()) {
            let (line, col) = locate_key(source, &["theme", key]);
            warn_unknown(label, key, line, col, "[theme]");
        }
    }
    let mut wrapped = toml::Table::new();
    wrapped.insert("theme".into(), toml::Value::Table(tbl));
    let serialized = match toml::to_string(&wrapped) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("config: {label}: failed to re-encode [theme]: {e}");
            return;
        }
    };
    match Theme::from_toml(&serialized) {
        Ok(t) => config.theme = t,
        Err(e) => eprintln!("config: {label}: [theme] rejected: {e}"),
    }
}

// ─── [status_bar] (#87) ───────────────────────────────────

fn apply_status_bar(config: &mut EzpnConfig, tbl: toml::Table, label: &str) {
    let mut sb = StatusBarConfig::default();
    if let Some(v) = tbl.get("left") {
        match value_as_string_array(v) {
            Ok(list) => sb.left = list,
            Err(why) => {
                eprintln!("config: {label}: [status_bar] left invalid ({why}), ignoring");
            }
        }
    }
    if let Some(v) = tbl.get("right") {
        match value_as_string_array(v) {
            Ok(list) => sb.right = list,
            Err(why) => {
                eprintln!("config: {label}: [status_bar] right invalid ({why}), ignoring");
            }
        }
    }
    if let Some(toml::Value::Table(seg_tbl)) = tbl.get("segments") {
        for (name, raw) in seg_tbl {
            match parse_segment(name, raw) {
                Ok(seg) => sb.segments.push(seg),
                Err(why) => eprintln!(
                    "config: {label}: [status_bar.segments.{name}] rejected: {why} — dropping"
                ),
            }
        }
    } else if let Some(other) = tbl.get("segments") {
        eprintln!(
            "config: {label}: [status_bar].segments must be a table, got {} — ignoring",
            other.type_str()
        );
    }
    config.status_bar = sb;
}

fn value_as_string_array(v: &toml::Value) -> Result<Vec<String>, String> {
    let arr = v
        .as_array()
        .ok_or_else(|| format!("expected array, got {}", v.type_str()))?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let s = item
            .as_str()
            .ok_or_else(|| format!("entry {i} must be a string, got {}", item.type_str()))?;
        out.push(strip_braces(s).to_string());
    }
    Ok(out)
}

/// Accept both `"{session}"` and `"session"` forms; the curly braces are
/// purely a visual hint in TOML and stripped here so segment lookup is
/// canonical.
fn strip_braces(s: &str) -> &str {
    let trimmed = s.trim();
    if let Some(inner) = trimmed.strip_prefix('{').and_then(|x| x.strip_suffix('}')) {
        inner.trim()
    } else {
        trimmed
    }
}

fn parse_segment(name: &str, raw: &toml::Value) -> Result<StatusBarSegment, String> {
    let tbl = raw
        .as_table()
        .ok_or_else(|| format!("expected table, got {}", raw.type_str()))?;
    let kind_str = tbl
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing required `type` field".to_string())?;
    let kind = match kind_str {
        "builtin" => {
            let inner = tbl
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "builtin segment requires `name`".to_string())?;
            SegmentKind::Builtin(inner.to_string())
        }
        "literal" => {
            let text = tbl
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "literal segment requires `text`".to_string())?;
            SegmentKind::Literal(text.to_string())
        }
        "key_hints" => {
            let mode = match tbl.get("mode").and_then(|v| v.as_str()) {
                Some(s) => {
                    HintMode::from_str(s).ok_or_else(|| format!("unknown key_hints mode '{s}'"))?
                }
                None => HintMode::Auto,
            };
            let max_width = match tbl.get("max_width") {
                Some(toml::Value::Integer(n)) => {
                    if *n < 0 || *n > u16::MAX as i64 {
                        return Err(format!("max_width out of range: {n}"));
                    }
                    *n as u16
                }
                Some(other) => {
                    return Err(format!(
                        "max_width must be a non-negative integer, got {}",
                        other.type_str()
                    ))
                }
                None => 60,
            };
            let mut keys = Vec::new();
            if let Some(toml::Value::Array(arr)) = tbl.get("keys") {
                for (i, entry) in arr.iter().enumerate() {
                    let entry_tbl = entry
                        .as_table()
                        .ok_or_else(|| format!("keys[{i}] must be a table"))?;
                    let key = entry_tbl
                        .get("key")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| format!("keys[{i}] missing `key`"))?
                        .to_string();
                    let label = entry_tbl
                        .get("label")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| format!("keys[{i}] missing `label`"))?
                        .to_string();
                    keys.push(KeyHint { key, label });
                }
            }
            SegmentKind::KeyHints {
                mode,
                max_width,
                keys,
            }
        }
        other => return Err(format!("unknown segment type '{other}'")),
    };
    Ok(StatusBarSegment {
        name: name.to_string(),
        kind,
    })
}

fn apply_clipboard(config: &mut EzpnConfig, c: ClipboardSection, label: &str) {
    if let Some(s) = c.osc52_set {
        config.clipboard.set = s;
    }
    if let Some(g) = c.osc52_get {
        config.clipboard.get = g;
    }
    if let Some(n) = c.osc52_max_bytes {
        if n <= 0 {
            eprintln!("config: {label}: clipboard.osc52_max_bytes must be positive, ignoring");
        } else {
            // Cap at 16 MiB even if the user asks for more — defence in depth.
            const HARD_CAP: i64 = 16 * 1024 * 1024;
            config.clipboard.max_bytes = n.min(HARD_CAP) as usize;
        }
    }
    if let Some(argv) = c.copy_command {
        if argv.iter().any(|s| s.is_empty()) {
            eprintln!("config: {label}: clipboard.copy_command contains an empty entry, ignoring");
        } else {
            // Empty array == auto-detect; non-empty == verbatim override
            // (no PATH lookup — trust the user).
            config.clipboard_copy_command = argv;
        }
    }
    if let Some(argv) = c.paste_command {
        if argv.iter().any(|s| s.is_empty()) {
            eprintln!("config: {label}: clipboard.paste_command contains an empty entry, ignoring");
        } else {
            config.clipboard_paste_command = argv;
        }
    }
}

fn apply_global(config: &mut EzpnConfig, g: GlobalSection, label: &str) {
    if let Some(b) = g.border {
        match BorderStyle::from_str(&b) {
            Some(style) => config.border = style,
            None => eprintln!("config: {label}: unknown border style '{b}', using default"),
        }
    }
    if let Some(s) = g.shell {
        config.shell = s;
    }
    if let Some(n) = g.scrollback {
        if n < 0 {
            eprintln!("config: {label}: scrollback must be non-negative, ignoring");
        } else {
            config.scrollback = (n as usize).min(SCROLLBACK_MAX);
        }
    }
    if let Some(v) = g.scrollback_bytes {
        match parse_scrollback_bytes(&v) {
            Ok(n) => config.scrollback_bytes = n.min(SCROLLBACK_BYTES_MAX),
            Err(msg) => {
                eprintln!("config: {label}: scrollback_bytes invalid ({msg}), ignoring");
            }
        }
    }
    if let Some(s) = g.scrollback_eviction {
        match ScrollbackEviction::from_str(&s) {
            Some(e) => config.scrollback_eviction = e,
            None => eprintln!(
                "config: {label}: scrollback_eviction must be 'oldest_line' or 'largest_line', \
                 got '{s}' — ignoring"
            ),
        }
    }
    if let Some(b) = g.status_bar {
        config.show_status_bar = b;
    }
    if let Some(b) = g.tab_bar {
        config.show_tab_bar = b;
    }
    if let Some(b) = g.persist_scrollback {
        config.persist_scrollback = b;
    }
}

fn apply_keys(config: &mut EzpnConfig, k: KeysSection, label: &str) {
    if let Some(p) = k.prefix {
        let lower = p.to_lowercase();
        match lower.chars().next() {
            Some(c) if c.is_ascii_lowercase() => config.prefix_key = c,
            _ => eprintln!("config: {label}: keys.prefix must be an ASCII letter, ignoring"),
        }
    }
}

// ─── Legacy flat-format fallback (v0.5.x) ──────────────────

fn apply_legacy_flat(config: &mut EzpnConfig, contents: &str, label: &str) {
    for (idx, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = strip_quotes(value.trim());
        match key {
            "border" => {
                if let Some(style) = BorderStyle::from_str(value) {
                    config.border = style;
                }
            }
            "shell" => config.shell = value.to_string(),
            "scrollback" => {
                if let Ok(n) = value.parse::<usize>() {
                    config.scrollback = n.min(SCROLLBACK_MAX);
                }
            }
            "status_bar" => config.show_status_bar = value == "true",
            "tab_bar" => config.show_tab_bar = value == "true",
            "prefix" => {
                let lower = value.to_lowercase();
                if let Some(c) = lower.chars().next() {
                    if c.is_ascii_lowercase() {
                        config.prefix_key = c;
                    }
                }
            }
            _ => {
                eprintln!(
                    "config: {label}: unknown key '{key}' at line {n} \
                     (legacy format) — ignored",
                    n = idx + 1
                );
            }
        }
    }
}

/// Parse a `scrollback_bytes` value. Accepts a TOML integer (raw bytes)
/// or a humansize string with optional suffix `K`/`KB`/`KiB`/`M`/`MB`/`MiB`/
/// `G`/`GB`/`GiB`. Both decimal and IEC binary suffixes are mapped to powers
/// of 1024 (`32M` == `32 MiB`) for parity with tmux's mental model.
///
/// Returns `Ok(0)` to disable the cap; `Err` for negatives, fractional
/// suffixes, or unparseable strings.
pub fn parse_scrollback_bytes(value: &toml::Value) -> Result<usize, String> {
    match value {
        toml::Value::Integer(n) => {
            if *n < 0 {
                Err("must be non-negative".into())
            } else {
                Ok(*n as usize)
            }
        }
        toml::Value::String(s) => parse_byte_size_str(s),
        other => Err(format!(
            "expected integer or string, got {}",
            other.type_str()
        )),
    }
}

fn parse_byte_size_str(s: &str) -> Result<usize, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("empty string".into());
    }
    // Split numeric prefix from suffix.
    let split = trimmed
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
        .unwrap_or(trimmed.len());
    let (num_part, suffix) = trimmed.split_at(split);
    let num_part = num_part.trim();
    let suffix = suffix.trim();

    if num_part.is_empty() {
        return Err(format!("missing numeric prefix in '{s}'"));
    }
    if num_part.starts_with('-') {
        return Err("must be non-negative".into());
    }

    let multiplier: u64 = match suffix.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        other => return Err(format!("unknown suffix '{other}'")),
    };

    // Allow fractional values for human convenience: "1.5G".
    if num_part.contains('.') {
        let v: f64 = num_part
            .parse()
            .map_err(|_| format!("cannot parse number '{num_part}'"))?;
        if v < 0.0 {
            return Err("must be non-negative".into());
        }
        let bytes = (v * multiplier as f64).round();
        if bytes < 0.0 {
            return Err("must be non-negative".into());
        }
        Ok(bytes as usize)
    } else {
        let v: u64 = num_part
            .parse()
            .map_err(|_| format!("cannot parse integer '{num_part}'"))?;
        Ok(v.saturating_mul(multiplier) as usize)
    }
}

fn strip_quotes(value: &str) -> &str {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let first = bytes[0];
        let last = bytes[value.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}

// ─── Helpers ───────────────────────────────────────────────

fn warn_unknown(label: &str, key: &str, line: usize, col: usize, scope: &str) {
    eprintln!(
        "config: {label}: unknown {scope} key '{key}' at line {line}, column {col} — ignored"
    );
}

/// Quick check: does the document contain at least one `[section]` header?
fn has_toml_table_header(contents: &str) -> bool {
    contents.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with('[') && !t.starts_with("[[")
    })
}

/// Best-effort line/column for an unknown key. `path` is the dotted key
/// path: e.g. ["global", "scrollbac"] looks for `scrollbac =` inside the
/// first `[global]` section.
fn locate_key(source: &str, path: &[&str]) -> (usize, usize) {
    if path.is_empty() {
        return (1, 1);
    }
    let (section, leaf) = if path.len() == 1 {
        (None, path[0])
    } else {
        (Some(path[0]), path[path.len() - 1])
    };

    let mut current_section: Option<String> = None;
    for (idx, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix('[') {
            if let Some(end) = rest.find(']') {
                let name = rest[..end].trim().to_string();
                current_section = Some(name);
                continue;
            }
        }
        let same_section = match (section, current_section.as_deref()) {
            (None, None) => true,
            (Some(s), Some(cur)) => s == cur,
            _ => false,
        };
        if !same_section {
            continue;
        }
        // Match `leaf =` (allowing whitespace and bare/quoted keys).
        let leading_ws = line.len() - trimmed.len();
        let candidates = [leaf.to_string(), format!("\"{leaf}\""), format!("'{leaf}'")];
        for cand in &candidates {
            if trimmed.starts_with(cand.as_str()) {
                let after = &trimmed[cand.len()..];
                if after.trim_start().starts_with('=') {
                    return (idx + 1, leading_ws + 1);
                }
            }
        }
    }
    (1, 1)
}

/// Convert a `toml::de::Error` byte span into (line, column), 1-indexed.
fn error_line_col(err: &toml::de::Error, source: &str) -> (usize, usize) {
    let span = match err.span() {
        Some(s) => s,
        None => return (1, 1),
    };
    let offset = span.start.min(source.len());
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn config_path() -> Option<PathBuf> {
    let dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let mut home = dirs_fallback();
            home.push(".config");
            home
        });
    let path = dir.join("ezpn").join("config.toml");
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

fn dirs_fallback() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

// ─── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse with no source path; warnings still go to stderr but tests
    /// only assert on the resulting `EzpnConfig`.
    fn parse(s: &str) -> EzpnConfig {
        parse_config(s, None)
    }

    // ─── strip_quotes ──────────────────────────────────────

    #[test]
    fn strip_quotes_handles_bare_and_quoted() {
        assert_eq!(strip_quotes("rounded"), "rounded");
        assert_eq!(strip_quotes("\"rounded\""), "rounded");
        assert_eq!(strip_quotes("'/bin/zsh'"), "/bin/zsh");
        assert_eq!(strip_quotes(""), "");
        assert_eq!(strip_quotes("\""), "\"");
    }

    // ─── Modern schema ─────────────────────────────────────

    #[test]
    fn parses_full_modern_schema() {
        let src = r#"
            [global]
            border = "double"
            shell = "/bin/zsh"
            scrollback = 5000
            status_bar = false
            tab_bar = false

            [keys]
            prefix = "a"
        "#;
        let c = parse(src);
        assert_eq!(c.border, BorderStyle::Double);
        assert_eq!(c.shell, "/bin/zsh");
        assert_eq!(c.scrollback, 5000);
        assert!(!c.show_status_bar);
        assert!(!c.show_tab_bar);
        assert_eq!(c.prefix_key, 'a');
    }

    #[test]
    fn nested_keys_table_parsed_correctly() {
        let src = r#"
            [keys]
            prefix = "x"
        "#;
        let c = parse(src);
        assert_eq!(c.prefix_key, 'x');
        // Other fields stay at defaults.
        let d = EzpnConfig::default();
        assert_eq!(c.border, d.border);
        assert_eq!(c.scrollback, d.scrollback);
    }

    #[test]
    fn scrollback_clamped_to_max() {
        let src = "[global]\nscrollback = 999999\n";
        let c = parse(src);
        assert_eq!(c.scrollback, SCROLLBACK_MAX);
    }

    #[test]
    fn negative_scrollback_falls_back_to_default() {
        let src = "[global]\nscrollback = -10\n";
        let c = parse(src);
        assert_eq!(c.scrollback, EzpnConfig::default().scrollback);
    }

    #[test]
    fn unknown_global_key_uses_default_and_does_not_panic() {
        // Typo'd `prefex` at top level (legacy users) and `bordr` inside
        // [global] — both should be ignored, defaults preserved.
        let src = r#"
            [global]
            bordr = "double"
            border = "heavy"
        "#;
        let c = parse(src);
        // Known key still applied.
        assert_eq!(c.border, BorderStyle::Heavy);
    }

    #[test]
    fn unknown_top_level_section_ignored() {
        let src = r#"
            [global]
            shell = "/bin/fish"

            [mystery]
            foo = 1
        "#;
        let c = parse(src);
        assert_eq!(c.shell, "/bin/fish");
    }

    #[test]
    fn malformed_toml_returns_defaults() {
        // Unclosed string in a sectioned doc — must not panic, must keep defaults.
        let src = "[global]\nshell = \"/bin/zsh\nscrollback = 1000\n";
        let c = parse(src);
        let d = EzpnConfig::default();
        assert_eq!(c.shell, d.shell);
        assert_eq!(c.scrollback, d.scrollback);
    }

    #[test]
    fn malformed_toml_error_has_line_and_column() {
        // Verify the helper that converts byte offset to line/col.
        let src = "[global]\nshell = =\n";
        let err = toml::from_str::<RawConfig>(src).unwrap_err();
        let (line, col) = error_line_col(&err, src);
        assert!(line >= 1, "line must be 1-indexed, got {line}");
        assert!(col >= 1, "col must be 1-indexed, got {col}");
        // The bad `=` is on line 2.
        assert_eq!(line, 2, "expected error on line 2, got {line}");
    }

    #[test]
    fn locate_key_finds_unknown_global_key() {
        let src = "[global]\nborder = \"rounded\"\nbogus = 1\n";
        let (line, col) = locate_key(src, &["global", "bogus"]);
        assert_eq!(line, 3);
        assert_eq!(col, 1);
    }

    #[test]
    fn locate_key_handles_indented_keys() {
        let src = "[global]\n    bogus = 1\n";
        let (line, col) = locate_key(src, &["global", "bogus"]);
        assert_eq!(line, 2);
        assert_eq!(col, 5);
    }

    // ─── Legacy v0.5 flat format ───────────────────────────

    #[test]
    fn legacy_flat_parses_v05_format_identically() {
        // The v0.5.0 config example from the issue.
        let src = "border = rounded\n\
                   shell = /bin/zsh\n\
                   scrollback = 10000\n\
                   status_bar = true\n\
                   tab_bar = true\n\
                   prefix = b\n";
        let c = parse(src);
        let d = EzpnConfig::default();
        assert_eq!(c.border, BorderStyle::Rounded);
        assert_eq!(c.shell, "/bin/zsh");
        assert_eq!(c.scrollback, 10_000);
        assert!(c.show_status_bar);
        assert!(c.show_tab_bar);
        assert_eq!(c.prefix_key, 'b');
        // Sanity: defaults match the values just set.
        assert_eq!(c.border, d.border);
        assert_eq!(c.prefix_key, d.prefix_key);
    }

    #[test]
    fn legacy_flat_with_quoted_values() {
        let src = "border = \"heavy\"\nshell = '/bin/fish'\n";
        let c = parse(src);
        assert_eq!(c.border, BorderStyle::Heavy);
        assert_eq!(c.shell, "/bin/fish");
    }

    #[test]
    fn legacy_flat_unknown_key_ignored() {
        let src = "prefex = b\nborder = double\n";
        let c = parse(src);
        // `prefex` typo ignored; default prefix retained, border applied.
        assert_eq!(c.prefix_key, EzpnConfig::default().prefix_key);
        assert_eq!(c.border, BorderStyle::Double);
    }

    // ─── Misc ──────────────────────────────────────────────

    #[test]
    fn empty_input_returns_defaults() {
        let c = parse("");
        let d = EzpnConfig::default();
        assert_eq!(c.border, d.border);
        assert_eq!(c.scrollback, d.scrollback);
        assert_eq!(c.prefix_key, d.prefix_key);
    }

    #[test]
    fn comments_only_returns_defaults() {
        let c = parse("# just a comment\n# another\n");
        let d = EzpnConfig::default();
        assert_eq!(c.scrollback, d.scrollback);
    }

    // ─── scrollback_bytes / scrollback_eviction ────────────

    #[test]
    fn parses_scrollback_bytes_string_suffixes() {
        assert_eq!(parse_byte_size_str("32M").unwrap(), 32 * 1024 * 1024);
        assert_eq!(parse_byte_size_str("512K").unwrap(), 512 * 1024);
        assert_eq!(parse_byte_size_str("2G").unwrap(), 2 * 1024 * 1024 * 1024);
        assert_eq!(parse_byte_size_str("0").unwrap(), 0);
        assert_eq!(parse_byte_size_str("1024B").unwrap(), 1024);
        // Lowercase + IEC variants.
        assert_eq!(parse_byte_size_str("4mib").unwrap(), 4 * 1024 * 1024);
    }

    #[test]
    fn rejects_negative_or_garbage_scrollback_bytes() {
        assert!(parse_byte_size_str("-1").is_err());
        assert!(parse_byte_size_str("-32M").is_err());
        assert!(parse_byte_size_str("garbage").is_err());
        assert!(parse_byte_size_str("12X").is_err());
        assert!(parse_byte_size_str("").is_err());
    }

    #[test]
    fn parses_fractional_scrollback_bytes() {
        assert_eq!(
            parse_byte_size_str("1.5G").unwrap(),
            (1.5 * 1024.0 * 1024.0 * 1024.0) as usize
        );
    }

    #[test]
    fn applies_scrollback_bytes_string_from_toml() {
        let src = "[global]\nscrollback_bytes = \"64M\"\n";
        let c = parse(src);
        assert_eq!(c.scrollback_bytes, 64 * 1024 * 1024);
    }

    #[test]
    fn applies_scrollback_bytes_integer_from_toml() {
        let src = "[global]\nscrollback_bytes = 1048576\n";
        let c = parse(src);
        assert_eq!(c.scrollback_bytes, 1024 * 1024);
    }

    #[test]
    fn scrollback_bytes_zero_disables_cap() {
        let src = "[global]\nscrollback_bytes = 0\n";
        let c = parse(src);
        assert_eq!(c.scrollback_bytes, 0);
    }

    #[test]
    fn scrollback_bytes_default_when_unset() {
        let src = "[global]\nshell = \"/bin/zsh\"\n";
        let c = parse(src);
        assert_eq!(c.scrollback_bytes, DEFAULT_SCROLLBACK_BYTES);
    }

    #[test]
    fn scrollback_bytes_clamped_to_ceiling() {
        // 8 GiB > 4 GiB ceiling.
        let src = "[global]\nscrollback_bytes = \"8G\"\n";
        let c = parse(src);
        assert_eq!(c.scrollback_bytes, SCROLLBACK_BYTES_MAX);
    }

    #[test]
    fn applies_scrollback_eviction_largest_line() {
        let src = "[global]\nscrollback_eviction = \"largest_line\"\n";
        let c = parse(src);
        assert_eq!(c.scrollback_eviction, ScrollbackEviction::LargestLine);
    }

    #[test]
    fn unknown_scrollback_eviction_falls_back_to_default() {
        let src = "[global]\nscrollback_eviction = \"random\"\n";
        let c = parse(src);
        assert_eq!(c.scrollback_eviction, ScrollbackEviction::OldestLine);
    }

    // ─── persist_scrollback (#69) ──────────────────────────────

    #[test]
    fn persist_scrollback_default_is_false() {
        let c = parse("");
        assert!(!c.persist_scrollback);
    }

    #[test]
    fn persist_scrollback_can_be_enabled_globally() {
        let src = "[global]\npersist_scrollback = true\n";
        let c = parse(src);
        assert!(c.persist_scrollback);
    }

    #[test]
    fn persist_scrollback_explicit_false_overrides_default() {
        let src = "[global]\npersist_scrollback = false\n";
        let c = parse(src);
        assert!(!c.persist_scrollback);
    }

    // ─── [clipboard] copy_command / paste_command (#92) ──────────

    #[test]
    fn clipboard_copy_command_override_parsed() {
        let src = r#"
            [clipboard]
            copy_command = ["wl-copy"]
        "#;
        let c = parse(src);
        assert_eq!(c.clipboard_copy_command, vec!["wl-copy".to_string()]);
    }

    #[test]
    fn clipboard_copy_command_with_args_parsed() {
        let src = r#"
            [clipboard]
            copy_command = ["xclip", "-selection", "clipboard"]
        "#;
        let c = parse(src);
        assert_eq!(
            c.clipboard_copy_command,
            vec![
                "xclip".to_string(),
                "-selection".to_string(),
                "clipboard".to_string(),
            ]
        );
    }

    #[test]
    fn clipboard_copy_command_empty_means_auto_detect() {
        let src = r#"
            [clipboard]
            copy_command = []
        "#;
        let c = parse(src);
        assert!(c.clipboard_copy_command.is_empty());
    }

    #[test]
    fn clipboard_copy_command_with_empty_entry_rejected() {
        let src = r#"
            [clipboard]
            copy_command = ["", "--clip"]
        "#;
        let c = parse(src);
        assert!(c.clipboard_copy_command.is_empty());
    }

    #[test]
    fn clipboard_paste_command_override_parsed() {
        let src = r#"
            [clipboard]
            paste_command = ["wl-paste", "-n"]
        "#;
        let c = parse(src);
        assert_eq!(
            c.clipboard_paste_command,
            vec!["wl-paste".to_string(), "-n".to_string()]
        );
    }

    #[test]
    fn has_toml_table_header_detects_sections() {
        assert!(has_toml_table_header("[global]\nx = 1\n"));
        assert!(!has_toml_table_header("x = 1\n"));
        assert!(!has_toml_table_header("# [global]\nx = 1\n"));
        // Array-of-tables is not a plain section header for our purposes.
        assert!(!has_toml_table_header("[[arr]]\nx = 1\n"));
    }

    // ─── [[hooks]] (#83) ───────────────────────────────────────

    #[test]
    fn parse_hooks_extracts_well_formed_entries() {
        let src = r#"
            [[hooks]]
            event = "after_pane_exit"
            exec = ["notify-send", "exit", "${pane.exit_code}"]

            [[hooks]]
            event = "on_cwd_change"
            exec = ["sh", "-c", "echo cwd=${pane.cwd}"]
            when = "${pane.exit_code} != 0"
        "#;
        let hooks = parse_hooks(src, None);
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].event, crate::hooks::HookEvent::AfterPaneExit);
        assert_eq!(hooks[0].exec[0], "notify-send");
        assert!(hooks[1].when.is_some());
    }

    #[test]
    fn parse_hooks_drops_invalid_entries_without_panicking() {
        let src = r#"
            [[hooks]]
            event = "after_pane_exit"
            exec = ["true"]

            [[hooks]]
            event = "after_typo"
            exec = ["true"]
        "#;
        let hooks = parse_hooks(src, None);
        assert_eq!(hooks.len(), 1, "bad entry must be dropped, good kept");
    }

    #[test]
    fn parse_hooks_returns_empty_when_no_section() {
        let src = "[global]\nshell = \"/bin/sh\"\n";
        assert!(parse_hooks(src, None).is_empty());
    }

    // ─── [keymap.<table>] (#84) ────────────────────────────────

    #[test]
    fn apply_keymap_overrides_layers_user_on_defaults() {
        let mut km = load_defaults();
        let before_prefix = km.len(crate::keymap::KeymapTable::Prefix);
        let src = r#"
            [keymap.prefix]
            "z" = "kill-pane"
        "#;
        apply_keymap_overrides(&mut km, src).unwrap();
        // One additional binding on top of defaults.
        assert_eq!(
            km.len(crate::keymap::KeymapTable::Prefix),
            before_prefix + 1
        );
    }

    #[test]
    fn apply_keymap_overrides_propagates_load_errors() {
        let mut km = load_defaults();
        let src = r#"
            [keymap.prefix]
            "z" = "frobnicate"
        "#;
        let err = apply_keymap_overrides(&mut km, src).unwrap_err();
        assert_eq!(err.table, "prefix");
        assert!(err.message.contains("unknown action"), "{err}");
    }

    // ─── [theme] (#85) ─────────────────────────────────────────

    #[test]
    fn theme_name_loads_builtin() {
        let src = "[theme]\nname = \"nord\"\n";
        let c = parse(src);
        assert_eq!(c.theme.name, "nord");
    }

    #[test]
    fn theme_unknown_name_keeps_default() {
        let src = "[theme]\nname = \"hot-pink-2099\"\n";
        let c = parse(src);
        assert_eq!(c.theme.name, EzpnConfig::default().theme.name);
    }

    #[test]
    fn theme_inline_palette_overrides_default() {
        let src = "[theme]\n\
                   name = \"custom\"\n\
                   fg = \"#ffffff\"\n\
                   bg = \"#000000\"\n\
                   border = \"#888888\"\n\
                   border_active = \"#aabbcc\"\n\
                   status_bg = \"#111111\"\n\
                   status_fg = \"#eeeeee\"\n\
                   tab_active_bg = \"#222222\"\n\
                   tab_active_fg = \"#ffffff\"\n\
                   tab_inactive_fg = \"#777777\"\n\
                   selection = \"#333333\"\n\
                   search_match = \"#ffd700\"\n\
                   broadcast_indicator = \"#ff0000\"\n\
                   copy_mode_indicator = \"#00ff00\"\n";
        let c = parse(src);
        assert_eq!(c.theme.name, "custom");
        assert_eq!(c.theme.fg.r, 0xff);
    }

    #[test]
    fn theme_inline_bad_hex_keeps_default() {
        let src = "[theme]\n\
                   name = \"broken\"\n\
                   fg = \"not-a-hex\"\n\
                   bg = \"#000000\"\n\
                   border = \"#888888\"\n\
                   border_active = \"#aabbcc\"\n\
                   status_bg = \"#111111\"\n\
                   status_fg = \"#eeeeee\"\n\
                   tab_active_bg = \"#222222\"\n\
                   tab_active_fg = \"#ffffff\"\n\
                   tab_inactive_fg = \"#777777\"\n\
                   selection = \"#333333\"\n\
                   search_match = \"#ffd700\"\n\
                   broadcast_indicator = \"#ff0000\"\n\
                   copy_mode_indicator = \"#00ff00\"\n";
        let c = parse(src);
        assert_eq!(c.theme.name, EzpnConfig::default().theme.name);
    }

    // ─── [status_bar] (#87) ────────────────────────────────────

    #[test]
    fn status_bar_left_right_strip_braces() {
        let src = "[status_bar]\n\
                   left  = [\"{session}\", \"{tab_count}\", \"{mode}\"]\n\
                   right = [\"{key_hints}\", \"{time}\"]\n";
        let c = parse(src);
        assert_eq!(c.status_bar.left, vec!["session", "tab_count", "mode"]);
        assert_eq!(c.status_bar.right, vec!["key_hints", "time"]);
    }

    #[test]
    fn status_bar_key_hints_segment_parsed() {
        let src = "[status_bar]\n\
                   left = []\n\
                   right = [\"{key_hints}\"]\n\
                   \n\
                   [status_bar.segments.key_hints]\n\
                   type = \"key_hints\"\n\
                   mode = \"prefix\"\n\
                   max_width = 60\n\
                   keys = [\n\
                       { key = \"C-b d\", label = \"detach\" },\n\
                       { key = \"C-b c\", label = \"new tab\" },\n\
                   ]\n";
        let c = parse(src);
        let seg = c
            .status_bar
            .segment("key_hints")
            .expect("key_hints segment must be registered");
        match &seg.kind {
            SegmentKind::KeyHints {
                mode,
                max_width,
                keys,
            } => {
                assert_eq!(*mode, HintMode::Prefix);
                assert_eq!(*max_width, 60);
                assert_eq!(keys.len(), 2);
                assert_eq!(keys[0].key, "C-b d");
                assert_eq!(keys[0].label, "detach");
            }
            other => panic!("expected KeyHints, got {other:?}"),
        }
    }

    #[test]
    fn status_bar_unknown_segment_type_dropped() {
        let src = "[status_bar]\n\
                   left = []\n\
                   right = [\"{bogus}\"]\n\
                   \n\
                   [status_bar.segments.bogus]\n\
                   type = \"frobnicate\"\n";
        let c = parse(src);
        assert!(c.status_bar.segment("bogus").is_none());
    }

    #[test]
    fn status_bar_literal_segment_parsed() {
        let src = "[status_bar]\n\
                   left = [\"{brand}\"]\n\
                   right = []\n\
                   \n\
                   [status_bar.segments.brand]\n\
                   type = \"literal\"\n\
                   text = \"ezpn>\"\n";
        let c = parse(src);
        let seg = c.status_bar.segment("brand").expect("brand segment");
        match &seg.kind {
            SegmentKind::Literal(s) => assert_eq!(s, "ezpn>"),
            other => panic!("expected Literal, got {other:?}"),
        }
    }

    #[test]
    fn status_bar_missing_returns_default_empty() {
        let c = parse("[global]\nshell = \"/bin/sh\"\n");
        assert!(c.status_bar.left.is_empty());
        assert!(c.status_bar.right.is_empty());
        assert!(c.status_bar.segments.is_empty());
    }
}
