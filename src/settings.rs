#[cfg(test)]
use crate::vt100;
use std::io::Write;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use crossterm::{cursor, queue, style::*, Command};

use crate::config::{self, EzpnConfig};
use crate::render::{AnsiBackground, AnsiForeground, BorderStyle};
use crate::theme::{ColorDepth, Resolved, ResolvedPalette, RgbColor, Theme};

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
    /// Live tab count for reserving a borderless tab footer; never serialized.
    pub tab_count: usize,
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
    /// Active theme (#85). Updated when `[theme]` is reloaded; downgraded
    /// once into [`Self::resolved_palette`] for the renderer.
    pub theme: Theme,
    /// Pre-resolved palette in the live `ColorDepth`. The renderer reads
    /// this every frame; recompute by calling [`Settings::set_theme`] when
    /// the source theme changes.
    pub resolved_palette: ResolvedPalette,
    color_depth: ColorDepth,
    reloaded_bindings: Option<ReloadedBindings>,
}

#[derive(PartialEq)]
pub enum SettingsAction {
    None,
    Close,
    Changed,
    BroadcastToggle,
}

pub struct ReloadedBindings {
    pub hooks: Vec<crate::hooks::Hook>,
    pub keymap: crate::keymap::Keymap,
}

impl Settings {
    pub fn new(border: BorderStyle) -> Self {
        // Default to TrueColor; phase 2b (server boot) replaces this with a
        // `ColorDepth::detect()` result + the user's configured theme.
        let theme = Theme::default_theme();
        let resolved_palette = theme.resolve(ColorDepth::TrueColor);
        Self {
            visible: false,
            border_style: border,
            show_status_bar: true,
            show_tab_bar: true,
            tab_count: 1,
            focused: I_ROUNDED,
            runtime: None,
            reload_request: false,
            reload_dirty: false,
            flash_message: None,
            theme,
            resolved_palette,
            color_depth: ColorDepth::TrueColor,
            reloaded_bindings: None,
        }
    }

    /// Replace the active theme and re-resolve the palette at `depth`.
    /// Call this after config load + `ColorDepth::detect`, and again on
    /// hot-reload when `[theme]` changes.
    pub fn set_theme(&mut self, theme: Theme, depth: ColorDepth) {
        self.color_depth = depth;
        self.resolved_palette = theme.resolve(depth);
        self.theme = theme;
    }

    /// Attach the freshly-loaded `EzpnConfig` so hot-reload can diff against
    /// it. Should be called once during daemon startup, after `load_config`.
    pub fn bind_runtime(&mut self, config: EzpnConfig) {
        self.runtime = Some(RuntimeSettings { config });
    }

    /// Consume registries parsed from the same file bytes as the last
    /// successful reload. Merge trusted project hooks in the caller.
    pub fn take_reloaded_bindings(&mut self) -> Option<ReloadedBindings> {
        self.reloaded_bindings.take()
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
    // reason: per-frame flash-expiry tick consumed by the flash-overlay
    // wiring (#64); covered by this module's `tick_flash_clears_after_duration`
    // test today.
    #[allow(dead_code)]
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
    /// Reloadable: border, bars, prefix, theme, hooks, and keybindings.
    /// Non-reloadable (warn on change): shell, scrollback limits/policy,
    /// clipboard settings, and persistence defaults. Per-pane keys
    /// (command, env) live in the project file and are not handled here.
    ///
    /// `bind_runtime` must have been called first; if not, falls back to
    /// `EzpnConfig::default()` for the diff baseline.
    pub fn reload_config(&mut self, path: &Path) -> ReloadOutcome {
        self.reloaded_bindings = None;
        // 1. Read file.
        let contents = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("read {}: {}", path.display(), e);
                tracing::warn!(target: "config_reload", "{msg}");
                return ReloadOutcome::Error(msg);
            }
        };

        // Parse the bytes we just read, including semantic validation. A
        // second XDG read could load another file or another edit entirely.
        let mut new_config = match config::parse_config_checked(&contents, path) {
            Ok(config) => config,
            Err(msg) => {
                tracing::warn!(target: "config_reload", path = %path.display(), "{msg}");
                return ReloadOutcome::Error(msg);
            }
        };
        let bindings = match (
            config::parse_hooks_checked(&contents),
            config::parse_keymap_checked(&contents),
        ) {
            (Ok(hooks), Ok(keymap)) => ReloadedBindings { hooks, keymap },
            (Err(error), _) | (_, Err(error)) => return ReloadOutcome::Error(error),
        };

