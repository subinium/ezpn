use std::collections::HashMap;
use std::io;

use crossterm::{
    cursor, queue,
    style::{
        Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
    },
    terminal::{self, ClearType},
};

use crate::layout::{Layout, Rect};
use crate::pane::Pane;

const ACTIVE_COLOR: Color = Color::Cyan;
const BORDER_COLOR: Color = Color::DarkGrey;
const STATUS_BG: Color = Color::Rgb {
    r: 30,
    g: 30,
    b: 30,
};
const STATUS_FG: Color = Color::White;
const CLOSE_COLOR: Color = Color::DarkRed;
const DEAD_FG: Color = Color::DarkGrey;
const DRAG_COLOR: Color = Color::Yellow;

// ─── Border Styles ─────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
pub enum BorderStyle {
    Single,
    Rounded,
    Heavy,
    Double,
}

impl BorderStyle {
    pub fn index(self) -> usize {
        match self {
            Self::Single => 0,
            Self::Rounded => 1,
            Self::Heavy => 2,
            Self::Double => 3,
        }
    }

    pub fn from_index(i: usize) -> Self {
        match i % 4 {
            0 => Self::Single,
            1 => Self::Rounded,
            2 => Self::Heavy,
            _ => Self::Double,
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "single" => Some(Self::Single),
            "rounded" => Some(Self::Rounded),
            "heavy" => Some(Self::Heavy),
            "double" => Some(Self::Double),
            _ => None,
        }
    }

    pub fn chars(self) -> BorderChars {
        match self {
            Self::Single => BorderChars {
                h: "─",
                v: "│",
                tl: "┌",
                tr: "┐",
                bl: "└",
                br: "┘",
                tj: "┬",
                bj: "┴",
                lj: "├",
                rj: "┤",
                xj: "┼",
            },
            Self::Rounded => BorderChars {
                h: "─",
                v: "│",
                tl: "╭",
                tr: "╮",
                bl: "╰",
                br: "╯",
                tj: "┬",
                bj: "┴",
                lj: "├",
                rj: "┤",
                xj: "┼",
            },
            Self::Heavy => BorderChars {
                h: "━",
                v: "┃",
                tl: "┏",
                tr: "┓",
                bl: "┗",
                br: "┛",
                tj: "┳",
                bj: "┻",
                lj: "┣",
                rj: "┫",
                xj: "╋",
            },
            Self::Double => BorderChars {
                h: "═",
                v: "║",
                tl: "╔",
                tr: "╗",
                bl: "╚",
                br: "╝",
                tj: "╦",
                bj: "╩",
                lj: "╠",
                rj: "╣",
                xj: "╬",
            },
        }
    }
}

pub struct BorderChars {
    pub h: &'static str,
    pub v: &'static str,
    pub tl: &'static str,
    pub tr: &'static str,
    pub bl: &'static str,
    pub br: &'static str,
    pub tj: &'static str,
    pub bj: &'static str,
    pub lj: &'static str,
    pub rj: &'static str,
    pub xj: &'static str,
}

// ─── Border Map ────────────────────────────────────────────

struct BorderMap {
    cells: HashMap<(u16, u16), [bool; 4]>,
}

impl BorderMap {
    fn new() -> Self {
        Self {
            cells: HashMap::new(),
        }
    }

    fn add_h_line(&mut self, x1: u16, x2: u16, y: u16) {
        for x in x1..=x2 {
            let e = self.cells.entry((x, y)).or_insert([false; 4]);
            if x > x1 {
                e[2] = true;
            }
            if x < x2 {
                e[3] = true;
            }
        }
    }

    fn add_v_line(&mut self, x: u16, y1: u16, y2: u16) {
        for y in y1..=y2 {
            let e = self.cells.entry((x, y)).or_insert([false; 4]);
            if y > y1 {
                e[0] = true;
            }
            if y < y2 {
                e[1] = true;
            }
        }
    }
}

