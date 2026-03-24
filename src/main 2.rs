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

mod ipc;
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

enum LayoutSpec {
    Grid { rows: usize, cols: usize },
    Spec(String),
}

struct Config {
    layout: LayoutSpec,
    border: BorderStyle,
    shell: String,
    commands: Vec<String>, // per-pane commands via -e
}

fn parse_args() -> anyhow::Result<Config> {
    let args: Vec<String> = std::env::args().collect();
    let mut rows = 1usize;
    let mut cols = 2usize;
    let mut border = BorderStyle::Rounded;
    let mut shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut vertical = false;
    let mut layout_spec: Option<String> = None;
    let mut commands: Vec<String> = Vec::new();
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
            "-l" | "--layout" => {
                i += 1;
                if i < args.len() {
                    layout_spec = Some(args[i].clone());
                }
            }
            "-e" | "--exec" => {
                i += 1;
                if i < args.len() {
                    commands.push(args[i].clone());
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

    let layout = if let Some(spec) = layout_spec {
        LayoutSpec::Spec(spec)
    } else {
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
        LayoutSpec::Grid { rows, cols }
    };

    Ok(Config {
        layout,
        border,
        shell,
        commands,
    })
}

fn print_help() {
    println!(
        "\
ezpn — Dead simple terminal pane splitting

USAGE:
  ezpn [OPTIONS] [COLS]
  ezpn [OPTIONS] [ROWS] [COLS]
  ezpn [OPTIONS] --layout <SPEC>

EXAMPLES:
  ezpn                              Two panes side by side
  ezpn 2 3                          2x3 grid (6 panes)
  ezpn --layout '7:3'               70/30 horizontal split
  ezpn --layout '1:1:1'             3 equal columns
  ezpn --layout '7:3/5:5'           2 rows with different ratios
  ezpn --layout '1/1:1'             top full, bottom 2 panes
  ezpn -e 'make watch' -e 'npm dev' Per-pane commands
  ezpn --layout '7:3' -e vim -e bash

OPTIONS:
  -l, --layout <SPEC>   Layout spec (rows with /, cols with :, weights 1-9)
  -e, --exec <CMD>      Command for each pane (repeatable, default: $SHELL)
  -b, --border <STYLE>  single, rounded, heavy, double (default: rounded)
  -d, --direction <DIR> h (horizontal, default) or v (vertical)
  -s, --shell <SHELL>   Default shell (default: $SHELL)
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
    let mut layout = match &config.layout {
        LayoutSpec::Grid { rows, cols } => Layout::from_grid(*rows, *cols),
        LayoutSpec::Spec(spec) => Layout::from_spec(spec).map_err(|e| anyhow::anyhow!("{}", e))?,
    };

    // Spawn panes with per-pane commands
    let mut panes: HashMap<usize, Pane> = HashMap::new();
    let inner = make_inner(term_w, term_h, settings.show_status_bar);
    let ids = layout.pane_ids();
    let rects = layout.pane_rects(&inner);
    for (i, &pid) in ids.iter().enumerate() {
        if let Some(rect) = rects.get(&pid) {
            let cmd = config
                .commands
                .get(i)
                .map(|s| s.as_str())
                .unwrap_or(&config.shell);
            panes.insert(pid, Pane::new(cmd, rect.w.max(1), rect.h.max(1))?);
        }
    }

    let mut active = *layout.pane_ids().first().unwrap_or(&0);
    let mut tw = term_w;
    let mut th = term_h;
    let mut drag: Option<DragState> = None;

    // Start IPC listener
    let ipc_rx = ipc::start_listener().ok();

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

        if event::poll(Duration::from_millis(16))? {
            // ~60fps cap
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

        // ── IPC commands ──
        if let Some(ref rx) = ipc_rx {
            while let Ok((cmd, resp_tx)) = rx.try_recv() {
                let response = handle_ipc_command(
                    cmd,
                    &mut layout,
                    &mut panes,
                    &mut active,
                    &config.shell,
                    tw,
                    th,
                    &settings,
                );
                let _ = resp_tx.send(response);
                dirty = true;
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

    ipc::cleanup();
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

// ─── IPC Command Handler ──────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn handle_ipc_command(
    cmd: ipc::AppCommand,
    layout: &mut Layout,
    panes: &mut HashMap<usize, Pane>,
    active: &mut usize,
    shell: &str,
    tw: u16,
    th: u16,
    settings: &Settings,
) -> ipc::AppResponse {
    match cmd {
        ipc::AppCommand::Split { direction, pane } => {
            let target = pane.unwrap_or(*active);
            if !panes.contains_key(&target) {
                return ipc::AppResponse::error("pane not found");
            }
            let dir = match direction.as_str() {
                "horizontal" | "h" => Direction::Horizontal,
                "vertical" | "v" => Direction::Vertical,
                _ => {
                    return ipc::AppResponse::error("direction must be 'horizontal' or 'vertical'")
                }
            };
            match do_split(layout, panes, target, dir, shell, tw, th, settings) {
                Ok(()) => ipc::AppResponse::success("split ok"),
                Err(e) => ipc::AppResponse::error(&e.to_string()),
            }
        }
        ipc::AppCommand::Close { pane } => {
            if !panes.contains_key(&pane) && !layout.pane_ids().contains(&pane) {
                return ipc::AppResponse::error("pane not found");
            }
            close_pane(layout, panes, active, pane);
            ipc::AppResponse::success("closed")
        }
        ipc::AppCommand::Focus { pane } => {
            if panes.contains_key(&pane) {
                *active = pane;
                ipc::AppResponse::success("focused")
            } else {
                ipc::AppResponse::error("pane not found")
            }
        }
        ipc::AppCommand::Equalize => {
            layout.equalize();
            resize_all(panes, layout, tw, th, settings);
            ipc::AppResponse::success("equalized")
        }
        ipc::AppCommand::List => {
            let ids = layout.pane_ids();
            let inner = make_inner(tw, th, settings.show_status_bar);
            let rects = layout.pane_rects(&inner);
            let mut info = Vec::new();
            for (i, &pid) in ids.iter().enumerate() {
                let alive = panes.get(&pid).is_some_and(|p| p.is_alive());
                let (w, h) = rects.get(&pid).map(|r| (r.w, r.h)).unwrap_or((0, 0));
                let focused = pid == *active;
                info.push(format!(
                    "  {} id={} {}x{} {}{}",
                    i + 1,
                    pid,
                    w,
                    h,
                    if alive { "alive" } else { "dead" },
                    if focused { " *" } else { "" },
                ));
            }
            ipc::AppResponse::success(&info.join("\n"))
        }
        ipc::AppCommand::Layout { spec } => {
            match Layout::from_spec(&spec) {
                Ok(new_layout) => {
                    // Kill all existing panes
                    for (_, mut p) in panes.drain() {
                        p.kill();
                    }
                    *layout = new_layout;
                    // Spawn new panes
                    let inner = make_inner(tw, th, settings.show_status_bar);
                    for (&pid, rect) in &layout.pane_rects(&inner) {
                        if let Ok(p) = Pane::new(shell, rect.w.max(1), rect.h.max(1)) {
                            panes.insert(pid, p);
                        }
                    }
                    *active = *layout.pane_ids().first().unwrap_or(&0);
                    ipc::AppResponse::success("layout applied")
                }
                Err(e) => ipc::AppResponse::error(&e),
            }
        }
        ipc::AppCommand::Exec { pane, command } => {
            if !panes.contains_key(&pane) {
                return ipc::AppResponse::error("pane not found");
            }
            let inner = make_inner(tw, th, settings.show_status_bar);
            if let Some(rect) = layout.pane_rects(&inner).get(&pane) {
                match Pane::new(&command, rect.w.max(1), rect.h.max(1)) {
                    Ok(new_pane) => {
                        if let Some(mut old) = panes.insert(pane, new_pane) {
                            old.kill();
                        }
                        ipc::AppResponse::success("exec ok")
                    }
                    Err(e) => ipc::AppResponse::error(&e.to_string()),
                }
            } else {
                ipc::AppResponse::error("pane rect not found")
            }
        }
    }
}