        // 4. Diff non-reloadable fields against the previous snapshot.
        let defaults = EzpnConfig::default();
        let previous = self.runtime.as_ref().map_or(&defaults, |rt| &rt.config);
        let mut changed_non_reloadable: Vec<&'static str> = Vec::new();
        // Keep the effective runtime values for settings the caller cannot
        // apply. Otherwise a second reload falsely reports them as applied.
        macro_rules! preserve {
            ($($field:ident => $value:expr),+ $(,)?) => { $(
                if new_config.$field != previous.$field {
                    changed_non_reloadable.push(stringify!($field));
                }
                new_config.$field = $value;
            )+ };
        }
        preserve!(shell => previous.shell.clone(), scrollback => previous.scrollback,
            scrollback_bytes => previous.scrollback_bytes, scrollback_eviction => previous.scrollback_eviction,
            clipboard_copy_command => previous.clipboard_copy_command.clone(),
            clipboard_paste_command => previous.clipboard_paste_command.clone(),
            persist_scrollback => previous.persist_scrollback);
        if new_config.clipboard.set != previous.clipboard.set
            || new_config.clipboard.get != previous.clipboard.get
            || new_config.clipboard.max_bytes != previous.clipboard.max_bytes
        {
            changed_non_reloadable.push("clipboard");
        }
        new_config.clipboard = previous.clipboard;
        for f in &changed_non_reloadable {
            tracing::warn!(
                target: "config_reload",
                field = f,
                "non-reloadable field changed; restart the session to pick it up"
            );
        }