fn border_char<'a>(flags: &[bool; 4], ch: &'a BorderChars) -> &'a str {
    match (flags[2], flags[3], flags[0], flags[1]) {
        (true, true, true, true) => ch.xj,
        (true, true, false, false) => ch.h,
        (false, false, true, true) => ch.v,
        (false, true, false, true) => ch.tl,
        (true, false, false, true) => ch.tr,
        (false, true, true, false) => ch.bl,
        (true, false, true, false) => ch.br,
        (true, true, false, true) => ch.tj,
        (true, true, true, false) => ch.bj,
        (true, false, true, true) => ch.rj,
        (false, true, true, true) => ch.lj,
        (_, true, false, false) | (true, _, false, false) => ch.h,
        (false, false, _, true) | (false, false, true, _) => ch.v,
        _ => " ",
    }
}

// ─── Rendering ─────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn render_panes(
    stdout: &mut io::Stdout,
    panes: &HashMap<usize, Pane>,
    layout: &Layout,
    active_id: usize,
    border_style: BorderStyle,
    show_status_bar: bool,
    term_w: u16,
    term_h: u16,
    dragging_sep: bool,
) -> anyhow::Result<()> {
    queue!(stdout, terminal::Clear(ClearType::All))?;

    let chars = border_style.chars();
    let status_h = if show_status_bar { 1u16 } else { 0 };
    let border_h = term_h.saturating_sub(status_h);

    let outer = Rect {
        x: 0,
        y: 0,
        w: term_w,
        h: border_h,
    };
    let inner = Rect {
        x: 1,
        y: 1,
        w: term_w.saturating_sub(2),
        h: border_h.saturating_sub(2),
    };

    // Terminal too small
    if inner.w == 0 || inner.h == 0 {
        let msg = "Terminal too small";
        let mx = term_w.saturating_sub(msg.len() as u16) / 2;
        let my = term_h / 2;
        queue!(
            stdout,
            cursor::MoveTo(mx, my),
            SetForegroundColor(Color::Red),
            Print(msg)
        )?;
        queue!(stdout, ResetColor)?;
        return Ok(());
    }

    let pane_rects = layout.pane_rects(&inner);
    let separators = layout.separators(&inner, &outer);

    // Build border map
    let mut bmap = BorderMap::new();
    bmap.add_h_line(outer.x, outer.x + outer.w - 1, outer.y);
    bmap.add_h_line(outer.x, outer.x + outer.w - 1, outer.y + outer.h - 1);
    bmap.add_v_line(outer.x, outer.y, outer.y + outer.h - 1);
    bmap.add_v_line(outer.x + outer.w - 1, outer.y, outer.y + outer.h - 1);
    for sep in &separators {
        if sep.horizontal {
            bmap.add_h_line(sep.x, sep.x + sep.length - 1, sep.y);
        } else {
            bmap.add_v_line(sep.x, sep.y, sep.y + sep.length - 1);
        }
    }

    // Render border map
    let active_rect = pane_rects.get(&active_id);
    for (&(x, y), flags) in &bmap.cells {
        let is_active = active_rect
            .map(|r| is_pane_border(x, y, r))
            .unwrap_or(false);
        let color = if dragging_sep {
            DRAG_COLOR
        } else if is_active {
            ACTIVE_COLOR
        } else {
            BORDER_COLOR
        };
        queue!(
            stdout,
            cursor::MoveTo(x, y),
            SetForegroundColor(color),
            Print(border_char(flags, &chars))
        )?;
    }

    // Pane titles + content
    let ids = layout.pane_ids();
    for (display_idx, &pid) in ids.iter().enumerate() {
        if let Some(rect) = pane_rects.get(&pid) {
            let is_active = pid == active_id;
            let is_alive = panes.get(&pid).is_some_and(|p| p.is_alive());
            draw_pane_title(stdout, rect, display_idx, is_active, is_alive, &chars)?;
            if let Some(pane) = panes.get(&pid) {
                draw_content(stdout, pane, rect, is_alive)?;
            }
            // Dead pane overlay
            if !is_alive {
                draw_dead_overlay(stdout, rect)?;
            }
        }
    }

    // Status bar
    if show_status_bar {
        let active_idx = ids.iter().position(|&id| id == active_id).unwrap_or(0);
        draw_status_bar(stdout, term_w, term_h, active_idx, ids.len())?;
    }

    // Cursor
    if let (Some(rect), Some(pane)) = (pane_rects.get(&active_id), panes.get(&active_id)) {
        if pane.is_alive() {
            let screen = pane.screen();
            let (cr, cc) = screen.cursor_position();
            if cc < rect.w && cr < rect.h {
                queue!(
                    stdout,
                    cursor::MoveTo(rect.x + cc, rect.y + cr),
                    cursor::Show
                )?;
            }
        }
    }

    queue!(stdout, ResetColor, SetAttribute(Attribute::Reset))?;
    Ok(())
}

