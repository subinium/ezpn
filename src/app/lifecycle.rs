//! Pane lifecycle helpers shared by the foreground loop and the daemon.
//!
//! These functions all hover around the `(Layout, HashMap<usize, Pane>)`
//! pair plus a `Settings` reference. Splitting them into smaller modules
//! would force callers to import three files for any non-trivial mutation,
//! so they live together.
//!
//! Bucket overview:
//! - **Per-pane spawn**: [`spawn_pane`], [`spawn_layout_panes`],
//!   [`spawn_snapshot_panes`], [`spawn_project_panes`], [`replace_pane`].
//! - **Mutations**: [`do_split`], [`close_pane`], [`resize_all`],
//!   [`apply_snapshot`], [`kill_all_panes`].
//! - **Selection / clipboard**: [`extract_selected_text`], [`base64_encode`].
//!
//! Initial bring-up (snapshot/project/Procfile selection) lives next door
//! in [`super::bootstrap`].

use std::collections::HashMap;

use crate::app::render_ctl::make_inner;
use crate::layout::{Direction, Layout};
use crate::pane::{Pane, PaneLaunch};
use crate::project;
use crate::settings::Settings;
use crate::workspace::{self, WorkspaceSnapshot};

/// Extract selected text — server-friendly version that takes individual coords.
pub(crate) fn extract_selected_text(
    screen: &vt100::Screen,
    _pane_id: usize,
    start_row: u16,
    start_col: u16,
    end_row: u16,
    end_col: u16,
) -> String {
    // Normalize
    let (sr, sc, er, ec) = if start_row < end_row || (start_row == end_row && start_col <= end_col)
    {
        (start_row, start_col, end_row, end_col)
    } else {
        (end_row, end_col, start_row, start_col)
    };

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
        let trimmed = row_text.trim_end();
        text.push_str(trimmed);
        if r < er {
            text.push('\n');
        }
    }
    text
}

pub(crate) fn spawn_layout_panes(
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
            match handle.join() {
                Ok(result) => results.push(result),
                Err(payload) => {
                    let reason = match payload.downcast_ref::<&'static str>() {
                        Some(s) => (*s).to_string(),
                        None => match payload.downcast_ref::<String>() {
                            Some(s) => s.clone(),
                            None => "unknown panic payload".to_string(),
                        },
                    };
                    eprintln!("ezpn: pane spawn thread panicked: {}", reason);
                    // Continue with the panes that did spawn — partial workspace
                    // is preferable to aborting the entire session.
                }
            }
        }
    });

    let mut panes = HashMap::new();
    for (pid, result) in results {
        panes.insert(pid, result?);
    }
    Ok(panes)
}

pub(crate) fn spawn_snapshot_panes(
    layout: &Layout,
    tab: &workspace::TabSnapshot,
    shell: &str,
    tw: u16,
    th: u16,
    settings: &Settings,
    scrollback: usize,
) -> anyhow::Result<HashMap<usize, Pane>> {
    let inner = make_inner(tw, th, settings.show_status_bar);
    let rects = layout.pane_rects(&inner);
    let mut panes = HashMap::new();

    for ps in &tab.panes {
        let rect = rects.get(&ps.id).cloned().unwrap_or(crate::layout::Rect {
            x: 0,
            y: 0,
            w: 80,
            h: 24,
        });
        let cols = rect.w.max(1);
        let rows = rect.h.max(1);
        let pane_shell = ps.shell.as_deref().unwrap_or(shell);
        let cwd = ps.cwd.as_ref().map(std::path::PathBuf::from);
        let cwd_ref = cwd.as_deref();
        let mut pane = Pane::with_full_config(
            pane_shell,
            ps.launch.clone(),
            cols,
            rows,
            scrollback,
            cwd_ref,
            &ps.env,
        )?;
        if let Some(name) = &ps.name {
            pane.set_name(Some(name.clone()));
        }
        if ps.shell.is_some() {
            pane.set_initial_shell(ps.shell.clone());
        }
        // Replay v3 scrollback blob if present. On error, warn and continue
        // with an empty scrollback — never fail the whole restore.
        if let Some(blob) = &ps.scrollback_blob {
            if let Err(e) = crate::snapshot_blob::decode_scrollback(blob, pane.parser_mut()) {
                eprintln!("ezpn: scrollback restore failed for pane {}: {}", ps.id, e);
            }
        }
        panes.insert(ps.id, pane);
    }
    Ok(panes)
}