        // 5. Atomically apply reloadable fields + replace stored config.
        //    Done last so any failure above leaves state untouched.
        let theme_changed = self.theme != new_config.theme;
        let visual_changed = self.border_style != new_config.border
            || self.show_status_bar != new_config.show_status_bar
            || self.show_tab_bar != new_config.show_tab_bar
            || theme_changed
            || previous.status_bar != new_config.status_bar;
        self.border_style = new_config.border;
        self.show_status_bar = new_config.show_status_bar;
        self.show_tab_bar = new_config.show_tab_bar;
        if theme_changed {
            self.set_theme(new_config.theme.clone(), self.color_depth);
        }
        self.runtime = Some(RuntimeSettings { config: new_config });
        self.reloaded_bindings = Some(bindings);
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
            background(Color::Rgb { r: 4, g: 5, b: 8 }, self.color_depth),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
        )?;

        // Panel background
        let blank = " ".repeat(W as usize);
        for dy in 0..H {
            queue!(
                stdout,
                cursor::MoveTo(ox, oy + dy),
                background(BG, self.color_depth),
                Print(&blank)
            )?;
        }

        let x = ox + PAD; // content left edge
        let xr = ox + W - PAD; // content right edge

        // Title
        text(
            stdout,
            x,
            oy + Y_TITLE,
            BG,
            Color::White,
            true,
            "Settings",
            self.color_depth,
        )?;
        text(
            stdout,
            x,
            oy + Y_HINT,
            BG,
            DIM_FG,
            false,
            "j/k move  Enter apply  1-5 border  q close",
            self.color_depth,
        )?;

        // Section: Border Style
        text(
            stdout,
            x,
            oy + Y_SEC1,
            BG,
            SEC_FG,
            true,
            "BORDER STYLE",
            self.color_depth,
        )?;
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
        div(stdout, x, oy + Y_DIV1, inner_w, self.color_depth)?;
        text(
            stdout,
            x,
            oy + Y_SEC2,
            BG,
            SEC_FG,
            true,
            "DISPLAY",
            self.color_depth,
        )?;
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
        div(stdout, x, oy + Y_DIV2, inner_w, self.color_depth)?;
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

        row_bg(
            stdout,
            x - 1,
            y,
            (xr - x + 2) as usize,
            bg,
            self.color_depth,
        )?;
        if f {
            focus_marker(stdout, x - 1, y, self.color_depth)?;
        }

        let icon = if sel { "●" } else { "○" };
        let icon_fg = if sel { ACCENT } else { DIM_FG };
        let nx = if f { x + 3 } else { x + 1 };

        queue!(
            stdout,
            cursor::MoveTo(nx, y),
            background(bg, self.color_depth),
            foreground(icon_fg, self.color_depth),
            Print(icon),
            Print(" "),
            foreground(if f { Color::White } else { LBL_FG }, self.color_depth),
        )?;
        if f {
            queue!(stdout, SetAttribute(Attribute::Bold))?;
        }
        queue!(stdout, Print(name))?;
        if f {
            queue!(stdout, SetAttribute(Attribute::Reset))?;
        }

        if sel {
            right_tag(stdout, xr, y, bg, ACCENT, "active", self.color_depth)?;
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

        row_bg(
            stdout,
            x - 1,
            y,
            (xr - x + 2) as usize,
            bg,
            self.color_depth,
        )?;
        if f {
            focus_marker(stdout, x - 1, y, self.color_depth)?;
        }

        let nx = if f { x + 3 } else { x + 1 };
        queue!(
            stdout,
            cursor::MoveTo(nx, y),
            background(bg, self.color_depth),
            foreground(if f { Color::White } else { LBL_FG }, self.color_depth),
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
        right_tag(stdout, xr, y, bg, tag_fg, tag, self.color_depth)?;
        Ok(())
    }

    fn item_close(&self, stdout: &mut impl Write, x: u16, xr: u16, oy: u16) -> anyhow::Result<()> {
        let y = oy + ITEM_Y[I_CLOSE];
        let f = self.focused == I_CLOSE;
        let bg = if f { FOCUS_BG } else { BG };

        row_bg(
            stdout,
            x - 1,
            y,
            (xr - x + 2) as usize,
            bg,
            self.color_depth,
        )?;
        if f {
            focus_marker(stdout, x - 1, y, self.color_depth)?;
        }

        let nx = if f { x + 3 } else { x + 1 };
        queue!(
            stdout,
            cursor::MoveTo(nx, y),
            background(bg, self.color_depth),
            foreground(if f { WARN_FG } else { DIM_FG }, self.color_depth),
        )?;
        if f {
            queue!(stdout, SetAttribute(Attribute::Bold))?;
        }
        queue!(stdout, Print("Close Settings"))?;
        if f {
            queue!(stdout, SetAttribute(Attribute::Reset))?;
        }

        right_tag(stdout, xr, y, bg, DIM_FG, "q / Esc", self.color_depth)?;
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

#[allow(clippy::too_many_arguments)]
fn text(
    out: &mut impl Write,
    x: u16,
    y: u16,
    bg: Color,
    fg: Color,
    bold: bool,
    s: &str,
    depth: ColorDepth,
) -> anyhow::Result<()> {
    queue!(
        out,
        cursor::MoveTo(x, y),
        background(bg, depth),
        foreground(fg, depth)
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

fn div(out: &mut impl Write, x: u16, y: u16, w: usize, depth: ColorDepth) -> anyhow::Result<()> {
    queue!(
        out,
        cursor::MoveTo(x, y),
        background(BG, depth),
        foreground(DIV_FG, depth),
        Print("─".repeat(w))
    )?;
    Ok(())
}

fn row_bg(
    out: &mut impl Write,
    x: u16,
    y: u16,
    w: usize,
    bg: Color,
    depth: ColorDepth,
) -> anyhow::Result<()> {
    queue!(out, cursor::MoveTo(x, y), background(bg, depth))?;
    for _ in 0..w {
        queue!(out, Print(" "))?;
    }
    Ok(())
}

fn focus_marker(out: &mut impl Write, x: u16, y: u16, depth: ColorDepth) -> anyhow::Result<()> {
    queue!(
        out,
        cursor::MoveTo(x, y),
        background(FOCUS_BG, depth),
        foreground(ACCENT, depth),
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
    depth: ColorDepth,
) -> anyhow::Result<()> {
    let tx = xr.saturating_sub(tag.len() as u16);
    queue!(
        out,
        cursor::MoveTo(tx, y),
        background(bg, depth),
        foreground(fg, depth),
        Print(tag)
    )?;
    Ok(())
}

struct OverlayColor {
    color: Color,
    depth: ColorDepth,
    background: bool,
}

fn foreground(color: Color, depth: ColorDepth) -> OverlayColor {
    OverlayColor {
        color,
        depth,
        background: false,
    }
}

fn background(color: Color, depth: ColorDepth) -> OverlayColor {
    OverlayColor {
        color,
        depth,
        background: true,
    }
}

impl OverlayColor {
    fn resolved(&self) -> Color {
        match self.color {
            Color::Rgb { r, g, b } => match RgbColor::new(r, g, b).downgrade_to(self.depth) {
                Resolved::Rgb(c) => Color::Rgb {
                    r: c.r,
                    g: c.g,
                    b: c.b,
                },
                Resolved::Indexed(i) => Color::AnsiValue(i),
            },
            color => color,
        }
    }
}

impl Command for OverlayColor {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        if self.background {
            AnsiBackground(self.resolved()).write_ansi(f)
        } else {
            AnsiForeground(self.resolved()).write_ansi(f)
        }
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        if self.background {
            AnsiBackground(self.resolved()).execute_winapi()
        } else {
            AnsiForeground(self.resolved()).execute_winapi()
        }
    }
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
    // reason: read by `Settings::tick_flash` (also a #64-deferred consumer);
    // covered by this module's `flash_kind_durations` test today.
    #[allow(dead_code)]
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

// ─── Tests ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    #[test]
    fn reliability_reload_uses_supplied_path_and_keeps_color_depth() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_config(
            tmp.path(),
            "[global]\nborder = \"double\"\n[theme]\nname = \"nord\"\n",
        );
        let mut settings = Settings::new(BorderStyle::Rounded);
        settings.set_theme(Theme::default_theme(), ColorDepth::Palette16);
        settings.bind_runtime(EzpnConfig::default());
        assert!(matches!(
            settings.reload_config(&path),
            ReloadOutcome::Reloaded { .. }
        ));
        assert_eq!(settings.border_style, BorderStyle::Double);
        assert_eq!(
            settings.resolved_palette,
            Theme::builtin("nord")
                .unwrap()
                .resolve(ColorDepth::Palette16)
        );
    }

    #[test]
    fn reliability_reload_rejects_semantically_invalid_config_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_config(tmp.path(), "[global]\nborder = 42\n");
        let mut settings = Settings::new(BorderStyle::Heavy);
        settings.bind_runtime(EzpnConfig::default());
        assert!(matches!(
            settings.reload_config(&path),
            ReloadOutcome::Error(_)
        ));
        assert_eq!(settings.border_style, BorderStyle::Heavy);
        assert!(!settings.reload_dirty);
    }

    #[test]
    fn reliability_settings_overlay_uses_terminal_color_depth() {
        for depth in [ColorDepth::Palette16, ColorDepth::Palette256] {
            let mut settings = Settings::new(BorderStyle::Rounded);
            settings.set_theme(Theme::default_theme(), depth);
            let mut bytes = Vec::new();
            settings.render_overlay(&mut bytes, 80, 30, false).unwrap();
            let text = String::from_utf8(bytes.clone()).unwrap();
            assert!(!text.contains("38;2;") && !text.contains("48;2;"));
            if depth == ColorDepth::Palette16 {
                assert!(!text.contains("38;5;") && !text.contains("48;5;"));
            }
            let mut parser = vt100::Parser::new(30, 80, 0);
            parser.process(&bytes);
            assert!(parser.screen().contents().contains("Settings"));
            assert!(parser.screen().contents().contains("Close Settings"));
        }
    }

    #[test]
    fn reliability_reload_hands_off_exact_validated_bindings() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_config(tmp.path(), "[global]\nborder = \"heavy\"\n[[hooks]]\nevent = \"after_attach\"\nexec = [\"true\"]\n");
        let mut settings = Settings::new(BorderStyle::Rounded);
        settings.bind_runtime(EzpnConfig::default());
        assert!(matches!(
            settings.reload_config(&path),
            ReloadOutcome::Reloaded { .. }
        ));
        fs::write(
            &path,
            "[[hooks]]\nevent = \"bad-event\"\nexec = [\"false\"]\n",
        )
        .unwrap();
        let bindings = settings.take_reloaded_bindings().unwrap();
        assert_eq!(bindings.hooks.len(), 1);
        assert_eq!(bindings.hooks[0].exec, vec!["true"]);
        assert!(settings.take_reloaded_bindings().is_none());
        assert!(matches!(
            settings.reload_config(&path),
            ReloadOutcome::Error(_)
        ));
        assert_eq!(settings.border_style, BorderStyle::Heavy);
        assert!(settings.take_reloaded_bindings().is_none());
    }

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
        // The held config describes effective values until session restart.
        assert_eq!(settings.config().shell, "/bin/zsh");

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
    fn reload_keeps_nonreloadable_effective_values_on_repeated_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_config(
            tmp.path(),
            "[global]\nscrollback = 7\nscrollback_bytes = 99\npersist_scrollback = true\n",
        );
        let mut settings = Settings::new(BorderStyle::Rounded);
        settings.bind_runtime(EzpnConfig::default());
        for _ in 0..2 {
            let ReloadOutcome::Reloaded {
                non_reloadable_changed,
            } = settings.reload_config(&path)
            else {
                panic!("reload failed");
            };
            assert!(non_reloadable_changed.contains(&"scrollback"));
            assert!(non_reloadable_changed.contains(&"scrollback_bytes"));
            assert!(non_reloadable_changed.contains(&"persist_scrollback"));
            assert_eq!(
                settings.config().scrollback,
                EzpnConfig::default().scrollback
            );
            assert_eq!(
                settings.config().scrollback_bytes,
                config::DEFAULT_SCROLLBACK_BYTES
            );
            assert!(!settings.config().persist_scrollback);
        }
    }
}
