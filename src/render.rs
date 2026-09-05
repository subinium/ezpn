//! Rendering primitives.
//!
//! Theme integration (#85): the six palette-driven Color constants below
//! (`ACTIVE_COLOR`, `BORDER_COLOR`, `STATUS_BG`, `STATUS_FG`, `BROADCAST_COLOR`,
//! `DEAD_FG`) are kept as fallbacks for callers that do not yet plumb a
//! `ResolvedPalette`. The themed render entry points (`draw_status_bar_full`,
//! `draw_tab_bar`, `render_panes`) accept an `Option<&ResolvedPalette>` and
//! prefer palette-resolved colours when present. The four remaining constants
//! (`HINT_FG`, `CLOSE_COLOR`, `DRAG_COLOR`, `MUTED_FG`) have no direct
//! `ResolvedPalette` field today and stay hardcoded — v0.14 may extend the
//! theme schema with `hint_fg`, `close_fg`, `drag_indicator`, and `muted_fg`.

use crate::vt100;

use std::collections::{HashMap, HashSet};
use std::io::Write;

use unicode_width::UnicodeWidthStr;

use crossterm::{
    cursor, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute},
    terminal::{self, ClearType},
};
use serde::{Deserialize, Serialize};

use crate::copy_mode::CopyModeState;
use crate::fuzzy;
use crate::layout::{Layout, Rect};
use crate::pane::Pane;
use crate::theme::{Resolved, ResolvedPalette};

/// UI colour commands using actual ANSI-16 SGR for palette indices 0..15.
/// Crossterm's built-in named colours otherwise also emit 256-colour SGR.
pub struct AnsiForeground(pub Color);
pub struct AnsiBackground(pub Color);

fn write_ui_color(
    out: &mut impl std::fmt::Write,
    color: Color,
    foreground: bool,
) -> std::fmt::Result {
    if crossterm::style::Colored::ansi_color_disabled_memoized() {
        return Ok(());
    }
    let index = match color {
        Color::Black => Some(0),
        Color::DarkRed => Some(1),
        Color::DarkGreen => Some(2),
        Color::DarkYellow => Some(3),
        Color::DarkBlue => Some(4),
        Color::DarkMagenta => Some(5),
        Color::DarkCyan => Some(6),
        Color::Grey => Some(7),
        Color::DarkGrey => Some(8),
        Color::Red => Some(9),
        Color::Green => Some(10),
        Color::Yellow => Some(11),
        Color::Blue => Some(12),
        Color::Magenta => Some(13),
        Color::Cyan => Some(14),
        Color::White => Some(15),
        Color::AnsiValue(i) if i < 16 => Some(i),
        _ => None,
    };
    if let Some(index) = index {
        let base = if foreground { 30 } else { 40 };
        return write!(
            out,
            "\x1b[{}m",
            base + u16::from(index % 8) + if index >= 8 { 60 } else { 0 }
        );
    }
    if foreground {
        crossterm::Command::write_ansi(&crossterm::style::SetForegroundColor(color), out)
    } else {
        crossterm::Command::write_ansi(&crossterm::style::SetBackgroundColor(color), out)
    }
}

impl crossterm::Command for AnsiForeground {
    fn write_ansi(&self, out: &mut impl std::fmt::Write) -> std::fmt::Result {
        write_ui_color(out, self.0, true)
    }
    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        crossterm::Command::execute_winapi(&crossterm::style::SetForegroundColor(self.0))
    }
}

impl crossterm::Command for AnsiBackground {
    fn write_ansi(&self, out: &mut impl std::fmt::Write) -> std::fmt::Result {
        write_ui_color(out, self.0, false)
    }
    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        crossterm::Command::execute_winapi(&crossterm::style::SetBackgroundColor(self.0))
    }
}

// ─── Palette helpers (#85) ─────────────────────────────────

/// Convert a [`Resolved`] palette entry into the matching `crossterm::Color`.
/// `Indexed(i)` becomes `Color::AnsiValue(i)`; `Rgb(c)` becomes `Color::Rgb`.
pub fn resolved_to_crossterm(resolved: &Resolved) -> Color {
    match *resolved {
        Resolved::Rgb(c) => Color::Rgb {
            r: c.r,
            g: c.g,
            b: c.b,
        },
        Resolved::Indexed(i) => Color::AnsiValue(i),
    }
}

/// Resolve a palette entry, falling back to a hardcoded constant when the
/// palette is `None` (e.g. boot before a theme is bound, or callers that
/// have not yet been threaded through). Keeps the legacy look as the
/// no-palette default so partial integration never regresses visuals.
fn palette_color(palette: Option<&ResolvedPalette>, pick: PaletteSlot) -> Color {
    match (palette, pick) {
        (Some(p), PaletteSlot::BorderActive) => resolved_to_crossterm(&p.border_active),
        (Some(p), PaletteSlot::Border) => resolved_to_crossterm(&p.border),
        (Some(p), PaletteSlot::StatusBg) => resolved_to_crossterm(&p.status_bg),
        (Some(p), PaletteSlot::StatusFg) => resolved_to_crossterm(&p.status_fg),
        (Some(p), PaletteSlot::Broadcast) => resolved_to_crossterm(&p.broadcast_indicator),
        (Some(p), PaletteSlot::DeadFg) => resolved_to_crossterm(&p.tab_inactive_fg),
        (None, PaletteSlot::BorderActive) => ACTIVE_COLOR,
        (None, PaletteSlot::Border) => BORDER_COLOR,
        (None, PaletteSlot::StatusBg) => STATUS_BG,
        (None, PaletteSlot::StatusFg) => STATUS_FG,
        (None, PaletteSlot::Broadcast) => BROADCAST_COLOR,
        (None, PaletteSlot::DeadFg) => DEAD_FG,
    }
}

#[derive(Clone, Copy)]
enum PaletteSlot {
    BorderActive,
    Border,
    StatusBg,
    StatusFg,
    Broadcast,
    // reason: consumed by `palette_color` in the dead-pane render branch
    // (#tab-bar inactive tab fg path) — wiring is one renderer slice away.
    #[allow(dead_code)]
    DeadFg,
}

// ─── Legacy constants (fallback when no palette is bound) ──

const ACTIVE_COLOR: Color = Color::Cyan;
const BORDER_COLOR: Color = Color::DarkGrey;
const STATUS_BG: Color = Color::Rgb {
    r: 36,
    g: 38,
    b: 48,
};
const STATUS_FG: Color = Color::White;
// Unmapped: no `hint_fg` field on `ResolvedPalette` yet — see module doc.
const HINT_FG: Color = Color::Rgb {
    r: 160,
    g: 170,
    b: 190,
};
// Unmapped: see module doc.
const CLOSE_COLOR: Color = Color::DarkRed;
const DEAD_FG: Color = Color::DarkGrey;
const BROADCAST_COLOR: Color = Color::Rgb {
    r: 255,
    g: 140,
    b: 50,
};
// Unmapped: see module doc.
const DRAG_COLOR: Color = Color::Yellow;
// Unmapped: see module doc.
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
    None,
}

