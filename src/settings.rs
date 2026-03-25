use std::io::Write;

use crossterm::event::{KeyCode, KeyEvent};
use crossterm::{cursor, queue, style::*};

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
const Y_DIV1: u16 = 9;
const Y_SEC2: u16 = 10; // "PANE"
const Y_I4: u16 = 11; // Split H
const Y_I5: u16 = 12; // Split V
const Y_DIV2: u16 = 13;
const Y_SEC3: u16 = 14; // "DISPLAY"
const Y_I6: u16 = 15; // Status Bar
const Y_DIV3: u16 = 16;
const Y_I7: u16 = 17; // Close

const ITEM_Y: [u16; 8] = [Y_I0, Y_I1, Y_I2, Y_I3, Y_I4, Y_I5, Y_I6, Y_I7];
const ITEM_COUNT: usize = 8;

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
const I_SPLIT_H: usize = 4;
const I_SPLIT_V: usize = 5;
const I_STATUS: usize = 6;
const I_CLOSE: usize = 7;

// ─── State ─────────────────────────────────────────────

pub struct Settings {
    pub visible: bool,
    pub border_style: BorderStyle,
    pub show_status_bar: bool,
    focused: usize,
}

#[derive(PartialEq)]
pub enum SettingsAction {
    None,
    Close,
    Changed,
    SplitH,
    SplitV,
}

impl Settings {
    pub fn new(border: BorderStyle) -> Self {
        Self {
            visible: false,
            border_style: border,
            show_status_bar: true,
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
                BorderStyle::None => I_SINGLE, // default focus position
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

    pub fn render_overlay(&self, stdout: &mut impl Write, tw: u16, th: u16) -> anyhow::Result<()> {
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
            "j/k move  Enter apply  1-4 border  q close",
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

        // Divider + Section: Pane
        div(stdout, x, oy + Y_DIV1, inner_w)?;
        text(stdout, x, oy + Y_SEC2, BG, SEC_FG, true, "PANE")?;
        self.item_action(stdout, x, xr, oy, I_SPLIT_H, "Split Left | Right", "Ctrl+D")?;
        self.item_action(stdout, x, xr, oy, I_SPLIT_V, "Split Top / Bottom", "Ctrl+E")?;

        // Divider + Section: Display
        div(stdout, x, oy + Y_DIV2, inner_w)?;
        text(stdout, x, oy + Y_SEC3, BG, SEC_FG, true, "DISPLAY")?;
        self.item_toggle(stdout, x, xr, oy)?;

        // Divider + Close
        div(stdout, x, oy + Y_DIV3, inner_w)?;
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
    fn item_action(
        &self,
        stdout: &mut impl Write,
        x: u16,
        xr: u16,
        oy: u16,
        item: usize,
        name: &str,
        hint: &str,
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
        queue!(stdout, Print(name))?;
        if f {
            queue!(stdout, SetAttribute(Attribute::Reset))?;
        }

        right_tag(stdout, xr, y, bg, DIM_FG, hint)?;
        Ok(())
    }

    fn item_toggle(&self, stdout: &mut impl Write, x: u16, xr: u16, oy: u16) -> anyhow::Result<()> {
        let y = oy + ITEM_Y[I_STATUS];
        let f = self.focused == I_STATUS;
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
        queue!(stdout, Print("Status Bar"))?;
        if f {
            queue!(stdout, SetAttribute(Attribute::Reset))?;
        }

        let (tag, tag_fg) = if self.show_status_bar {
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
            I_SINGLE | I_ROUNDED | I_HEAVY | I_DOUBLE => {
                let o = [
                    BorderStyle::Single,
                    BorderStyle::Rounded,
                    BorderStyle::Heavy,
                    BorderStyle::Double,
                ];
                let i = o.iter().position(|s| *s == self.border_style).unwrap_or(1);
                let n = ((i as isize + delta).rem_euclid(4)) as usize;
                self.border_style = o[n];
                self.focused = n;
                SettingsAction::Changed
            }
            I_STATUS => {
                self.show_status_bar = !self.show_status_bar;
                SettingsAction::Changed
            }
            _ => SettingsAction::None,
        }
    }

    fn activate(&mut self, item: usize) -> SettingsAction {
        match item {
            I_SINGLE => self.set_border(BorderStyle::Single, I_SINGLE),
            I_ROUNDED => self.set_border(BorderStyle::Rounded, I_ROUNDED),
            I_HEAVY => self.set_border(BorderStyle::Heavy, I_HEAVY),
            I_DOUBLE => self.set_border(BorderStyle::Double, I_DOUBLE),
            I_SPLIT_H => {
                self.visible = false;
                SettingsAction::SplitH
            }
            I_SPLIT_V => {
                self.visible = false;
                SettingsAction::SplitV
            }
            I_STATUS => {
                self.show_status_bar = !self.show_status_bar;
                SettingsAction::Changed
            }
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
