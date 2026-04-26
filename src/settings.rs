use std::io::Write;

use crossterm::event::{KeyCode, KeyEvent};
use crossterm::{cursor, queue, style::*};

use crate::render::BorderStyle;
use crate::theme::AdaptedTheme;

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
const Y_FOOTER: u16 = 18; // "Saved to ~/.config/ezpn/config.toml"

const ITEM_Y: [u16; 9] = [Y_I0, Y_I1, Y_I2, Y_I3, Y_I4, Y_I5, Y_I6, Y_I7, Y_I8];
const ITEM_COUNT: usize = 9;

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
    /// Adapted (terminal-quantized) color palette used by every renderer
    /// in the project.  Cloned cheaply (`Color`s are `Copy` and the name
    /// `String` is owned per-Settings).
    pub theme: AdaptedTheme,
    focused: usize,
}

#[derive(PartialEq)]
pub enum SettingsAction {
    None,
    Close,
    Changed,
    BroadcastToggle,
}

impl Settings {
    /// Constructor for tests / boot-before-config.  Uses a truecolor-quantized
    /// default palette; production code should call [`Settings::with_theme`]
    /// with a config-derived [`AdaptedTheme`].
    #[allow(dead_code)]
    pub fn new(border: BorderStyle) -> Self {
        Self::with_theme(border, AdaptedTheme::default_truecolor())
    }

