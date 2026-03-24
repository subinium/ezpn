use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::time::Duration;

use crossterm::event::{KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode},
    execute, queue,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};

mod config;
mod ipc;
mod layout;
mod pane;
mod render;
mod settings;
mod tab;
mod workspace;

use layout::{Direction, Layout, NavDir, Rect, SepHit};
use pane::{Pane, PaneLaunch};
use render::{BorderCache, BorderStyle};
use settings::{Settings, SettingsAction};
use std::time::Instant;
#[allow(unused_imports)]
use tab::{Tab, TabManager};
use workspace::WorkspaceSnapshot;

/// Input state machine for prefix key support.
enum InputMode {
    Normal,
    Prefix { entered_at: Instant },
    ScrollMode,
    QuitConfirm,
}

fn main() -> anyhow::Result<()> {
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
    commands: Vec<String>,
    restore: Option<String>,
}

fn parse_args() -> anyhow::Result<Config> {
    let args: Vec<String> = std::env::args().collect();
    let mut rows = 1usize;
    let mut cols = 2usize;
    let mut border = BorderStyle::Rounded;
    let mut shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut vertical = false;
    let mut layout_spec: Option<String> = None;
    let mut commands = Vec::new();
    let mut restore = None;
    let mut positional = Vec::new();

    let mut border_set = false;
    let mut shell_set = false;
    let mut direction_set = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-b" | "--border" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--border requires a style"))?;
                border = BorderStyle::from_str(value).ok_or_else(|| {
                    anyhow::anyhow!(
                        "Unknown border style: '{}'. Options: single, rounded, heavy, double",
                        value
                    )
                })?;
                border_set = true;
            }
            "-s" | "--shell" => {
                i += 1;
                shell = args
                    .get(i)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("--shell requires a path"))?;
                shell_set = true;
            }
            "-d" | "--direction" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--direction requires h|v"))?;
                match value.as_str() {
                    "v" | "vertical" => vertical = true,
                    "h" | "horizontal" => vertical = false,
                    other => anyhow::bail!("Unknown direction: '{}'. Options: h, v", other),
                }
                direction_set = true;
            }
            "-l" | "--layout" => {
                i += 1;
                layout_spec = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--layout requires a spec"))?,
                );
            }
            "-e" | "--exec" => {
                i += 1;
                commands.push(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--exec requires a command"))?,
                );
            }
            "-r" | "--restore" => {
                i += 1;
                restore = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--restore requires a file path"))?,
                );
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other if other.starts_with('-') => anyhow::bail!("Unknown option: {}", other),
            _ => positional.push(args[i].clone()),
        }
        i += 1;
    }

    if layout_spec.is_some() && !positional.is_empty() {
        anyhow::bail!("--layout cannot be combined with positional rows/cols");
    }

    if restore.is_some()
        && (layout_spec.is_some()
            || !commands.is_empty()
            || !positional.is_empty()
            || border_set
            || shell_set
            || direction_set)
    {
        anyhow::bail!("--restore cannot be combined with layout, command, shell, or grid flags");
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
        restore,
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
  ezpn --restore <FILE>

EXAMPLES:
  ezpn                              Two panes side by side
  ezpn 2 3                          2x3 grid (6 panes)
  ezpn --layout '7:3'               70/30 horizontal split
  ezpn --layout '1:1:1'             3 equal columns
  ezpn --layout '7:3/5:5'           2 rows with different ratios
  ezpn -e 'make watch' -e 'npm dev' Per-pane commands via shell -lc
  ezpn --restore .ezpn-session.json Restore a saved workspace

OPTIONS:
  -l, --layout <SPEC>   Layout spec (rows with /, cols with :, weights 1-9)
  -e, --exec <CMD>      Command for each pane (repeatable, default: interactive $SHELL)
  -r, --restore <FILE>  Restore a saved workspace snapshot
  -b, --border <STYLE>  single, rounded, heavy, double (default: rounded)
  -d, --direction <DIR> h (horizontal, default) or v (vertical)
  -s, --shell <SHELL>   Default shell path (default: $SHELL)
  -h, --help            Show this help

CONTROLS:
  Mouse click       Select pane
  Drag border       Resize panes
  Click [━][┃][×]   Split/close buttons on title bar
  Ctrl+D            Split left|right (auto-equalizes)
  Ctrl+E            Split top/bottom (auto-equalizes)
  Ctrl+N            Next pane
  F2                Equalize all pane sizes
  Ctrl+B <key>      Prefix mode (tmux keys: % \" o x E [ d s)
  Ctrl+G / F1       Settings panel (j/k/Enter/1-4/q)
  Alt+Arrow         Navigate (needs Meta key on macOS)
  Ctrl+W            Quit"
    );
}

