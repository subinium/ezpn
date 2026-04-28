use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use crossterm::{cursor, queue, style::*};

use crate::config::{self, EzpnConfig};
use crate::render::BorderStyle;

// ─── Layout constants ──────────────────────────────────

const W: u16 = 52; // panel width
const H: u16 = 20; // panel height
const PAD: u16 = 4; // left/right inner padding

// Item Y offsets (from panel top)
const Y_TITLE: u16 = 1;
const Y_HINT: u16 = 2;
const Y_SEC1: u16 = 4; // "BORDER STYLE"
const Y_I0: u16 = 5; // Single
const Y_I1: u16 = 6; // Rounded
const Y_I2: u16 = 7; // Heavy
const Y_I3: u16 = 8; // Double
const Y_I4: u16 = 9; // None
const Y_DIV1: u16 = 10;
const Y_SEC2: u16 = 11; // "DISPLAY"
const Y_I5: u16 = 12; // Status Bar
const Y_I6: u16 = 13; // Tab Bar
const Y_I7: u16 = 14; // Broadcast
const Y_DIV2: u16 = 15;
const Y_I8: u16 = 16; // Close

const ITEM_Y: [u16; 9] = [Y_I0, Y_I1, Y_I2, Y_I3, Y_I4, Y_I5, Y_I6, Y_I7, Y_I8];
const ITEM_COUNT: usize = 9;

// ─── Colors ────────────────────────────────────────────

const BG: Color = Color::Rgb {
    r: 16,
    g: 18,
    b: 24,
};
const FOCUS_BG: Color = Color::Rgb {
    r: 26,
    g: 32,
    b: 44,
};
const SEC_FG: Color = Color::Rgb {
    r: 75,
    g: 90,
    b: 110,
};
const LBL_FG: Color = Color::Rgb {
    r: 190,
    g: 200,
    b: 212,
};
const DIM_FG: Color = Color::Rgb {
    r: 90,
    g: 98,
    b: 110,
};
const ACCENT: Color = Color::Rgb {
    r: 102,
    g: 217,
    b: 239,
};
const DIV_FG: Color = Color::Rgb {
    r: 36,
    g: 42,
    b: 52,
};
const WARN_FG: Color = Color::Rgb {
    r: 255,
    g: 110,
    b: 110,
};

// ─── Item indices ──────────────────────────────────────

const I_SINGLE: usize = 0;
const I_ROUNDED: usize = 1;
const I_HEAVY: usize = 2;
const I_DOUBLE: usize = 3;
const I_NONE: usize = 4;
const I_STATUS: usize = 5;
const I_TAB_BAR: usize = 6;
const I_BROADCAST: usize = 7;
const I_CLOSE: usize = 8;

// ─── State ─────────────────────────────────────────────

pub struct Settings {
    pub visible: bool,
    pub border_style: BorderStyle,
    pub show_status_bar: bool,
    pub show_tab_bar: bool,
    focused: usize,
    /// Live snapshot of the on-disk config, kept around so hot-reload can
    /// diff non-reloadable fields and emit warnings. Populated lazily by
    /// `bind_runtime` — see `RuntimeSettings` below.
    runtime: Option<RuntimeSettings>,
    /// Set by the prefix-mode `r` handler; consumed by the main loop's
    /// signal-polling block, which runs the actual reload alongside SIGHUP.
    pub reload_request: bool,
    /// Set by `reload_config` on success when the new state differs from the
    /// previous one in a way that requires a full re-render (border style,
    /// status/tab-bar visibility). Consumed once per frame by the main loop.
    pub reload_dirty: bool,
    /// Transient status-bar overlay (success / failure flash). Owned here
    /// rather than on RuntimeSettings so the renderer can read it without
    /// caring whether the runtime config has been bound yet.
    //
    // FLASH-MSG-COORDINATE-WITH-#58
    pub flash_message: Option<(String, FlashKind, Instant)>,
}

#[derive(PartialEq)]
pub enum SettingsAction {
    None,
    Close,
    Changed,
    BroadcastToggle,
}

impl Settings {
    pub fn new(border: BorderStyle) -> Self {
        Self {
            visible: false,
            border_style: border,
            show_status_bar: true,
            show_tab_bar: true,
            focused: I_ROUNDED,
            runtime: None,
            reload_request: false,
            reload_dirty: false,
            flash_message: None,
        }
    }

