use std::collections::HashMap;
use std::io::{self, Write};
use std::time::Duration;

use crossterm::event::{KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode},
    execute, queue,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};

mod layout;
mod pane;
mod render;
mod settings;

use layout::{Direction, Layout, NavDir, Rect, SepHit};
use pane::Pane;
use render::BorderStyle;
use settings::{Settings, SettingsAction};

fn main() -> anyhow::Result<()> {
    // Prevent nesting: if we're already inside ezpn, bail out
    if std::env::var("EZPN").is_ok() {
        eprintln!("ezpn: cannot run inside an existing ezpn session");
        std::process::exit(1);
    }

    let config = parse_args()?;

    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        event::EnableMouseCapture,
        cursor::Hide
    )?;

    let result = run(&mut stdout, &config);

    let _ = execute!(
        io::stdout(),
        cursor::Show,
        event::DisableMouseCapture,
        LeaveAlternateScreen
    );
    let _ = terminal::disable_raw_mode();

    result
}

struct Config {
    rows: usize,
    cols: usize,
    border: BorderStyle,
    shell: String,
}

fn parse_args() -> anyhow::Result<Config> {
    let args: Vec<String> = std::env::args().collect();
    let mut rows = 1usize;
    let mut cols = 2usize;
    let mut border = BorderStyle::Rounded;
    let mut shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut vertical = false; // -d v → vertical default direction
    let mut i = 1;
    let mut positional = Vec::new();

    while i < args.len() {
        match args[i].as_str() {
            "-b" | "--border" => {
                i += 1;
                if i < args.len() {
                    border = BorderStyle::from_str(&args[i]).ok_or_else(|| {
                        anyhow::anyhow!(
                            "Unknown border style: '{}'. Options: single, rounded, heavy, double",
                            &args[i]
                        )
                    })?;
                }
            }
            "-s" | "--shell" => {
                i += 1;
                if i < args.len() {
                    shell = args[i].clone();
                }
            }
            "-d" | "--direction" => {
                i += 1;
                if i < args.len() {
                    match args[i].as_str() {
                        "v" | "vertical" => vertical = true,
                        "h" | "horizontal" => vertical = false,
                        other => anyhow::bail!("Unknown direction: '{}'. Options: h, v", other),
                    }
                }
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            a if a.starts_with('-') => anyhow::bail!("Unknown option: {}", a),
            _ => positional.push(args[i].clone()),
        }
        i += 1;
    }

    match positional.len() {
        0 => {}
        1 => {
            let n: usize = positional[0].parse()?;
            if vertical {
                rows = n;
                cols = 1;
            } else {
                rows = 1;
                cols = n;
            }
        }
        2 => {
            rows = positional[0].parse()?;
            cols = positional[1].parse()?;
        }
        _ => anyhow::bail!("Too many arguments. See: ezpn --help"),
    }
    if rows == 0 || cols == 0 {
        anyhow::bail!("Rows and cols must be >= 1");
    }
    if rows * cols > 100 {
        anyhow::bail!("Maximum 100 panes (got {}x{}={})", rows, cols, rows * cols);
    }
    Ok(Config {
        rows,
        cols,
        border,
        shell,
    })
}

fn print_help() {
    println!(
        "\
ezpn — Dead simple terminal pane splitting

USAGE:
  ezpn [OPTIONS] [COLS]
  ezpn [OPTIONS] [ROWS] [COLS]

EXAMPLES:
  ezpn            Two panes side by side (1x2)
  ezpn 3          Three horizontal panes (1x3)
  ezpn 3 -d v     Three vertical panes (3x1)
  ezpn 2 3        Six panes in 2x3 grid
  ezpn 1          Single pane (split later with Ctrl+D)

OPTIONS:
  -b, --border <STYLE>  single, rounded, heavy, double (default: rounded)
  -d, --direction <DIR> h (horizontal, default) or v (vertical)
  -s, --shell <SHELL>   Shell to spawn (default: $SHELL)
  -h, --help            Show this help

CONTROLS:
  Mouse click       Select pane
  Drag border       Resize panes
  Click x           Close pane (auto-collapses)
  Ctrl+D            Split left|right (auto-equalizes)
  Ctrl+E            Split top/bottom (auto-equalizes)
  F2                Equalize all pane sizes
  Ctrl+]            Next pane
  Ctrl+G / F1       Settings panel
  Alt+Arrow         Navigate (needs Meta key on macOS)
  Ctrl+\\            Force quit all"
    );
}

