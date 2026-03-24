use std::collections::{HashMap, HashSet};
use std::io;

use crossterm::{
    cursor, queue,
    style::{
        Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
    },
    terminal::{self, ClearType},
};
use serde::{Deserialize, Serialize};

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
const MUTED_FG: Color = Color::Rgb {
    r: 100,
    g: 100,
    b: 110,
};

// ─── Border Styles ─────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BorderStyle {
    Single,
    Rounded,
    Heavy,
    Double,
}

impl BorderStyle {
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

pub struct BorderCell {
    pub x: u16,
    pub y: u16,
    pub flags: [bool; 4],
}

pub struct BorderCache {
    inner: Rect,
    pane_rects: HashMap<usize, Rect>,
    cells: Vec<BorderCell>,
}

impl BorderCache {
    pub fn pane_rects(&self) -> &HashMap<usize, Rect> {
        &self.pane_rects
    }

    pub fn inner(&self) -> &Rect {
        &self.inner
    }
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

pub fn build_border_cache(
    layout: &Layout,
    show_status_bar: bool,
    term_w: u16,
    term_h: u16,
) -> BorderCache {
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

    let pane_rects = layout.pane_rects(&inner);
    let separators = layout.separators(&inner, &outer);

    let mut bmap = BorderMap::new();
    if outer.w > 0 && outer.h > 0 {
        bmap.add_h_line(outer.x, outer.x + outer.w - 1, outer.y);
        bmap.add_h_line(outer.x, outer.x + outer.w - 1, outer.y + outer.h - 1);
        bmap.add_v_line(outer.x, outer.y, outer.y + outer.h - 1);
        bmap.add_v_line(outer.x + outer.w - 1, outer.y, outer.y + outer.h - 1);
    }
    for sep in &separators {
        if sep.horizontal {
            bmap.add_h_line(sep.x, sep.x + sep.length - 1, sep.y);
        } else {
            bmap.add_v_line(sep.x, sep.y, sep.y + sep.length - 1);
        }
    }

    let cells = bmap
        .cells
        .into_iter()
        .map(|((x, y), flags)| BorderCell { x, y, flags })
        .collect();

    BorderCache {
        inner,
        pane_rects,
        cells,
    }
}

// ─── Rendering ─────────────────────────────────────────────

/// Selection range for a specific pane: (pane_id, start_row, start_col, end_row, end_col).
/// Coordinates are normalized (start <= end).
pub type PaneSelection = Option<(usize, u16, u16, u16, u16)>;

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
    border_cache: &BorderCache,
    dirty_panes: &HashSet<usize>,
    full_redraw: bool,
    selection: PaneSelection,
) -> anyhow::Result<()> {
    queue!(stdout, cursor::Hide)?;

    if full_redraw {
        queue!(stdout, terminal::Clear(ClearType::All))?;
    }

    let chars = border_style.chars();
    let inner = border_cache.inner();

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

    let pane_rects = border_cache.pane_rects();

    if full_redraw {
        let active_rect = pane_rects.get(&active_id);
        for cell in &border_cache.cells {
            let is_active = active_rect
                .map(|r| is_pane_border(cell.x, cell.y, r))
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
                cursor::MoveTo(cell.x, cell.y),
                SetForegroundColor(color),
                Print(border_char(&cell.flags, &chars))
            )?;
        }
    }

