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
mod project;
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
    ResizeMode,
    PaneSelect,
    HelpOverlay,
}

fn main() -> anyhow::Result<()> {
    // Handle subcommands before anything else
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("init") => return cmd_init(),
        Some("from") => return cmd_from(args.get(2).map(|s| s.as_str())),
        _ => {}
    }

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

fn cmd_init() -> anyhow::Result<()> {
    let path = std::path::Path::new(".ezpn.toml");
    if path.exists() {
        eprintln!("ezpn: .ezpn.toml already exists");
        std::process::exit(1);
    }

    let template = r#"# ezpn project workspace
# Run `ezpn` in this directory to auto-load this config.

[workspace]
# Layout spec: ratios separated by : (cols) and / (rows)
# Examples: "7:3", "1:1:1", "7:3/5:5", "1/1:1"
layout = "1:1"
# Or use grid: rows = 2, cols = 3

[[pane]]
name = "editor"
# command = "nvim ."
# cwd = "."
# shell = "/bin/zsh"
# restart = "never"  # never | on_failure | always
# [pane.env]
# NODE_ENV = "development"

[[pane]]
name = "shell"
# command = "npm run dev"
# cwd = "./frontend"
"#;

    std::fs::write(path, template)?;
    println!("Created .ezpn.toml — edit it and run `ezpn` to launch.");
    Ok(())
}

/// Generate .ezpn.toml from Procfile or docker-compose.yml
fn cmd_from(source: Option<&str>) -> anyhow::Result<()> {
    let source = source.unwrap_or("Procfile");
    let path = std::path::Path::new(source);
    if !path.exists() {
        eprintln!("ezpn: {} not found", source);
        std::process::exit(1);
    }

    let out_path = std::path::Path::new(".ezpn.toml");
    if out_path.exists() {
        eprintln!("ezpn: .ezpn.toml already exists (delete it first or edit manually)");
        std::process::exit(1);
    }

    let contents = std::fs::read_to_string(path)?;
    let entries = parse_procfile(&contents);

    if entries.is_empty() {
        eprintln!("ezpn: no processes found in {}", source);
        std::process::exit(1);
    }

    let mut toml = String::new();
    toml.push_str(&format!("# Generated from {}\n\n", source));

    // Auto-select layout based on count
    let layout = match entries.len() {
        1 => "1",
        2 => "1:1",
        3 => "1:1:1",
        4 => "1:1/1:1",
        n if n <= 6 => "1:1:1/1:1:1",
        _ => "1:1:1/1:1:1",
    };
    toml.push_str(&format!("[workspace]\nlayout = \"{}\"\n\n", layout));

    for (name, command) in &entries {
        toml.push_str("[[pane]]\n");
        toml.push_str(&format!("name = \"{}\"\n", name));
        toml.push_str(&format!(
            "command = \"{}\"\n\n",
            command.replace('"', "\\\"")
        ));
    }

    std::fs::write(out_path, &toml)?;
    println!(
        "Created .ezpn.toml from {} ({} processes). Run `ezpn` to launch.",
        source,
        entries.len()
    );
    Ok(())
}

/// Parse Procfile format: `name: command`
fn parse_procfile(contents: &str) -> Vec<(String, String)> {
    contents
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (name, cmd) = line.split_once(':')?;
            let name = name.trim();
            let cmd = cmd.trim();
            if name.is_empty() || cmd.is_empty() {
                return None;
            }
            Some((name.to_string(), cmd.to_string()))
        })
        .collect()
}

enum LayoutSpec {
    Grid { rows: usize, cols: usize },
    Spec(String),
}