    /// Attach the freshly-loaded `EzpnConfig` so hot-reload can diff against
    /// it. Should be called once during daemon startup, after `load_config`.
    pub fn bind_runtime(&mut self, config: EzpnConfig) {
        self.runtime = Some(RuntimeSettings { config });
    }

    /// Borrow the held config (panics if `bind_runtime` hasn't been called).
    /// Tests + reload paths use this; normal render code reads the cached
    /// flat fields (`border_style` etc.) directly.
    pub fn config(&self) -> &EzpnConfig {
        self.runtime
            .as_ref()
            .map(|r| &r.config)
            .expect("Settings::bind_runtime must be called before config()")
    }

    /// Set a transient status-bar flash. Overwrites any pending message.
    //
    // FLASH-MSG-COORDINATE-WITH-#58
    pub fn set_flash(&mut self, msg: impl Into<String>, kind: FlashKind) {
        self.flash_message = Some((msg.into(), kind, Instant::now()));
    }

    /// Drop the flash if its duration has elapsed. Call once per frame.
    //
    // FLASH-MSG-COORDINATE-WITH-#58
    pub fn tick_flash(&mut self) {
        if let Some((_, kind, started)) = &self.flash_message {
            if started.elapsed() >= kind.duration() {
                self.flash_message = None;
            }
        }
    }