    // Pane titles + content
    let ids = layout.pane_ids();
    for (display_idx, &pid) in ids.iter().enumerate() {
        if !full_redraw && !dirty_panes.contains(&pid) {
            continue;
        }
        if let Some(rect) = pane_rects.get(&pid) {
            if !full_redraw {
                clear_rect(stdout, rect)?;
                clear_title(stdout, rect)?;
            }
            let is_active = pid == active_id;
            let pane_ref = panes.get(&pid);
            let is_alive = pane_ref.is_some_and(|p| p.is_alive());
            let label = pane_ref.map(|p| p.launch_label("")).unwrap_or_default();
            let is_scrolled = pane_ref.is_some_and(|p| p.is_scrolled());
            let exit_code = pane_ref.and_then(|p| p.exit_code());
            draw_pane_title(
                stdout,
                rect,
                display_idx,
                is_active,
                is_alive,
                &label,
                is_scrolled,
                &chars,
                exit_code,
            )?;
            if let Some(pane) = panes.get(&pid) {
                let pane_sel = selection
                    .filter(|(sel_pid, ..)| *sel_pid == pid)
                    .map(|(_, sr, sc, er, ec)| (sr, sc, er, ec));
                draw_content(stdout, pane, rect, is_alive, pane_sel)?;
            }
            // Dead pane overlay
            if !is_alive {
                draw_dead_overlay(stdout, rect)?;
            }
        }
    }