fn is_pane_border(x: u16, y: u16, r: &Rect) -> bool {
    let top = r.y.saturating_sub(1);
    let bot = r.y + r.h;
    let left = r.x.saturating_sub(1);
    let right = r.x + r.w;
    (y == top || y == bot) && x >= left && x <= right
        || (x == left || x == right) && y >= top && y <= bot
}

fn draw_pane_title(
    stdout: &mut io::Stdout,
    rect: &Rect,
    idx: usize,
    is_active: bool,
    is_alive: bool,
    chars: &BorderChars,
) -> anyhow::Result<()> {
    let title_y = rect.y.saturating_sub(1);
    let title_x = rect.x;
    let avail = rect.w as usize;
    if avail < 4 {
        return Ok(());
    }

    let title = if is_alive {
        format!(" {} ", idx + 1)
    } else {
        format!(" {} [exited] ", idx + 1)
    };
    let tlen = title.len();
    // Always show close button (alive or dead) so dead panes can be removed
    let show_close = avail >= tlen + 4;
    let close_len = if show_close { 2 } else { 0 };

    if avail >= tlen + 1 + close_len {
        let color = if is_active {
            ACTIVE_COLOR
        } else {
            BORDER_COLOR
        };
        queue!(
            stdout,
            cursor::MoveTo(title_x, title_y),
            SetForegroundColor(color)
        )?;
        queue!(stdout, Print(chars.h))?;

        if is_active {
            queue!(
                stdout,
                SetForegroundColor(Color::White),
                SetAttribute(Attribute::Bold)
            )?;
        }
        if !is_alive {
            queue!(stdout, SetForegroundColor(DEAD_FG))?;
        }
        queue!(stdout, Print(&title))?;
        queue!(
            stdout,
            SetAttribute(Attribute::Reset),
            SetForegroundColor(color)
        )?;

        let fill = avail - tlen - 1 - close_len;
        for _ in 0..fill {
            queue!(stdout, Print(chars.h))?;
        }

        if show_close {
            queue!(stdout, SetForegroundColor(CLOSE_COLOR), Print(" ×"))?;
        }
    }

    queue!(stdout, ResetColor)?;
    Ok(())
}

/// Draw dimmed overlay on dead panes with centered message.
fn draw_dead_overlay(stdout: &mut io::Stdout, rect: &Rect) -> anyhow::Result<()> {
    if rect.w < 5 || rect.h < 1 {
        return Ok(());
    }
    let msg = "[press Enter to respawn]";
    let my = rect.y + rect.h / 2;
    let mx = rect.x + rect.w.saturating_sub(msg.len() as u16) / 2;
    queue!(
        stdout,
        cursor::MoveTo(mx, my),
        SetForegroundColor(Color::DarkGrey),
        SetAttribute(Attribute::Italic),
        Print(msg),
        SetAttribute(Attribute::Reset),
    )?;
    Ok(())
}