fn run(stdout: &mut io::Stdout, config: &Config) -> anyhow::Result<()> {
    let (mut tw, mut th) = terminal::size()?;
    let mut default_shell = config.shell.clone();
    let mut settings = Settings::new(config.border);

    let (mut layout, mut panes, mut active) = if let Some(path) = &config.restore {
        let snapshot = workspace::load_snapshot(path)?;
        let layout = snapshot.layout.clone();
        default_shell = snapshot.shell.clone();
        settings.border_style = snapshot.border_style;
        settings.show_status_bar = snapshot.show_status_bar;
        let panes = spawn_snapshot_panes(&layout, &snapshot, &default_shell, tw, th, &settings)?;
        let active = snapshot.active_pane;
        (layout, panes, active)
    } else {
        let layout = match &config.layout {
            LayoutSpec::Grid { rows, cols } => Layout::from_grid(*rows, *cols),
            LayoutSpec::Spec(spec) => {
                Layout::from_spec(spec).map_err(|error| anyhow::anyhow!(error))?
            }
        };
        let panes = spawn_layout_panes(
            &layout,
            build_command_launches(&layout, &config.commands),
            &default_shell,
            tw,
            th,
            &settings,
        )?;
        let active = *layout.pane_ids().first().unwrap_or(&0);
        (layout, panes, active)
    };

    let mut drag: Option<DragState> = None;
    let mut mode = InputMode::Normal;
    let ipc_rx = ipc::start_listener()
        .map_err(|e| eprintln!("ezpn: IPC unavailable ({e}), ezpn-ctl disabled"))
        .ok();
    let mut border_cache = render::build_border_cache(&layout, settings.show_status_bar, tw, th);
    let initial_dirty = layout.pane_ids().into_iter().collect::<HashSet<_>>();
    render_frame(
        stdout,
        &panes,
        &layout,
        active,
        &settings,
        tw,
        th,
        false,
        &border_cache,
        &initial_dirty,
        true,
        "",
    )?;

    loop {
        let mut update = RenderUpdate::default();

        for (&pid, pane) in &mut panes {
            if pane.read_output() {
                update.dirty_panes.insert(pid);
            }
        }

        if panes.is_empty() || panes.values().all(|pane| !pane.is_alive()) {
            break;
        }

        // Prefix mode timeout
        if let InputMode::Prefix { entered_at } = &mode {
            if entered_at.elapsed() > Duration::from_secs(1) {
                mode = InputMode::Normal;
                update.full_redraw = true;
            }
        }

        // Drain pending events. First poll waits up to 8ms (frame budget),
        // subsequent polls are non-blocking to batch input without busy-spinning.
        let mut first_poll = true;
        while event::poll(Duration::from_millis(if first_poll { 8 } else { 0 }))? {
            first_poll = false;
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    let alt = key.modifiers.contains(KeyModifiers::ALT);

                    // ── Quit confirmation mode ──
                    if matches!(mode, InputMode::QuitConfirm) {
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Enter => break,
                            _ => {
                                mode = InputMode::Normal;
                                update.full_redraw = true;
                            }
                        }
                    }
                    // ── Scroll mode: arrow/pgup/pgdn navigate, q/Esc exits ──
                    else if matches!(mode, InputMode::ScrollMode) {
                        match key.code {
                            KeyCode::Up | KeyCode::Char('k') => {
                                if let Some(p) = panes.get_mut(&active) {
                                    p.scroll_up(1);
                                }
                                update.dirty_panes.insert(active);
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                if let Some(p) = panes.get_mut(&active) {
                                    p.scroll_down(1);
                                }
                                update.dirty_panes.insert(active);
                            }
                            KeyCode::PageUp | KeyCode::Char('u') if ctrl => {
                                if let Some(p) = panes.get_mut(&active) {
                                    p.scroll_up(20);
                                }
                                update.dirty_panes.insert(active);
                            }
                            KeyCode::PageDown | KeyCode::Char('d') if ctrl => {
                                if let Some(p) = panes.get_mut(&active) {
                                    p.scroll_down(20);
                                }
                                update.dirty_panes.insert(active);
                            }
                            KeyCode::PageUp => {
                                if let Some(p) = panes.get_mut(&active) {
                                    p.scroll_up(20);
                                }
                                update.dirty_panes.insert(active);
                            }
                            KeyCode::PageDown => {
                                if let Some(p) = panes.get_mut(&active) {
                                    p.scroll_down(20);
                                }
                                update.dirty_panes.insert(active);
                            }
                            KeyCode::Char('g') => {
                                // gg = go to top (first press sets flag, handled simply as top)
                                if let Some(p) = panes.get_mut(&active) {
                                    p.scroll_up(usize::MAX);
                                }
                                update.dirty_panes.insert(active);
                            }
                            KeyCode::Char('G') => {
                                if let Some(p) = panes.get_mut(&active) {
                                    p.snap_to_bottom();
                                }
                                update.dirty_panes.insert(active);
                            }
                            KeyCode::Char('q') | KeyCode::Esc => {
                                if let Some(p) = panes.get_mut(&active) {
                                    p.snap_to_bottom();
                                }
                                mode = InputMode::Normal;
                                update.dirty_panes.insert(active);
                            }
                            _ => {}
                        }
                    }
                    // ── Prefix mode: Ctrl+B was pressed, interpret next key ──
                    else if matches!(mode, InputMode::Prefix { .. }) {
                        mode = InputMode::Normal;
                        update.full_redraw = true; // clear [PREFIX] indicator
                        match key.code {
                            // Split
                            KeyCode::Char('%') => {
                                do_split(
                                    &mut layout,
                                    &mut panes,
                                    active,
                                    Direction::Horizontal,
                                    &default_shell,
                                    tw,
                                    th,
                                    &settings,
                                )?;
                                update.mark_all(&layout);
                                update.border_dirty = true;
                            }
                            KeyCode::Char('"') => {
                                do_split(
                                    &mut layout,
                                    &mut panes,
                                    active,
                                    Direction::Vertical,
                                    &default_shell,
                                    tw,
                                    th,
                                    &settings,
                                )?;
                                update.mark_all(&layout);
                                update.border_dirty = true;
                            }
                            // Navigate
                            KeyCode::Char('o') => {
                                active = layout.next_pane(active);
                            }
                            KeyCode::Left => {
                                let i = make_inner(tw, th, settings.show_status_bar);
                                if let Some(n) = layout.navigate(active, NavDir::Left, &i) {
                                    active = n;
                                }
                            }
                            KeyCode::Right => {
                                let i = make_inner(tw, th, settings.show_status_bar);
                                if let Some(n) = layout.navigate(active, NavDir::Right, &i) {
                                    active = n;
                                }
                            }
                            KeyCode::Up => {
                                let i = make_inner(tw, th, settings.show_status_bar);
                                if let Some(n) = layout.navigate(active, NavDir::Up, &i) {
                                    active = n;
                                }
                            }
                            KeyCode::Down => {
                                let i = make_inner(tw, th, settings.show_status_bar);
                                if let Some(n) = layout.navigate(active, NavDir::Down, &i) {
                                    active = n;
                                }
                            }
                            // Close pane
                            KeyCode::Char('x') => {
                                let target = active;
                                close_pane(&mut layout, &mut panes, &mut active, target);
                                update.mark_all(&layout);
                                update.border_dirty = true;
                            }
                            // Equalize
                            KeyCode::Char('E') => {
                                layout.equalize();
                                resize_all(&mut panes, &layout, tw, th, &settings);
                                update.mark_all(&layout);
                                update.border_dirty = true;
                            }
                            // Scroll mode
                            KeyCode::Char('[') => {
                                mode = InputMode::ScrollMode;
                            }
                            // Quit with confirmation
                            KeyCode::Char('d') => {
                                let live = panes.values().filter(|p| p.is_alive()).count();
                                if live == 0 {
                                    break;
                                }
                                mode = InputMode::QuitConfirm;
                                update.full_redraw = true;
                            }
                            // Toggle status bar
                            KeyCode::Char('s') => {
                                settings.show_status_bar = !settings.show_status_bar;
                                resize_all(&mut panes, &layout, tw, th, &settings);
                                update.mark_all(&layout);
                                update.border_dirty = true;
                            }
                            _ => {} // unknown prefix command, ignore
                        }
                    }
                    // ── Normal mode ──
                    else {
                        // Ctrl+B → enter prefix mode
                        if key.code == KeyCode::Char('b') && ctrl {
                            mode = InputMode::Prefix {
                                entered_at: Instant::now(),
                            };
                            update.full_redraw = true; // show [PREFIX] indicator
                        }
                        // Settings toggle (direct shortcut, kept for convenience)
                        else if (key.code == KeyCode::Char('g') && ctrl)
                            || key.code == KeyCode::F(1)
                        {
                            settings.toggle();
                            update.full_redraw = true;
                        }
                        // Force quit: Ctrl+\ or Ctrl+Q or Ctrl+W
                        else if ctrl
                            && (key.code == KeyCode::Char('\\')
                                || key.code == KeyCode::Char('q')
                                || key.code == KeyCode::Char('w'))
                        {
                            break;
                        }
                        // Settings visible
                        else if settings.visible {
                            let prev_border = settings.border_style;
                            let prev_status = settings.show_status_bar;
                            let action = settings.handle_key(key);
                            match action {
                                SettingsAction::SplitH => {
                                    do_split(
                                        &mut layout,
                                        &mut panes,
                                        active,
                                        Direction::Horizontal,
                                        &default_shell,
                                        tw,
                                        th,
                                        &settings,
                                    )?;
                                    update.mark_all(&layout);
                                    update.border_dirty = true;
                                }
                                SettingsAction::SplitV => {
                                    do_split(
                                        &mut layout,
                                        &mut panes,
                                        active,
                                        Direction::Vertical,
                                        &default_shell,
                                        tw,
                                        th,
                                        &settings,
                                    )?;
                                    update.mark_all(&layout);
                                    update.border_dirty = true;
                                }
                                _ => {}
                            }
                            if settings.border_style != prev_border {
                                update.full_redraw = true;
                            }
                            if settings.show_status_bar != prev_status {
                                resize_all(&mut panes, &layout, tw, th, &settings);
                                update.border_dirty = true;
                                update.mark_all(&layout);
                            }
                            // Any settings interaction needs a redraw (focus movement, value change, close)
                            {
                                update.full_redraw = true;
                            }
                        }
                        // Direct shortcuts (kept alongside prefix mode)
                        else if key.code == KeyCode::Char('d') && ctrl {
                            do_split(
                                &mut layout,
                                &mut panes,
                                active,
                                Direction::Horizontal,
                                &default_shell,
                                tw,
                                th,
                                &settings,
                            )?;
                            update.mark_all(&layout);
                            update.border_dirty = true;
                        } else if key.code == KeyCode::Char('e') && ctrl {
                            do_split(
                                &mut layout,
                                &mut panes,
                                active,
                                Direction::Vertical,
                                &default_shell,
                                tw,
                                th,
                                &settings,
                            )?;
                            update.mark_all(&layout);
                            update.border_dirty = true;
                        } else if ctrl
                            && (key.code == KeyCode::Char(']') || key.code == KeyCode::Char('n'))
                        {
                            active = layout.next_pane(active);
                            update.full_redraw = true;
                        } else if key.code == KeyCode::F(2) {
                            layout.equalize();
                            resize_all(&mut panes, &layout, tw, th, &settings);
                            update.mark_all(&layout);
                            update.border_dirty = true;
                        } else if alt {
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
                                    update.full_redraw = true;
                                }
                            } else if let Some(pane) = panes.get_mut(&active) {
                                if pane.is_alive() {
                                    pane.write_key(key);
                                }
                            }
                        } else if key.code == KeyCode::Enter
                            && panes.get(&active).is_some_and(|p| !p.is_alive())
                        {
                            let launch = panes
                                .get(&active)
                                .map(|p| p.launch().clone())
                                .unwrap_or(PaneLaunch::Shell);
                            replace_pane(
                                &mut panes,
                                &layout,
                                active,
                                launch,
                                &default_shell,
                                tw,
                                th,
                                &settings,
                            )?;
                            update.dirty_panes.insert(active);
                        } else if let Some(pane) = panes.get_mut(&active) {
                            if pane.is_alive() {
                                pane.write_key(key);
                            }
                        }
                    }

                    // Prefix mode timeout (1 second)
                    if let InputMode::Prefix { entered_at } = &mode {
                        if entered_at.elapsed() > Duration::from_secs(1) {
                            mode = InputMode::Normal;
                            update.full_redraw = true;
                        }
                    }
                }
                Event::Mouse(mouse) => {
                    let inner = make_inner(tw, th, settings.show_status_bar);
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            if settings.visible {
                                let prev_border = settings.border_style;
                                let prev_status = settings.show_status_bar;
                                let action = settings.handle_click(mouse.column, mouse.row, tw, th);
                                match action {
                                    SettingsAction::SplitH => {
                                        do_split(
                                            &mut layout,
                                            &mut panes,
                                            active,
                                            Direction::Horizontal,
                                            &default_shell,
                                            tw,
                                            th,
                                            &settings,
                                        )?;
                                        update.mark_all(&layout);
                                        update.border_dirty = true;
                                    }
                                    SettingsAction::SplitV => {
                                        do_split(
                                            &mut layout,
                                            &mut panes,
                                            active,
                                            Direction::Vertical,
                                            &default_shell,
                                            tw,
                                            th,
                                            &settings,
                                        )?;
                                        update.mark_all(&layout);
                                        update.border_dirty = true;
                                    }
                                    SettingsAction::Changed
                                    | SettingsAction::Close
                                    | SettingsAction::None => {}
                                }
                                if settings.border_style != prev_border {
                                    update.full_redraw = true;
                                }
                                if settings.show_status_bar != prev_status {
                                    resize_all(&mut panes, &layout, tw, th, &settings);
                                    update.border_dirty = true;
                                    update.mark_all(&layout);
                                }
                                if action == SettingsAction::Changed
                                    || action == SettingsAction::Close
                                {
                                    update.full_redraw = true;
                                }
                            } else if let Some(action) =
                                render::title_button_hit(mouse.column, mouse.row, &layout, &inner)
                            {
                                match action {
                                    render::TitleAction::Close(pid) => {
                                        close_pane(&mut layout, &mut panes, &mut active, pid);
                                    }
                                    render::TitleAction::SplitH(pid) => {
                                        // ━ button = horizontal line = top/bottom split
                                        let _ = do_split(
                                            &mut layout,
                                            &mut panes,
                                            pid,
                                            Direction::Vertical,
                                            &default_shell,
                                            tw,
                                            th,
                                            &settings,
                                        );
                                    }
                                    render::TitleAction::SplitV(pid) => {
                                        // ┃ button = vertical line = left/right split
                                        let _ = do_split(
                                            &mut layout,
                                            &mut panes,
                                            pid,
                                            Direction::Horizontal,
                                            &default_shell,
                                            tw,
                                            th,
                                            &settings,
                                        );
                                    }
                                }
                                update.mark_all(&layout);
                                update.border_dirty = true;
                            } else if let Some(hit) =
                                layout.find_separator_at(mouse.column, mouse.row, &inner)
                            {
                                drag = Some(DragState::from_hit(hit));
                                update.full_redraw = true;
                            } else if let Some(pid) =
                                layout.find_at(mouse.column, mouse.row, &inner)
                            {
                                if pid != active && panes.contains_key(&pid) {
                                    active = pid;
                                    update.full_redraw = true;
                                }
                            }
                        }
                        MouseEventKind::Drag(MouseButton::Left) => {
                            if let Some(ref ds) = drag {
                                let new_ratio = ds.calc_ratio(mouse.column, mouse.row);
                                layout.set_ratio_at_path(&ds.path, new_ratio);
                                resize_all(&mut panes, &layout, tw, th, &settings);
                                update.mark_all(&layout);
                                update.border_dirty = true;
                            }
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            if drag.take().is_some() {
                                resize_all(&mut panes, &layout, tw, th, &settings);
                                update.mark_all(&layout);
                                update.border_dirty = true;
                            }
                        }
                        MouseEventKind::ScrollUp => {
                            if let Some(pane) = panes.get_mut(&active) {
                                if pane.is_alive() {
                                    // Send 3 arrow-ups (works in less, man, shell history, etc.)
                                    for _ in 0..3 {
                                        pane.write_bytes(b"\x1b[A");
                                    }
                                }
                            }
                        }
                        MouseEventKind::ScrollDown => {
                            if let Some(pane) = panes.get_mut(&active) {
                                if pane.is_alive() {
                                    for _ in 0..3 {
                                        pane.write_bytes(b"\x1b[B");
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Event::Resize(w, h) => {
                    tw = w;
                    th = h;
                    drag = None;
                    resize_all(&mut panes, &layout, tw, th, &settings);
                    update.mark_all(&layout);
                    update.border_dirty = true;
                }
                _ => {}
            }
        }

        if let Some(ref rx) = ipc_rx {
            while let Ok((cmd, resp_tx)) = rx.try_recv() {
                let (response, mut ipc_update) = handle_ipc_command(
                    cmd,
                    &mut layout,
                    &mut panes,
                    &mut active,
                    &mut default_shell,
                    tw,
                    th,
                    &mut settings,
                );
                update.merge(&mut ipc_update);
                let _ = resp_tx.send(response);
            }
        }

        // When modal is visible, only redraw if the modal itself changed (full_redraw),
        // not when background panes have new output (dirty_panes only).
        if settings.visible && !update.full_redraw {
            update.dirty_panes.clear(); // suppress background pane redraws
        }

        if update.border_dirty {
            border_cache = render::build_border_cache(&layout, settings.show_status_bar, tw, th);
        }

        if update.needs_render() {
            let mode_label = match &mode {
                InputMode::Prefix { .. } => "PREFIX",
                InputMode::ScrollMode => "SCROLL",
                InputMode::QuitConfirm => "QUIT? y/n",
                InputMode::Normal => "",
            };
            render_frame(
                stdout,
                &panes,
                &layout,
                active,
                &settings,
                tw,
                th,
                drag.is_some(),
                &border_cache,
                &update.dirty_panes,
                update.full_redraw,
                mode_label,
            )?;
        }
    }

    ipc::cleanup();
    Ok(())
}

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
    border_cache: &BorderCache,
    dirty_panes: &HashSet<usize>,
    full_redraw: bool,
    mode_label: &str,
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
        border_cache,
        dirty_panes,
        full_redraw,
    )?;
    // Mode-aware status bar (render over the default one if we have a mode)
    if settings.show_status_bar && !mode_label.is_empty() {
        let ids = layout.pane_ids();
        let active_idx = ids.iter().position(|&id| id == active).unwrap_or(0);
        render::draw_status_bar(stdout, tw, th, active_idx, ids.len(), mode_label)?;
    }
    if settings.visible {
        settings.render_overlay(stdout, tw, th)?;
        queue!(stdout, cursor::Hide)?; // no blinking cursor over modal
    }
    queue!(stdout, terminal::EndSynchronizedUpdate)?;
    stdout.flush()?;
    Ok(())
}

