use std::io;

use crossterm::event::{KeyCode, KeyEvent};
use crossterm::{cursor, queue, style::*};

use crate::render::BorderStyle;

const PANEL_W: u16 = 86;
const PANEL_H: u16 = 24;

const BACKDROP_BG: Color = Color::Rgb { r: 8, g: 10, b: 14 };
const PANEL_BG: Color = Color::Rgb {
    r: 18,
    g: 22,
    b: 28,
};
const HEADER_FG: Color = Color::Rgb {
    r: 240,
    g: 242,
    b: 245,
};
const SECTION_FG: Color = Color::Rgb {
    r: 120,
    g: 145,
    b: 170,
};
const LABEL_FG: Color = Color::Rgb {
    r: 205,
    g: 214,
    b: 224,
};
const MUTED_FG: Color = Color::Rgb {
    r: 130,
    g: 138,
    b: 148,
};
const HIGHLIGHT: Color = Color::Rgb {
    r: 102,
    g: 217,
    b: 239,
};
const FOCUS_BG: Color = Color::Rgb {
    r: 28,
    g: 36,
    b: 46,
};

const ITEM_SINGLE: usize = 0;
const ITEM_ROUNDED: usize = 1;
const ITEM_HEAVY: usize = 2;
const ITEM_DOUBLE: usize = 3;
const ITEM_SPLIT_H: usize = 4;
const ITEM_SPLIT_V: usize = 5;
const ITEM_STATUS: usize = 6;
const ITEM_CLOSE: usize = 7;
const ITEM_COUNT: usize = 8;