pub(crate) fn spawn_pane(
    shell: &str,
    launch: &PaneLaunch,
    cols: u16,
    rows: u16,
    scrollback: usize,
) -> anyhow::Result<Pane> {
    Pane::with_scrollback(shell, launch.clone(), cols, rows, scrollback)
}

pub(crate) fn spawn_project_panes(
    proj: &project::ResolvedProject,
    shell: &str,
    tw: u16,
    th: u16,
    settings: &Settings,
    scrollback: usize,
    max_scrollback: usize,
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
        // SPEC 02: per-pane override takes precedence; capped against the
        // workspace-wide max so a stray `scrollback_lines = 1_000_000` in
        // `.ezpn.toml` cannot bypass the global ceiling.
        let pane_scrollback = proj
            .scrollback_overrides
            .get(&pid)
            .copied()
            .unwrap_or(scrollback)
            .min(max_scrollback);
        let mut pane =
            Pane::with_full_config(pane_shell, launch, cols, rows, pane_scrollback, cwd, &env)?;
        if let Some(name) = proj.names.get(&pid) {
            pane.set_name(Some(name.clone()));
        }
        // Track per-pane shell override for snapshot/restart
        if proj.shells.contains_key(&pid) {
            pane.set_initial_shell(Some(pane_shell.to_string()));
        }
        panes.insert(pid, pane);
    }
    Ok(panes)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn replace_pane(
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
    // Extract cwd/env from the old pane before replacing
    let (cwd, env) = panes
        .get(&pane_id)
        .map(|p| {
            (
                p.live_cwd()
                    .or_else(|| p.initial_cwd().map(|c| c.to_path_buf())),
                p.initial_env().clone(),
            )
        })
        .unwrap_or((None, std::collections::HashMap::new()));
    let inner = make_inner(tw, th, settings.show_status_bar);
    let rect = layout
        .pane_rects(&inner)
        .remove(&pane_id)
        .ok_or_else(|| anyhow::anyhow!("pane rect not found"))?;
    let new_pane = Pane::with_full_config(
        shell,
        launch,
        rect.w.max(1),
        rect.h.max(1),
        scrollback,
        cwd.as_deref(),
        &env,
    )?;
    if let Some(mut old_pane) = panes.insert(pane_id, new_pane) {
        old_pane.kill();
    }
    Ok(())
}

pub(crate) fn kill_all_panes(panes: &mut HashMap<usize, Pane>) {
    for (_, mut pane) in panes.drain() {
        pane.kill();
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_snapshot(
    snapshot: WorkspaceSnapshot,
    layout: &mut Layout,
    panes: &mut HashMap<usize, Pane>,
    active: &mut usize,
    shell: &mut String,
    settings: &mut Settings,
    tw: u16,
    th: u16,
    _scrollback: usize,
) -> anyhow::Result<()> {
    let tab = &snapshot.tabs[snapshot.active_tab];
    let mut next_settings = Settings::with_theme(snapshot.border_style, settings.theme.clone());
    next_settings.show_status_bar = snapshot.show_status_bar;
    next_settings.show_tab_bar = snapshot.show_tab_bar;
    let next_layout = tab.layout.clone();
    let next_panes = spawn_snapshot_panes(
        &next_layout,
        tab,
        &snapshot.shell,
        tw,
        th,
        &next_settings,
        snapshot.scrollback,
    )?;

    kill_all_panes(panes);
    *shell = snapshot.shell.clone();
    *layout = next_layout;
    *panes = next_panes;
    *settings = next_settings;
    settings.visible = false;
    *active = tab.active_pane;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn do_split(
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

pub(crate) fn close_pane(
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

pub(crate) fn resize_all(
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

/// Minimal base64 encoder for OSC 52 clipboard.
pub(crate) fn base64_encode(data: &[u8]) -> String {
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