    // Status bar
    if show_status_bar && full_redraw {
        let active_idx = ids.iter().position(|&id| id == active_id).unwrap_or(0);
        draw_status_bar(stdout, term_w, term_h, active_idx, ids.len(), "")?;
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

fn clear_rect(stdout: &mut io::Stdout, rect: &Rect) -> anyhow::Result<()> {
    if rect.w == 0 || rect.h == 0 {
        return Ok(());
    }

    for row in 0..rect.h {
        queue!(stdout, cursor::MoveTo(rect.x, rect.y + row))?;
        for _ in 0..rect.w {
            queue!(stdout, ResetColor, Print(" "))?;
        }
    }

    Ok(())
}

fn clear_title(stdout: &mut io::Stdout, rect: &Rect) -> anyhow::Result<()> {
    if rect.w == 0 {
        return Ok(());
    }

    let y = rect.y.saturating_sub(1);
    let x = rect.x;
    let width = rect.w;
    queue!(stdout, cursor::MoveTo(x, y))?;
    for _ in 0..width {
        queue!(stdout, ResetColor, Print(" "))?;
    }
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

#[allow(clippy::too_many_arguments)]
fn draw_pane_title(
    stdout: &mut io::Stdout,
    rect: &Rect,
    idx: usize,
    is_active: bool,
    is_alive: bool,
    label: &str,
    is_scrolled: bool,
    chars: &BorderChars,
    exit_code: Option<u32>,
) -> anyhow::Result<()> {
    let title_y = rect.y.saturating_sub(1);
    let title_x = rect.x;
    let avail = rect.w as usize;
    if avail < 4 {
        return Ok(());
    }

    let scroll_ind = if is_scrolled { " [SCROLL]" } else { "" };
    let title = if !is_alive {
        match exit_code {
            Some(code) => format!(" {} [exit {}] ", idx + 1, code),
            None => format!(" {} [exited] ", idx + 1),
        }
    } else if label.is_empty() || avail < 12 {
        format!(" {}{} ", idx + 1, scroll_ind)
    } else {
        // Truncate label to fit
        let max_label = avail.saturating_sub(8 + scroll_ind.len()); // room for " N: ... "
        let short = truncate_label(label, max_label);
        format!(" {}:{}{} ", idx + 1, short, scroll_ind)
    };
    let tlen = title.len();
    // Buttons: [━] [┃] [×] — 11 display columns total
    let show_buttons = avail >= tlen + 13;
    let btn_len = if show_buttons { 11 } else { 0 };
    // Fallback: just close button
    let show_close = !show_buttons && avail >= tlen + 4;
    let close_len = if show_close { 2 } else { 0 };
    let right_len = btn_len + close_len;

    if avail >= tlen + 1 + right_len {
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

        let fill = avail - tlen - 1 - right_len;
        for _ in 0..fill {
            queue!(stdout, Print(chars.h))?;
        }

        if show_buttons {
            let btn_fg = if is_active { MUTED_FG } else { BORDER_COLOR };
            queue!(
                stdout,
                SetForegroundColor(btn_fg),
                Print("[━] [┃] "),
                SetForegroundColor(CLOSE_COLOR),
                Print("[×]")
            )?;
        } else if show_close {
            queue!(stdout, SetForegroundColor(CLOSE_COLOR), Print(" ×"))?;
        }
    }

    queue!(stdout, ResetColor)?;
    Ok(())
}

fn truncate_label(label: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut out = String::new();
    for ch in label.chars().take(max_chars) {
        out.push(ch);
    }
    out
}

/// Draw dimmed overlay on dead panes with centered message.
fn draw_dead_overlay(stdout: &mut io::Stdout, rect: &Rect) -> anyhow::Result<()> {
    if rect.w < 5 || rect.h < 1 {
        return Ok(());
    }

    // Dim background for dead pane
    let dim_bg = Color::Rgb {
        r: 10,
        g: 10,
        b: 14,
    };
    for row in 0..rect.h {
        queue!(
            stdout,
            cursor::MoveTo(rect.x, rect.y + row),
            SetBackgroundColor(dim_bg),
        )?;
        for _ in 0..rect.w {
            queue!(stdout, Print(" "))?;
        }
    }

    let my = rect.y + rect.h / 2;

    // "Process exited" label
    if rect.h >= 3 {
        let label = "Process exited";
        let lx = rect.x + rect.w.saturating_sub(label.len() as u16) / 2;
        queue!(
            stdout,
            cursor::MoveTo(lx, my.saturating_sub(1)),
            SetBackgroundColor(dim_bg),
            SetForegroundColor(Color::Rgb {
                r: 120,
                g: 60,
                b: 60
            }),
            SetAttribute(Attribute::Bold),
            Print(label),
            SetAttribute(Attribute::Reset),
        )?;
    }

    // "Press Enter to respawn" hint
    let msg = "Press Enter to respawn";
    let mx = rect.x + rect.w.saturating_sub(msg.len() as u16) / 2;
    queue!(
        stdout,
        cursor::MoveTo(mx, my),
        SetBackgroundColor(dim_bg),
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
    selection: Option<(u16, u16, u16, u16)>,
) -> anyhow::Result<()> {
    let screen = pane.screen();
    if rect.w == 0 || rect.h == 0 {
        return Ok(());
    }

    let mut last_fg = Color::Reset;
    let mut last_bg = Color::Reset;
    let mut has_attrs = false;
    // Reusable buffer: batch consecutive plain-text cells into one Print call
    let mut buf = String::with_capacity(rect.w as usize);

    for r in 0..rect.h {
        queue!(stdout, cursor::MoveTo(rect.x, rect.y + r))?;
        buf.clear();

        for c in 0..rect.w {
            let is_selected = selection.is_some_and(|(sr, sc, er, ec)| {
                if r < sr || r > er {
                    false
                } else if r == sr && r == er {
                    c >= sc && c <= ec
                } else if r == sr {
                    c >= sc
                } else if r == er {
                    c <= ec
                } else {
                    true
                }
            });

            if let Some(cell) = screen.cell(r, c) {
                // Skip wide character continuation cells — the wide char itself
                // already occupies 2 display columns when printed.
                if cell.is_wide_continuation() {
                    continue;
                }

                // If this is a wide char at the last column, it would overflow.
                // Print a space instead to stay within bounds.
                if cell.is_wide() && c + 1 >= rect.w {
                    buf.push(' ');
                    continue;
                }

                let (mut fg, bg) = if is_selected {
                    // Invert colors for selection highlight
                    let f = vt100_to_crossterm(cell.bgcolor());
                    let b = vt100_to_crossterm(cell.fgcolor());
                    // Use readable defaults if both are Reset
                    (
                        if f == Color::Reset { Color::Black } else { f },
                        if b == Color::Reset { Color::White } else { b },
                    )
                } else {
                    (
                        vt100_to_crossterm(cell.fgcolor()),
                        vt100_to_crossterm(cell.bgcolor()),
                    )
                };
                if !is_alive {
                    fg = Color::DarkGrey;
                }

                let ca = cell.bold() || cell.italic() || cell.underline() || cell.inverse();
                let style_changed =
                    fg != last_fg || bg != last_bg || (ca && !has_attrs) || (!ca && has_attrs);

                // Flush buffer if style changes
                if style_changed && !buf.is_empty() {
                    queue!(stdout, Print(&buf))?;
                    buf.clear();
                }

                if fg != last_fg {
                    queue!(stdout, SetForegroundColor(fg))?;
                    last_fg = fg;
                }
                if bg != last_bg {
                    queue!(stdout, SetBackgroundColor(bg))?;
                    last_bg = bg;
                }

                if ca && is_alive {
                    if !has_attrs {
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
                    }
                    // Attributed cells: print individually (attrs may differ per cell)
                    let contents = cell.contents();
                    if contents.is_empty() {
                        queue!(stdout, Print(" "))?;
                    } else {
                        queue!(stdout, Print(contents))?;
                    }
                } else {
                    if has_attrs {
                        queue!(stdout, SetAttribute(Attribute::Reset))?;
                        last_fg = Color::Reset;
                        last_bg = Color::Reset;
                        has_attrs = false;
                    }
                    // Plain cells: batch into buffer
                    let contents = cell.contents();
                    if contents.is_empty() {
                        buf.push(' ');
                    } else {
                        buf.push_str(&contents);
                    }
                }
            } else {
                buf.push(' ');
            }
        }

        // Flush remaining buffer at end of row
        if !buf.is_empty() {
            queue!(stdout, Print(&buf))?;
            buf.clear();
        }
    }

    queue!(stdout, ResetColor, SetAttribute(Attribute::Reset))?;
    Ok(())
}

pub fn draw_status_bar(
    stdout: &mut io::Stdout,
    term_w: u16,
    term_h: u16,
    active_idx: usize,
    total: usize,
    mode_label: &str,
) -> anyhow::Result<()> {
    draw_status_bar_full(stdout, term_w, term_h, active_idx, total, mode_label, "", 0)
}

#[allow(clippy::too_many_arguments)]
pub fn draw_status_bar_full(
    stdout: &mut io::Stdout,
    term_w: u16,
    term_h: u16,
    active_idx: usize,
    total: usize,
    mode_label: &str,
    pane_name: &str,
    selection_chars: usize,
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

    // Left: pane info + name
    queue!(
        stdout,
        cursor::MoveTo(1, y),
        SetForegroundColor(ACTIVE_COLOR),
        SetAttribute(Attribute::Bold)
    )?;
    let left = if pane_name.is_empty() {
        format!("Pane {}/{}", active_idx + 1, total)
    } else {
        format!("Pane {}/{} {}", active_idx + 1, total, pane_name)
    };
    queue!(stdout, Print(&left))?;
    let mut left_end = 1 + left.len();

    // Mode indicator or selection char count
    if selection_chars > 0 {
        let sel_label = format!("{} chars", selection_chars);
        queue!(
            stdout,
            SetAttribute(Attribute::Reset),
            SetBackgroundColor(Color::Rgb {
                r: 40,
                g: 20,
                b: 60
            }),
            SetForegroundColor(Color::Rgb {
                r: 200,
                g: 160,
                b: 255
            }),
            SetAttribute(Attribute::Bold),
            Print(format!(" {} ", sel_label)),
            SetAttribute(Attribute::Reset),
            SetBackgroundColor(STATUS_BG),
        )?;
        left_end += sel_label.len() + 2;
    } else if !mode_label.is_empty() {
        queue!(
            stdout,
            SetAttribute(Attribute::Reset),
            SetBackgroundColor(Color::Rgb {
                r: 60,
                g: 40,
                b: 10
            }),
            SetForegroundColor(Color::Rgb {
                r: 255,
                g: 200,
                b: 50
            }),
            SetAttribute(Attribute::Bold),
            Print(format!(" {} ", mode_label)),
            SetAttribute(Attribute::Reset),
            SetBackgroundColor(STATUS_BG),
        )?;
        left_end += mode_label.len() + 2;
    }

    // Clock (HH:MM) on the far right
    let clock = {
        let now = now_hhmm();
        format!(" {} ", now)
    };
    let clock_len = clock.len();

    // Right: context-aware hints based on mode
    queue!(
        stdout,
        SetAttribute(Attribute::Reset),
        SetBackgroundColor(STATUS_BG)
    )?;
    let hints: &[&str] = match mode_label {
        "PREFIX" => &[
            "%/\" split H/V",
            "o next",
            "←↑↓→ navigate",
            "z zoom",
            "B broadcast",
            "R resize",
            "[ scroll",
            "q pane-jump",
            "{/} swap",
            "x close",
            "? help",
        ],
        "RESIZE" => &["←→↑↓/hjkl resize pane", "q/Esc exit resize"],
        "SCROLL" => &[
            "j/k line",
            "Ctrl+U/D half-page",
            "PgUp/PgDn page",
            "g top  G bottom",
            "q exit scroll",
        ],
        "SELECT" => &["1-9 jump to pane", "0 for 10th", "any key cancel"],
        "QUIT? y/n" => &["y quit", "any key cancel"],
        "ZOOM" => &["Ctrl+B z unzoom", "Ctrl+D/E split", "type normally"],
        "BROADCAST" => &["typing in ALL panes", "Ctrl+B B stop broadcast"],
        _ => &[
            "Ctrl+D/E split",
            "Ctrl+N next",
            "Ctrl+B prefix",
            "drag text→copy",
            "scroll↕output",
            "Ctrl+G settings",
            "Ctrl+B ? help",
        ],
    };
    let mut right_str = String::new();
    for hint in hints.iter() {
        let candidate = if right_str.is_empty() {
            hint.to_string()
        } else {
            format!("{}  {}", right_str, hint)
        };
        // Reserve space for clock on the far right
        if candidate.len() + left_end + 4 + clock_len < w {
            right_str = candidate;
        } else {
            break;
        }
    }
    if !right_str.is_empty() {
        let rx = (w as u16).saturating_sub(right_str.len() as u16 + clock_len as u16 + 1);
        queue!(
            stdout,
            cursor::MoveTo(rx, y),
            SetForegroundColor(Color::Grey),
            Print(&right_str)
        )?;
    }

    // Draw clock at far right
    if clock_len + 1 < w {
        let cx = (w as u16).saturating_sub(clock_len as u16);
        queue!(
            stdout,
            cursor::MoveTo(cx, y),
            SetBackgroundColor(STATUS_BG),
            SetForegroundColor(Color::DarkGrey),
            Print(&clock),
        )?;
    }

    queue!(stdout, ResetColor, SetAttribute(Attribute::Reset))?;
    Ok(())
}

/// Get current time as HH:MM using libc (no chrono dependency).
fn now_hhmm() -> String {
    #[cfg(unix)]
    {
        let mut t: libc::time_t = 0;
        unsafe { libc::time(&mut t) };
        let tm = unsafe { libc::localtime(&t) };
        if tm.is_null() {
            return "--:--".to_string();
        }
        let tm = unsafe { &*tm };
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
    #[cfg(not(unix))]
    {
        "--:--".to_string()
    }
}

/// Close button hit detection — 2-cell wide hit area for " ×".
pub enum TitleAction {
    Close(usize),
    SplitH(usize),
    SplitV(usize),
}

/// Hit test title bar buttons. Layout: "━ ┃ ×" at the right end of the title.
/// Positions from right edge: × at w-1, ┃ at w-3, ━ at w-5
pub fn title_button_hit(x: u16, y: u16, layout: &Layout, inner: &Rect) -> Option<TitleAction> {
    let rects = layout.pane_rects(inner);
    for (&pid, rect) in &rects {
        let btn_y = rect.y.saturating_sub(1);
        if y != btn_y {
            continue;
        }
        let avail = rect.w as usize;
        if avail >= 13 {
            // Full button set: [━] [┃] [×] — 11 display cols from right edge
            // Rendered as: "[━] [┃] [×]"
            //              -11        -1
            let end = rect.x + rect.w; // 1 past the last content col
                                       // [×] at end-3..end-1 (3 chars)
            if x >= end.saturating_sub(3) && x < end {
                return Some(TitleAction::Close(pid));
            }
            // [┃] at end-7..end-5 (3 chars)
            if x >= end.saturating_sub(7) && x < end.saturating_sub(4) {
                return Some(TitleAction::SplitV(pid));
            }
            // [━] at end-11..end-9 (3 chars)
            if x >= end.saturating_sub(11) && x < end.saturating_sub(8) {
                return Some(TitleAction::SplitH(pid));
            }
        } else if avail >= 4 {
            // Just close button
            let btn_x = rect.x + rect.w - 1;
            if x == btn_x || x == btn_x.saturating_sub(1) {
                return Some(TitleAction::Close(pid));
            }
        }
    }
    None
}

// ─── Zoomed Pane Rendering ─────────────────────────────────

/// Render a single pane at full terminal size (zoom mode).
#[allow(clippy::too_many_arguments)]
pub fn render_zoomed_pane(
    stdout: &mut io::Stdout,
    pane: &Pane,
    pane_idx: usize,
    label: &str,
    border_style: BorderStyle,
    term_w: u16,
    term_h: u16,
    show_status_bar: bool,
) -> anyhow::Result<()> {
    queue!(stdout, cursor::Hide, terminal::Clear(ClearType::All))?;

    let chars = border_style.chars();
    let status_h = if show_status_bar { 1u16 } else { 0 };
    let border_h = term_h.saturating_sub(status_h);

    if term_w == 0 || border_h == 0 {
        return Ok(());
    }

    // Draw outer border
    let mut bmap = BorderMap::new();
    if term_w > 0 && border_h > 0 {
        bmap.add_h_line(0, term_w - 1, 0);
        bmap.add_h_line(0, term_w - 1, border_h - 1);
        bmap.add_v_line(0, 0, border_h - 1);
        bmap.add_v_line(term_w - 1, 0, border_h - 1);
    }
    for ((x, y), flags) in &bmap.cells {
        queue!(
            stdout,
            cursor::MoveTo(*x, *y),
            SetForegroundColor(ACTIVE_COLOR),
            Print(border_char(flags, &chars))
        )?;
    }

    // Title bar
    let title = format!(" {}:{} [ZOOM] ", pane_idx + 1, label);
    let avail = term_w.saturating_sub(2) as usize;
    if avail > title.len() + 1 {
        queue!(
            stdout,
            cursor::MoveTo(1, 0),
            SetForegroundColor(ACTIVE_COLOR),
            Print(chars.h),
            SetForegroundColor(Color::White),
            SetAttribute(Attribute::Bold),
            Print(&title),
            SetAttribute(Attribute::Reset),
            SetForegroundColor(ACTIVE_COLOR),
        )?;
        for _ in 0..avail - title.len() - 1 {
            queue!(stdout, Print(chars.h))?;
        }
    }

    // Content area
    let rect = Rect {
        x: 1,
        y: 1,
        w: term_w.saturating_sub(2),
        h: border_h.saturating_sub(2),
    };
    draw_content(stdout, pane, &rect, pane.is_alive(), None)?;

    // Cursor
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

    queue!(stdout, ResetColor, SetAttribute(Attribute::Reset))?;
    Ok(())
}

// ─── Help Overlay ──────────────────────────────────────────

pub fn draw_help_overlay(stdout: &mut io::Stdout, term_w: u16, term_h: u16) -> anyhow::Result<()> {
    let help_lines = [
        "",
        "  DIRECT SHORTCUTS",
        "  Ctrl+D        Split left|right",
        "  Ctrl+E        Split top/bottom",
        "  Ctrl+N        Next pane",
        "  Ctrl+G / F1   Settings panel",
        "  F2            Equalize all pane sizes",
        "  Alt+Arrow     Navigate directional",
        "  Ctrl+W        Quit",
        "",
        "  PREFIX MODE (Ctrl+B then)",
        "  %             Split left|right",
        "  \"             Split top/bottom",
        "  o             Next pane",
        "  Arrow         Navigate directional",
        "  x             Close pane",
        "  z             Zoom toggle",
        "  B             Broadcast mode (type in all)",
        "  R             Resize mode (arrows/hjkl, q)",
        "  { }           Swap pane prev/next",
        "  [             Scroll mode (j/k/g/G, q)",
        "  q             Show pane numbers + jump",
        "  E             Equalize sizes",
        "  s             Toggle status bar",
        "  d             Quit (with confirmation)",
        "  ?             This help",
        "",
        "  MOUSE",
        "  Click         Select pane",
        "  Double-click  Zoom toggle",
        "  Drag border   Resize panes",
        "  Drag text     Select & copy to clipboard",
        "  Scroll wheel  Scrollback history",
        "  [━][┃][×]     Split/close buttons",
        "",
        "  FEATURES",
        "  .ezpn.toml    Project config (ezpn init)",
        "  Layout DSL    -l '7:3/5:5' or presets",
        "  Auto-restart  restart = always|on_failure",
        "  Broadcast     Type in all panes at once",
        "  Workspaces    Save/load via ezpn-ctl",
        "",
        "          Press any key to close",
    ];

    let w: usize = 50;
    let h = help_lines.len() + 2; // +2 for top/bottom border
    let ox = term_w.saturating_sub(w as u16) / 2;
    let oy = term_h.saturating_sub(h as u16) / 2;

    let bg = Color::Rgb {
        r: 16,
        g: 18,
        b: 24,
    };
    let border_fg = Color::Rgb {
        r: 80,
        g: 90,
        b: 110,
    };

    // Backdrop
    queue!(
        stdout,
        SetBackgroundColor(Color::Rgb { r: 4, g: 5, b: 8 }),
        terminal::Clear(ClearType::All)
    )?;

    // Panel background
    let blank = " ".repeat(w);
    for dy in 0..h as u16 {
        queue!(
            stdout,
            cursor::MoveTo(ox, oy + dy),
            SetBackgroundColor(bg),
            Print(&blank)
        )?;
    }

    // Top border
    queue!(
        stdout,
        cursor::MoveTo(ox, oy),
        SetBackgroundColor(bg),
        SetForegroundColor(border_fg),
    )?;
    let title = " Help (Ctrl+B ?) ";
    let pad = w.saturating_sub(title.len() + 2);
    let lp = pad / 2;
    let rp = pad - lp;
    queue!(
        stdout,
        Print("─".repeat(lp)),
        SetForegroundColor(Color::White),
        SetAttribute(Attribute::Bold),
        Print(title),
        SetAttribute(Attribute::Reset),
        SetForegroundColor(border_fg),
        SetBackgroundColor(bg),
        Print("─".repeat(rp)),
    )?;

    // Content
    for (i, line) in help_lines.iter().enumerate() {
        let y = oy + 1 + i as u16;
        queue!(stdout, cursor::MoveTo(ox, y), SetBackgroundColor(bg))?;

        if line.contains("SHORTCUTS")
            || line.contains("PREFIX MODE")
            || line.contains("MOUSE")
            || line.contains("FEATURES")
        {
            queue!(
                stdout,
                SetForegroundColor(Color::Rgb {
                    r: 102,
                    g: 217,
                    b: 239
                }),
                SetAttribute(Attribute::Bold),
                Print(format!("{:<width$}", line, width = w)),
                SetAttribute(Attribute::Reset),
            )?;
        } else if line.contains("Press any key") {
            queue!(
                stdout,
                SetForegroundColor(Color::Rgb {
                    r: 90,
                    g: 98,
                    b: 110
                }),
                Print(format!("{:<width$}", line, width = w)),
            )?;
        } else {
            // Split at first run of spaces >= 8 for key/description alignment
            queue!(
                stdout,
                SetForegroundColor(Color::Rgb {
                    r: 190,
                    g: 200,
                    b: 212,
                }),
                Print(format!("{:<width$}", line, width = w)),
            )?;
        }
    }

    // Bottom border
    queue!(
        stdout,
        cursor::MoveTo(ox, oy + h as u16 - 1),
        SetBackgroundColor(bg),
        SetForegroundColor(border_fg),
        Print("─".repeat(w)),
    )?;

    queue!(
        stdout,
        ResetColor,
        SetAttribute(Attribute::Reset),
        cursor::Hide
    )?;
    Ok(())
}

// ─── Pane Number Overlay ───────────────────────────────────

/// Draw large pane numbers overlaid on each pane for quick-jump (Ctrl+B q).
pub fn draw_pane_numbers(
    stdout: &mut io::Stdout,
    layout: &Layout,
    inner: &Rect,
) -> anyhow::Result<()> {
    let rects = layout.pane_rects(inner);
    let ids = layout.pane_ids();

    for (display_idx, &pid) in ids.iter().enumerate() {
        let Some(num) = quick_jump_label(display_idx) else {
            continue;
        };
        if let Some(rect) = rects.get(&pid) {
            let num = num.to_string();
            let num_w = num.len() as u16;

            if rect.w < num_w + 2 || rect.h < 3 {
                continue;
            }

            let cx = rect.x + (rect.w - num_w - 2) / 2;
            let cy = rect.y + rect.h / 2 - 1;

            let bg = Color::Rgb {
                r: 20,
                g: 24,
                b: 32,
            };
            let fg = Color::Rgb {
                r: 102,
                g: 217,
                b: 239,
            };
            let box_w = (num_w + 4) as usize;

            // Box background (3 rows)
            for dy in 0..3u16 {
                queue!(
                    stdout,
                    cursor::MoveTo(cx, cy + dy),
                    SetBackgroundColor(bg),
                    Print(" ".repeat(box_w)),
                )?;
            }

            // Number centered in middle row
            queue!(
                stdout,
                cursor::MoveTo(cx + 2, cy + 1),
                SetBackgroundColor(bg),
                SetForegroundColor(fg),
                SetAttribute(Attribute::Bold),
                Print(&num),
                SetAttribute(Attribute::Reset),
            )?;
        }
    }

    // Hint at bottom
    let hint = "Press 1-9 or 0 to jump, any other key to cancel";
    let hx = inner.x + inner.w.saturating_sub(hint.len() as u16) / 2;
    let hy = inner.y + inner.h;
    queue!(
        stdout,
        cursor::MoveTo(hx, hy),
        SetForegroundColor(Color::Rgb {
            r: 90,
            g: 98,
            b: 110,
        }),
        Print(hint),
        ResetColor,
        cursor::Hide,
    )?;

    Ok(())
}

fn quick_jump_label(index: usize) -> Option<char> {
    match index {
        0..=8 => char::from_u32('1' as u32 + index as u32),
        9 => Some('0'),
        _ => None,
    }
}

fn vt100_to_crossterm(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::AnsiValue(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb { r, g, b },
    }
}