    /// Re-read the config file at `path`, validate it, and atomically apply
    /// the reloadable subset to `self`. On parse / IO error the previous
    /// `EzpnConfig` is retained and `ReloadOutcome::Error` is returned.
    ///
    /// Reloadable: border, status_bar, tab_bar, prefix.
    /// Non-reloadable (warn on change): shell, scrollback. Per-pane keys
    /// (command, env) live in the project file and are not handled here.
    ///
    /// `bind_runtime` must have been called first; if not, falls back to
    /// `EzpnConfig::default()` for the diff baseline.
    pub fn reload_config(&mut self, path: &Path) -> ReloadOutcome {
        // 1. Read file.
        let contents = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("read {}: {}", path.display(), e);
                tracing::warn!(target: "config_reload", "{msg}");
                return ReloadOutcome::Error(msg);
            }
        };

        // 2. Pre-validate TOML syntax. A `[section]` document must round-trip
        //    through `toml::Table` before we accept it; otherwise we'd be at
        //    the mercy of `load_config`'s lenient stderr-only error path and
        //    silently fall back to defaults — a bug in the context of
        //    hot-reload (would wipe the running config).
        if has_toml_table_header(&contents) {
            if let Err(e) = toml::from_str::<toml::Table>(&contents) {
                let msg = e.message().to_string();
                tracing::warn!(target: "config_reload", path = %path.display(), "{msg}");
                return ReloadOutcome::Error(msg);
            }
        }

        // 3. Parse into a fresh `EzpnConfig`. `load_config` reads from the
        //    XDG path; for hot-reload we want the same behavior.
        let new_config = config::load_config();

        // 4. Diff non-reloadable fields against the previous snapshot.
        let prev_shell;
        let prev_scrollback;
        if let Some(rt) = &self.runtime {
            prev_shell = rt.config.shell.clone();
            prev_scrollback = rt.config.scrollback;
        } else {
            let d = EzpnConfig::default();
            prev_shell = d.shell;
            prev_scrollback = d.scrollback;
        }
        let mut changed_non_reloadable: Vec<&'static str> = Vec::new();
        if new_config.shell != prev_shell && !is_reloadable("shell") {
            changed_non_reloadable.push("shell");
        }
        if new_config.scrollback != prev_scrollback && !is_reloadable("scrollback") {
            changed_non_reloadable.push("scrollback");
        }
        for f in &changed_non_reloadable {
            tracing::warn!(
                target: "config_reload",
                field = f,
                "non-reloadable field changed; restart the session to pick it up"
            );
        }

        // 5. Atomically apply reloadable fields + replace stored config.
        //    Done last so any failure above leaves state untouched.
        let visual_changed = self.border_style != new_config.border
            || self.show_status_bar != new_config.show_status_bar
            || self.show_tab_bar != new_config.show_tab_bar;
        self.border_style = new_config.border;
        self.show_status_bar = new_config.show_status_bar;
        self.show_tab_bar = new_config.show_tab_bar;
        self.runtime = Some(RuntimeSettings { config: new_config });
        if visual_changed {
            self.reload_dirty = true;
        }

        ReloadOutcome::Reloaded {
            non_reloadable_changed: changed_non_reloadable,
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.focused = match self.border_style {
                BorderStyle::Single => I_SINGLE,
                BorderStyle::Rounded => I_ROUNDED,
                BorderStyle::Heavy => I_HEAVY,
                BorderStyle::Double => I_DOUBLE,
                BorderStyle::None => I_NONE,
            };
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> SettingsAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.visible = false;
                SettingsAction::Close
            }
            KeyCode::Up | KeyCode::BackTab | KeyCode::Char('k') => {
                self.focused = self.focused.saturating_sub(1);
                SettingsAction::None
            }
            KeyCode::Down | KeyCode::Tab | KeyCode::Char('j') => {
                self.focused = (self.focused + 1).min(ITEM_COUNT - 1);
                SettingsAction::None
            }
            KeyCode::Left | KeyCode::Char('h') => self.adjust(-1),
            KeyCode::Right | KeyCode::Char('l') => self.adjust(1),
            KeyCode::Char('1') => self.set_border(BorderStyle::Single, I_SINGLE),
            KeyCode::Char('2') => self.set_border(BorderStyle::Rounded, I_ROUNDED),
            KeyCode::Char('3') => self.set_border(BorderStyle::Heavy, I_HEAVY),
            KeyCode::Char('4') => self.set_border(BorderStyle::Double, I_DOUBLE),
            KeyCode::Char('5') => self.set_border(BorderStyle::None, I_NONE),
            KeyCode::Enter | KeyCode::Char(' ') => self.activate(self.focused),
            _ => SettingsAction::None,
        }
    }

    pub fn handle_click(&mut self, mx: u16, my: u16, tw: u16, th: u16) -> SettingsAction {
        let (ox, oy) = origin(tw, th);
        if mx < ox || mx >= ox + W || my < oy || my >= oy + H {
            self.visible = false;
            return SettingsAction::Close;
        }
        for (i, &row) in ITEM_Y.iter().enumerate() {
            if my == oy + row {
                self.focused = i;
                return self.activate(i);
            }
        }
        SettingsAction::None
    }

    // ─── Render ────────────────────────────────────────

    pub fn render_overlay(
        &self,
        stdout: &mut impl Write,
        tw: u16,
        th: u16,
        broadcast: bool,
    ) -> anyhow::Result<()> {
        if tw < W + 4 || th < H + 2 {
            return Ok(());
        }
        let (ox, oy) = origin(tw, th);
        let inner_w = (W - PAD * 2) as usize;

        // Backdrop
        queue!(
            stdout,
            SetBackgroundColor(Color::Rgb { r: 4, g: 5, b: 8 }),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
        )?;

        // Panel background
        let blank = " ".repeat(W as usize);
        for dy in 0..H {
            queue!(
                stdout,
                cursor::MoveTo(ox, oy + dy),
                SetBackgroundColor(BG),
                Print(&blank)
            )?;
        }

        let x = ox + PAD; // content left edge
        let xr = ox + W - PAD; // content right edge

        // Title
        text(stdout, x, oy + Y_TITLE, BG, Color::White, true, "Settings")?;
        text(
            stdout,
            x,
            oy + Y_HINT,
            BG,
            DIM_FG,
            false,
            "j/k move  Enter apply  1-5 border  q close",
        )?;

        // Section: Border Style
        text(stdout, x, oy + Y_SEC1, BG, SEC_FG, true, "BORDER STYLE")?;
        self.item_border(stdout, x, xr, oy, I_SINGLE, "Single", BorderStyle::Single)?;
        self.item_border(
            stdout,
            x,
            xr,
            oy,
            I_ROUNDED,
            "Rounded",
            BorderStyle::Rounded,
        )?;
        self.item_border(stdout, x, xr, oy, I_HEAVY, "Heavy", BorderStyle::Heavy)?;
        self.item_border(stdout, x, xr, oy, I_DOUBLE, "Double", BorderStyle::Double)?;
        self.item_border(stdout, x, xr, oy, I_NONE, "None", BorderStyle::None)?;

        // Divider + Section: Display
        div(stdout, x, oy + Y_DIV1, inner_w)?;
        text(stdout, x, oy + Y_SEC2, BG, SEC_FG, true, "DISPLAY")?;
        self.item_toggle(
            stdout,
            x,
            xr,
            oy,
            I_STATUS,
            "Status Bar",
            self.show_status_bar,
        )?;
        self.item_toggle(stdout, x, xr, oy, I_TAB_BAR, "Tab Bar", self.show_tab_bar)?;
        self.item_toggle(stdout, x, xr, oy, I_BROADCAST, "Broadcast", broadcast)?;

        // Divider + Close
        div(stdout, x, oy + Y_DIV2, inner_w)?;
        self.item_close(stdout, x, xr, oy)?;

        queue!(stdout, ResetColor, SetAttribute(Attribute::Reset))?;
        Ok(())
    }

    // ─── Item renderers ────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn item_border(
        &self,
        stdout: &mut impl Write,
        x: u16,
        xr: u16,
        oy: u16,
        item: usize,
        name: &str,
        style: BorderStyle,
    ) -> anyhow::Result<()> {
        let y = oy + ITEM_Y[item];
        let f = self.focused == item;
        let sel = self.border_style == style;
        let bg = if f { FOCUS_BG } else { BG };

        row_bg(stdout, x - 1, y, (xr - x + 2) as usize, bg)?;
        if f {
            focus_marker(stdout, x - 1, y)?;
        }

        let icon = if sel { "●" } else { "○" };
        let icon_fg = if sel { ACCENT } else { DIM_FG };
        let nx = if f { x + 3 } else { x + 1 };

        queue!(
            stdout,
            cursor::MoveTo(nx, y),
            SetBackgroundColor(bg),
            SetForegroundColor(icon_fg),
            Print(icon),
            Print(" "),
            SetForegroundColor(if f { Color::White } else { LBL_FG }),
        )?;
        if f {
            queue!(stdout, SetAttribute(Attribute::Bold))?;
        }
        queue!(stdout, Print(name))?;
        if f {
            queue!(stdout, SetAttribute(Attribute::Reset))?;
        }

        if sel {
            right_tag(stdout, xr, y, bg, ACCENT, "active")?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn item_toggle(
        &self,
        stdout: &mut impl Write,
        x: u16,
        xr: u16,
        oy: u16,
        item: usize,
        label: &str,
        value: bool,
    ) -> anyhow::Result<()> {
        let y = oy + ITEM_Y[item];
        let f = self.focused == item;
        let bg = if f { FOCUS_BG } else { BG };

        row_bg(stdout, x - 1, y, (xr - x + 2) as usize, bg)?;
        if f {
            focus_marker(stdout, x - 1, y)?;
        }

        let nx = if f { x + 3 } else { x + 1 };
        queue!(
            stdout,
            cursor::MoveTo(nx, y),
            SetBackgroundColor(bg),
            SetForegroundColor(if f { Color::White } else { LBL_FG }),
        )?;
        if f {
            queue!(stdout, SetAttribute(Attribute::Bold))?;
        }
        queue!(stdout, Print(label))?;
        if f {
            queue!(stdout, SetAttribute(Attribute::Reset))?;
        }

        let (tag, tag_fg) = if value {
            ("ON", ACCENT)
        } else {
            ("OFF", DIM_FG)
        };
        right_tag(stdout, xr, y, bg, tag_fg, tag)?;
        Ok(())
    }

    fn item_close(&self, stdout: &mut impl Write, x: u16, xr: u16, oy: u16) -> anyhow::Result<()> {
        let y = oy + ITEM_Y[I_CLOSE];
        let f = self.focused == I_CLOSE;
        let bg = if f { FOCUS_BG } else { BG };

        row_bg(stdout, x - 1, y, (xr - x + 2) as usize, bg)?;
        if f {
            focus_marker(stdout, x - 1, y)?;
        }

        let nx = if f { x + 3 } else { x + 1 };
        queue!(
            stdout,
            cursor::MoveTo(nx, y),
            SetBackgroundColor(bg),
            SetForegroundColor(if f { WARN_FG } else { DIM_FG }),
        )?;
        if f {
            queue!(stdout, SetAttribute(Attribute::Bold))?;
        }
        queue!(stdout, Print("Close Settings"))?;
        if f {
            queue!(stdout, SetAttribute(Attribute::Reset))?;
        }

        right_tag(stdout, xr, y, bg, DIM_FG, "q / Esc")?;
        Ok(())
    }

    // ─── Logic ─────────────────────────────────────────

    fn adjust(&mut self, delta: isize) -> SettingsAction {
        match self.focused {
            I_SINGLE | I_ROUNDED | I_HEAVY | I_DOUBLE | I_NONE => {
                let o = [
                    BorderStyle::Single,
                    BorderStyle::Rounded,
                    BorderStyle::Heavy,
                    BorderStyle::Double,
                    BorderStyle::None,
                ];
                let i = o.iter().position(|s| *s == self.border_style).unwrap_or(1);
                let n = ((i as isize + delta).rem_euclid(5)) as usize;
                self.border_style = o[n];
                self.focused = n;
                SettingsAction::Changed
            }
            I_STATUS => {
                self.show_status_bar = !self.show_status_bar;
                SettingsAction::Changed
            }
            I_TAB_BAR => {
                self.show_tab_bar = !self.show_tab_bar;
                SettingsAction::Changed
            }
            I_BROADCAST => SettingsAction::BroadcastToggle,
            _ => SettingsAction::None,
        }
    }

    fn activate(&mut self, item: usize) -> SettingsAction {
        match item {
            I_SINGLE => self.set_border(BorderStyle::Single, I_SINGLE),
            I_ROUNDED => self.set_border(BorderStyle::Rounded, I_ROUNDED),
            I_HEAVY => self.set_border(BorderStyle::Heavy, I_HEAVY),
            I_DOUBLE => self.set_border(BorderStyle::Double, I_DOUBLE),
            I_NONE => self.set_border(BorderStyle::None, I_NONE),
            I_STATUS => {
                self.show_status_bar = !self.show_status_bar;
                SettingsAction::Changed
            }
            I_TAB_BAR => {
                self.show_tab_bar = !self.show_tab_bar;
                SettingsAction::Changed
            }
            I_BROADCAST => SettingsAction::BroadcastToggle,
            I_CLOSE => {
                self.visible = false;
                SettingsAction::Close
            }
            _ => SettingsAction::None,
        }
    }

    fn set_border(&mut self, style: BorderStyle, focused: usize) -> SettingsAction {
        self.border_style = style;
        self.focused = focused;
        SettingsAction::Changed
    }
}

// ─── Drawing primitives ────────────────────────────────

fn origin(tw: u16, th: u16) -> (u16, u16) {
    (tw.saturating_sub(W) / 2, th.saturating_sub(H) / 2)
}

fn text(
    out: &mut impl Write,
    x: u16,
    y: u16,
    bg: Color,
    fg: Color,
    bold: bool,
    s: &str,
) -> anyhow::Result<()> {
    queue!(
        out,
        cursor::MoveTo(x, y),
        SetBackgroundColor(bg),
        SetForegroundColor(fg)
    )?;
    if bold {
        queue!(out, SetAttribute(Attribute::Bold))?;
    }
    queue!(out, Print(s))?;
    if bold {
        queue!(out, SetAttribute(Attribute::Reset))?;
    }
    Ok(())
}

fn div(out: &mut impl Write, x: u16, y: u16, w: usize) -> anyhow::Result<()> {
    queue!(
        out,
        cursor::MoveTo(x, y),
        SetBackgroundColor(BG),
        SetForegroundColor(DIV_FG),
        Print("─".repeat(w))
    )?;
    Ok(())
}

fn row_bg(out: &mut impl Write, x: u16, y: u16, w: usize, bg: Color) -> anyhow::Result<()> {
    queue!(out, cursor::MoveTo(x, y), SetBackgroundColor(bg))?;
    for _ in 0..w {
        queue!(out, Print(" "))?;
    }
    Ok(())
}

fn focus_marker(out: &mut impl Write, x: u16, y: u16) -> anyhow::Result<()> {
    queue!(
        out,
        cursor::MoveTo(x, y),
        SetBackgroundColor(FOCUS_BG),
        SetForegroundColor(ACCENT),
        Print("▎›")
    )?;
    Ok(())
}

fn right_tag(
    out: &mut impl Write,
    xr: u16,
    y: u16,
    bg: Color,
    fg: Color,
    tag: &str,
) -> anyhow::Result<()> {
    let tx = xr.saturating_sub(tag.len() as u16);
    queue!(
        out,
        cursor::MoveTo(tx, y),
        SetBackgroundColor(bg),
        SetForegroundColor(fg),
        Print(tag)
    )?;
    Ok(())
}

// ─── Hot-reload (issue #64) ────────────────────────────

/// Status-bar flash kind. Drives color + duration in the renderer.
//
// FLASH-MSG-COORDINATE-WITH-#58
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashKind {
    /// Success — green, 1 s.
    Info,
    /// Failure — red, 3 s.
    Error,
}

impl FlashKind {
    pub fn duration(self) -> Duration {
        match self {
            FlashKind::Info => Duration::from_secs(1),
            FlashKind::Error => Duration::from_secs(3),
        }
    }
}

/// Outcome of a hot-reload attempt.
#[derive(Debug)]
pub enum ReloadOutcome {
    /// Config reloaded successfully. `non_reloadable_changed` lists the keys
    /// the user changed in the on-disk file that ezpn cannot apply at runtime
    /// (shell, scrollback, …) — caller should surface those in a warning.
    Reloaded {
        non_reloadable_changed: Vec<&'static str>,
    },
    /// Parse / IO error. Previous config is retained; caller flashes the
    /// error message.
    Error(String),
}

/// Whether a config field can be hot-reloaded into a running session.
///
/// Reloadable: visual-only knobs that don't affect already-spawned processes
/// or terminal buffers.
/// Non-reloadable: anything that would require killing/respawning panes
/// (shell, scrollback buffer for existing panes, per-pane command/env).
fn is_reloadable(field: &'static str) -> bool {
    match field {
        // Reloadable visual + binding changes.
        "border" | "status_bar" | "tab_bar" | "prefix" => true,
        // Non-reloadable — require session restart.
        "shell" | "scrollback" => false,
        // Unknown field: treat as non-reloadable so callers warn instead of
        // silently dropping it.
        _ => false,
    }
}

/// Internal holder for the live `EzpnConfig`. Lives inside `Settings::runtime`
/// so the hot-reload path (`Settings::reload_config`) can diff non-reloadable
/// fields against the previous snapshot without separate plumbing.
struct RuntimeSettings {
    config: EzpnConfig,
}

/// XDG-aware config file path, for callers that want to wire a reload trigger
/// (Ctrl+B r, SIGHUP) without poking at internal helpers.
pub fn config_path() -> PathBuf {
    let dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let mut home = std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/tmp"));
            home.push(".config");
            home
        });
    dir.join("ezpn").join("config.toml")
}

