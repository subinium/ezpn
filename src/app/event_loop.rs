//! Foreground event loop for `ezpn --no-daemon`.
//!
//! [`run`] owns the entire input → render cycle when the user opts out of
//! the daemon. It blends together:
//! 1. Initial workspace bring-up (config + project file + Procfile fallback).
//! 2. The blocking event poll (key, mouse, resize) plus IPC drain.
//! 3. Frame composition through [`render_ctl::render_frame`].
//!
//! The function intentionally lives in a single block. The handlers
//! mutate ~15 local variables in tight interplay (mode machine, drag,
//! zoom, broadcast, selection anchors, restart bookkeeping…), and
//! peeling each branch into its own function would either explode the
//! parameter list or require a context struct that materially changes
//! borrow patterns. Splitting it further is a follow-up tracked in the
//! same issue thread.
//!
//! [`base64_encode`] sits next to the loop because it's only used for
//! the OSC-52 clipboard write fired on selection release.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::event::{KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use crossterm::{
    event::{self, Event, KeyCode},
    queue, terminal,
};

use crate::app::bootstrap::{build_command_launches, try_load_procfile};
use crate::app::input_dispatch::handle_ipc_command;
use crate::app::lifecycle::{
    base64_encode, close_pane, do_split, extract_selected_text, replace_pane, resize_all,
    spawn_layout_panes, spawn_project_panes, spawn_snapshot_panes,
};
use crate::app::render_ctl::{
    collect_render_targets, make_inner, render_frame, reset_render_targets, resize_zoomed_pane,
    selection_char_count_from_synced, sync_render_targets,
};
use crate::app::state::{DragState, InputMode, RenderUpdate, TextSelection};
use crate::cli::parse::{Config, LayoutSpec};
use crate::config;
use crate::ipc;
use crate::layout::{Direction, Layout, NavDir};
use crate::pane::PaneLaunch;
use crate::project;
use crate::render;
use crate::settings::{Settings, SettingsAction};
use crate::theme;
use crate::workspace;