const ITEM_ROWS: [u16; ITEM_COUNT] = [6, 8, 10, 12, 14, 16, 18, 20];

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
            focused: ITEM_ROUNDED,
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            // Focus on the currently selected border style
            self.focused = match self.border_style {
                BorderStyle::Single => ITEM_SINGLE,
                BorderStyle::Rounded => ITEM_ROUNDED,
                BorderStyle::Heavy => ITEM_HEAVY,
                BorderStyle::Double => ITEM_DOUBLE,
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
            KeyCode::Char('1') => self.select_border(BorderStyle::Single, ITEM_SINGLE),
            KeyCode::Char('2') => self.select_border(BorderStyle::Rounded, ITEM_ROUNDED),
            KeyCode::Char('3') => self.select_border(BorderStyle::Heavy, ITEM_HEAVY),
            KeyCode::Char('4') => self.select_border(BorderStyle::Double, ITEM_DOUBLE),
            KeyCode::Enter | KeyCode::Char(' ') => self.activate(self.focused),
            _ => SettingsAction::None,
        }
    }

    pub fn handle_click(&mut self, mx: u16, my: u16, tw: u16, th: u16) -> SettingsAction {
        let (ox, oy) = panel_origin(tw, th);
        if mx < ox || mx >= ox + PANEL_W || my < oy || my >= oy + PANEL_H {
            self.visible = false;
            return SettingsAction::Close;
        }

        for (index, row) in ITEM_ROWS.iter().enumerate() {
            // Each item occupies 2 rows: [row, row+1]
            let y0 = oy + row;
            let y1 = oy + row + 1;
            if my >= y0 && my <= y1 {
                self.focused = index;
                return self.activate(index);
            }
        }

        SettingsAction::None
    }

    pub fn render_overlay(&self, stdout: &mut io::Stdout, tw: u16, th: u16) -> anyhow::Result<()> {
        if tw < PANEL_W + 2 || th < PANEL_H + 2 {
            return Ok(());
        }

        let (ox, oy) = panel_origin(tw, th);

        // Backdrop: single clear + bg color (instead of per-cell Print)
        queue!(
            stdout,
            SetBackgroundColor(BACKDROP_BG),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
        )?;

        // Panel background: use a pre-built row string
        let pw = PANEL_W.min(tw.saturating_sub(2));
        let ph = PANEL_H.min(th.saturating_sub(2));
        let panel_row: String = " ".repeat(pw as usize);
        for y in 0..ph {
            queue!(
                stdout,
                cursor::MoveTo(ox, oy + y),
                SetBackgroundColor(PANEL_BG),
                SetForegroundColor(LABEL_FG),
                Print(&panel_row)
            )?;
        }

        self.draw_title(stdout, ox, oy)?;
        self.draw_table_header(stdout, ox, oy + 4)?;
        self.draw_border_item(
            stdout,
            ox,
            oy + ITEM_ROWS[ITEM_SINGLE],
            ITEM_SINGLE,
            BorderStyle::Single,
            "Single",
            "clean straight joins",
        )?;
        self.draw_border_item(
            stdout,
            ox,
            oy + ITEM_ROWS[ITEM_ROUNDED],
            ITEM_ROUNDED,
            BorderStyle::Rounded,
            "Rounded",
            "soft corners, default",
        )?;
        self.draw_border_item(
            stdout,
            ox,
            oy + ITEM_ROWS[ITEM_HEAVY],
            ITEM_HEAVY,
            BorderStyle::Heavy,
            "Heavy",
            "high contrast dividers",
        )?;
        self.draw_border_item(
            stdout,
            ox,
            oy + ITEM_ROWS[ITEM_DOUBLE],
            ITEM_DOUBLE,
            BorderStyle::Double,
            "Double",
            "dense framed look",
        )?;

        self.draw_divider(stdout, ox, oy + 13)?;
        self.draw_action_item(
            stdout,
            ox,
            oy + ITEM_ROWS[ITEM_SPLIT_H],
            ITEM_SPLIT_H,
            "Split Left | Right",
            "Ctrl+D",
        )?;
        self.draw_action_item(
            stdout,
            ox,
            oy + ITEM_ROWS[ITEM_SPLIT_V],
            ITEM_SPLIT_V,
            "Split Top / Bottom",
            "Ctrl+E",
        )?;

        self.draw_divider(stdout, ox, oy + 17)?;
        self.draw_toggle_item(stdout, ox, oy + ITEM_ROWS[ITEM_STATUS])?;
        self.draw_close_item(stdout, ox, oy + ITEM_ROWS[ITEM_CLOSE])?;

        queue!(stdout, ResetColor, SetAttribute(Attribute::Reset))?;
        Ok(())
    }

    fn draw_title(&self, stdout: &mut io::Stdout, ox: u16, oy: u16) -> anyhow::Result<()> {
        queue!(
            stdout,
            cursor::MoveTo(ox + 4, oy + 1),
            SetBackgroundColor(PANEL_BG),
            SetForegroundColor(HEADER_FG),
            SetAttribute(Attribute::Bold),
            Print("SETTINGS :: CONTROL TABLE"),
            SetAttribute(Attribute::Reset),
            SetForegroundColor(MUTED_FG),
            cursor::MoveTo(ox + 4, oy + 2),
            Print(
                "j/k or Arrows: move  h/l: adjust  Enter: apply  1-4: border style  q/Esc: close"
            )
        )?;
        Ok(())
    }

    fn draw_table_header(&self, stdout: &mut io::Stdout, ox: u16, y: u16) -> anyhow::Result<()> {
        let rule = "─".repeat((PANEL_W - 8) as usize);
        queue!(
            stdout,
            cursor::MoveTo(ox + 4, y),
            SetBackgroundColor(PANEL_BG),
            SetForegroundColor(SECTION_FG),
            SetAttribute(Attribute::Bold),
            Print("GROUP    KEY      ITEM                      STATE     DETAIL"),
            cursor::MoveTo(ox + 4, y + 1),
            Print(&rule),
            SetAttribute(Attribute::Reset)
        )?;
        Ok(())
    }

    fn draw_divider(&self, stdout: &mut io::Stdout, ox: u16, y: u16) -> anyhow::Result<()> {
        let rule = "─".repeat((PANEL_W - 8) as usize);
        queue!(
            stdout,
            cursor::MoveTo(ox + 4, y),
            SetBackgroundColor(PANEL_BG),
            SetForegroundColor(SECTION_FG),
            Print(&rule)
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_border_item(
        &self,
        stdout: &mut io::Stdout,
        ox: u16,
        y: u16,
        item: usize,
        style: BorderStyle,
        title: &str,
        desc: &str,
    ) -> anyhow::Result<()> {
        let selected = self.border_style == style;
        let focused = self.focused == item;
        self.draw_table_row(
            stdout,
            ox,
            y,
            focused,
            "VISUAL",
            match item {
                ITEM_SINGLE => "1",
                ITEM_ROUNDED => "2",
                ITEM_HEAVY => "3",
                _ => "4",
            },
            title,
            if selected { "ACTIVE" } else { "READY" },
            desc,
        )
    }

    fn draw_action_item(
        &self,
        stdout: &mut io::Stdout,
        ox: u16,
        y: u16,
        item: usize,
        title: &str,
        hint: &str,
    ) -> anyhow::Result<()> {
        self.draw_table_row(
            stdout,
            ox,
            y,
            self.focused == item,
            "PANE",
            "Enter",
            title,
            "ACTION",
            hint,
        )
    }

    fn draw_toggle_item(&self, stdout: &mut io::Stdout, ox: u16, y: u16) -> anyhow::Result<()> {
        self.draw_table_row(
            stdout,
            ox,
            y,
            self.focused == ITEM_STATUS,
            "UI",
            "Space",
            "Status Bar",
            if self.show_status_bar { "ON" } else { "OFF" },
            "footer hints and pane index",
        )
    }

    fn draw_close_item(&self, stdout: &mut io::Stdout, ox: u16, y: u16) -> anyhow::Result<()> {
        self.draw_table_row(
            stdout,
            ox,
            y,
            self.focused == ITEM_CLOSE,
            "SYS",
            "q/Esc",
            "Close Settings",
            "EXIT",
            "return to workspace view",
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_table_row(
        &self,
        stdout: &mut io::Stdout,
        ox: u16,
        y: u16,
        focused: bool,
        group: &str,
        key: &str,
        title: &str,
        state: &str,
        detail: &str,
    ) -> anyhow::Result<()> {
        let bg = if focused { FOCUS_BG } else { PANEL_BG };
        let fg = if focused { Color::White } else { LABEL_FG };
        let line_w = PANEL_W.saturating_sub(8);
        let key_fg = if focused { HIGHLIGHT } else { SECTION_FG };
        let state_fg = match state {
            "ACTIVE" | "ON" => HIGHLIGHT,
            "EXIT" => Color::Rgb {
                r: 255,
                g: 120,
                b: 120,
            },
            _ => SECTION_FG,
        };

        for dy in 0..2 {
            queue!(
                stdout,
                cursor::MoveTo(ox + 3, y + dy),
                SetBackgroundColor(bg),
                SetForegroundColor(fg)
            )?;
            // Left accent bar for focused items
            if focused {
                queue!(
                    stdout,
                    SetForegroundColor(HIGHLIGHT),
                    Print("▎"),
                    SetForegroundColor(fg)
                )?;
                for _ in 1..line_w {
                    queue!(stdout, Print(" "))?;
                }
            } else {
                for _ in 0..line_w {
                    queue!(stdout, Print(" "))?;
                }
            }
        }

        queue!(
            stdout,
            cursor::MoveTo(ox + 5, y),
            SetBackgroundColor(bg),
            SetForegroundColor(key_fg),
            Print(if focused { "›" } else { " " }),
            cursor::MoveTo(ox + 7, y),
            Print(format!("{:<8}", group)),
            cursor::MoveTo(ox + 16, y),
            SetForegroundColor(key_fg),
            Print(format!("{:<8}", key)),
            cursor::MoveTo(ox + 26, y),
            SetForegroundColor(fg),
            SetAttribute(Attribute::Bold),
            Print(format!("{:<24}", title)),
            SetAttribute(Attribute::Reset),
            cursor::MoveTo(ox + 52, y),
            SetForegroundColor(state_fg),
            Print(format!("{:<8}", state)),
            cursor::MoveTo(ox + 62, y),
            SetForegroundColor(MUTED_FG),
            Print(detail)
        )?;

        Ok(())
    }

    fn adjust(&mut self, delta: isize) -> SettingsAction {
        match self.focused {
            ITEM_SINGLE | ITEM_ROUNDED | ITEM_HEAVY | ITEM_DOUBLE => {
                let order = [
                    BorderStyle::Single,
                    BorderStyle::Rounded,
                    BorderStyle::Heavy,
                    BorderStyle::Double,
                ];
                let index = order
                    .iter()
                    .position(|style| *style == self.border_style)
                    .unwrap_or(1);
                let next = ((index as isize + delta).rem_euclid(order.len() as isize)) as usize;
                self.border_style = order[next];
                self.focused = next;
                SettingsAction::Changed
            }
            ITEM_STATUS => {
                self.show_status_bar = !self.show_status_bar;
                SettingsAction::Changed
            }
            _ => SettingsAction::None,
        }
    }

    fn activate(&mut self, item: usize) -> SettingsAction {
        match item {
            ITEM_SINGLE => self.select_border(BorderStyle::Single, ITEM_SINGLE),
            ITEM_ROUNDED => self.select_border(BorderStyle::Rounded, ITEM_ROUNDED),
            ITEM_HEAVY => self.select_border(BorderStyle::Heavy, ITEM_HEAVY),
            ITEM_DOUBLE => self.select_border(BorderStyle::Double, ITEM_DOUBLE),
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

    fn select_border(&mut self, style: BorderStyle, focused: usize) -> SettingsAction {
        self.border_style = style;
        self.focused = focused;
        SettingsAction::Changed
    }
}

fn panel_origin(tw: u16, th: u16) -> (u16, u16) {
    (
        tw.saturating_sub(PANEL_W) / 2,
        th.saturating_sub(PANEL_H) / 2,
    )
}