fn build_command_launches(layout: &Layout, commands: &[String]) -> HashMap<usize, PaneLaunch> {
    layout
        .pane_ids()
        .into_iter()
        .enumerate()
        .map(|(index, id)| {
            let launch = commands
                .get(index)
                .map(|command| PaneLaunch::Command(command.clone()))
                .unwrap_or(PaneLaunch::Shell);
            (id, launch)
        })
        .collect()
}

fn build_snapshot_launches(snapshot: &WorkspaceSnapshot) -> HashMap<usize, PaneLaunch> {
    snapshot
        .panes
        .iter()
        .map(|pane| (pane.id, pane.launch.clone()))
        .collect()
}

fn spawn_layout_panes(
    layout: &Layout,
    launches: HashMap<usize, PaneLaunch>,
    shell: &str,
    tw: u16,
    th: u16,
    settings: &Settings,
) -> anyhow::Result<HashMap<usize, Pane>> {
    let inner = make_inner(tw, th, settings.show_status_bar);
    let rects = layout.pane_rects(&inner);

    // Collect spawn tasks
    let tasks: Vec<(usize, PaneLaunch, u16, u16)> = rects
        .iter()
        .map(|(&pid, rect)| {
            let launch = launches.get(&pid).cloned().unwrap_or(PaneLaunch::Shell);
            (pid, launch, rect.w.max(1), rect.h.max(1))
        })
        .collect();

    // Spawn panes in parallel using scoped threads
    let mut results: Vec<(usize, anyhow::Result<Pane>)> = Vec::new();
    std::thread::scope(|s| {
        let handles: Vec<_> = tasks
            .iter()
            .map(|(pid, launch, cols, rows)| {
                let pid = *pid;
                let cols = *cols;
                let rows = *rows;
                s.spawn(move || (pid, spawn_pane(shell, launch, cols, rows)))
            })
            .collect();
        for handle in handles {
            results.push(handle.join().expect("pane spawn thread panicked"));
        }
    });

    let mut panes = HashMap::new();
    for (pid, result) in results {
        panes.insert(pid, result?);
    }
    Ok(panes)
}