// ─── Main Loop ─────────────────────────────────────────────

fn run(stdout: &mut io::Stdout, config: &Config) -> anyhow::Result<()> {
    let (term_w, term_h) = terminal::size()?;
    let mut settings = Settings::new(config.border);
    let mut layout = Layout::from_grid(config.rows, config.cols);

    // Spawn panes
    let mut panes: HashMap<usize, Pane> = HashMap::new();
    let inner = make_inner(term_w, term_h, settings.show_status_bar);
    for (&pid, rect) in &layout.pane_rects(&inner) {
        panes.insert(pid, Pane::new(&config.shell, rect.w.max(1), rect.h.max(1))?);
    }

    let mut active = *layout.pane_ids().first().unwrap_or(&0);
    let mut tw = term_w;
    let mut th = term_h;
    let mut drag: Option<DragState> = None;

    render_frame(stdout, &panes, &layout, active, &settings, tw, th, false)?;

    loop {
        let mut dirty = false;
        for pane in panes.values_mut() {
            if pane.read_output() {
                dirty = true;
            }
        }

        // All dead → exit
        if panes.is_empty() || panes.values().all(|p| !p.is_alive()) {
            break;
        }

        if event::poll(Duration::from_millis(10))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    let alt = key.modifiers.contains(KeyModifiers::ALT);

                    // ── Ctrl+G / F1 → settings ──
                    if (key.code == KeyCode::Char('g') && ctrl) || key.code == KeyCode::F(1) {
                        settings.toggle();
                        dirty = true;
                    }
                    // ── Ctrl+\ → quit ──
                    else if key.code == KeyCode::Char('\\') && ctrl {
                        break;
                    }
                    // ── Settings mode ──
                    else if settings.visible {
                        let action = settings.handle_key(key);
                        if action == SettingsAction::SplitH {
                            do_split(
                                &mut layout,
                                &mut panes,
                                active,
                                Direction::Horizontal,
                                &config.shell,
                                tw,
                                th,
                                &settings,
                            )?;
                        } else if action == SettingsAction::SplitV {
                            do_split(
                                &mut layout,
                                &mut panes,
                                active,
                                Direction::Vertical,
                                &config.shell,
                                tw,
                                th,
                                &settings,
                            )?;
                        }
                        dirty = true;
                    }
                    // ── Ctrl+D → split horizontal (left|right) ──
                    else if key.code == KeyCode::Char('d') && ctrl {
                        do_split(
                            &mut layout,
                            &mut panes,
                            active,
                            Direction::Horizontal,
                            &config.shell,
                            tw,
                            th,
                            &settings,
                        )?;
                        dirty = true;
                    }
                    // ── Ctrl+E → split vertical (top/bottom) ──
                    else if key.code == KeyCode::Char('e') && ctrl {
                        do_split(
                            &mut layout,
                            &mut panes,
                            active,
                            Direction::Vertical,
                            &config.shell,
                            tw,
                            th,
                            &settings,
                        )?;
                        dirty = true;
                    }
                    // ── Ctrl+] → next pane ──
                    else if key.code == KeyCode::Char(']') && ctrl {
                        active = layout.next_pane(active);
                        dirty = true;
                    }
                    // ── F2 → equalize all pane sizes ──
                    else if key.code == KeyCode::F(2) {
                        layout.equalize();
                        resize_all(&mut panes, &layout, tw, th, &settings);
                        dirty = true;
                    }
                    // ── Alt+Arrow → navigate ──
                    else if alt {
                        let inner = make_inner(tw, th, settings.show_status_bar);
                        let nav = match key.code {
                            KeyCode::Left => Some(NavDir::Left),
                            KeyCode::Right => Some(NavDir::Right),
                            KeyCode::Up => Some(NavDir::Up),
                            KeyCode::Down => Some(NavDir::Down),
                            _ => None,
                        };
                        if let Some(dir) = nav {
                            if let Some(next) = layout.navigate(active, dir, &inner) {
                                active = next;
                                dirty = true;
                            }
                        } else {
                            // Forward Alt+other to pane
                            if let Some(pane) = panes.get_mut(&active) {
                                if pane.is_alive() {
                                    pane.write_key(key);
                                }
                            }
                            dirty = true;
                        }
                    }
                    // ── Enter on dead pane → respawn ──
                    else if key.code == KeyCode::Enter
                        && panes.get(&active).is_some_and(|p| !p.is_alive())
                    {
                        let inner = make_inner(tw, th, settings.show_status_bar);
                        if let Some(rect) = layout.pane_rects(&inner).get(&active) {
                            if let Ok(new_pane) =
                                Pane::new(&config.shell, rect.w.max(1), rect.h.max(1))
                            {
                                panes.insert(active, new_pane);
                            }
                        }
                        dirty = true;
                    }
                    // ── Forward to active pane ──
                    else {
                        if let Some(pane) = panes.get_mut(&active) {
                            if pane.is_alive() {
                                pane.write_key(key);
                            }
                        }
                        dirty = true;
                    }
                }
                Event::Mouse(mouse) => {
                    let inner = make_inner(tw, th, settings.show_status_bar);
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            if settings.visible {
                                let action = settings.handle_click(mouse.column, mouse.row, tw, th);
                                if action == SettingsAction::SplitH {
                                    do_split(
                                        &mut layout,
                                        &mut panes,
                                        active,
                                        Direction::Horizontal,
                                        &config.shell,
                                        tw,
                                        th,
                                        &settings,
                                    )?;
                                } else if action == SettingsAction::SplitV {
                                    do_split(
                                        &mut layout,
                                        &mut panes,
                                        active,
                                        Direction::Vertical,
                                        &config.shell,
                                        tw,
                                        th,
                                        &settings,
                                    )?;
                                }
                                dirty = true;
                            } else if let Some(pid) =
                                render::close_button_hit(mouse.column, mouse.row, &layout, &inner)
                            {
                                // Allow closing both alive and dead panes
                                close_pane(&mut layout, &mut panes, &mut active, pid);
                                dirty = true;
                            } else if let Some(hit) =
                                layout.find_separator_at(mouse.column, mouse.row, &inner)
                            {
                                // Start drag-to-resize
                                drag = Some(DragState::from_hit(hit));
                                dirty = true;
                            } else if let Some(pid) =
                                layout.find_at(mouse.column, mouse.row, &inner)
                            {
                                if pid != active && panes.contains_key(&pid) {
                                    active = pid;
                                    dirty = true;
                                }
                            }
                        }
                        MouseEventKind::Drag(MouseButton::Left) => {
                            if let Some(ref ds) = drag {
                                let new_ratio = ds.calc_ratio(mouse.column, mouse.row);
                                layout.set_ratio_at_path(&ds.path, new_ratio);
                                resize_all(&mut panes, &layout, tw, th, &settings);
                                dirty = true;
                            }
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            if drag.take().is_some() {
                                resize_all(&mut panes, &layout, tw, th, &settings);
                                dirty = true;
                            }
                        }
                        MouseEventKind::ScrollUp => {
                            if let Some(pane) = panes.get_mut(&active) {
                                if pane.is_alive() {
                                    pane.write_bytes(b"\x1b[A"); // up arrow
                                }
                            }
                        }
                        MouseEventKind::ScrollDown => {
                            if let Some(pane) = panes.get_mut(&active) {
                                if pane.is_alive() {
                                    pane.write_bytes(b"\x1b[B"); // down arrow
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Event::Resize(w, h) => {
                    tw = w;
                    th = h;
                    drag = None; // cancel any in-progress drag
                    resize_all(&mut panes, &layout, tw, th, &settings);
                    dirty = true;
                }
                _ => {}
            }
        }

        if dirty {
            render_frame(
                stdout,
                &panes,
                &layout,
                active,
                &settings,
                tw,
                th,
                drag.is_some(),
            )?;
        }
    }

    Ok(())
}