pub(crate) fn run(stdout: &mut io::Stdout, config: &Config) -> anyhow::Result<()> {
    let (mut tw, mut th) = terminal::size()?;

    // Load config file defaults, then overlay CLI args
    let file_config = config::load_config();
    let effective_scrollback = file_config.scrollback;
    let effective_max_scrollback = file_config.scrollback_max_lines;
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
    let theme = theme::load_theme(&file_config.theme).adapt(theme::detect_caps());
    let mut settings = Settings::with_theme(effective_border, theme);
    settings.show_status_bar = file_config.show_status_bar;

    // Auto-restart state (populated from .ezpn.toml if present)
    let mut restart_policies: HashMap<usize, project::RestartPolicy> = HashMap::new();

    let (mut layout, mut panes, mut active) = if let Some(path) = &config.restore {
        let snapshot = workspace::load_snapshot(path)?;
        let tab = &snapshot.tabs[snapshot.active_tab];
        let layout = tab.layout.clone();
        default_shell = snapshot.shell.clone();
        settings.border_style = snapshot.border_style;
        settings.show_status_bar = snapshot.show_status_bar;
        settings.show_tab_bar = snapshot.show_tab_bar;
        let panes = spawn_snapshot_panes(
            &layout,
            tab,
            &default_shell,
            tw,
            th,
            &settings,
            snapshot.scrollback,
        )?;
        let active = tab.active_pane;
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
                effective_max_scrollback,
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
    let mut last_title_state: Option<(usize, usize)> = None;
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
        false,
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

                let (launch, old_name, pane_shell) = panes
                    .get(&pid)
                    .map(|p| {
                        (
                            p.launch().clone(),
                            p.name().map(String::from),
                            p.initial_shell().map(String::from),
                        )
                    })
                    .unwrap_or((PaneLaunch::Shell, None, None));
                let eff_shell = pane_shell.as_deref().unwrap_or(&default_shell);
                if replace_pane(
                    &mut panes,
                    &layout,
                    pid,
                    launch,
                    eff_shell,
                    tw,
                    th,
                    &settings,
                    effective_scrollback,
                )
                .is_ok()
                {
                    // Preserve pane name and shell override
                    if let Some(pane) = panes.get_mut(&pid) {
                        pane.set_name(old_name);
                        if let Some(ref s) = pane_shell {
                            pane.set_initial_shell(Some(s.clone()));
                        }
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
                            KeyCode::Left | KeyCode::Char('h')
                                if layout.resize_pane(active, NavDir::Left, 0.05) =>
                            {
                                resize_all(&mut panes, &layout, tw, th, &settings);
                                update.mark_all(&layout);
                                update.border_dirty = true;
                            }
                            KeyCode::Right | KeyCode::Char('l')
                                if layout.resize_pane(active, NavDir::Right, 0.05) =>
                            {
                                resize_all(&mut panes, &layout, tw, th, &settings);
                                update.mark_all(&layout);
                                update.border_dirty = true;
                            }
                            KeyCode::Up | KeyCode::Char('k')
                                if layout.resize_pane(active, NavDir::Up, 0.05) =>
                            {
                                resize_all(&mut panes, &layout, tw, th, &settings);
                                update.mark_all(&layout);
                                update.border_dirty = true;
                            }
                            KeyCode::Down | KeyCode::Char('j')
                                if layout.resize_pane(active, NavDir::Down, 0.05) =>
                            {
                                resize_all(&mut panes, &layout, tw, th, &settings);
                                update.mark_all(&layout);
                                update.border_dirty = true;
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
                                close_pane(
                                    &mut layout,
                                    &mut panes,
                                    &mut active,
                                    target,
                                    &mut restart_policies,
                                    &mut restart_state,
                                    &mut zoomed_pane,
                                );
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
                            KeyCode::Char(';') if panes.contains_key(&last_active) => {
                                active = last_active;
                                update.full_redraw = true;
                            }
                            // Cycle layout (tmux Space)
                            KeyCode::Char(' ') => {
                                layout.equalize();
                                resize_all(&mut panes, &layout, tw, th, &settings);
                                update.mark_all(&layout);
                                update.border_dirty = true;
                            }
                            // New pane (split + focus) — in --no-daemon mode only.
                            // Daemon mode (default) maps 'c' to new tab.
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
                            let prev_tab_bar = settings.show_tab_bar;
                            let action = settings.handle_key(key);
                            if action == SettingsAction::BroadcastToggle {
                                broadcast = !broadcast;
                            }
                            if settings.border_style != prev_border {
                                update.full_redraw = true;
                            }
                            if settings.show_status_bar != prev_status
                                || settings.show_tab_bar != prev_tab_bar
                            {
                                resize_all(&mut panes, &layout, tw, th, &settings);
                                update.border_dirty = true;
                                update.mark_all(&layout);
                            }
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
                            let (launch, old_name, pane_shell) = panes
                                .get(&active)
                                .map(|p| {
                                    (
                                        p.launch().clone(),
                                        p.name().map(String::from),
                                        p.initial_shell().map(String::from),
                                    )
                                })
                                .unwrap_or((PaneLaunch::Shell, None, None));
                            let eff_shell = pane_shell.as_deref().unwrap_or(&default_shell);
                            if replace_pane(
                                &mut panes,
                                &layout,
                                active,
                                launch,
                                eff_shell,
                                tw,
                                th,
                                &settings,
                                effective_scrollback,
                            )
                            .is_ok()
                            {
                                if let Some(pane) = panes.get_mut(&active) {
                                    pane.set_name(old_name);
                                    if let Some(ref s) = pane_shell {
                                        pane.set_initial_shell(Some(s.clone()));
                                    }
                                }
                            }
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
                                let prev_tab_bar = settings.show_tab_bar;
                                let action = settings.handle_click(mouse.column, mouse.row, tw, th);
                                if action == SettingsAction::BroadcastToggle {
                                    broadcast = !broadcast;
                                }
                                if settings.border_style != prev_border {
                                    update.full_redraw = true;
                                }
                                if settings.show_status_bar != prev_status
                                    || settings.show_tab_bar != prev_tab_bar
                                {
                                    resize_all(&mut panes, &layout, tw, th, &settings);
                                    update.border_dirty = true;
                                    update.mark_all(&layout);
                                }
                                if action == SettingsAction::Changed
                                    || action == SettingsAction::Close
                                    || action == SettingsAction::BroadcastToggle
                                {
                                    update.full_redraw = true;
                                }
                            } else if let Some(action) =
                                render::title_button_hit(mouse.column, mouse.row, &layout, &inner)
                            {
                                match action {
                                    render::TitleAction::Close(pid) => {
                                        close_pane(
                                            &mut layout,
                                            &mut panes,
                                            &mut active,
                                            pid,
                                            &mut restart_policies,
                                            &mut restart_state,
                                            &mut zoomed_pane,
                                        );
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
                                    let text = extract_selected_text(
                                        pane.screen(),
                                        sel.pane_id,
                                        sel.start_row,
                                        sel.start_col,
                                        sel.end_row,
                                        sel.end_col,
                                    );
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
                    effective_max_scrollback,
                    &mut restart_policies,
                    &mut restart_state,
                    &mut zoomed_pane,
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

            let needs_selection_chars =
                zoomed_pane.is_none() && settings.show_status_bar && text_selection.is_some();
            let render_targets = collect_render_targets(
                &panes,
                &update.dirty_panes,
                update.full_redraw,
                zoomed_pane,
                needs_selection_chars
                    .then(|| text_selection.as_ref().map(|s| s.pane_id))
                    .flatten(),
            );
            sync_render_targets(&mut panes, &render_targets);

            if let Some(zpid) = zoomed_pane {
                // Zoomed mode: render only the zoomed pane at full size
                queue!(stdout, terminal::BeginSynchronizedUpdate)?;
                let pane_order = border_cache.pane_order();
                let pane_idx = pane_order.iter().position(|&id| id == zpid).unwrap_or(0);
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
                        &settings.theme,
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
                        pane_order.len(),
                        zoom_label,
                        pane_name,
                        0,
                        &settings.theme,
                    )?;
                }
                queue!(stdout, terminal::EndSynchronizedUpdate)?;
                stdout.flush()?;
            } else {
                let sel_for_render = text_selection.as_ref().map(|s| {
                    let (sr, sc, er, ec) = s.normalized();
                    (s.pane_id, sr, sc, er, ec)
                });
                let sel_chars = if needs_selection_chars {
                    selection_char_count_from_synced(&panes, sel_for_render)
                } else {
                    0
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
                    sel_for_render,
                    sel_chars,
                    broadcast,
                )?;
            }

            // Overlays on top of the main render
            if matches!(mode, InputMode::HelpOverlay) {
                queue!(stdout, terminal::BeginSynchronizedUpdate)?;
                render::draw_help_overlay(stdout, tw, th, &settings.theme)?;
                queue!(stdout, terminal::EndSynchronizedUpdate)?;
                stdout.flush()?;
            }
            if matches!(mode, InputMode::PaneSelect) {
                let inner = make_inner(tw, th, settings.show_status_bar);
                queue!(stdout, terminal::BeginSynchronizedUpdate)?;
                render::draw_pane_numbers(stdout, &layout, &inner, &settings.theme)?;
                queue!(stdout, terminal::EndSynchronizedUpdate)?;
                stdout.flush()?;
            }

            reset_render_targets(&mut panes, &render_targets);
        }

        // Update window title with pane count
        {
            let pane_order = border_cache.pane_order();
            let idx = pane_order.iter().position(|&id| id == active).unwrap_or(0);
            let next_title = (idx, pane_order.len());
            if last_title_state != Some(next_title) {
                let _ = write!(stdout, "\x1b]0;ezpn [{}/{}]\x07", idx + 1, pane_order.len());
                last_title_state = Some(next_title);
            }
        }
    }

    // Restore window title
    let _ = write!(stdout, "\x1b]0;\x07");
    ipc::cleanup();
    Ok(())
}