    pub fn with_theme(border: BorderStyle, theme: AdaptedTheme) -> Self {
        Self {
            visible: false,
            border_style: border,
            show_status_bar: true,
            show_tab_bar: true,
            theme,
            focused: I_ROUNDED,
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
        let theme = &self.theme;
        let bg_color = theme.bg;
        let sec_fg = theme.sec_fg;
        let dim_fg = theme.dim_fg;
        let lbl_fg = theme.lbl_fg;
        let (ox, oy) = origin(tw, th);
        let inner_w = (W - PAD * 2) as usize;

        // Backdrop — re-uses the theme background; with truecolor adapters
        // this is visually identical to the previous near-black backdrop.
        queue!(
            stdout,
            SetBackgroundColor(bg_color),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
        )?;

        // Panel background
        let blank = " ".repeat(W as usize);
        for dy in 0..H {
            queue!(
                stdout,
                cursor::MoveTo(ox, oy + dy),
                SetBackgroundColor(bg_color),
                Print(&blank)
            )?;
        }

        let x = ox + PAD; // content left edge
        let xr = ox + W - PAD; // content right edge

        // Title
        text(stdout, x, oy + Y_TITLE, bg_color, lbl_fg, true, "Settings")?;
        text(
            stdout,
            x,
            oy + Y_HINT,
            bg_color,
            dim_fg,
            false,
            "j/k move  Enter apply  1-5 border  q close",
        )?;

        // Section: Border Style
        text(
            stdout,
            x,
            oy + Y_SEC1,
            bg_color,
            sec_fg,
            true,
            "BORDER STYLE",
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
        div(stdout, x, oy + Y_DIV1, inner_w, bg_color, theme.div_fg)?;
        text(stdout, x, oy + Y_SEC2, bg_color, sec_fg, true, "DISPLAY")?;
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
        div(stdout, x, oy + Y_DIV2, inner_w, bg_color, theme.div_fg)?;
        self.item_close(stdout, x, xr, oy)?;

        // Footer: where settings persist to. Subtle, single line.
        let scope = format!("Saved to {}", crate::config::display_config_path());
        let scope = truncate_to_width(&scope, inner_w);
        text(
            stdout,
            x,
            oy + Y_FOOTER,
            theme.bg,
            theme.dim_fg,
            false,
            &scope,
        )?;

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
        let theme = &self.theme;
        let y = oy + ITEM_Y[item];
        let f = self.focused == item;
        let sel = self.border_style == style;
        let bg = if f { theme.focus_bg } else { theme.bg };

        row_bg(stdout, x - 1, y, (xr - x + 2) as usize, bg)?;
        if f {
            focus_marker(stdout, x - 1, y, theme.focus_bg, theme.accent)?;
        }

        let icon = if sel { "●" } else { "○" };
        let icon_fg = if sel { theme.accent } else { theme.dim_fg };
        let nx = if f { x + 3 } else { x + 1 };

        queue!(
            stdout,
            cursor::MoveTo(nx, y),
            SetBackgroundColor(bg),
            SetForegroundColor(icon_fg),
            Print(icon),
            Print(" "),
            SetForegroundColor(if f { theme.status_fg } else { theme.lbl_fg }),
        )?;
        if f {
            queue!(stdout, SetAttribute(Attribute::Bold))?;
        }
        queue!(stdout, Print(name))?;
        if f {
            queue!(stdout, SetAttribute(Attribute::Reset))?;
        }

        if sel {
            right_tag(stdout, xr, y, bg, theme.accent, "active")?;
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
        let theme = &self.theme;
        let y = oy + ITEM_Y[item];
        let f = self.focused == item;
        let bg = if f { theme.focus_bg } else { theme.bg };

        row_bg(stdout, x - 1, y, (xr - x + 2) as usize, bg)?;
        if f {
            focus_marker(stdout, x - 1, y, theme.focus_bg, theme.accent)?;
        }

        let nx = if f { x + 3 } else { x + 1 };
        queue!(
            stdout,
            cursor::MoveTo(nx, y),
            SetBackgroundColor(bg),
            SetForegroundColor(if f { theme.status_fg } else { theme.lbl_fg }),
        )?;
        if f {
            queue!(stdout, SetAttribute(Attribute::Bold))?;
        }
        queue!(stdout, Print(label))?;
        if f {
            queue!(stdout, SetAttribute(Attribute::Reset))?;
        }

        let (tag, tag_fg) = if value {
            ("ON", theme.accent)
        } else {
            ("OFF", theme.dim_fg)
        };
        right_tag(stdout, xr, y, bg, tag_fg, tag)?;
        Ok(())
    }

    fn item_close(&self, stdout: &mut impl Write, x: u16, xr: u16, oy: u16) -> anyhow::Result<()> {
        let theme = &self.theme;
        let y = oy + ITEM_Y[I_CLOSE];
        let f = self.focused == I_CLOSE;
        let bg = if f { theme.focus_bg } else { theme.bg };

        row_bg(stdout, x - 1, y, (xr - x + 2) as usize, bg)?;
        if f {
            focus_marker(stdout, x - 1, y, theme.focus_bg, theme.accent)?;
        }

        let nx = if f { x + 3 } else { x + 1 };
        queue!(
            stdout,
            cursor::MoveTo(nx, y),
            SetBackgroundColor(bg),
            SetForegroundColor(if f { theme.warn_fg } else { theme.dim_fg }),
        )?;
        if f {
            queue!(stdout, SetAttribute(Attribute::Bold))?;
        }
        queue!(stdout, Print("Close Settings"))?;
        if f {
            queue!(stdout, SetAttribute(Attribute::Reset))?;
        }

        right_tag(stdout, xr, y, bg, theme.dim_fg, "q / Esc")?;
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

fn div(out: &mut impl Write, x: u16, y: u16, w: usize, bg: Color, fg: Color) -> anyhow::Result<()> {
    queue!(
        out,
        cursor::MoveTo(x, y),
        SetBackgroundColor(bg),
        SetForegroundColor(fg),
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

fn focus_marker(out: &mut impl Write, x: u16, y: u16, bg: Color, fg: Color) -> anyhow::Result<()> {
    queue!(
        out,
        cursor::MoveTo(x, y),
        SetBackgroundColor(bg),
        SetForegroundColor(fg),
        Print("▎›")
    )?;
    Ok(())
}

/// Truncate a string to at most `max` display columns, suffixing with "…"
/// when something was cut. Operates on byte width since the panel content
/// is ASCII-only ("Saved to /home/user/.config/...").
fn truncate_to_width(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let cut = max.saturating_sub(1);
    let mut out: String = s.chars().take(cut).collect();
    out.push('…');
    out
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