fn spawn_snapshot_panes(
    layout: &Layout,
    snapshot: &WorkspaceSnapshot,
    shell: &str,
    tw: u16,
    th: u16,
    settings: &Settings,
) -> anyhow::Result<HashMap<usize, Pane>> {
    spawn_layout_panes(
        layout,
        build_snapshot_launches(snapshot),
        shell,
        tw,
        th,
        settings,
    )
}

fn spawn_pane(shell: &str, launch: &PaneLaunch, cols: u16, rows: u16) -> anyhow::Result<Pane> {
    match launch {
        PaneLaunch::Shell => Pane::new(shell, cols, rows),
        PaneLaunch::Command(command) => Pane::with_command(shell, command, cols, rows),
    }
}

#[allow(clippy::too_many_arguments)]
fn replace_pane(
    panes: &mut HashMap<usize, Pane>,
    layout: &Layout,
    pane_id: usize,
    launch: PaneLaunch,
    shell: &str,
    tw: u16,
    th: u16,
    settings: &Settings,
) -> anyhow::Result<()> {
    let inner = make_inner(tw, th, settings.show_status_bar);
    let rect = layout
        .pane_rects(&inner)
        .remove(&pane_id)
        .ok_or_else(|| anyhow::anyhow!("pane rect not found"))?;
    let new_pane = spawn_pane(shell, &launch, rect.w.max(1), rect.h.max(1))?;
    if let Some(mut old_pane) = panes.insert(pane_id, new_pane) {
        old_pane.kill();
    }
    Ok(())
}