struct Config {
    layout: LayoutSpec,
    border: BorderStyle,
    has_border_override: bool,
    shell: String,
    has_shell_override: bool,
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
            "-V" | "--version" => {
                println!("ezpn {}", env!("CARGO_PKG_VERSION"));
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
        has_border_override: border_set,
        shell,
        has_shell_override: shell_set,
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
  ezpn init                         Generate .ezpn.toml template

EXAMPLES:
  ezpn                              Two panes side by side (or load .ezpn.toml)
  ezpn 2 3                          2x3 grid (6 panes)
  ezpn -l dev                       70/30 split (preset)
  ezpn -l ide                       IDE layout (editor + sidebar + 2 bottom)
  ezpn -l monitor                   3 equal columns
  ezpn -l '7:3/5:5'                 Custom: 2 rows with different ratios
  ezpn -e 'make watch' -e 'npm dev' Per-pane commands via shell -lc
  ezpn --restore .ezpn-session.json Restore a saved workspace
  ezpn init                         Create .ezpn.toml in current directory

OPTIONS:
  -l, --layout <SPEC>   Layout spec or preset name (see PRESETS below)
  -e, --exec <CMD>      Command for each pane (repeatable, default: interactive $SHELL)
  -r, --restore <FILE>  Restore a saved workspace snapshot
  -b, --border <STYLE>  single, rounded, heavy, double (default: rounded)
  -d, --direction <DIR> h (horizontal, default) or v (vertical)
  -s, --shell <SHELL>   Default shell path (default: $SHELL)
  -h, --help            Show this help
  -V, --version         Show version

LAYOUT PRESETS:
  dev       7:3         Main editor + side terminal
  ide       7:3/1:1     Editor + sidebar + 2 bottom panes
  monitor   1:1:1       3 equal columns
  quad      2x2         4 panes in a grid
  stack     1/1/1       3 stacked rows
  main      6:4/1       Wide top pair + full-width bottom
  trio      1/1:1       Full top + 2 bottom panes

PROJECT CONFIG (.ezpn.toml):
  Place .ezpn.toml in your project root. Run `ezpn init` to generate a template.
  Supports: layout, per-pane commands, cwd, env vars, custom names,
  auto-restart (never/on_failure/always), per-pane shell override.

CONTROLS:
  Mouse click       Select pane
  Drag border       Resize panes
  Click [━][┃][×]   Split/close buttons on title bar
  Ctrl+D            Split left|right (auto-equalizes)
  Ctrl+E            Split top/bottom (auto-equalizes)
  Ctrl+N            Next pane
  F2                Equalize all pane sizes
  Ctrl+B <key>      Prefix mode (tmux keys: % \" o x z R q ? {{ }} E B [ d s)
  Ctrl+G / F1       Settings panel (j/k/Enter/1-4/q)
  Alt+Arrow         Navigate (needs Meta key on macOS)
  Double-click      Zoom toggle
  Ctrl+W            Quit

PREFIX KEYS (Ctrl+B then):
  c                 New pane (split + focus)
  ;                 Last active pane (toggle back)
  Space             Equalize layout
  z                 Zoom toggle
  B                 Broadcast mode (type in all panes)
  R                 Resize mode (arrow/hjkl, q to exit)
  q                 Show pane numbers + quick jump (0-9)
  ?                 Help overlay
  {{ }}               Swap pane with prev/next"
    );
}

fn run(stdout: &mut io::Stdout, config: &Config) -> anyhow::Result<()> {
    let (mut tw, mut th) = terminal::size()?;

    // Load config file defaults, then overlay CLI args
    let file_config = config::load_config();
    let effective_scrollback = file_config.scrollback;
    let mut default_shell = if config.has_shell_override {
        config.shell.clone()
    } else {
        file_config.shell
    };
    let effective_border = if config.has_border_override {
        config.border
    } else {
        file_config.border
    };
    let mut settings = Settings::new(effective_border);
    settings.show_status_bar = file_config.show_status_bar;

    // Auto-restart state (populated from .ezpn.toml if present)
    let mut restart_policies: HashMap<usize, project::RestartPolicy> = HashMap::new();

    let (mut layout, mut panes, mut active) = if let Some(path) = &config.restore {
        let snapshot = workspace::load_snapshot(path)?;
        let layout = snapshot.layout.clone();
        default_shell = snapshot.shell.clone();
        settings.border_style = snapshot.border_style;
        settings.show_status_bar = snapshot.show_status_bar;
        let panes = spawn_snapshot_panes(
            &layout,
            &snapshot,
            &default_shell,
            tw,
            th,
            &settings,
            effective_scrollback,
        )?;
        let active = snapshot.active_pane;
        (layout, panes, active)
    } else if config.commands.is_empty()
        && matches!(config.layout, LayoutSpec::Grid { rows: 1, cols: 2 })
    {
        // No explicit args — try loading .ezpn.toml from current directory
        if let Some(result) = project::load_project() {
            let proj = result.map_err(|e| anyhow::anyhow!("{e}"))?;
            let panes = spawn_project_panes(
                &proj,
                &default_shell,
                tw,
                th,
                &settings,
                effective_scrollback,
            )?;
            // Store restart policies and pane launch info for auto-restart
            restart_policies = proj.restarts.clone();
            let active = *proj.layout.pane_ids().first().unwrap_or(&0);
            (proj.layout, panes, active)
        } else if let Some((layout, launches)) = try_load_procfile() {
            // Auto-detected Procfile
            let panes = spawn_layout_panes(
                &layout,
                launches,
                &default_shell,
                tw,
                th,
                &settings,
                effective_scrollback,
            )?;
            let active = *layout.pane_ids().first().unwrap_or(&0);
            (layout, panes, active)
        } else {
            // No .ezpn.toml or Procfile, use default 1x2 grid
            let layout = Layout::from_grid(1, 2);
            let panes = spawn_layout_panes(
                &layout,
                build_command_launches(&layout, &config.commands),
                &default_shell,
                tw,
                th,
                &settings,
                effective_scrollback,
            )?;
            let active = *layout.pane_ids().first().unwrap_or(&0);
            (layout, panes, active)
        }
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
            effective_scrollback,
        )?;
        let active = *layout.pane_ids().first().unwrap_or(&0);
        (layout, panes, active)
    };