// ─── Helpers ───────────────────────────────────────────────

fn make_inner(tw: u16, th: u16, show_status_bar: bool) -> Rect {
    let sh = if show_status_bar { 1u16 } else { 0 };
    Rect {
        x: 1,
        y: 1,
        w: tw.saturating_sub(2),
        h: th.saturating_sub(sh + 2),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_frame(
    stdout: &mut io::Stdout,
    panes: &HashMap<usize, Pane>,
    layout: &Layout,
    active: usize,
    settings: &Settings,
    tw: u16,
    th: u16,
    dragging: bool,
) -> anyhow::Result<()> {
    queue!(stdout, terminal::BeginSynchronizedUpdate)?;
    render::render_panes(
        stdout,
        panes,
        layout,
        active,
        settings.border_style,
        settings.show_status_bar,
        tw,
        th,
        dragging,
    )?;
    if settings.visible {
        settings.render_overlay(stdout, tw, th)?;
    }
    queue!(stdout, terminal::EndSynchronizedUpdate)?;
    stdout.flush()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn do_split(
    layout: &mut Layout,
    panes: &mut HashMap<usize, Pane>,
    active: usize,
    dir: Direction,
    shell: &str,
    tw: u16,
    th: u16,
    settings: &Settings,
) -> anyhow::Result<()> {
    // Minimum pane size guard: refuse split if result would be too small
    let inner = make_inner(tw, th, settings.show_status_bar);
    if let Some(rect) = layout.pane_rects(&inner).get(&active) {
        let min_w = 6u16;
        let min_h = 3u16;
        let too_small = match dir {
            Direction::Horizontal => rect.w < min_w * 2 + 1,
            Direction::Vertical => rect.h < min_h * 2 + 1,
        };
        if too_small {
            return Ok(()); // silently refuse — pane too small to split
        }
    }

    let new_id = layout.split(active, dir);
    let rects = layout.pane_rects(&inner);
    if let Some(rect) = rects.get(&new_id) {
        panes.insert(new_id, Pane::new(shell, rect.w.max(1), rect.h.max(1))?);
    }
    resize_all(panes, layout, tw, th, settings);
    Ok(())
}

fn close_pane(
    layout: &mut Layout,
    panes: &mut HashMap<usize, Pane>,
    active: &mut usize,
    pid: usize,
) {
    if let Some(mut p) = panes.remove(&pid) {
        p.kill();
    }
    layout.remove(pid);
    // If active was the closed pane, switch to another
    if *active == pid {
        *active = *layout.pane_ids().first().unwrap_or(&0);
    }
}

fn resize_all(
    panes: &mut HashMap<usize, Pane>,
    layout: &Layout,
    tw: u16,
    th: u16,
    settings: &Settings,
) {
    let inner = make_inner(tw, th, settings.show_status_bar);
    let rects = layout.pane_rects(&inner);
    for (&pid, rect) in &rects {
        if let Some(pane) = panes.get_mut(&pid) {
            pane.resize(rect.w.max(1), rect.h.max(1));
        }
    }
}

// ─── Drag-to-Resize ───────────────────────────────────────

struct DragState {
    path: Vec<bool>,
    direction: Direction,
    area: Rect,
}

impl DragState {
    fn from_hit(hit: SepHit) -> Self {
        Self {
            path: hit.path,
            direction: hit.direction,
            area: hit.area,
        }
    }

    fn calc_ratio(&self, mx: u16, my: u16) -> f32 {
        match self.direction {
            Direction::Horizontal => {
                let usable = self.area.w.saturating_sub(1) as f32;
                if usable <= 0.0 {
                    return 0.5;
                }
                ((mx as f32 - self.area.x as f32) / usable).clamp(0.1, 0.9)
            }
            Direction::Vertical => {
                let usable = self.area.h.saturating_sub(1) as f32;
                if usable <= 0.0 {
                    return 0.5;
                }
                ((my as f32 - self.area.y as f32) / usable).clamp(0.1, 0.9)
            }
        }
    }
}