/// Same heuristic as `config::has_toml_table_header` — duplicated locally
/// because `config.rs` keeps it private and the issue forbids touching that
/// file. Kept tiny and side-effect-free.
fn has_toml_table_header(contents: &str) -> bool {
    contents.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with('[') && !t.starts_with("[[")
    })
}

// ─── Tests ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    /// `XDG_CONFIG_HOME` is process-global; serialize tests that mutate it.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Write `contents` to `<tmp>/ezpn/config.toml`, point `XDG_CONFIG_HOME`
    /// at `<tmp>`, and return the full file path.
    fn write_config(tmp: &std::path::Path, contents: &str) -> PathBuf {
        let dir = tmp.join("ezpn");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, contents).unwrap();
        path
    }

    /// Build a freshly-bound `Settings` for the test: load_config from the
    /// XDG_CONFIG_HOME we just set, copy the visual fields onto a new
    /// `Settings`, attach the runtime.
    fn build_settings() -> Settings {
        let cfg = config::load_config();
        let mut s = Settings::new(cfg.border);
        s.show_status_bar = cfg.show_status_bar;
        s.show_tab_bar = cfg.show_tab_bar;
        s.bind_runtime(cfg);
        s
    }

    #[test]
    fn reload_applies_border_change() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path = write_config(tmp.path(), "[global]\nborder = \"rounded\"\n");
        std::env::set_var("XDG_CONFIG_HOME", tmp.path());

        let mut settings = build_settings();
        assert_eq!(settings.border_style, BorderStyle::Rounded);

        // Edit the file, then reload.
        fs::write(&path, "[global]\nborder = \"heavy\"\n").unwrap();
        let outcome = settings.reload_config(&path);

        assert!(matches!(outcome, ReloadOutcome::Reloaded { .. }));
        assert_eq!(settings.border_style, BorderStyle::Heavy);
        assert_eq!(settings.config().border, BorderStyle::Heavy);

        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn reload_warns_on_non_reloadable_shell_change() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path = write_config(
            tmp.path(),
            "[global]\nshell = \"/bin/zsh\"\nborder = \"single\"\n",
        );
        std::env::set_var("XDG_CONFIG_HOME", tmp.path());

        let mut settings = build_settings();
        assert_eq!(settings.config().shell, "/bin/zsh");

        // Change shell + a reloadable border key.
        fs::write(
            &path,
            "[global]\nshell = \"/bin/fish\"\nborder = \"double\"\n",
        )
        .unwrap();
        let outcome = settings.reload_config(&path);

        match outcome {
            ReloadOutcome::Reloaded {
                non_reloadable_changed,
            } => {
                assert!(
                    non_reloadable_changed.contains(&"shell"),
                    "expected `shell` in warn list, got {non_reloadable_changed:?}"
                );
            }
            ReloadOutcome::Error(e) => panic!("expected Reloaded, got Error({e})"),
        }
        // Reloadable field applied.
        assert_eq!(settings.border_style, BorderStyle::Double);
        // Shell *value* in the held config follows the file (we surface the
        // diff via the warn list, not by ignoring the new value) so users can
        // see what they changed.
        assert_eq!(settings.config().shell, "/bin/fish");

        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn reload_parse_error_retains_previous_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path = write_config(tmp.path(), "[global]\nborder = \"heavy\"\n");
        std::env::set_var("XDG_CONFIG_HOME", tmp.path());

        let mut settings = build_settings();
        assert_eq!(settings.border_style, BorderStyle::Heavy);
        let prev_border = settings.config().border;

        // Corrupt the file: unclosed string in a sectioned doc.
        fs::write(&path, "[global]\nshell = \"/bin/zsh\nborder = \"double\"\n").unwrap();
        let outcome = settings.reload_config(&path);

        assert!(
            matches!(outcome, ReloadOutcome::Error(_)),
            "expected Error on malformed TOML"
        );
        // Settings + held config unchanged.
        assert_eq!(settings.border_style, BorderStyle::Heavy);
        assert_eq!(settings.config().border, prev_border);

        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn flash_kind_durations_match_spec() {
        // Spec: success 1 s, error 3 s.
        assert_eq!(FlashKind::Info.duration(), Duration::from_secs(1));
        assert_eq!(FlashKind::Error.duration(), Duration::from_secs(3));
    }

    #[test]
    fn tick_flash_clears_after_duration() {
        let mut settings = Settings::new(BorderStyle::Rounded);
        settings.set_flash("hello", FlashKind::Info);
        assert!(settings.flash_message.is_some());

        // Forge an old timestamp so we don't sleep.
        if let Some((msg, kind, _)) = settings.flash_message.take() {
            settings.flash_message = Some((msg, kind, Instant::now() - Duration::from_secs(2)));
        }
        settings.tick_flash();
        assert!(settings.flash_message.is_none());
    }

    #[test]
    fn is_reloadable_classification() {
        assert!(is_reloadable("border"));
        assert!(is_reloadable("status_bar"));
        assert!(is_reloadable("tab_bar"));
        assert!(is_reloadable("prefix"));
        assert!(!is_reloadable("shell"));
        assert!(!is_reloadable("scrollback"));
        // Unknown -> non-reloadable (caller warns).
        assert!(!is_reloadable("mystery"));
    }
}