fn draw_content(
    stdout: &mut io::Stdout,
    pane: &Pane,
    rect: &Rect,
    is_alive: bool,
) -> anyhow::Result<()> {
    let screen = pane.screen();
    if rect.w == 0 || rect.h == 0 {
        return Ok(());
    }

    let mut last_fg = Color::Reset;
    let mut last_bg = Color::Reset;
    let mut has_attrs = false;

    for r in 0..rect.h {
        queue!(stdout, cursor::MoveTo(rect.x, rect.y + r))?;
        for c in 0..rect.w {
            if let Some(cell) = screen.cell(r, c) {
                let mut fg = vt100_to_crossterm(cell.fgcolor());
                let bg = vt100_to_crossterm(cell.bgcolor());

                // Dim dead pane content
                if !is_alive {
                    fg = Color::DarkGrey;
                }

                if fg != last_fg {
                    queue!(stdout, SetForegroundColor(fg))?;
                    last_fg = fg;
                }
                if bg != last_bg {
                    queue!(stdout, SetBackgroundColor(bg))?;
                    last_bg = bg;
                }

                let ca = cell.bold() || cell.italic() || cell.underline() || cell.inverse();
                if ca && is_alive {
                    if cell.bold() {
                        queue!(stdout, SetAttribute(Attribute::Bold))?;
                    }
                    if cell.italic() {
                        queue!(stdout, SetAttribute(Attribute::Italic))?;
                    }
                    if cell.underline() {
                        queue!(stdout, SetAttribute(Attribute::Underlined))?;
                    }
                    if cell.inverse() {
                        queue!(stdout, SetAttribute(Attribute::Reverse))?;
                    }
                    has_attrs = true;
                } else if has_attrs {
                    queue!(stdout, SetAttribute(Attribute::Reset))?;
                    last_fg = Color::Reset;
                    last_bg = Color::Reset;
                    has_attrs = false;
                }

                let contents = cell.contents();
                queue!(
                    stdout,
                    Print(if contents.is_empty() {
                        " ".to_string()
                    } else {
                        contents
                    })
                )?;
            } else {
                queue!(stdout, Print(" "))?;
            }
        }
    }

    queue!(stdout, ResetColor, SetAttribute(Attribute::Reset))?;
    Ok(())
}

fn draw_status_bar(
    stdout: &mut io::Stdout,
    term_w: u16,
    term_h: u16,
    active_idx: usize,
    total: usize,
) -> anyhow::Result<()> {
    let y = term_h - 1;
    let w = term_w as usize;

    queue!(
        stdout,
        cursor::MoveTo(0, y),
        SetBackgroundColor(STATUS_BG),
        SetForegroundColor(STATUS_FG)
    )?;
    for _ in 0..w {
        queue!(stdout, Print(" "))?;
    }

    // Left: pane info
    queue!(
        stdout,
        cursor::MoveTo(1, y),
        SetForegroundColor(ACTIVE_COLOR),
        SetAttribute(Attribute::Bold)
    )?;
    let left = format!("Pane {}/{}", active_idx + 1, total);
    queue!(stdout, Print(&left))?;
    let left_end = 1 + left.len();

    // Right: hints (truncate if too narrow)
    queue!(
        stdout,
        SetAttribute(Attribute::Reset),
        SetBackgroundColor(STATUS_BG)
    )?;
    let hints = [
        "Ctrl+D/E:split",
        "F2:equalize",
        "Ctrl+G:settings",
        "Ctrl+\\:quit",
    ];
    let mut right_str = String::new();
    for hint in hints.iter() {
        let candidate = if right_str.is_empty() {
            hint.to_string()
        } else {
            format!("{}  {}", right_str, hint)
        };
        if candidate.len() + left_end + 4 < w {
            right_str = candidate;
        } else {
            break;
        }
    }
    if !right_str.is_empty() {
        let rx = (w as u16).saturating_sub(right_str.len() as u16 + 1);
        queue!(
            stdout,
            cursor::MoveTo(rx, y),
            SetForegroundColor(Color::Grey),
            Print(&right_str)
        )?;
    }

    queue!(stdout, ResetColor, SetAttribute(Attribute::Reset))?;
    Ok(())
}

/// Close button hit detection — 2-cell wide hit area for " ×".
pub fn close_button_hit(x: u16, y: u16, layout: &Layout, inner: &Rect) -> Option<usize> {
    let rects = layout.pane_rects(inner);
    for (&pid, rect) in &rects {
        if rect.w < 6 {
            continue;
        }
        let btn_x = rect.x + rect.w - 1; // × position
        let btn_y = rect.y.saturating_sub(1);
        // 2-cell hit area: the space before × and × itself
        if y == btn_y && (x == btn_x || x == btn_x.saturating_sub(1)) {
            return Some(pid);
        }
    }
    None
}

fn vt100_to_crossterm(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::AnsiValue(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb { r, g, b },
    }
}