    let mut drag: Option<DragState> = None;
    let mut zoomed_pane: Option<usize> = None;
    let mut last_click: Option<(Instant, u16, u16)> = None;
    let mut broadcast = false;
    let mut last_active: usize = active; // for Ctrl+B ; (last pane)
    let mut selection_anchor: Option<(usize, u16, u16)> = None; // (pane_id, rel_col, rel_row)
    let mut text_selection: Option<TextSelection> = None;

    let mut restart_state: HashMap<usize, (Instant, u32)> = HashMap::new(); // (last_death, retries)
    const MAX_RESTART_RETRIES: u32 = 10;
    const RESTART_DELAY: Duration = Duration::from_secs(2);
    const RESTART_BACKOFF_THRESHOLD: u32 = 3; // after this many rapid restarts, increase delay

    // Set window title
    let _ = write!(stdout, "\x1b]0;ezpn\x07");
    let _ = stdout.flush();
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
        None,
        0,
    )?;

    let mut prev_active = active;
    loop {
        // Track last-active pane for Ctrl+B ;
        if active != prev_active {
            last_active = prev_active;
            prev_active = active;
        }

        let mut update = RenderUpdate::default();

        for (&pid, pane) in &mut panes {
            if pane.read_output() {
                update.dirty_panes.insert(pid);
            }
        }

        // Auto-restart dead panes with restart policy
        {
            let dead_restartable: Vec<usize> = panes
                .iter()
                .filter(|(pid, pane)| {
                    !pane.is_alive()
                        && restart_policies.get(pid).is_some_and(|p| {
                            *p == project::RestartPolicy::Always
                                || *p == project::RestartPolicy::OnFailure
                        })
                })
                .map(|(&pid, _)| pid)
                .collect();

            for pid in dead_restartable {
                let (last_death, retries) = restart_state
                    .entry(pid)
                    .or_insert((Instant::now() - RESTART_DELAY, 0));

                if *retries >= MAX_RESTART_RETRIES {
                    continue; // give up after too many retries
                }

                let delay = if *retries >= RESTART_BACKOFF_THRESHOLD {
                    RESTART_DELAY * (*retries - RESTART_BACKOFF_THRESHOLD + 1)
                } else {
                    RESTART_DELAY
                };

                if last_death.elapsed() < delay {
                    continue; // wait before restarting
                }

                let (launch, old_name) = panes
                    .get(&pid)
                    .map(|p| (p.launch().clone(), p.name().map(String::from)))
                    .unwrap_or((PaneLaunch::Shell, None));
                if replace_pane(
                    &mut panes,
                    &layout,
                    pid,
                    launch,
                    &default_shell,
                    tw,
                    th,
                    &settings,
                    effective_scrollback,
                )
                .is_ok()
                {
                    // Preserve the pane name from config
                    if let Some(pane) = panes.get_mut(&pid) {
                        pane.set_name(old_name);
                    }
                    *retries += 1;
                    *last_death = Instant::now();
                    update.dirty_panes.insert(pid);
                }
            }
        }

        let all_dead = panes.is_empty()
            || panes.iter().all(|(pid, pane)| {
                if pane.is_alive() {
                    return false; // alive pane → not all dead
                }
                // Dead pane — check if it can be restarted
                let has_restart = restart_policies.get(pid).is_some_and(|p| {
                    *p == project::RestartPolicy::Always || *p == project::RestartPolicy::OnFailure
                });
                if !has_restart {
                    return true; // dead with no restart policy
                }
                // Has restart policy — check if retries exhausted
                restart_state
                    .get(pid)
                    .is_some_and(|(_, retries)| *retries >= MAX_RESTART_RETRIES)
            });
        if all_dead {
            break;
        }

        // Unzoom if zoomed pane no longer exists
        if let Some(zpid) = zoomed_pane {
            if !panes.contains_key(&zpid) {
                zoomed_pane = None;
                resize_all(&mut panes, &layout, tw, th, &settings);
                update.mark_all(&layout);
                update.border_dirty = true;
            }
        }

        // Prefix mode timeout
        if let InputMode::Prefix { entered_at } = &mode {
            if entered_at.elapsed() > Duration::from_secs(3) {
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
                    // ── Help overlay: any key dismisses ──
                    else if matches!(mode, InputMode::HelpOverlay) {
                        mode = InputMode::Normal;
                        update.full_redraw = true;
                    }
                    // ── Pane select: digit jumps, any other key cancels ──
                    else if matches!(mode, InputMode::PaneSelect) {
                        let ids = layout.pane_ids();
                        if let KeyCode::Char(c @ '0'..='9') = key.code {
                            let idx = match c {
                                '1'..='9' => c as usize - '1' as usize,
                                '0' => 9,
                                _ => unreachable!(),
                            };
                            if let Some(&target) = ids.get(idx) {
                                if panes.contains_key(&target) {
                                    active = target;
                                }
                            }
                        }
                        mode = InputMode::Normal;
                        update.full_redraw = true;
                    }
                    // ── Resize mode: arrows resize, q/Esc exits ──
                    else if matches!(mode, InputMode::ResizeMode) {
                        match key.code {
                            KeyCode::Left | KeyCode::Char('h') => {
                                if layout.resize_pane(active, NavDir::Left, 0.05) {
                                    resize_all(&mut panes, &layout, tw, th, &settings);
                                    update.mark_all(&layout);
                                    update.border_dirty = true;
                                }
                            }
                            KeyCode::Right | KeyCode::Char('l') => {
                                if layout.resize_pane(active, NavDir::Right, 0.05) {
                                    resize_all(&mut panes, &layout, tw, th, &settings);
                                    update.mark_all(&layout);
                                    update.border_dirty = true;
                                }
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                if layout.resize_pane(active, NavDir::Up, 0.05) {
                                    resize_all(&mut panes, &layout, tw, th, &settings);
                                    update.mark_all(&layout);
                                    update.border_dirty = true;
                                }
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                if layout.resize_pane(active, NavDir::Down, 0.05) {
                                    resize_all(&mut panes, &layout, tw, th, &settings);
                                    update.mark_all(&layout);
                                    update.border_dirty = true;
                                }
                            }
                            KeyCode::Char('q') | KeyCode::Esc => {
                                mode = InputMode::Normal;
                                update.full_redraw = true;
                            }
                            _ => {}
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
                        update.full_redraw = true; // clear [PREFIX] indicator
                                                   // Default: return to Normal. Some keys transition to other modes.
                        let mut next_mode = InputMode::Normal;
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
                                    effective_scrollback,
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
                                    effective_scrollback,
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
                                resize_all(&mut panes, &layout, tw, th, &settings);
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
                                next_mode = InputMode::ScrollMode;
                            }
                            // Quit with confirmation
                            KeyCode::Char('d') => {
                                let live = panes.values().filter(|p| p.is_alive()).count();
                                if live == 0 {
                                    break;
                                }
                                next_mode = InputMode::QuitConfirm;
                            }
                            // Toggle status bar
                            KeyCode::Char('s') => {
                                settings.show_status_bar = !settings.show_status_bar;
                                resize_all(&mut panes, &layout, tw, th, &settings);
                                update.mark_all(&layout);
                                update.border_dirty = true;
                            }
                            // Zoom toggle
                            KeyCode::Char('z') => {
                                if zoomed_pane.is_some() {
                                    // Unzoom: restore normal layout sizes
                                    zoomed_pane = None;
                                    resize_all(&mut panes, &layout, tw, th, &settings);
                                    update.mark_all(&layout);
                                    update.border_dirty = true;
                                } else {
                                    // Zoom active pane
                                    zoomed_pane = Some(active);
                                    resize_zoomed_pane(&mut panes, active, tw, th, &settings);
                                }
                            }
                            // Resize mode
                            KeyCode::Char('R') => {
                                next_mode = InputMode::ResizeMode;
                            }
                            // Pane select (show numbers)
                            KeyCode::Char('q') => {
                                next_mode = InputMode::PaneSelect;
                            }
                            // Help overlay
                            KeyCode::Char('?') => {
                                next_mode = InputMode::HelpOverlay;
                            }
                            // Swap with previous pane
                            KeyCode::Char('{') => {
                                let prev = layout.prev_pane(active);
                                if prev != active {
                                    layout.swap_panes(active, prev);
                                    // active ID stays the same (it moved in the tree)
                                    update.mark_all(&layout);
                                    update.border_dirty = true;
                                }
                            }
                            // Swap with next pane
                            KeyCode::Char('}') => {
                                let next = layout.next_pane(active);
                                if next != active {
                                    layout.swap_panes(active, next);
                                    update.mark_all(&layout);
                                    update.border_dirty = true;
                                }
                            }
                            // Broadcast toggle
                            KeyCode::Char('B') => {
                                broadcast = !broadcast;
                                update.full_redraw = true;
                            }
                            // Last pane (tmux ;)
                            KeyCode::Char(';') => {
                                if panes.contains_key(&last_active) {
                                    active = last_active;
                                    update.full_redraw = true;
                                }
                            }
                            // Cycle layout (tmux Space)
                            KeyCode::Char(' ') => {
                                layout.equalize();
                                resize_all(&mut panes, &layout, tw, th, &settings);
                                update.mark_all(&layout);
                                update.border_dirty = true;
                            }
                            // New pane (tmux c) — split active pane horizontally
                            KeyCode::Char('c') => {
                                do_split(
                                    &mut layout,
                                    &mut panes,
                                    active,
                                    Direction::Horizontal,
                                    &default_shell,
                                    tw,
                                    th,
                                    &settings,
                                    effective_scrollback,
                                )?;
                                // Focus the new pane
                                active = layout.next_pane(active);
                                update.mark_all(&layout);
                                update.border_dirty = true;
                            }
                            _ => {} // unknown prefix command, ignore
                        }
                        mode = next_mode;
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
                                        effective_scrollback,
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
                                        effective_scrollback,
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
                                effective_scrollback,
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
                                effective_scrollback,
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
                            } else if broadcast {
                                for pane in panes.values_mut() {
                                    if pane.is_alive() {
                                        pane.write_key(key);
                                    }
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
                                effective_scrollback,
                            )?;
                            update.dirty_panes.insert(active);
                        } else if broadcast {
                            // Broadcast: send key to all live panes
                            for pane in panes.values_mut() {
                                if pane.is_alive() {
                                    pane.write_key(key);
                                }
                            }
                        } else if let Some(pane) = panes.get_mut(&active) {
                            if pane.is_alive() {
                                pane.write_key(key);
                            }
                        }
                    }

                    // Prefix mode timeout (1 second)
                    if let InputMode::Prefix { entered_at } = &mode {
                        if entered_at.elapsed() > Duration::from_secs(3) {
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
                                            effective_scrollback,
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
                                            effective_scrollback,
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
                                        resize_all(&mut panes, &layout, tw, th, &settings);
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
                                            effective_scrollback,
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
                                            effective_scrollback,
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
                                // Double-click detection → zoom toggle
                                let now = Instant::now();
                                let is_double = last_click
                                    .map(|(t, lx, ly)| {
                                        now.duration_since(t) < Duration::from_millis(400)
                                            && lx == mouse.column
                                            && ly == mouse.row
                                    })
                                    .unwrap_or(false);
                                last_click = Some((now, mouse.column, mouse.row));

                                if is_double && panes.contains_key(&pid) {
                                    // Toggle zoom
                                    if zoomed_pane.is_some() {
                                        zoomed_pane = None;
                                        resize_all(&mut panes, &layout, tw, th, &settings);
                                    } else {
                                        zoomed_pane = Some(pid);
                                        resize_zoomed_pane(&mut panes, pid, tw, th, &settings);
                                    }
                                    active = pid;
                                    update.mark_all(&layout);
                                    update.border_dirty = true;
                                } else if pid != active && panes.contains_key(&pid) {
                                    active = pid;
                                    update.full_redraw = true;
                                }
                                // Forward click to child if it wants mouse, or start selection
                                if !is_double {
                                    if let Some(pane) = panes.get_mut(&pid) {
                                        if pane.wants_mouse() {
                                            if let Some(rect) = border_cache.pane_rects().get(&pid)
                                            {
                                                let rel_col = mouse.column.saturating_sub(rect.x);
                                                let rel_row = mouse.row.saturating_sub(rect.y);
                                                pane.send_mouse_event(0, rel_col, rel_row, false);
                                            }
                                        } else if pid == active {
                                            // Start potential text selection in active non-mouse pane
                                            if let Some(rect) = border_cache.pane_rects().get(&pid)
                                            {
                                                let rel_col = mouse.column.saturating_sub(rect.x);
                                                let rel_row = mouse.row.saturating_sub(rect.y);
                                                selection_anchor = Some((pid, rel_col, rel_row));
                                                // Clear any existing selection
                                                if text_selection.is_some() {
                                                    text_selection = None;
                                                    update.dirty_panes.insert(pid);
                                                }
                                            }
                                        }
                                    }
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
                            } else if let Some((pid, anchor_col, anchor_row)) = selection_anchor {
                                // Update text selection during drag
                                if let Some(rect) = border_cache.pane_rects().get(&pid) {
                                    let rel_col = mouse
                                        .column
                                        .saturating_sub(rect.x)
                                        .min(rect.w.saturating_sub(1));
                                    let rel_row = mouse
                                        .row
                                        .saturating_sub(rect.y)
                                        .min(rect.h.saturating_sub(1));
                                    text_selection = Some(TextSelection {
                                        pane_id: pid,
                                        start_row: anchor_row,
                                        start_col: anchor_col,
                                        end_row: rel_row,
                                        end_col: rel_col,
                                    });
                                    update.dirty_panes.insert(pid);
                                }
                            }
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            if drag.take().is_some() {
                                resize_all(&mut panes, &layout, tw, th, &settings);
                                update.mark_all(&layout);
                                update.border_dirty = true;
                            } else if let Some(ref sel) = text_selection {
                                // Copy selected text to clipboard via OSC 52
                                if let Some(pane) = panes.get_mut(&sel.pane_id) {
                                    pane.sync_scrollback();
                                    let text = extract_selected_text(pane.screen(), sel);
                                    pane.reset_scrollback_view();
                                    if !text.is_empty() {
                                        let encoded = base64_encode(text.as_bytes());
                                        let _ = write!(stdout, "\x1b]52;c;{}\x07", encoded);
                                        let _ = stdout.flush();
                                    }
                                }
                                let pid = sel.pane_id;
                                text_selection = None;
                                selection_anchor = None;
                                update.dirty_panes.insert(pid);
                            } else {
                                selection_anchor = None;
                                // Forward release to child if it wants mouse
                                if let Some(pane) = panes.get_mut(&active) {
                                    if pane.wants_mouse() {
                                        if let Some(rect) = border_cache.pane_rects().get(&active) {
                                            let rel_col = mouse.column.saturating_sub(rect.x);
                                            let rel_row = mouse.row.saturating_sub(rect.y);
                                            pane.send_mouse_event(0, rel_col, rel_row, true);
                                        }
                                    }
                                }
                            }
                        }
                        MouseEventKind::ScrollUp => {
                            // Target pane under cursor, not just active
                            let target = layout
                                .find_at(mouse.column, mouse.row, &inner)
                                .unwrap_or(active);
                            if let Some(pane) = panes.get_mut(&target) {
                                if pane.is_alive() {
                                    if pane.wants_mouse() {
                                        // Forward scroll to child in its encoding
                                        if let Some(rect) = border_cache.pane_rects().get(&target) {
                                            let rel_col = mouse.column.saturating_sub(rect.x);
                                            let rel_row = mouse.row.saturating_sub(rect.y);
                                            for _ in 0..3 {
                                                pane.send_mouse_scroll(true, rel_col, rel_row);
                                            }
                                        }
                                    } else {
                                        // No mouse reporting — scroll through ezpn scrollback
                                        pane.scroll_up(3);
                                        update.dirty_panes.insert(target);
                                    }
                                }
                            }
                        }
                        MouseEventKind::ScrollDown => {
                            let target = layout
                                .find_at(mouse.column, mouse.row, &inner)
                                .unwrap_or(active);
                            if let Some(pane) = panes.get_mut(&target) {
                                if pane.is_alive() {
                                    if pane.wants_mouse() {
                                        if let Some(rect) = border_cache.pane_rects().get(&target) {
                                            let rel_col = mouse.column.saturating_sub(rect.x);
                                            let rel_row = mouse.row.saturating_sub(rect.y);
                                            for _ in 0..3 {
                                                pane.send_mouse_scroll(false, rel_col, rel_row);
                                            }
                                        }
                                    } else {
                                        // No mouse reporting — scroll through ezpn scrollback
                                        pane.scroll_down(3);
                                        update.dirty_panes.insert(target);
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
                    effective_scrollback,
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

        if zoomed_pane.is_some() {
            zoomed_pane = Some(active);
            resize_zoomed_pane(&mut panes, active, tw, th, &settings);
        }

        if update.needs_render() {
            // Sync scrollback offsets to vt100 parser before rendering
            for pane in panes.values_mut() {
                pane.sync_scrollback();
            }

            let mode_label = match &mode {
                InputMode::Prefix { .. } => "PREFIX",
                InputMode::ScrollMode => "SCROLL",
                InputMode::QuitConfirm => "QUIT? y/n",
                InputMode::ResizeMode => "RESIZE",
                InputMode::PaneSelect => "SELECT",
                InputMode::HelpOverlay => "",
                InputMode::Normal if broadcast => "BROADCAST",
                InputMode::Normal => "",
            };

            if let Some(zpid) = zoomed_pane {
                // Zoomed mode: render only the zoomed pane at full size
                queue!(stdout, terminal::BeginSynchronizedUpdate)?;
                let ids = layout.pane_ids();
                let pane_idx = ids.iter().position(|&id| id == zpid).unwrap_or(0);
                let label = panes
                    .get(&zpid)
                    .map(|p| p.launch_label(&default_shell))
                    .unwrap_or_default();
                if let Some(pane) = panes.get(&zpid) {
                    render::render_zoomed_pane(
                        stdout,
                        pane,
                        pane_idx,
                        &label,
                        settings.border_style,
                        tw,
                        th,
                        settings.show_status_bar,
                    )?;
                }
                // Status bar
                if settings.show_status_bar {
                    let zoom_label = if mode_label.is_empty() {
                        "ZOOM"
                    } else {
                        mode_label
                    };
                    let pane_name = panes.get(&zpid).and_then(|p| p.name()).unwrap_or("");
                    render::draw_status_bar_full(
                        stdout,
                        tw,
                        th,
                        pane_idx,
                        ids.len(),
                        zoom_label,
                        pane_name,
                        0,
                    )?;
                }
                queue!(stdout, terminal::EndSynchronizedUpdate)?;
                stdout.flush()?;
            } else {
                let sel_for_render = text_selection.as_ref().map(|s| {
                    let (sr, sc, er, ec) = s.normalized();
                    (s.pane_id, sr, sc, er, ec)
                });
                // Compute selection char count for status bar
                let sel_chars = text_selection
                    .as_ref()
                    .and_then(|sel| {
                        panes.get(&sel.pane_id).map(|pane| {
                            let text = extract_selected_text(pane.screen(), sel);
                            text.chars().count()
                        })
                    })
                    .unwrap_or(0);
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
                    sel_for_render,
                    sel_chars,
                )?;
            }

            // Overlays on top of the main render
            if matches!(mode, InputMode::HelpOverlay) {
                queue!(stdout, terminal::BeginSynchronizedUpdate)?;
                render::draw_help_overlay(stdout, tw, th)?;
                queue!(stdout, terminal::EndSynchronizedUpdate)?;
                stdout.flush()?;
            }
            if matches!(mode, InputMode::PaneSelect) {
                let inner = make_inner(tw, th, settings.show_status_bar);
                queue!(stdout, terminal::BeginSynchronizedUpdate)?;
                render::draw_pane_numbers(stdout, &layout, &inner)?;
                queue!(stdout, terminal::EndSynchronizedUpdate)?;
                stdout.flush()?;
            }

            // Reset scrollback view so process() isn't affected
            for pane in panes.values_mut() {
                pane.reset_scrollback_view();
            }
        }

        // Update window title with pane count
        {
            let ids = layout.pane_ids();
            let idx = ids.iter().position(|&id| id == active).unwrap_or(0);
            let _ = write!(stdout, "\x1b]0;ezpn [{}/{}]\x07", idx + 1, ids.len());
        }
    }

    // Restore window title
    let _ = write!(stdout, "\x1b]0;\x07");
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

fn zoomed_content_size(tw: u16, th: u16, show_status_bar: bool) -> (u16, u16) {
    let sh = if show_status_bar { 1u16 } else { 0 };
    (tw.saturating_sub(2), th.saturating_sub(sh + 2))
}

fn resize_zoomed_pane(
    panes: &mut HashMap<usize, Pane>,
    pane_id: usize,
    tw: u16,
    th: u16,
    settings: &Settings,
) {
    let (cols, rows) = zoomed_content_size(tw, th, settings.show_status_bar);
    if let Some(pane) = panes.get_mut(&pane_id) {
        pane.resize(cols, rows);
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
    selection: render::PaneSelection,
    selection_chars: usize,
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
        selection,
    )?;
    // Mode-aware status bar (render over the default one if we have a mode)
    if settings.show_status_bar && (!mode_label.is_empty() || selection_chars > 0) {
        let ids = layout.pane_ids();
        let active_idx = ids.iter().position(|&id| id == active).unwrap_or(0);
        let pane_name = panes.get(&active).and_then(|p| p.name()).unwrap_or("");
        render::draw_status_bar_full(
            stdout,
            tw,
            th,
            active_idx,
            ids.len(),
            mode_label,
            pane_name,
            selection_chars,
        )?;
    }
    if settings.visible {
        settings.render_overlay(stdout, tw, th)?;
        queue!(stdout, cursor::Hide)?; // no blinking cursor over modal
    }
    queue!(stdout, terminal::EndSynchronizedUpdate)?;
    stdout.flush()?;
    Ok(())
}

/// Try to load a Procfile from the current directory. Returns layout + launches.
fn try_load_procfile() -> Option<(Layout, HashMap<usize, PaneLaunch>)> {
    let path = std::path::Path::new("Procfile");
    if !path.exists() {
        return None;
    }
    let contents = std::fs::read_to_string(path).ok()?;
    let entries = parse_procfile(&contents);
    if entries.is_empty() {
        return None;
    }
    let count = entries.len();
    let layout = match count {
        1 => Layout::from_grid(1, 1),
        2 => Layout::from_spec("1:1").unwrap_or_else(|_| Layout::from_grid(1, 2)),
        3 => Layout::from_spec("1:1:1").unwrap_or_else(|_| Layout::from_grid(1, 3)),
        _ => Layout::from_grid(count.div_ceil(3).max(1), 3.min(count)),
    };
    let ids = layout.pane_ids();
    let launches: HashMap<usize, PaneLaunch> = ids
        .iter()
        .enumerate()
        .map(|(i, &id)| {
            let launch = entries
                .get(i)
                .map(|(_, cmd)| PaneLaunch::Command(cmd.clone()))
                .unwrap_or(PaneLaunch::Shell);
            (id, launch)
        })
        .collect();
    Some((layout, launches))
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
    scrollback: usize,
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
                s.spawn(move || (pid, spawn_pane(shell, launch, cols, rows, scrollback)))
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
    scrollback: usize,
) -> anyhow::Result<HashMap<usize, Pane>> {
    spawn_layout_panes(
        layout,
        build_snapshot_launches(snapshot),
        shell,
        tw,
        th,
        settings,
        scrollback,
    )
}

fn spawn_pane(
    shell: &str,
    launch: &PaneLaunch,
    cols: u16,
    rows: u16,
    scrollback: usize,
) -> anyhow::Result<Pane> {
    Pane::with_scrollback(shell, launch.clone(), cols, rows, scrollback)
}

fn spawn_project_panes(
    proj: &project::ResolvedProject,
    shell: &str,
    tw: u16,
    th: u16,
    settings: &Settings,
    scrollback: usize,
) -> anyhow::Result<HashMap<usize, Pane>> {
    let inner = make_inner(tw, th, settings.show_status_bar);
    let rects = proj.layout.pane_rects(&inner);
    let mut panes = HashMap::new();

    for (&pid, rect) in &rects {
        let launch = proj
            .launches
            .get(&pid)
            .cloned()
            .unwrap_or(PaneLaunch::Shell);
        let cols = rect.w.max(1);
        let rows = rect.h.max(1);
        let pane_shell = proj.shells.get(&pid).map(|s| s.as_str()).unwrap_or(shell);
        let cwd = proj.cwds.get(&pid).map(|p| p.as_path());
        let env = proj.envs.get(&pid).cloned().unwrap_or_default();
        let mut pane =
            Pane::with_full_config(pane_shell, launch, cols, rows, scrollback, cwd, &env)?;
        if let Some(name) = proj.names.get(&pid) {
            pane.set_name(Some(name.clone()));
        }
        panes.insert(pid, pane);
    }
    Ok(panes)
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
    scrollback: usize,
) -> anyhow::Result<()> {
    let inner = make_inner(tw, th, settings.show_status_bar);
    let rect = layout
        .pane_rects(&inner)
        .remove(&pane_id)
        .ok_or_else(|| anyhow::anyhow!("pane rect not found"))?;
    let new_pane = spawn_pane(shell, &launch, rect.w.max(1), rect.h.max(1), scrollback)?;
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
    scrollback: usize,
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
        scrollback,
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
    scrollback: usize,
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
        panes.insert(
            new_id,
            spawn_pane(
                shell,
                &PaneLaunch::Shell,
                rect.w.max(1),
                rect.h.max(1),
                scrollback,
            )?,
        );
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

/// Text selection state for copy-on-drag.
#[derive(Clone)]
struct TextSelection {
    pane_id: usize,
    start_row: u16,
    start_col: u16,
    end_row: u16,
    end_col: u16,
}

impl TextSelection {
    /// Normalized range: (min_row, min_col, max_row, max_col)
    fn normalized(&self) -> (u16, u16, u16, u16) {
        if self.start_row < self.end_row
            || (self.start_row == self.end_row && self.start_col <= self.end_col)
        {
            (self.start_row, self.start_col, self.end_row, self.end_col)
        } else {
            (self.end_row, self.end_col, self.start_row, self.start_col)
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

/// Extract text from vt100 screen within a selection range.
fn extract_selected_text(screen: &vt100::Screen, sel: &TextSelection) -> String {
    let (sr, sc, er, ec) = sel.normalized();
    let mut text = String::new();

    for r in sr..=er {
        let col_start = if r == sr { sc } else { 0 };
        let col_end = if r == er { ec } else { u16::MAX };
        let mut row_text = String::new();
        let mut c = col_start;
        loop {
            if c > col_end {
                break;
            }
            if let Some(cell) = screen.cell(r, c) {
                let contents = cell.contents();
                if contents.is_empty() {
                    row_text.push(' ');
                } else {
                    row_text.push_str(&contents);
                }
            } else {
                break;
            }
            c += 1;
        }
        // Trim trailing spaces per line
        let trimmed = row_text.trim_end();
        text.push_str(trimmed);
        if r < er {
            text.push('\n');
        }
    }
    text
}

/// Minimal base64 encoder for OSC 52 clipboard.
fn base64_encode(data: &[u8]) -> String {
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHA[((triple >> 18) & 0x3F) as usize] as char);
        out.push(ALPHA[((triple >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHA[((triple >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHA[(triple & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
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
    scrollback: usize,
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
                match do_split(
                    layout, panes, target, dir, shell, tw, th, settings, scrollback,
                ) {
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
                resize_all(panes, layout, tw, th, settings);
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
                match spawn_layout_panes(
                    &new_layout,
                    HashMap::new(),
                    shell,
                    tw,
                    th,
                    settings,
                    scrollback,
                ) {
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
                    scrollback,
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
                match apply_snapshot(
                    snapshot, layout, panes, active, shell, settings, tw, th, scrollback,
                ) {
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