impl BorderStyle {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "single" => Some(Self::Single),
            "rounded" => Some(Self::Rounded),
            "heavy" => Some(Self::Heavy),
            "double" => Some(Self::Double),
            "none" | "borderless" => Some(Self::None),
            _ => Option::None,
        }
    }

    pub fn is_none(self) -> bool {
        matches!(self, Self::None)
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
            Self::None => BorderChars {
                h: " ",
                v: "│",
                tl: " ",
                tr: " ",
                bl: " ",
                br: " ",
                tj: "│",
                bj: "│",
                lj: "│",
                rj: "│",
                xj: "│",
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
    pane_order: Vec<usize>,
    pane_rects: HashMap<usize, Rect>,
    cells: Vec<BorderCell>,
}

impl BorderCache {
    pub fn pane_order(&self) -> &[usize] {
        &self.pane_order
    }

    pub fn pane_rects(&self) -> &HashMap<usize, Rect> {
        &self.pane_rects
    }

    pub fn inner(&self) -> &Rect {
        &self.inner
    }
}

// ─── Border Map ────────────────────────────────────────────

struct BorderMap {
    width: u16,
    height: u16,
    dense: Vec<u8>,
    sparse: HashMap<(u16, u16), u8>,
}

impl BorderMap {
    fn new(width: u16, height: u16) -> Self {
        let size = usize::from(width) * usize::from(height);
        Self {
            width,
            height,
            // Runtime dimensions fit this budget. Keep sparse fallback for
            // defensive callers with extreme u16 rectangles.
            dense: if size <= 512 * 1024 {
                vec![0; size]
            } else {
                Vec::new()
            },
            sparse: HashMap::new(),
        }
    }

    fn mark(&mut self, x: u16, y: u16, bits: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        if self.dense.is_empty() {
            *self.sparse.entry((x, y)).or_default() |= bits | 16;
        } else {
            self.dense[usize::from(y) * usize::from(self.width) + usize::from(x)] |= bits | 16;
        }
    }

    fn into_cells(self) -> Vec<BorderCell> {
        let cell = |x, y, bits: u8| BorderCell {
            x,
            y,
            flags: [1, 2, 4, 8].map(|flag| bits & flag != 0),
        };
        if !self.dense.is_empty() {
            let width = usize::from(self.width);
            return self
                .dense
                .into_iter()
                .enumerate()
                .filter(|(_, bits)| *bits != 0)
                .map(|(i, bits)| cell((i % width) as u16, (i / width) as u16, bits))
                .collect();
        }
        let mut cells: Vec<_> = self
            .sparse
            .into_iter()
            .map(|((x, y), bits)| cell(x, y, bits))
            .collect();
        cells.sort_unstable_by_key(|cell| (cell.y, cell.x));
        cells
    }

    fn add_h_line(&mut self, x1: u16, x2: u16, y: u16) {
        for x in x1..=x2 {
            self.mark(x, y, (u8::from(x > x1) * 4) | (u8::from(x < x2) * 8));
        }
    }

    fn add_v_line(&mut self, x: u16, y1: u16, y2: u16) {
        for y in y1..=y2 {
            self.mark(x, y, u8::from(y > y1) | (u8::from(y < y2) * 2));
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

/// Pane content inside the terminal area supplied by the caller.
/// Subtract a visible tab-bar row from `term_h` before calling this helper.
pub fn content_area(term_w: u16, term_h: u16, show_status_bar: bool, style: BorderStyle) -> Rect {
    let border_h = term_h.saturating_sub(u16::from(show_status_bar));
    let side = u16::from(!style.is_none());
    Rect {
        x: side.min(term_w),
        y: 1.min(border_h),
        w: term_w.saturating_sub(side * 2),
        h: border_h.saturating_sub(1 + side),
    }
}

pub fn build_border_cache_with_style(
    layout: &Layout,
    show_status_bar: bool,
    term_w: u16,
    term_h: u16,
    style: BorderStyle,
) -> BorderCache {
    let status_h = if show_status_bar { 1u16 } else { 0 };
    let border_h = term_h.saturating_sub(status_h);
    let borderless = style.is_none();

    let outer = Rect {
        x: 0,
        y: 0,
        w: term_w,
        h: border_h,
    };
    let inner = content_area(term_w, term_h, show_status_bar, style);

    let pane_order = layout.pane_ids();
    let pane_rects = layout.pane_rects(&inner);
    let separators = layout.separators(&inner, &outer);

    let mut bmap = if borderless && separators.is_empty() {
        BorderMap::new(0, 0)
    } else {
        BorderMap::new(term_w, border_h)
    };
    // Only draw outer frame for bordered styles
    if !borderless && outer.w > 0 && outer.h > 0 {
        bmap.add_h_line(outer.x, outer.x + outer.w - 1, outer.y);
        bmap.add_h_line(outer.x, outer.x + outer.w - 1, outer.y + outer.h - 1);
        bmap.add_v_line(outer.x, outer.y, outer.y + outer.h - 1);
        bmap.add_v_line(outer.x + outer.w - 1, outer.y, outer.y + outer.h - 1);
    }
    // Internal separators always drawn (thin line for borderless)
    for sep in &separators {
        if sep.horizontal {
            bmap.add_h_line(sep.x, sep.x + sep.length - 1, sep.y);
        } else {
            bmap.add_v_line(sep.x, sep.y, sep.y + sep.length - 1);
        }
    }

    let cells = bmap.into_cells();

    BorderCache {
        inner,
        pane_order,
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
    stdout: &mut impl Write,
    panes: &HashMap<usize, Pane>,
    _layout: &Layout,
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
    broadcast: bool,
    palette: Option<&ResolvedPalette>,
) -> anyhow::Result<()> {
    if term_w == 0 || term_h == 0 {
        return Ok(());
    }
    queue!(
        stdout,
        cursor::Hide,
        SetAttribute(Attribute::Reset),
        ResetColor
    )?;

    if full_redraw {
        queue!(stdout, terminal::Clear(ClearType::All))?;
    }

    let chars = border_style.chars();
    let inner = border_cache.inner();

    // Terminal too small
    if inner.w == 0 || inner.h == 0 {
        let msg = truncate_label("Terminal too small", term_w as usize);
        let mx = term_w.saturating_sub(msg.len() as u16) / 2;
        let my = term_h / 2;
        queue!(
            stdout,
            cursor::MoveTo(mx, my),
            AnsiForeground(Color::Red),
            Print(msg)
        )?;
        queue!(stdout, ResetColor)?;
        return Ok(());
    }

    let pane_rects = border_cache.pane_rects();

    if full_redraw {
        let active_rect = pane_rects.get(&active_id);
        let border_active_color = palette_color(palette, PaletteSlot::BorderActive);
        let border_color = palette_color(palette, PaletteSlot::Border);
        let broadcast_color = palette_color(palette, PaletteSlot::Broadcast);
        for cell in &border_cache.cells {
            let is_active = active_rect
                .map(|r| is_pane_border(cell.x, cell.y, r))
                .unwrap_or(false);
            let color = if border_style.is_none() {
                // Borderless: visible but minimal separator
                Color::Rgb {
                    r: 70,
                    g: 75,
                    b: 90,
                }
            } else if dragging_sep {
                DRAG_COLOR
            } else if broadcast {
                broadcast_color
            } else if is_active {
                border_active_color
            } else {
                border_color
            };
            queue!(
                stdout,
                cursor::MoveTo(cell.x, cell.y),
                AnsiForeground(color),
                Print(border_char(&cell.flags, &chars))
            )?;
        }
    }

    // Pane titles + content
    let ids = border_cache.pane_order();
    for (display_idx, &pid) in ids.iter().enumerate() {
        if !full_redraw && !dirty_panes.contains(&pid) {
            continue;
        }
        if let Some(rect) = pane_rects.get(&pid) {
            if rect.w == 0 || rect.h == 0 {
                continue;
            }
            if !full_redraw {
                clear_title(stdout, rect, &chars)?;
            }
            let is_active = pid == active_id;
            let pane_ref = panes.get(&pid);
            let is_alive = pane_ref.is_some_and(|p| p.is_alive());
            let label = pane_ref.map(|p| p.launch_label("")).unwrap_or_default();
            let is_scrolled = pane_ref.is_some_and(|p| p.is_scrolled());
            let exit_code = pane_ref.and_then(|p| p.exit_code());
            {
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
            }
            if let Some(pane) = panes.get(&pid) {
                let pane_sel = selection
                    .filter(|(sel_pid, ..)| *sel_pid == pid)
                    .map(|(_, sr, sc, er, ec)| (sr, sc, er, ec));
                draw_content(stdout, pane, rect, is_alive, pane_sel)?;
            } else {
                clear_rect(stdout, rect)?;
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
        draw_pane_cursor(stdout, pane, rect)?;
    }

    queue!(stdout, ResetColor, SetAttribute(Attribute::Reset))?;
    Ok(())
}

fn clear_rect(stdout: &mut impl Write, rect: &Rect) -> anyhow::Result<()> {
    if rect.w == 0 || rect.h == 0 {
        return Ok(());
    }

    let blanks = " ".repeat(rect.w as usize);
    for row in 0..rect.h {
        queue!(
            stdout,
            cursor::MoveTo(rect.x, rect.y + row),
            ResetColor,
            Print(&blanks)
        )?;
    }

    Ok(())
}

fn clear_title(stdout: &mut impl Write, rect: &Rect, chars: &BorderChars) -> anyhow::Result<()> {
    if rect.w == 0 {
        return Ok(());
    }

    let y = rect.y.saturating_sub(1);
    let x = rect.x;
    let line = chars.h.repeat(rect.w as usize);
    queue!(
        stdout,
        cursor::MoveTo(x, y),
        SetAttribute(Attribute::Reset),
        AnsiForeground(BORDER_COLOR),
        Print(&line)
    )?;
    Ok(())
}

fn is_pane_border(x: u16, y: u16, r: &Rect) -> bool {
    let top = r.y.saturating_sub(1);
    let bot = r.y.saturating_add(r.h);
    let left = r.x.saturating_sub(1);
    let right = r.x.saturating_add(r.w);
    (y == top || y == bot) && x >= left && x <= right
        || (x == left || x == right) && y >= top && y <= bot
}

#[allow(clippy::too_many_arguments)]
fn draw_pane_title(
    stdout: &mut impl Write,
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

    let borderless = chars.h == " ";
    let scroll_ind = if is_scrolled { " [SCROLL]" } else { "" };

    // Borderless: "N:label" no leading space. Bordered: " N:label "
    let title = if !is_alive {
        match exit_code {
            Some(code) => {
                if borderless {
                    format!("{} [exit {}]", idx + 1, code)
                } else {
                    format!(" {} [exit {}] ", idx + 1, code)
                }
            }
            None => {
                if borderless {
                    format!("{} [exited]", idx + 1)
                } else {
                    format!(" {} [exited] ", idx + 1)
                }
            }
        }
    } else if label.is_empty() || avail < 12 {
        if borderless {
            format!("{}{}", idx + 1, scroll_ind)
        } else {
            format!(" {}{} ", idx + 1, scroll_ind)
        }
    } else {
        let pad = if borderless { 4 } else { 8 };
        let max_label = avail.saturating_sub(pad + scroll_ind.len());
        let short = truncate_label(label, max_label);
        if borderless {
            format!("{}:{}{}", idx + 1, short, scroll_ind)
        } else {
            format!(" {}:{}{} ", idx + 1, short, scroll_ind)
        }
    };
    // Keep visibility identical to title_button_hit, independent of label length.
    let show_buttons = avail >= 13;
    let btn_len = if show_buttons { 11 } else { 0 };
    let show_close = !show_buttons && avail >= 4;
    let close_len = if show_close { 2 } else { 0 };
    let right_len = btn_len + close_len;
    let title = truncate_label(&title, avail.saturating_sub(1 + right_len));
    let tlen = title.width();

    if avail >= tlen + 1 + right_len {
        let color = if is_active {
            ACTIVE_COLOR
        } else {
            BORDER_COLOR
        };
        queue!(
            stdout,
            SetAttribute(Attribute::Reset),
            AnsiForeground(color)
        )?;

        // Borderless: fill title row with subtle background for visual separation
        if borderless {
            let title_bg = if is_active {
                Color::Rgb {
                    r: 30,
                    g: 34,
                    b: 46,
                }
            } else {
                Color::Rgb {
                    r: 18,
                    g: 20,
                    b: 28,
                }
            };
            queue!(
                stdout,
                cursor::MoveTo(title_x, title_y),
                AnsiBackground(title_bg),
            )?;
            let blanks = " ".repeat(avail);
            queue!(stdout, Print(&blanks))?;
            queue!(stdout, cursor::MoveTo(title_x, title_y))?;
        } else {
            queue!(
                stdout,
                cursor::MoveTo(title_x, title_y),
                AnsiForeground(color)
            )?;
            queue!(stdout, Print(chars.h))?;
        }

        if is_active {
            queue!(
                stdout,
                AnsiForeground(Color::White),
                SetAttribute(Attribute::Bold)
            )?;
        }
        if !is_alive {
            queue!(stdout, AnsiForeground(DEAD_FG))?;
        }
        queue!(stdout, Print(&title))?;
        queue!(stdout, SetAttribute(Attribute::Reset))?;

        // For borderless: keep the title_bg set so buttons share the same background
        if borderless {
            let title_bg = if is_active {
                Color::Rgb {
                    r: 30,
                    g: 34,
                    b: 46,
                }
            } else {
                Color::Rgb {
                    r: 18,
                    g: 20,
                    b: 28,
                }
            };
            queue!(stdout, AnsiBackground(title_bg))?;
        }
        queue!(stdout, AnsiForeground(color))?;

        let leading = if borderless { 0 } else { 1 };
        let fill = avail.saturating_sub(tlen + leading + right_len);
        if !borderless {
            for _ in 0..fill {
                queue!(stdout, Print(chars.h))?;
            }
        }

        if show_buttons {
            let btn_fg = if is_active { MUTED_FG } else { BORDER_COLOR };
            if borderless {
                let btn_x = title_x + (avail as u16).saturating_sub(11);
                queue!(stdout, cursor::MoveTo(btn_x, title_y))?;
            }
            queue!(
                stdout,
                AnsiForeground(btn_fg),
                Print("[━] [┃] "),
                AnsiForeground(CLOSE_COLOR),
                Print("[×]")
            )?;
        } else if show_close {
            if borderless {
                let btn_x = title_x + (avail as u16).saturating_sub(2);
                queue!(stdout, cursor::MoveTo(btn_x, title_y))?;
            }
            queue!(stdout, AnsiForeground(CLOSE_COLOR), Print(" ×"))?;
        }
    }

    queue!(stdout, ResetColor, SetAttribute(Attribute::Reset))?;
    Ok(())
}

fn truncate_label(label: &str, max_cols: usize) -> String {
    if max_cols == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut width = 0usize;
    for ch in label.chars() {
        if ch.is_control() {
            continue;
        }
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + cw > max_cols {
            break;
        }
        out.push(ch);
        width += cw;
    }
    out
}

fn tail_label(label: &str, max_cols: usize) -> String {
    let clean = truncate_label(label, usize::MAX);
    let mut width = 0;
    let mut start = clean.len();
    for (index, ch) in clean.char_indices().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + cw > max_cols {
            break;
        }
        width += cw;
        start = index;
    }
    clean[start..]
        .trim_start_matches(|ch| unicode_width::UnicodeWidthChar::width(ch) == Some(0))
        .to_string()
}

/// Draw dimmed overlay on dead panes with centered message.
fn draw_dead_overlay(stdout: &mut impl Write, rect: &Rect) -> anyhow::Result<()> {
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
            AnsiBackground(dim_bg),
        )?;
        for _ in 0..rect.w {
            queue!(stdout, Print(" "))?;
        }
    }

    let my = rect.y + rect.h / 2;

    // "Process exited" label
    if rect.h >= 3 {
        let label = truncate_label("Process exited", rect.w as usize);
        let lx = rect.x + rect.w.saturating_sub(label.len() as u16) / 2;
        queue!(
            stdout,
            cursor::MoveTo(lx, my.saturating_sub(1)),
            AnsiBackground(dim_bg),
            AnsiForeground(Color::Rgb {
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
    let msg = truncate_label("Press Enter to respawn", rect.w as usize);
    let mx = rect.x + rect.w.saturating_sub(msg.len() as u16) / 2;
    queue!(
        stdout,
        cursor::MoveTo(mx, my),
        AnsiBackground(dim_bg),
        AnsiForeground(Color::DarkGrey),
        SetAttribute(Attribute::Italic),
        Print(msg),
        SetAttribute(Attribute::Reset),
    )?;
    Ok(())
}

fn draw_content(
    stdout: &mut impl Write,
    pane: &Pane,
    rect: &Rect,
    is_alive: bool,
    selection: Option<(u16, u16, u16, u16)>,
) -> anyhow::Result<()> {
    draw_screen_content(stdout, pane.screen(), rect, is_alive, selection)
}

fn draw_screen_content(
    stdout: &mut impl Write,
    screen: &vt100::Screen,
    rect: &Rect,
    is_alive: bool,
    selection: Option<(u16, u16, u16, u16)>,
) -> anyhow::Result<()> {
    draw_screen_content_with_matches(stdout, screen, rect, is_alive, selection, &[])
}

fn draw_screen_content_with_matches(
    stdout: &mut impl Write,
    screen: &vt100::Screen,
    rect: &Rect,
    is_alive: bool,
    selection: Option<(u16, u16, u16, u16)>,
    matches: &[(u16, u16, u16)],
) -> anyhow::Result<()> {
    if rect.w == 0 || rect.h == 0 {
        return Ok(());
    }

    queue!(stdout, SetAttribute(Attribute::Reset), ResetColor)?;
    let mut last_style = None;
    // Reusable buffer: batch consecutive plain-text cells into one Print call
    let mut buf = String::with_capacity(rect.w as usize);

    for r in 0..rect.h {
        queue!(stdout, cursor::MoveTo(rect.x, rect.y + r))?;
        buf.clear();
        let mut match_index = matches.partition_point(|(row, _, _)| *row < r);

        for c in 0..rect.w {
            let selected = |column| {
                selection.is_some_and(|(sr, sc, er, ec)| {
                    if r < sr || r > er {
                        false
                    } else if r == sr && r == er {
                        column >= sc && column <= ec
                    } else if r == sr {
                        column >= sc
                    } else if r == er {
                        column <= ec
                    } else {
                        true
                    }
                })
            };

            if let Some(cell) = screen.cell(r, c) {
                // Skip wide character continuation cells — the wide char itself
                // already occupies 2 display columns when printed.
                if cell.is_wide_continuation() {
                    continue;
                }

                let is_selected = selected(c) || (cell.is_wide() && selected(c + 1));
                while matches
                    .get(match_index)
                    .is_some_and(|&(row, col, len)| row == r && col.saturating_add(len) <= c)
                {
                    match_index += 1;
                }
                let match_hit = matches.get(match_index).is_some_and(|&(row, col, len)| {
                    row == r
                        && len > 0
                        && col < c.saturating_add(if cell.is_wide() { 2 } else { 1 })
                        && col.saturating_add(len) > c
                });
                let mut fg = if match_hit {
                    vt100::Color::Idx(0)
                } else {
                    cell.fgcolor()
                };
                let bg = if match_hit {
                    vt100::Color::Idx(3)
                } else {
                    cell.bgcolor()
                };
                if !is_alive {
                    fg = vt100::Color::Idx(8);
                }

                let attrs = (
                    is_alive && cell.bold(),
                    is_alive && cell.italic(),
                    is_alive && cell.underline(),
                    (is_alive && cell.inverse()) ^ is_selected,
                    is_alive && cell.dim(),
                );
                let style = (fg, bg, attrs);
                let style_changed = last_style != Some(style);

                // Flush buffer if style changes
                if style_changed && !buf.is_empty() {
                    queue!(stdout, Print(&buf))?;
                    buf.clear();
                }

                if style_changed {
                    // Reset before colours: SGR 0 resets colours as well as attributes.
                    queue!(stdout, SetAttribute(Attribute::Reset))?;
                    write_pane_color(stdout, fg, true)?;
                    write_pane_color(stdout, bg, false)?;
                    for (enabled, attr) in [
                        (attrs.0, Attribute::Bold),
                        (attrs.1, Attribute::Italic),
                        (attrs.2, Attribute::Underlined),
                        (attrs.3, Attribute::Reverse),
                        (attrs.4, Attribute::Dim),
                    ] {
                        if enabled {
                            queue!(stdout, SetAttribute(attr))?;
                        }
                    }
                    last_style = Some(style);
                }
                let contents = cell.contents();
                if contents.is_empty() || (cell.is_wide() && c + 1 >= rect.w) {
                    buf.push(' ');
                } else if contents.is_ascii() {
                    buf.push_str(contents);
                } else {
                    // Anchor Unicode cells to the parser's columns. Adjacent emoji
                    // cells must not collapse subsequent columns into a ZWJ cluster.
                    if !buf.is_empty() {
                        queue!(stdout, Print(&buf))?;
                        buf.clear();
                    }
                    queue!(
                        stdout,
                        cursor::MoveTo(rect.x + c, rect.y + r),
                        Print(contents)
                    )?;
                    let next = c + if cell.is_wide() { 2 } else { 1 };
                    if next < rect.w {
                        queue!(stdout, cursor::MoveTo(rect.x + next, rect.y + r))?;
                    }
                }
            } else {
                if last_style.is_some() {
                    queue!(
                        stdout,
                        Print(&buf),
                        SetAttribute(Attribute::Reset),
                        ResetColor
                    )?;
                    buf.clear();
                    last_style = None;
                }
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

/// Draw from the currently synchronized scrollback view, after pane chrome.
/// Search matches are the row/column-sorted display spans produced by copy mode.
pub fn draw_copy_mode_overlay(
    stdout: &mut impl Write,
    screen: &vt100::Screen,
    rect: &Rect,
    state: &CopyModeState,
) -> anyhow::Result<()> {
    draw_screen_content_with_matches(
        stdout,
        screen,
        rect,
        true,
        state.selection(),
        &state.search_matches,
    )?;
    queue!(stdout, cursor::Hide)?;
    if state.cursor_row < rect.h && state.cursor_col < rect.w {
        queue!(
            stdout,
            cursor::MoveTo(rect.x + state.cursor_col, rect.y + state.cursor_row),
            cursor::Show
        )?;
    }
    Ok(())
}

/// Restore the application's cursor after status/tab rendering moved it.
pub fn draw_pane_cursor(stdout: &mut impl Write, pane: &Pane, rect: &Rect) -> anyhow::Result<()> {
    queue!(stdout, cursor::Hide)?;
    let screen = pane.screen();
    let (row, col) = screen.cursor_position();
    if pane.is_alive()
        && !pane.is_scrolled()
        && !screen.hide_cursor()
        && row < rect.h
        && col < rect.w
    {
        queue!(
            stdout,
            cursor::MoveTo(rect.x + col, rect.y + row),
            cursor::Show
        )?;
    }
    Ok(())
}

pub fn draw_status_bar(
    stdout: &mut impl Write,
    term_w: u16,
    term_h: u16,
    active_idx: usize,
    total: usize,
    mode_label: &str,
) -> anyhow::Result<()> {
    draw_status_bar_full(
        stdout, term_w, term_h, active_idx, total, mode_label, "", 0, None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn draw_status_bar_full(
    stdout: &mut impl Write,
    term_w: u16,
    term_h: u16,
    active_idx: usize,
    total: usize,
    mode_label: &str,
    pane_name: &str,
    selection_chars: usize,
    palette: Option<&ResolvedPalette>,
) -> anyhow::Result<()> {
    if term_w == 0 || term_h == 0 {
        return Ok(());
    }
    let y = term_h - 1;
    let w = term_w as usize;

    let status_bg = palette_color(palette, PaletteSlot::StatusBg);
    let status_fg = palette_color(palette, PaletteSlot::StatusFg);
    let accent = palette_color(palette, PaletteSlot::BorderActive);

    queue!(
        stdout,
        SetAttribute(Attribute::Reset),
        cursor::MoveTo(0, y),
        AnsiBackground(status_bg),
        AnsiForeground(status_fg)
    )?;
    queue!(stdout, Print(" ".repeat(w)))?;
    let left_x = usize::from(w > 1);
    let clock_space = if w > 8 { 7 } else { 0 };
    let left_limit = w.saturating_sub(clock_space);

    // Left: pane info + name
    queue!(
        stdout,
        cursor::MoveTo(left_x as u16, y),
        AnsiForeground(accent),
        SetAttribute(Attribute::Bold)
    )?;
    let left = if pane_name.is_empty() {
        format!("Pane {}/{}", active_idx + 1, total)
    } else {
        format!("Pane {}/{} {}", active_idx + 1, total, pane_name)
    };
    let left = truncate_label(&left, left_limit.saturating_sub(left_x));
    queue!(stdout, Print(&left))?;
    let mut left_end = left_x + left.width();

    // Mode indicator or selection char count
    if selection_chars > 0 && left_end < left_limit {
        let sel_label = format!("{} chars", selection_chars);
        let badge = truncate_label(&format!(" {} ", sel_label), left_limit - left_end);
        queue!(
            stdout,
            SetAttribute(Attribute::Reset),
            AnsiBackground(Color::Rgb {
                r: 40,
                g: 20,
                b: 60
            }),
            AnsiForeground(Color::Rgb {
                r: 200,
                g: 160,
                b: 255
            }),
            SetAttribute(Attribute::Bold),
            Print(&badge),
            SetAttribute(Attribute::Reset),
            AnsiBackground(status_bg),
        )?;
        left_end += badge.width();
    } else if !mode_label.is_empty() && left_end < left_limit {
        let badge = truncate_label(&format!(" {} ", mode_label), left_limit - left_end);
        queue!(
            stdout,
            SetAttribute(Attribute::Reset),
            AnsiBackground(Color::Rgb {
                r: 60,
                g: 40,
                b: 10
            }),
            AnsiForeground(Color::Rgb {
                r: 255,
                g: 200,
                b: 50
            }),
            SetAttribute(Attribute::Bold),
            Print(&badge),
            SetAttribute(Attribute::Reset),
            AnsiBackground(status_bg),
        )?;
        left_end += badge.width();
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
        AnsiBackground(status_bg)
    )?;
    let hints: &[&str] = match mode_label {
        "PREFIX" => &[
            "c new-tab",
            "n/p next/prev-tab",
            "%/\" split H/V",
            "o next-pane",
            "←↑↓→ navigate",
            "z zoom",
            "B broadcast",
            "R resize",
            "[ scroll",
            "d detach",
            "x close-pane",
            "& close-tab",
            "? help",
        ],
        "RESIZE" => &["←→↑↓/hjkl resize pane", "q/Esc exit resize"],
        "COPY" => &[
            "hjkl move",
            "v select",
            "V line-select",
            "y copy",
            "/? search",
            "n/N next/prev",
            "w/b word",
            "q exit",
        ],
        "VISUAL" | "V-LINE" => &["hjkl extend", "y copy+exit", "v/V toggle", "Esc cancel"],
        "SEARCH" => &["type query", "Enter find", "n/N next/prev", "Esc cancel"],
        "SELECT" => &["1-9 jump to pane", "0 for 10th", "any key cancel"],
        "KILL SESSION? y/n" => &["y kill session", "any key cancel"],
        "CLOSE PANE? y/n" => &["y close pane", "any key cancel"],
        "CLOSE TAB? y/n" => &["y close tab", "any key cancel"],
        "ZOOM" => &["Ctrl+B z unzoom", "Ctrl+B %/\" split", "type normally"],
        "BROADCAST" => &["typing in ALL panes", "Ctrl+B B stop broadcast"],
        ":" => &["↑↓ navigate", "Enter select", "Tab complete", "Esc cancel"],
        "RENAME" => &["Enter confirm", "Esc cancel"],
        _ => &[
            "Ctrl+B prefix",
            "Ctrl+B %/\" split",
            "Ctrl+B o next",
            "Ctrl+B : palette",
            "drag text→copy",
            "scroll↕output",
            "F1 settings",
            "Ctrl+B ? help",
        ],
    };
    // Compute which hints fit, then render with styled key portions
    let separator = "  ";
    let sep_len = separator.len();
    let max_w = w.saturating_sub(left_end + 4 + clock_len);
    let mut fitted: Vec<&str> = Vec::new();
    let mut total_len = 0usize;
    for hint in hints.iter() {
        let added = if fitted.is_empty() {
            hint.width()
        } else {
            sep_len + hint.width()
        };
        if total_len + added <= max_w {
            total_len += added;
            fitted.push(hint);
        } else {
            break;
        }
    }
    if !fitted.is_empty() {
        let rx = (w as u16).saturating_sub(total_len as u16 + clock_len as u16 + 1);
        queue!(stdout, cursor::MoveTo(rx, y))?;
        let key_fg = Color::Rgb {
            r: 220,
            g: 225,
            b: 240,
        };
        let desc_fg = Color::Rgb {
            r: 120,
            g: 130,
            b: 150,
        };
        for (i, hint) in fitted.iter().enumerate() {
            if i > 0 {
                queue!(stdout, AnsiForeground(desc_fg), Print(separator))?;
            }
            // Split hint at first space: key part (bold) + desc part (dim)
            if let Some(sp) = hint.find(' ') {
                let (key, desc) = hint.split_at(sp);
                queue!(
                    stdout,
                    AnsiForeground(key_fg),
                    SetAttribute(Attribute::Bold),
                    Print(key),
                    SetAttribute(Attribute::Reset),
                    AnsiBackground(status_bg),
                    AnsiForeground(desc_fg),
                    Print(desc),
                )?;
            } else {
                queue!(
                    stdout,
                    AnsiForeground(key_fg),
                    SetAttribute(Attribute::Bold),
                    Print(*hint),
                    SetAttribute(Attribute::Reset),
                    AnsiBackground(status_bg),
                )?;
            }
        }
    }

    // Draw clock at far right
    if clock_space > 0 {
        let cx = (w as u16).saturating_sub(clock_len as u16);
        queue!(
            stdout,
            cursor::MoveTo(cx, y),
            AnsiBackground(status_bg),
            AnsiForeground(HINT_FG),
            Print(&clock),
        )?;
    }

    queue!(stdout, ResetColor, SetAttribute(Attribute::Reset))?;
    Ok(())
}

/// Status-bar flash kind, mirrors `settings::FlashKind` without pulling
/// `settings` into the renderer's dependency graph.
//
// FLASH-MSG-COORDINATE-WITH-#58
// reason: paired with `draw_flash_overlay` below — both consumed by the
// flash-message overlay wiring in #64; the producer side
// (`Settings::flash_message`) is already populated.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashLevel {
    Info,
    Error,
}

/// Overlay a one-line flash message on top of the status bar row. Intended
/// to be drawn AFTER `draw_status_bar_full` in the same frame, so the
/// underlying bar is visible if the message is shorter than the row.
///
/// Issue #64 uses this for `config reloaded` (Info, 1 s) and
/// `config error: <msg>` (Error, 3 s). Shared with the command palette work
/// in #58 — see the marker.
//
// FLASH-MSG-COORDINATE-WITH-#58
// reason: see `FlashLevel` above — same #64 wiring.
#[allow(dead_code)]
pub fn draw_flash_overlay(
    stdout: &mut impl Write,
    term_w: u16,
    term_h: u16,
    msg: &str,
    level: FlashLevel,
) -> anyhow::Result<()> {
    if term_w == 0 || term_h == 0 {
        return Ok(());
    }
    let y = term_h - 1;

    // Color scheme: green pill on success, red pill on failure. Background
    // colors are saturated enough to read against any pane content showing
    // through if the status bar happens to be hidden when the flash fires.
    let (bg, fg) = match level {
        FlashLevel::Info => (
            Color::Rgb {
                r: 30,
                g: 90,
                b: 50,
            },
            Color::Rgb {
                r: 220,
                g: 255,
                b: 220,
            },
        ),
        FlashLevel::Error => (
            Color::Rgb {
                r: 120,
                g: 30,
                b: 30,
            },
            Color::Rgb {
                r: 255,
                g: 220,
                b: 220,
            },
        ),
    };

    // Pad to a fixed minimum so the pill reads as a discrete block, then
    // truncate if the terminal is narrow. `+ 2` leaves a one-cell margin on
    // each side.
    let max_inner = (term_w as usize).saturating_sub(4);
    let truncated: String = if msg.width() > max_inner {
        let mut acc = String::new();
        let mut w = 0usize;
        for ch in msg.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if w + cw > max_inner.saturating_sub(1) {
                acc.push('…');
                break;
            }
            acc.push(ch);
            w += cw;
        }
        acc
    } else {
        msg.to_string()
    };

    let pill = truncate_label(&format!(" {} ", truncated), term_w as usize);
    let pill_len = pill.width() as u16;
    let x = if usize::from(pill_len) + 2 < usize::from(term_w) {
        2
    } else {
        0
    };

    queue!(
        stdout,
        cursor::MoveTo(x, y),
        AnsiBackground(bg),
        AnsiForeground(fg),
        SetAttribute(Attribute::Bold),
        Print(&pill),
        SetAttribute(Attribute::Reset),
        ResetColor,
    )?;
    Ok(())
}

/// Draw a text input bar at the bottom of the screen (replaces status bar temporarily).
pub fn draw_text_input(
    stdout: &mut impl Write,
    term_w: u16,
    term_h: u16,
    prompt: &str,
    buffer: &str,
) -> anyhow::Result<()> {
    if term_w == 0 || term_h == 0 {
        return Ok(());
    }
    let y = term_h - 1;
    let w = term_w as usize;
    let x = usize::from(w > 1);
    let prompt = truncate_label(prompt, w.saturating_sub(x + 1));
    let room = w.saturating_sub(x + prompt.width() + 1);
    let buffer = tail_label(buffer, room);

    let input_bg = Color::Rgb {
        r: 30,
        g: 35,
        b: 50,
    };
    let prompt_fg = Color::Rgb {
        r: 102,
        g: 217,
        b: 239,
    };
    let text_fg = Color::White;
    let cursor_bg = Color::Rgb {
        r: 80,
        g: 90,
        b: 120,
    };

    // Clear row
    queue!(
        stdout,
        SetAttribute(Attribute::Reset),
        cursor::MoveTo(0, y),
        AnsiBackground(input_bg),
        Print(" ".repeat(w))
    )?;

    // Prompt
    queue!(
        stdout,
        cursor::MoveTo(x as u16, y),
        AnsiBackground(input_bg),
        AnsiForeground(prompt_fg),
        SetAttribute(Attribute::Bold),
        Print(&prompt),
        SetAttribute(Attribute::Reset),
        AnsiBackground(input_bg),
        AnsiForeground(text_fg),
        Print(&buffer),
    )?;

    // Cursor block
    let cursor_x = (x + prompt.width() + buffer.width()) as u16;
    if (cursor_x as usize) < w {
        queue!(
            stdout,
            cursor::MoveTo(cursor_x, y),
            AnsiBackground(cursor_bg),
            Print(" "),
        )?;
    }

    queue!(stdout, ResetColor, SetAttribute(Attribute::Reset))?;
    Ok(())
}

// ─── Fuzzy command palette overlay (#86) ───────────────────

/// State the renderer needs to draw the bottom-anchored fuzzy palette
/// overlay. The owning state machine (server/mod.rs, phase 2b) populates
/// the slice + index every keystroke; the renderer is pure.
///
/// Lives on the renderer side so phase 2b can `use render::PaletteOverlayState`
/// without re-deriving the shape.
pub struct PaletteOverlayState<'a> {
    pub query: &'a str,
    pub matches: &'a [fuzzy::Match],
    pub selected: usize,
    /// Snapshot of the candidate list the matches index into. Borrowed from
    /// the same `FuzzyIndex` whose `search()` produced `matches`.
    pub entries: &'a [fuzzy::Entry],
}

/// Render the 8-row fuzzy command-palette overlay, anchored at the bottom
/// of the screen. Layout:
/// - Row 1: `: {query}` text-input style prompt
/// - Rows 2..7: top 6 matches; the selected row gets a `border_active`
///   background.
/// - Each match line: kind icon (`@` session, `#` pane, `T` tab, `>` cmd,
///   `*` recent) + display text + faded payload tail.
/// - When `matches` is empty: row 2 shows `(no matches)`.
///
/// `palette` drives the prompt accent and selection background; `None`
/// falls back to the legacy hardcoded look.
pub fn draw_palette_overlay(
    stdout: &mut impl Write,
    state: &PaletteOverlayState<'_>,
    term_width: u16,
    term_height: u16,
    palette: Option<&ResolvedPalette>,
) -> anyhow::Result<()> {
    let matches_ = state.matches;
    let entries = state.entries;
    let query = state.query;
    let selected = state.selected;
    if term_width < 10 || term_height < 8 {
        return Ok(());
    }
    const OVERLAY_ROWS: u16 = 8;
    let top = term_height.saturating_sub(OVERLAY_ROWS);
    let w = term_width as usize;

    // Colour selection.
    let bg = Color::Rgb {
        r: 18,
        g: 22,
        b: 32,
    };
    let prompt_fg = palette
        .map(|p| resolved_to_crossterm(&p.border_active))
        .unwrap_or(Color::Rgb {
            r: 102,
            g: 217,
            b: 239,
        });
    let select_bg = palette
        .map(|p| resolved_to_crossterm(&p.border_active))
        .unwrap_or(Color::Rgb {
            r: 50,
            g: 90,
            b: 130,
        });
    let select_fg = palette
        .map(|p| resolved_to_crossterm(&p.tab_active_fg))
        .unwrap_or(Color::White);
    let row_fg = palette
        .map(|p| resolved_to_crossterm(&p.fg))
        .unwrap_or(Color::Rgb {
            r: 220,
            g: 225,
            b: 240,
        });
    let dim_fg = Color::Rgb {
        r: 110,
        g: 120,
        b: 140,
    };
    let kind_fg = palette
        .map(|p| resolved_to_crossterm(&p.tab_inactive_fg))
        .unwrap_or(Color::Rgb {
            r: 130,
            g: 165,
            b: 200,
        });

    // Backdrop fill.
    let blank = " ".repeat(w);
    for dy in 0..OVERLAY_ROWS {
        queue!(
            stdout,
            cursor::MoveTo(0, top + dy),
            AnsiBackground(bg),
            Print(&blank)
        )?;
    }

    // Row 0: ": query" prompt line.
    queue!(
        stdout,
        cursor::MoveTo(1, top),
        AnsiBackground(bg),
        AnsiForeground(prompt_fg),
        SetAttribute(Attribute::Bold),
        Print(":"),
        SetAttribute(Attribute::Reset),
        AnsiBackground(bg),
        AnsiForeground(row_fg),
        Print(" "),
        Print(tail_label(query, w.saturating_sub(4))),
    )?;

    // Match rows.
    let visible_rows = (OVERLAY_ROWS - 2) as usize; // 6
    if matches_.is_empty() {
        queue!(
            stdout,
            cursor::MoveTo(2, top + 2),
            AnsiBackground(bg),
            AnsiForeground(dim_fg),
            SetAttribute(Attribute::Italic),
            Print(truncate_label("(no matches)", w.saturating_sub(2))),
            SetAttribute(Attribute::Reset),
        )?;
    } else {
        let first = selected.saturating_sub(visible_rows - 1);
        for (i, m) in matches_.iter().skip(first).take(visible_rows).enumerate() {
            let row_y = top + 2 + i as u16;
            let is_sel = i + first == selected;
            let row_bg = if is_sel { select_bg } else { bg };
            let label_fg = if is_sel { select_fg } else { row_fg };

            // Fill background for the row first.
            queue!(
                stdout,
                cursor::MoveTo(0, row_y),
                AnsiBackground(row_bg),
                Print(&blank),
                cursor::MoveTo(2, row_y),
            )?;

            let entry = entries.get(m.index);
            let icon = match entry.map(|e| e.kind) {
                Some(fuzzy::EntryKind::Session) => '@',
                Some(fuzzy::EntryKind::Pane) => '#',
                Some(fuzzy::EntryKind::Tab) => 'T',
                Some(fuzzy::EntryKind::Command) => '>',
                Some(fuzzy::EntryKind::Recent) => '*',
                None => '?',
            };

            // Icon
            queue!(
                stdout,
                AnsiForeground(if is_sel { select_fg } else { kind_fg }),
                SetAttribute(Attribute::Bold),
                Print(icon),
                SetAttribute(Attribute::Reset),
                AnsiBackground(row_bg),
                AnsiForeground(label_fg),
                Print(" "),
            )?;

            // Display text + faded payload tail (when payload != display).
            let display = entry.map(|e| e.display.as_str()).unwrap_or("?");
            let display = truncate_label(display, w.saturating_sub(4));
            queue!(stdout, Print(&display))?;
            if let Some(e) = entry {
                if e.payload != e.display {
                    let tail = format!("  {}", e.payload);
                    let tail = truncate_label(&tail, w.saturating_sub(4 + display.width()));
                    queue!(
                        stdout,
                        AnsiForeground(if is_sel { select_fg } else { dim_fg }),
                        Print(&tail),
                    )?;
                }
            }
        }
    }

    queue!(stdout, ResetColor, SetAttribute(Attribute::Reset))?;
    Ok(())
}

/// Draw the OSC 52 confirm prompt (#79).
///
/// Modal yellow pill at the bottom-left explaining which pane is asking to
/// write to the system clipboard and how big the payload is. The user
/// answers via `y` (allow), `n` (deny), or `Esc` (re-queue the prompt).
/// Rendered over the status bar so it's unmissable while the daemon is
/// blocked on the decision.
pub fn draw_osc52_confirm_overlay(
    stdout: &mut impl Write,
    pane_id: usize,
    byte_count: usize,
    palette: Option<&ResolvedPalette>,
    term_w: u16,
    term_h: u16,
) -> anyhow::Result<()> {
    if term_w == 0 || term_h == 0 {
        return Ok(());
    }
    let y = term_h.saturating_sub(1);
    let bg = Color::Rgb {
        r: 130,
        g: 90,
        b: 20,
    };
    let fg = palette
        .map(|p| resolved_to_crossterm(&p.tab_active_fg))
        .unwrap_or(Color::Rgb {
            r: 255,
            g: 240,
            b: 200,
        });
    let msg = format!(" OSC52 pane #{pane_id}: allow {byte_count} bytes to clipboard? [y/n/Esc] ");
    let max_inner = (term_w as usize).saturating_sub(2);
    let truncated: String = if msg.width() > max_inner {
        let mut acc = String::new();
        let mut w = 0usize;
        for ch in msg.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if w + cw > max_inner.saturating_sub(1) {
                acc.push('…');
                break;
            }
            acc.push(ch);
            w += cw;
        }
        acc
    } else {
        msg
    };
    queue!(
        stdout,
        cursor::MoveTo(0, y),
        AnsiBackground(bg),
        AnsiForeground(fg),
        SetAttribute(Attribute::Bold),
        Print(&truncated),
        SetAttribute(Attribute::Reset),
        ResetColor,
        cursor::Hide,
    )?;
    Ok(())
}

/// Draw a transient flash message on the status-bar row.
///
/// Used by the command palette to surface parse errors (e.g.
/// `unknown command: foo (try ?)`) and `:display-message` text for ~2 s.
/// Renders over the normal status bar when active so the message is
/// unmissable; the caller is responsible for clearing it after the TTL.
pub fn draw_flash_message(
    stdout: &mut impl Write,
    term_w: u16,
    term_h: u16,
    text: &str,
) -> anyhow::Result<()> {
    if term_w == 0 || term_h == 0 {
        return Ok(());
    }
    let y = term_h.saturating_sub(1);
    let w = term_w as usize;
    let x = usize::from(w > 1);

    // Yellow-on-dark — same family as the existing PREFIX badge so the row
    // reads as "ephemeral status" rather than a popup error dialog.
    let bg = Color::Rgb {
        r: 60,
        g: 30,
        b: 10,
    };
    let fg = Color::Rgb {
        r: 255,
        g: 200,
        b: 80,
    };

    queue!(stdout, cursor::MoveTo(0, y), AnsiBackground(bg))?;
    for _ in 0..w {
        queue!(stdout, Print(" "))?;
    }
    queue!(
        stdout,
        cursor::MoveTo(x as u16, y),
        AnsiForeground(fg),
        SetAttribute(Attribute::Bold),
        Print(truncate_label(text, w.saturating_sub(x))),
        SetAttribute(Attribute::Reset),
        ResetColor,
    )?;
    Ok(())
}

/// Y position of the tab bar given terminal dimensions.
pub fn tab_bar_y(term_h: u16, show_status_bar: bool) -> u16 {
    if show_status_bar {
        term_h.saturating_sub(2)
    } else {
        term_h.saturating_sub(1)
    }
}

/// Hit-test: which tab index was clicked at column `x` on the tab bar row?
/// `tabs` must be the same list passed to `draw_tab_bar`.
pub fn tab_bar_hit(x: u16, tabs: &[(usize, String, bool)], term_w: u16) -> Option<usize> {
    if tabs.len() <= 1 {
        return None;
    }
    let w = term_w as usize;
    let mut col = 1usize;
    for (idx, name, _) in tabs {
        let (_, _, tab_width) = tab_label_parts(*idx, name);
        if col + tab_width + 2 > w {
            break;
        }
        if (x as usize) >= col && (x as usize) < col + tab_width {
            return Some(*idx);
        }
        col += tab_width + 2; // tab + " │"
    }
    None
}

fn tab_label_parts(index: usize, name: &str) -> (String, String, usize) {
    let prefix = format!("  {}: ", index.saturating_add(1));
    let name = truncate_label(name, usize::MAX);
    let width = prefix.len() + name.width() + 2;
    (prefix, name, width)
}

/// Draw tab indicators in the status bar area.
/// Renders a tab bar above the main status bar (uses 1 extra row).
/// `tabs` is `(index, name, is_active)` for each tab.
///
/// `palette` (#85): when `Some`, overrides the active-tab fg/bg + index colour
/// from `ResolvedPalette::tab_active_*` and `border_active`. `None` keeps the
/// legacy hardcoded look.
pub fn draw_tab_bar(
    stdout: &mut impl Write,
    term_w: u16,
    term_h: u16,
    tabs: &[(usize, String, bool)],
    show_status_bar: bool,
    palette: Option<&ResolvedPalette>,
) -> anyhow::Result<()> {
    if tabs.len() <= 1 || term_w == 0 || term_h <= u16::from(show_status_bar) {
        return Ok(());
    }

    let y = tab_bar_y(term_h, show_status_bar);
    let w = term_w as usize;

    let tab_bg = Color::Rgb {
        r: 24,
        g: 26,
        b: 34,
    };
    let active_tab_bg = palette
        .map(|p| resolved_to_crossterm(&p.tab_active_bg))
        .unwrap_or(Color::Rgb {
            r: 50,
            g: 55,
            b: 70,
        });
    let active_tab_fg = palette
        .map(|p| resolved_to_crossterm(&p.tab_active_fg))
        .unwrap_or(Color::Rgb {
            r: 220,
            g: 225,
            b: 240,
        });
    let inactive_fg = palette
        .map(|p| resolved_to_crossterm(&p.tab_inactive_fg))
        .unwrap_or(Color::Rgb {
            r: 100,
            g: 110,
            b: 130,
        });
    let index_fg = palette
        .map(|p| resolved_to_crossterm(&p.border_active))
        .unwrap_or(Color::Rgb {
            r: 80,
            g: 180,
            b: 220,
        });
    let sep_fg = Color::Rgb {
        r: 50,
        g: 55,
        b: 65,
    };

    // Clear the row
    queue!(stdout, cursor::MoveTo(0, y), AnsiBackground(tab_bg),)?;
    for _ in 0..w {
        queue!(stdout, Print(" "))?;
    }

    // Render tabs with generous spacing: "  N : name  │"
    queue!(stdout, cursor::MoveTo(1, y))?;
    let mut col = 1usize;

    for (idx, name, is_active) in tabs {
        let (prefix, name, tab_width) = tab_label_parts(*idx, name);
        if col + tab_width + 2 > w {
            break;
        }

        let bg = if *is_active { active_tab_bg } else { tab_bg };

        queue!(stdout, AnsiBackground(bg), AnsiForeground(index_fg),)?;
        if *is_active {
            queue!(stdout, SetAttribute(Attribute::Bold))?;
        }
        queue!(stdout, Print(prefix))?;

        queue!(
            stdout,
            AnsiForeground(if *is_active {
                active_tab_fg
            } else {
                inactive_fg
            })
        )?;
        queue!(stdout, Print(name))?;
        queue!(stdout, Print("  "))?;

        if *is_active {
            queue!(stdout, SetAttribute(Attribute::Reset))?;
        }

        // Separator
        queue!(
            stdout,
            AnsiBackground(tab_bg),
            AnsiForeground(sep_fg),
            Print(" │"),
        )?;
        col += tab_width + 2; // tab + " │"
    }

    queue!(stdout, ResetColor, SetAttribute(Attribute::Reset))?;
    Ok(())
}

// ─── Partial-redraw helpers (#80) ──────────────────────────
//
// `render_panes` only repaints the status bar when `full_redraw` is
// set. Partial-redraw paths therefore left the clock, session name,
// and focus indicator stale until something forced a full repaint
// (visible regression on quiet sessions). The two helpers below let
// `render_glue.rs` repaint just the bar(s) when `RenderUpdate`'s new
// `status_dirty` / `tabs_dirty` bits are set without `full_redraw`.
//
// Both helpers wrap their writes in a synchronized-update block so a
// half-rendered bar never reaches the client. They re-emit `cursor::Hide`
// at the end because both bars sit on rows the cursor must not blink on.

/// Repaint only the status bar row. Partial-redraw fast path for
/// 1-Hz clock ticks, focus switches, broadcast toggles, etc — see
/// [`crate::bootstrap::RenderUpdate::status_dirty`].
///
/// Mirrors the call signature of [`draw_status_bar_full`] so the call
/// site can swap one for the other without re-marshalling state.
///
/// `#[allow(dead_code)]`: the consumer (`server/render_glue.rs`) is
/// off-limits for this slice — see the integration TODO on the host
/// branch PR.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
pub fn redraw_status_only(
    stdout: &mut impl Write,
    term_w: u16,
    term_h: u16,
    active_idx: usize,
    total: usize,
    mode_label: &str,
    pane_name: &str,
    selection_chars: usize,
    palette: Option<&ResolvedPalette>,
) -> anyhow::Result<()> {
    queue!(stdout, terminal::BeginSynchronizedUpdate)?;
    draw_status_bar_full(
        stdout,
        term_w,
        term_h,
        active_idx,
        total,
        mode_label,
        pane_name,
        selection_chars,
        palette,
    )?;
    queue!(stdout, cursor::Hide, terminal::EndSynchronizedUpdate)?;
    Ok(())
}

/// Repaint only the tab bar row. Partial-redraw fast path for
/// tab add / remove / rename / reorder / focus — see
/// [`crate::bootstrap::RenderUpdate::tabs_dirty`].
///
/// Mirrors the call signature of [`draw_tab_bar`].
///
/// `#[allow(dead_code)]`: same integration deferral as
/// [`redraw_status_only`].
#[allow(dead_code)]
pub fn redraw_tabs_only(
    stdout: &mut impl Write,
    term_w: u16,
    term_h: u16,
    tabs: &[(usize, String, bool)],
    show_status_bar: bool,
    palette: Option<&ResolvedPalette>,
) -> anyhow::Result<()> {
    queue!(stdout, terminal::BeginSynchronizedUpdate)?;
    draw_tab_bar(stdout, term_w, term_h, tabs, show_status_bar, palette)?;
    queue!(stdout, cursor::Hide, terminal::EndSynchronizedUpdate)?;
    Ok(())
}

/// Get current time as HH:MM using libc (no chrono dependency).
/// Uses `localtime_r` for thread safety.
fn now_hhmm() -> String {
    #[cfg(unix)]
    {
        let mut t: libc::time_t = 0;
        unsafe { libc::time(&mut t) };
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let result = unsafe { libc::localtime_r(&t, &mut tm) };
        if result.is_null() {
            return "--:--".to_string();
        }
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
        if y != btn_y || rect.w == 0 || rect.h == 0 {
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
    stdout: &mut impl Write,
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
    let mut bmap = if border_style.is_none() {
        BorderMap::new(0, 0)
    } else {
        BorderMap::new(term_w, border_h)
    };
    if !border_style.is_none() {
        bmap.add_h_line(0, term_w - 1, 0);
        bmap.add_h_line(0, term_w - 1, border_h - 1);
        bmap.add_v_line(0, 0, border_h - 1);
        bmap.add_v_line(term_w - 1, 0, border_h - 1);
    }
    for cell in bmap.into_cells() {
        queue!(
            stdout,
            cursor::MoveTo(cell.x, cell.y),
            AnsiForeground(ACTIVE_COLOR),
            Print(border_char(&cell.flags, &chars))
        )?;
    }

    // Title bar
    let rect = content_area(term_w, term_h, show_status_bar, border_style);
    let avail = rect.w as usize;
    let title = truncate_label(
        &format!(" {}:{} [ZOOM] ", pane_idx + 1, label),
        avail.saturating_sub(1),
    );
    if avail > title.width() {
        queue!(
            stdout,
            cursor::MoveTo(rect.x, 0),
            AnsiForeground(ACTIVE_COLOR),
            Print(chars.h),
            AnsiForeground(Color::White),
            SetAttribute(Attribute::Bold),
            Print(&title),
            SetAttribute(Attribute::Reset),
            AnsiForeground(ACTIVE_COLOR),
        )?;
        for _ in 0..avail - title.width() - 1 {
            queue!(stdout, Print(chars.h))?;
        }
    }

    // Content area
    draw_content(stdout, pane, &rect, pane.is_alive(), None)?;

    // Cursor
    draw_pane_cursor(stdout, pane, &rect)?;

    queue!(stdout, ResetColor, SetAttribute(Attribute::Reset))?;
    Ok(())
}

// ─── Help Overlay ──────────────────────────────────────────

pub fn draw_help_overlay(stdout: &mut impl Write, term_w: u16, term_h: u16) -> anyhow::Result<()> {
    if term_w == 0 || term_h == 0 {
        return Ok(());
    }
    let help_lines = [
        "",
        "  DEFAULT SHORTCUTS",
        "  F1            Settings panel",
        "  F2            Equalize all pane sizes",
        "  Alt+Arrow     Navigate directional",
        "",
        "  PREFIX MODE (Ctrl+B then)",
        "  TABS:",
        "  c             New tab",
        "  n / p         Next / previous tab",
        "  0-9           Jump to tab by number",
        "  &             Close current tab",
        "  PANES:",
        "  % / \"         Split H / V",
        "  o / Arrow     Next / navigate pane",
        "  x             Close pane",
        "  z             Zoom toggle",
        "  B             Broadcast mode (type in all)",
        "  R             Resize mode (arrows/hjkl, q)",
        "  { }           Swap pane prev/next",
        "  [             Copy mode (hjkl/v/y/search, q)",
        "  d             Detach session",
        "  s             Toggle status bar",
        "  r             Reload global config",
        "  :             Command palette",
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

    let w = 50.min(term_w as usize);
    let h = (help_lines.len() + 2).min(term_h as usize);
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
        AnsiBackground(Color::Rgb { r: 4, g: 5, b: 8 }),
        terminal::Clear(ClearType::All)
    )?;

    // Panel background
    let blank = " ".repeat(w);
    for dy in 0..h as u16 {
        queue!(
            stdout,
            cursor::MoveTo(ox, oy + dy),
            AnsiBackground(bg),
            Print(&blank)
        )?;
    }

    // Top border
    queue!(
        stdout,
        cursor::MoveTo(ox, oy),
        AnsiBackground(bg),
        AnsiForeground(border_fg),
    )?;
    let title = truncate_label(" Help (Ctrl+B ?) ", w);
    let pad = w.saturating_sub(title.len() + 2);
    let lp = pad / 2;
    let rp = pad - lp;
    queue!(
        stdout,
        Print("─".repeat(lp)),
        AnsiForeground(Color::White),
        SetAttribute(Attribute::Bold),
        Print(title),
        SetAttribute(Attribute::Reset),
        AnsiForeground(border_fg),
        AnsiBackground(bg),
        Print("─".repeat(rp)),
    )?;

    // Content
    for (i, line) in help_lines.iter().take(h.saturating_sub(2)).enumerate() {
        let line = truncate_label(line, w);
        let line = format!("{}{}", line, " ".repeat(w.saturating_sub(line.width())));
        let y = oy + 1 + i as u16;
        queue!(stdout, cursor::MoveTo(ox, y), AnsiBackground(bg))?;

        if line.contains("SHORTCUTS")
            || line.contains("PREFIX MODE")
            || line.contains("MOUSE")
            || line.contains("FEATURES")
        {
            queue!(
                stdout,
                AnsiForeground(Color::Rgb {
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
                AnsiForeground(Color::Rgb {
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
                AnsiForeground(Color::Rgb {
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
        AnsiBackground(bg),
        AnsiForeground(border_fg),
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
    stdout: &mut impl Write,
    layout: &Layout,
    inner: &Rect,
) -> anyhow::Result<()> {
    if inner.w == 0 || inner.h == 0 {
        return Ok(());
    }
    let rects = layout.pane_rects(inner);
    let ids = layout.pane_ids();

    for (display_idx, &pid) in ids.iter().enumerate() {
        let Some(num) = quick_jump_label(display_idx) else {
            continue;
        };
        if let Some(rect) = rects.get(&pid) {
            let num = num.to_string();
            let num_w = num.len() as u16;

            if rect.w < num_w + 4 || rect.h < 3 {
                continue;
            }

            let cx = rect.x + (rect.w - num_w - 4) / 2;
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
                    AnsiBackground(bg),
                    Print(" ".repeat(box_w)),
                )?;
            }

            // Number centered in middle row
            queue!(
                stdout,
                cursor::MoveTo(cx + 2, cy + 1),
                AnsiBackground(bg),
                AnsiForeground(fg),
                SetAttribute(Attribute::Bold),
                Print(&num),
                SetAttribute(Attribute::Reset),
            )?;
        }
    }

    // Hint at bottom
    let hint = truncate_label(
        "Press 1-9 or 0 to jump, any other key to cancel",
        inner.w as usize,
    );
    let hx = inner.x + inner.w.saturating_sub(hint.len() as u16) / 2;
    let hy = inner.y.saturating_add(inner.h.saturating_sub(1));
    queue!(
        stdout,
        cursor::MoveTo(hx, hy),
        AnsiForeground(Color::Rgb {
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

/// Child output must not inherit crossterm's process-wide NO_COLOR filtering.
fn write_pane_color(
    stdout: &mut impl Write,
    color: vt100::Color,
    foreground: bool,
) -> std::io::Result<()> {
    let slot = if foreground { 38 } else { 48 };
    match color {
        vt100::Color::Default => write!(stdout, "\x1b[{}m", slot + 1),
        vt100::Color::Idx(i) => write!(stdout, "\x1b[{slot};5;{i}m"),
        vt100::Color::Rgb(r, g, b) => write!(stdout, "\x1b[{slot};2;{r};{g};{b}m"),
    }
}

#[cfg(test)]
mod partial_redraw_tests {
    //! Smoke tests for the #80 partial-redraw helpers. We do not assert
    //! on the exact byte stream — that is the renderer-snapshot tests'
    //! job — only that:
    //!   1. The helpers run to completion without panicking on a fresh
    //!      buffer (no surprise unwrap on missing state).
    //!   2. They produce *some* output (so a "noop" regression is
    //!      caught) and end with a cursor-hide so the cursor never
    //!      blinks on the bar row.

    use super::*;

    #[test]
    fn bounded_dense_border_map_matches_sparse_flags_and_order() {
        let mut dense = BorderMap::new(80, 24);
        let mut sparse = BorderMap::new(80, 24);
        sparse.dense.clear();
        for map in [&mut dense, &mut sparse] {
            map.add_h_line(0, 79, 0);
            map.add_h_line(0, 79, 23);
            map.add_h_line(8, 60, 12);
            map.add_v_line(0, 0, 23);
            map.add_v_line(40, 0, 23);
            map.add_v_line(79, 0, 23);
        }
        let flatten = |map: BorderMap| {
            map.into_cells()
                .into_iter()
                .map(|c| (c.x, c.y, c.flags))
                .collect::<Vec<_>>()
        };
        assert_eq!(flatten(dense), flatten(sparse));
        assert!(BorderMap::new(u16::MAX, u16::MAX).dense.is_empty());
    }

    #[test]
    fn reliability_ansi16_commands_use_legacy_sgr_or_no_color() {
        let mut output = String::new();
        write_ui_color(&mut output, Color::AnsiValue(1), true).unwrap();
        write_ui_color(&mut output, Color::White, false).unwrap();
        if crossterm::style::Colored::ansi_color_disabled_memoized() {
            assert!(output.is_empty());
        } else {
            assert_eq!(output, "\x1b[31m\x1b[107m");
        }
    }

    #[test]
    fn reliability_copy_overlay_highlights_display_spans_and_shows_cursor() {
        let mut source = vt100::Parser::new(2, 10, 0);
        source.process("한 test".as_bytes());
        let mut state = CopyModeState::new(2, 10);
        state.cursor_row = 0;
        state.cursor_col = 3;
        state.search_matches = vec![(0, 0, 2)];
        state.phase = crate::copy_mode::Phase::VisualChar {
            anchor_row: 0,
            anchor_col: 3,
        };
        let mut output = Vec::new();
        draw_copy_mode_overlay(
            &mut output,
            source.screen(),
            &Rect {
                x: 1,
                y: 1,
                w: 10,
                h: 2,
            },
            &state,
        )
        .unwrap();
        let mut actual = vt100::Parser::new(4, 12, 0);
        actual.process(&output);
        assert_eq!(
            actual.screen().cell(1, 1).unwrap().bgcolor(),
            vt100::Color::Idx(3)
        );
        assert!(actual.screen().cell(1, 4).unwrap().inverse());
        assert_eq!(actual.screen().cursor_position(), (1, 4));
        assert!(!actual.screen().hide_cursor());
    }

    #[test]
    fn reliability_zero_sized_bars_emit_nothing() {
        for (w, h) in [(0, 24), (80, 0)] {
            let mut buf = Vec::new();
            draw_status_bar(&mut buf, w, h, 0, 1, "").unwrap();
            draw_text_input(&mut buf, w, h, ":", "text").unwrap();
            draw_flash_message(&mut buf, w, h, "message").unwrap();
            assert!(buf.is_empty());
        }
    }

    #[test]
    fn reliability_labels_cannot_inject_terminal_controls() {
        assert_eq!(truncate_label("hi\x1b\n\r\u{7}there", 20), "hithere");
    }

    #[test]
    fn reliability_text_input_uses_display_width_and_stays_on_row() {
        let mut buf = Vec::new();
        draw_text_input(&mut buf, 8, 3, ":", "한e\u{301}").unwrap();
        let mut parser = vt100::Parser::new(3, 8, 0);
        parser.process(&buf);
        assert_eq!(parser.screen().cursor_position(), (2, 6));
        buf.clear();
        draw_text_input(&mut buf, 8, 3, ":", &"x".repeat(100)).unwrap();
        parser.process(&buf);
        assert!(parser.screen().rows(0, 8).take(2).all(|row| row.is_empty()));
    }

    #[test]
    fn reliability_content_preserves_attributes_colours_and_unicode() {
        let mut source = vt100::Parser::new(2, 20, 0);
        source.process(
            "\x1b[1;31mA\x1b[22;3mB\x1b[23;4mC\x1b[24mD\x1b[7mE\x1b[0m한e\u{301}🙂X".as_bytes(),
        );
        let mut output = Vec::new();
        let rect = Rect {
            x: 1,
            y: 1,
            w: 20,
            h: 2,
        };
        draw_screen_content(&mut output, source.screen(), &rect, true, None).unwrap();
        let mut actual = vt100::Parser::new(4, 22, 0);
        actual.process(b"\x1b[1;32;45m");
        actual.process(&output);
        for row in 0..2 {
            for col in 0..20 {
                let expected = source.screen().cell(row, col).unwrap();
                let got = actual.screen().cell(row + 1, col + 1).unwrap();
                assert_eq!(
                    got.contents(),
                    if expected.contents().is_empty() && !expected.is_wide_continuation() {
                        " "
                    } else {
                        expected.contents()
                    },
                    "cell {row},{col}"
                );
                assert_eq!(
                    (got.bold(), got.italic(), got.underline(), got.inverse()),
                    (
                        expected.bold(),
                        expected.italic(),
                        expected.underline(),
                        expected.inverse()
                    ),
                    "attributes {row},{col}"
                );
                assert_eq!(
                    (got.fgcolor(), got.bgcolor()),
                    (expected.fgcolor(), expected.bgcolor()),
                    "colours {row},{col}"
                );
            }
        }
    }

    #[test]
    fn reliability_clipped_wide_cell_stays_inside_pane() {
        let mut source = vt100::Parser::new(1, 4, 0);
        source.process("a한".as_bytes());
        let mut buf = Vec::new();
        draw_screen_content(
            &mut buf,
            source.screen(),
            &Rect {
                x: 1,
                y: 0,
                w: 2,
                h: 1,
            },
            true,
            None,
        )
        .unwrap();
        let mut actual = vt100::Parser::new(2, 6, 0);
        actual.process(b"......\r\n......");
        actual.process(&buf);
        assert_eq!(actual.screen().cell(0, 3).unwrap().contents(), ".");
        assert_eq!(actual.screen().cell(0, 2).unwrap().contents(), " ");
    }

    #[test]
    fn reliability_border_cache_and_content_geometry_agree() {
        for style in [BorderStyle::Rounded, BorderStyle::None] {
            for status in [false, true] {
                for w in 0..10 {
                    for h in 0..10 {
                        let layout = Layout::from_grid(3, 3);
                        let cache = build_border_cache_with_style(&layout, status, w, h, style);
                        let area = content_area(w, h, status, style);
                        assert_eq!(
                            (cache.inner.x, cache.inner.y, cache.inner.w, cache.inner.h),
                            (area.x, area.y, area.w, area.h)
                        );
                        for cell in &cache.cells {
                            assert!(
                                cell.x < w && cell.y < h.saturating_sub(u16::from(status)),
                                "border cell {},{} in {w}x{h}",
                                cell.x,
                                cell.y
                            );
                        }
                        for rect in cache.pane_rects.values() {
                            assert!(rect.x + rect.w <= w && rect.y + rect.h <= h);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn reliability_tab_hits_match_multi_digit_and_cjk_labels() {
        let tabs = vec![(9, "한".to_string(), true), (10, "log".to_string(), false)];
        let mut output = Vec::new();
        draw_tab_bar(&mut output, 40, 3, &tabs, true, None).unwrap();
        let mut parser = vt100::Parser::new(3, 40, 0);
        parser.process(&output);
        let first_end = 1 + tab_label_parts(9, "한").2;
        assert_eq!(tab_bar_hit(1, &tabs, 40), Some(9));
        assert_eq!(tab_bar_hit(first_end as u16 - 1, &tabs, 40), Some(9));
        assert_eq!(tab_bar_hit(first_end as u16, &tabs, 40), None);
        assert_eq!(tab_bar_hit(first_end as u16 + 2, &tabs, 40), Some(10));
        assert_eq!(
            parser
                .screen()
                .cell(1, first_end as u16 + 1)
                .unwrap()
                .contents(),
            "│"
        );
    }

    #[test]
    fn redraw_status_only_emits_bytes_and_hides_cursor() {
        let mut buf: Vec<u8> = Vec::new();
        redraw_status_only(&mut buf, 80, 24, 0, 1, "PREFIX", "shell", 0, None)
            .expect("redraw_status_only succeeds");
        assert!(!buf.is_empty(), "status-only redraw must emit output");
        // crossterm's cursor::Hide writes ESC[?25l — the helper appends
        // it just before EndSynchronizedUpdate so any subsequent frame
        // does not flash the cursor on the status row.
        let s = String::from_utf8_lossy(&buf);
        assert!(
            s.contains("\x1b[?25l"),
            "status-only redraw must hide the cursor at the end"
        );
    }

    #[test]
    fn redraw_status_only_handles_empty_mode_label() {
        // The default-status-bar path takes a different branch when
        // both `mode_label` and `selection_chars` are empty; we still
        // need to repaint the row (clock tick scenario).
        let mut buf: Vec<u8> = Vec::new();
        redraw_status_only(&mut buf, 80, 24, 0, 1, "", "", 0, None).expect("succeeds");
        assert!(!buf.is_empty());
    }

    #[test]
    fn reliability_default_hints_do_not_advertise_shell_editing_keys() {
        for mode in ["", "ZOOM"] {
            let mut output = Vec::new();
            draw_status_bar(&mut output, 300, 4, 0, 1, mode).unwrap();
            let text = String::from_utf8_lossy(&output);
            for old_binding in ["Ctrl+D", "Ctrl+E", "Ctrl+N", "Ctrl+G", "Ctrl+B p"] {
                assert!(!text.contains(old_binding), "{mode}: stale {old_binding}");
            }
            assert!(text.contains("Ctrl+B"));
        }
        let mut output = Vec::new();
        draw_help_overlay(&mut output, 100, 100).unwrap();
        let text = String::from_utf8_lossy(&output);
        for old_binding in ["Ctrl+D", "Ctrl+E", "Ctrl+N", "Ctrl+G", "Ctrl+W"] {
            assert!(!text.contains(old_binding), "help: stale {old_binding}");
        }
    }

    #[test]
    fn redraw_status_only_handles_palette_mode_label() {
        // Issue #87: CommandPalette mode label is ":" — verify it
        // produces output (the new match arm with palette hints).
        let mut buf: Vec<u8> = Vec::new();
        redraw_status_only(&mut buf, 80, 24, 0, 1, ":", "shell", 0, None)
            .expect("palette mode succeeds");
        assert!(!buf.is_empty(), "palette mode must emit output");
    }

    #[test]
    fn redraw_status_only_handles_rename_mode_label() {
        // Issue #87: RENAME mode label triggers the rename hint arm.
        let mut buf: Vec<u8> = Vec::new();
        redraw_status_only(&mut buf, 80, 24, 0, 1, "RENAME", "shell", 0, None)
            .expect("rename mode succeeds");
        assert!(!buf.is_empty(), "rename mode must emit output");
    }

    #[test]
    fn redraw_tabs_only_emits_bytes_when_multiple_tabs() {
        let tabs = vec![
            (0_usize, "main".to_string(), true),
            (1_usize, "logs".to_string(), false),
        ];
        let mut buf: Vec<u8> = Vec::new();
        redraw_tabs_only(&mut buf, 80, 24, &tabs, true, None).expect("succeeds");
        assert!(!buf.is_empty(), "tab-only redraw must emit output");
        let s = String::from_utf8_lossy(&buf);
        assert!(s.contains("\x1b[?25l"), "must hide cursor at the end");
    }

    #[test]
    fn redraw_tabs_only_is_noop_with_single_tab() {
        // `draw_tab_bar` early-returns when `tabs.len() <= 1` since the
        // bar is hidden; the helper should still wrap that in the sync
        // block (which itself emits a few bytes — that's fine), but
        // must not panic.
        let tabs = vec![(0_usize, "main".to_string(), true)];
        let mut buf: Vec<u8> = Vec::new();
        redraw_tabs_only(&mut buf, 80, 24, &tabs, true, None).expect("succeeds");
        // BeginSynchronizedUpdate + EndSynchronizedUpdate + cursor::Hide
        // emit a small fixed envelope; we just ensure no panic and some
        // bytes for the envelope.
        assert!(!buf.is_empty());
    }
}
