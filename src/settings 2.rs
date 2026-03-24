use std::io;

use crossterm::event::{KeyCode, KeyEvent};
use crossterm::{cursor, queue, style::*};

use crate::render::BorderStyle;

const PANEL_W: u16 = 46;
const PANEL_H: u16 = 20;

const PANEL_BG: Color = Color::Rgb {
    r: 25,
    g: 25,
    b: 35,
};
const HEADER_FG: Color = Color::Rgb {
    r: 100,
    g: 100,
    b: 120,
};
const LABEL_FG: Color = Color::Rgb {
    r: 180,
    g: 180,
    b: 200,
};
const HIGHLIGHT: Color = Color::Cyan;
const DIM_FG: Color = Color::DarkGrey;
const FOCUS_BG: Color = Color::Rgb {
    r: 35,
    g: 35,
    b: 50,
};

const ITEM_BORDER: usize = 0;
const ITEM_SPLIT_H: usize = 1;
const ITEM_SPLIT_V: usize = 2;
const ITEM_STATUS: usize = 3;
const ITEM_CLOSE: usize = 4;
const ITEM_COUNT: usize = 5;

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
            focused: 0,
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        self.focused = 0;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> SettingsAction {
        match key.code {
            KeyCode::Esc => {
                self.visible = false;
                SettingsAction::Close
            }
            KeyCode::Up | KeyCode::BackTab => {
                self.focused = self.focused.saturating_sub(1);
                SettingsAction::None
            }
            KeyCode::Down | KeyCode::Tab => {
                self.focused = (self.focused + 1).min(ITEM_COUNT - 1);
                SettingsAction::None
            }
            KeyCode::Left => self.adjust(-1),
            KeyCode::Right => self.adjust(1),
            KeyCode::Enter | KeyCode::Char(' ') => self.activate(),
            _ => SettingsAction::None,
        }
    }

    pub fn handle_click(&mut self, mx: u16, my: u16, tw: u16, th: u16) -> SettingsAction {
        let ox = tw.saturating_sub(PANEL_W) / 2;
        let oy = th.saturating_sub(PANEL_H) / 2;

        if mx < ox || mx >= ox + PANEL_W || my < oy || my >= oy + PANEL_H {
            self.visible = false;
            return SettingsAction::Close;
        }

        let rx = mx.saturating_sub(ox + 1) as usize;
        let ry = my.saturating_sub(oy) as usize;

        match ry {
            3 => {
                self.focused = ITEM_BORDER;
                if rx < 11 {
                    self.border_style = BorderStyle::Single;
                } else if rx < 22 {
                    self.border_style = BorderStyle::Rounded;
                } else if rx < 31 {
                    self.border_style = BorderStyle::Heavy;
                } else {
                    self.border_style = BorderStyle::Double;
                }
                SettingsAction::Changed
            }
            6 => {
                if rx < 22 {
                    self.focused = ITEM_SPLIT_H;
                    self.visible = false;
                    SettingsAction::SplitH
                } else {
                    self.focused = ITEM_SPLIT_V;
                    self.visible = false;
                    SettingsAction::SplitV
                }
            }
            9 => {
                self.focused = ITEM_STATUS;
                self.show_status_bar = !self.show_status_bar;
                SettingsAction::Changed
            }
            18 => {
                self.focused = ITEM_CLOSE;
                self.visible = false;
                SettingsAction::Close
            }
            _ => SettingsAction::None,
        }
    }

    fn adjust(&mut self, delta: isize) -> SettingsAction {
        match self.focused {
            ITEM_BORDER => {
                let idx = self.border_style.index();
                let new = ((idx as isize + delta).rem_euclid(4)) as usize;
                self.border_style = BorderStyle::from_index(new);
                SettingsAction::Changed
            }
            ITEM_STATUS => {
                self.show_status_bar = !self.show_status_bar;
                SettingsAction::Changed
            }
            _ => SettingsAction::None,
        }
    }

    fn activate(&mut self) -> SettingsAction {
        match self.focused {
            ITEM_BORDER => self.adjust(1),
            ITEM_SPLIT_H => {
                self.visible = false;
                SettingsAction::SplitH
            }
            ITEM_SPLIT_V => {
                self.visible = false;
                SettingsAction::SplitV
            }
            ITEM_STATUS => {
                self.show_status_bar = !self.show_status_bar;
                SettingsAction::Changed
            }
            ITEM_CLOSE => {
                self.visible = false;
                SettingsAction::Close
            }
            _ => SettingsAction::None,
        }
    }

    // ─── Overlay Rendering ─────────────────────────────────

    pub fn render_overlay(&self, stdout: &mut io::Stdout, tw: u16, th: u16) -> anyhow::Result<()> {
        if tw < PANEL_W + 2 || th < PANEL_H + 2 {
            return Ok(());
        }

        let ox = tw.saturating_sub(PANEL_W) / 2;
        let oy = th.saturating_sub(PANEL_H) / 2;
        let iw = (PANEL_W - 2) as usize;

        for dy in 0..PANEL_H {
            let y = oy + dy;
            let line = dy as usize;
            let focused = self.line_focused(line);
            let bg = if focused { FOCUS_BG } else { PANEL_BG };

            queue!(stdout, cursor::MoveTo(ox, y))?;

            if dy == 0 {
                self.draw_top(stdout, iw)?;
            } else if dy == PANEL_H - 1 {
                self.draw_bottom(stdout, iw)?;
            } else {
                queue!(
                    stdout,
                    SetForegroundColor(HIGHLIGHT),
                    SetBackgroundColor(PANEL_BG),
                    Print("│")
                )?;
                queue!(stdout, SetBackgroundColor(bg))?;
                self.draw_line(stdout, line, iw, bg)?;
                queue!(
                    stdout,
                    SetForegroundColor(HIGHLIGHT),
                    SetBackgroundColor(PANEL_BG),
                    Print("│")
                )?;
            }
        }

        queue!(stdout, ResetColor, SetAttribute(Attribute::Reset))?;
        Ok(())
    }

    fn draw_top(&self, stdout: &mut io::Stdout, iw: usize) -> anyhow::Result<()> {
        queue!(
            stdout,
            SetForegroundColor(HIGHLIGHT),
            SetBackgroundColor(PANEL_BG),
            Print("╭")
        )?;
        let title = " Settings ";
        let left = (iw - title.len()) / 2;
        let right = iw - left - title.len();
        for _ in 0..left {
            queue!(stdout, Print("─"))?;
        }
        queue!(stdout, SetAttribute(Attribute::Bold), Print(title))?;
        queue!(
            stdout,
            SetAttribute(Attribute::Reset),
            SetForegroundColor(HIGHLIGHT),
            SetBackgroundColor(PANEL_BG)
        )?;
        for _ in 0..right {
            queue!(stdout, Print("─"))?;
        }
        queue!(stdout, Print("╮"))?;
        Ok(())
    }

    fn draw_bottom(&self, stdout: &mut io::Stdout, iw: usize) -> anyhow::Result<()> {
        queue!(
            stdout,
            SetForegroundColor(HIGHLIGHT),
            SetBackgroundColor(PANEL_BG),
            Print("╰")
        )?;
        for _ in 0..iw {
            queue!(stdout, Print("─"))?;
        }
        queue!(stdout, Print("╯"))?;
        Ok(())
    }

    fn line_focused(&self, line: usize) -> bool {
        matches!(
            (self.focused, line),
            (ITEM_BORDER, 3)
                | (ITEM_SPLIT_H, 6)
                | (ITEM_SPLIT_V, 6)
                | (ITEM_STATUS, 9)
                | (ITEM_CLOSE, 18)
        )
    }

    fn draw_line(
        &self,
        stdout: &mut io::Stdout,
        line: usize,
        iw: usize,
        bg: Color,
    ) -> anyhow::Result<()> {
        match line {
            2 => self.draw_header(stdout, "BORDER", iw, bg),
            3 => self.draw_border_options(stdout, iw, bg),
            5 => self.draw_header(stdout, "SPLIT ACTIVE PANE", iw, bg),
            6 => self.draw_split_buttons(stdout, iw, bg),
            8 => self.draw_header(stdout, "DISPLAY", iw, bg),
            9 => self.draw_checkbox(stdout, "Status bar", self.show_status_bar, iw, bg),
            11 => self.draw_header(stdout, "SHORTCUTS", iw, bg),
            12 => self.draw_shortcut(stdout, "Click", "Select pane", iw, bg),
            13 => self.draw_shortcut(stdout, "Click x", "Close pane", iw, bg),
            14 => self.draw_shortcut(stdout, "Ctrl+D/E", "Split H/V (auto-eq)", iw, bg),
            15 => self.draw_shortcut(stdout, "F2", "Equalize sizes", iw, bg),
            16 => self.draw_shortcut(stdout, "Ctrl+G / F1", "Settings", iw, bg),
            17 => self.draw_shortcut(stdout, "Ctrl+\\", "Quit", iw, bg),
            18 => self.draw_close_btn(stdout, iw, bg),
            _ => pad(stdout, iw),
        }
    }

    fn draw_header(
        &self,
        stdout: &mut io::Stdout,
        text: &str,
        iw: usize,
        bg: Color,
    ) -> anyhow::Result<()> {
        let s = format!("  {}", text);
        queue!(
            stdout,
            SetForegroundColor(HEADER_FG),
            SetAttribute(Attribute::Bold),
            Print(&s)
        )?;
        queue!(
            stdout,
            SetAttribute(Attribute::Reset),
            SetBackgroundColor(bg)
        )?;
        pad(stdout, iw.saturating_sub(s.len()))
    }

    fn draw_border_options(
        &self,
        stdout: &mut io::Stdout,
        iw: usize,
        bg: Color,
    ) -> anyhow::Result<()> {
        let styles = [
            (BorderStyle::Single, "Single"),
            (BorderStyle::Rounded, "Rounded"),
            (BorderStyle::Heavy, "Heavy"),
            (BorderStyle::Double, "Double"),
        ];
        queue!(stdout, Print("  "))?;
        let mut used = 2;
        for (i, (style, name)) in styles.iter().enumerate() {
            let sel = self.border_style == *style;
            let fg = if sel { HIGHLIGHT } else { LABEL_FG };
            let icon = if sel { "●" } else { "○" };
            queue!(stdout, SetForegroundColor(fg))?;
            if sel {
                queue!(stdout, SetAttribute(Attribute::Bold))?;
            }
            let e = format!("{} {}", icon, name);
            queue!(stdout, Print(&e))?;
            if sel {
                queue!(
                    stdout,
                    SetAttribute(Attribute::Reset),
                    SetBackgroundColor(bg)
                )?;
            }
            used += e.chars().count();
            if i < styles.len() - 1 {
                queue!(stdout, Print("  "))?;
                used += 2;
            }
        }
        pad(stdout, iw.saturating_sub(used))
    }

    fn draw_split_buttons(
        &self,
        stdout: &mut io::Stdout,
        iw: usize,
        bg: Color,
    ) -> anyhow::Result<()> {
        let b1 = "[ Split ── ]";
        let b2 = "[ Split │ ]";
        let gap = "   ";
        let total = b1.len() + gap.len() + b2.len() + 2;
        queue!(stdout, Print("  "))?;

        let f1 = self.focused == ITEM_SPLIT_H;
        let f2 = self.focused == ITEM_SPLIT_V;

        queue!(
            stdout,
            SetForegroundColor(if f1 { Color::White } else { LABEL_FG })
        )?;
        if f1 {
            queue!(stdout, SetAttribute(Attribute::Bold))?;
        }
        queue!(stdout, Print(b1))?;
        if f1 {
            queue!(
                stdout,
                SetAttribute(Attribute::Reset),
                SetBackgroundColor(bg)
            )?;
        }

        queue!(stdout, SetForegroundColor(LABEL_FG), Print(gap))?;

        queue!(
            stdout,
            SetForegroundColor(if f2 { Color::White } else { LABEL_FG })
        )?;
        if f2 {
            queue!(stdout, SetAttribute(Attribute::Bold))?;
        }
        queue!(stdout, Print(b2))?;
        if f2 {
            queue!(
                stdout,
                SetAttribute(Attribute::Reset),
                SetBackgroundColor(bg)
            )?;
        }

        pad(stdout, iw.saturating_sub(total))
    }

    fn draw_checkbox(
        &self,
        stdout: &mut io::Stdout,
        label: &str,
        checked: bool,
        iw: usize,
        bg: Color,
    ) -> anyhow::Result<()> {
        let icon = if checked { "x" } else { " " };
        let fg = if checked { HIGHLIGHT } else { DIM_FG };
        let s = format!("  [{}] {}", icon, label);
        queue!(
            stdout,
            SetForegroundColor(fg),
            Print(&s),
            SetBackgroundColor(bg)
        )?;
        pad(stdout, iw.saturating_sub(s.len()))
    }

    fn draw_shortcut(
        &self,
        stdout: &mut io::Stdout,
        key: &str,
        desc: &str,
        iw: usize,
        bg: Color,
    ) -> anyhow::Result<()> {
        let s = format!("  {:<14}{}", key, desc);
        queue!(
            stdout,
            SetForegroundColor(DIM_FG),
            Print(&s),
            SetBackgroundColor(bg)
        )?;
        pad(stdout, iw.saturating_sub(s.len()))
    }

    fn draw_close_btn(&self, stdout: &mut io::Stdout, iw: usize, bg: Color) -> anyhow::Result<()> {
        let btn = "[ Close ]";
        let lp = (iw.saturating_sub(btn.len())) / 2;
        pad(stdout, lp)?;
        let fg = if self.focused == ITEM_CLOSE {
            Color::White
        } else {
            LABEL_FG
        };
        queue!(stdout, SetForegroundColor(fg))?;
        if self.focused == ITEM_CLOSE {
            queue!(stdout, SetAttribute(Attribute::Bold))?;
        }
        queue!(stdout, Print(btn))?;
        if self.focused == ITEM_CLOSE {
            queue!(
                stdout,
                SetAttribute(Attribute::Reset),
                SetBackgroundColor(bg)
            )?;
        }
        pad(stdout, iw.saturating_sub(lp + btn.len()))
    }
}

fn pad(stdout: &mut io::Stdout, n: usize) -> anyhow::Result<()> {
    for _ in 0..n {
        queue!(stdout, Print(" "))?;
    }
    Ok(())
}