fn kill_all_panes(panes: &mut HashMap<usize, Pane>) {
    for (_, mut pane) in panes.drain() {
        pane.kill();
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_snapshot(
    snapshot: WorkspaceSnapshot,
    layout: &mut Layout,
    panes: &mut HashMap<usize, Pane>,
    active: &mut usize,
    shell: &mut String,
    settings: &mut Settings,
    tw: u16,
    th: u16,
) -> anyhow::Result<()> {
    let mut next_settings = Settings::new(snapshot.border_style);
    next_settings.show_status_bar = snapshot.show_status_bar;
    let next_layout = snapshot.layout.clone();
    let next_panes = spawn_snapshot_panes(
        &next_layout,
        &snapshot,
        &snapshot.shell,
        tw,
        th,
        &next_settings,
    )?;

    kill_all_panes(panes);
    *shell = snapshot.shell.clone();
    *layout = next_layout;
    *panes = next_panes;
    *settings = next_settings;
    settings.visible = false;
    *active = snapshot.active_pane;
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
    let inner = make_inner(tw, th, settings.show_status_bar);
    if let Some(rect) = layout.pane_rects(&inner).get(&active) {
        let min_w = 6u16;
        let min_h = 3u16;
        let too_small = match dir {
            Direction::Horizontal => rect.w < min_w * 2 + 1,
            Direction::Vertical => rect.h < min_h * 2 + 1,
        };
        if too_small {
            return Ok(());
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
    pane_id: usize,
) {
    if let Some(mut pane) = panes.remove(&pane_id) {
        pane.kill();
    }
    layout.remove(pane_id);
    if *active == pane_id {
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

#[derive(Default)]
struct RenderUpdate {
    dirty_panes: HashSet<usize>,
    full_redraw: bool,
    border_dirty: bool,
}

impl RenderUpdate {
    fn mark_all(&mut self, layout: &Layout) {
        self.full_redraw = true;
        self.dirty_panes.extend(layout.pane_ids());
    }

    fn merge(&mut self, other: &mut Self) {
        self.dirty_panes.extend(other.dirty_panes.drain());
        self.full_redraw |= other.full_redraw;
        self.border_dirty |= other.border_dirty;
    }

    fn needs_render(&self) -> bool {
        self.full_redraw || !self.dirty_panes.is_empty()
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_ipc_command(
    cmd: ipc::IpcRequest,
    layout: &mut Layout,
    panes: &mut HashMap<usize, Pane>,
    active: &mut usize,
    shell: &mut String,
    tw: u16,
    th: u16,
    settings: &mut Settings,
) -> (ipc::IpcResponse, RenderUpdate) {
    let mut update = RenderUpdate::default();

    let response = match cmd {
        ipc::IpcRequest::Split { direction, pane } => {
            let target = pane.unwrap_or(*active);
            if !panes.contains_key(&target) {
                ipc::IpcResponse::error("pane not found")
            } else {
                let dir = match direction {
                    ipc::SplitDirection::Horizontal => Direction::Horizontal,
                    ipc::SplitDirection::Vertical => Direction::Vertical,
                };
                match do_split(layout, panes, target, dir, shell, tw, th, settings) {
                    Ok(()) => {
                        update.mark_all(layout);
                        update.border_dirty = true;
                        ipc::IpcResponse::success("split ok")
                    }
                    Err(error) => ipc::IpcResponse::error(error.to_string()),
                }
            }
        }
        ipc::IpcRequest::Close { pane } => {
            if !panes.contains_key(&pane) && !layout.pane_ids().contains(&pane) {
                ipc::IpcResponse::error("pane not found")
            } else {
                close_pane(layout, panes, active, pane);
                update.mark_all(layout);
                update.border_dirty = true;
                ipc::IpcResponse::success("closed")
            }
        }
        ipc::IpcRequest::Focus { pane } => {
            if panes.contains_key(&pane) {
                *active = pane;
                update.full_redraw = true;
                ipc::IpcResponse::success("focused")
            } else {
                ipc::IpcResponse::error("pane not found")
            }
        }
        ipc::IpcRequest::Equalize => {
            layout.equalize();
            resize_all(panes, layout, tw, th, settings);
            update.mark_all(layout);
            update.border_dirty = true;
            ipc::IpcResponse::success("equalized")
        }
        ipc::IpcRequest::List => {
            let inner = make_inner(tw, th, settings.show_status_bar);
            let rects = layout.pane_rects(&inner);
            let panes = layout
                .pane_ids()
                .into_iter()
                .enumerate()
                .map(|(index, id)| {
                    let (cols, rows) = rects
                        .get(&id)
                        .map(|rect| (rect.w, rect.h))
                        .unwrap_or((0, 0));
                    let pane = panes.get(&id);
                    ipc::PaneInfo {
                        index,
                        id,
                        cols,
                        rows,
                        alive: pane.is_some_and(|pane| pane.is_alive()),
                        active: id == *active,
                        command: pane
                            .map(|pane| pane.launch_label(shell))
                            .unwrap_or_else(|| shell.to_string()),
                    }
                })
                .collect();
            ipc::IpcResponse::with_panes(panes)
        }
        ipc::IpcRequest::Layout { spec } => match Layout::from_spec(&spec) {
            Ok(new_layout) => {
                match spawn_layout_panes(&new_layout, HashMap::new(), shell, tw, th, settings) {
                    Ok(new_panes) => {
                        kill_all_panes(panes);
                        *layout = new_layout;
                        *panes = new_panes;
                        *active = *layout.pane_ids().first().unwrap_or(&0);
                        update.mark_all(layout);
                        update.border_dirty = true;
                        ipc::IpcResponse::success("layout applied")
                    }
                    Err(error) => ipc::IpcResponse::error(error.to_string()),
                }
            }
            Err(error) => ipc::IpcResponse::error(error),
        },
        ipc::IpcRequest::Exec { pane, command } => {
            if !panes.contains_key(&pane) {
                ipc::IpcResponse::error("pane not found")
            } else {
                match replace_pane(
                    panes,
                    layout,
                    pane,
                    PaneLaunch::Command(command),
                    shell,
                    tw,
                    th,
                    settings,
                ) {
                    Ok(()) => {
                        update.dirty_panes.insert(pane);
                        ipc::IpcResponse::success("exec ok")
                    }
                    Err(error) => ipc::IpcResponse::error(error.to_string()),
                }
            }
        }
        ipc::IpcRequest::Save { path } => {
            let snapshot = WorkspaceSnapshot::from_live(
                layout,
                panes,
                *active,
                shell,
                settings.border_style,
                settings.show_status_bar,
            );
            match workspace::save_snapshot(&path, &snapshot) {
                Ok(()) => ipc::IpcResponse::success(format!("saved {}", path)),
                Err(error) => ipc::IpcResponse::error(error.to_string()),
            }
        }
        ipc::IpcRequest::Load { path } => match workspace::load_snapshot(&path) {
            Ok(snapshot) => {
                match apply_snapshot(snapshot, layout, panes, active, shell, settings, tw, th) {
                    Ok(()) => {
                        update.mark_all(layout);
                        update.border_dirty = true;
                        ipc::IpcResponse::success(format!("loaded {}", path))
                    }
                    Err(error) => ipc::IpcResponse::error(error.to_string()),
                }
            }
            Err(error) => ipc::IpcResponse::error(error.to_string()),
        },
    };

    (response, update)
}
