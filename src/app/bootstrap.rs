//! Initial workspace bring-up.
//!
//! [`build_initial_state`] picks one of (snapshot restore | `.ezpn.toml` |
//! Procfile | bare grid) based on the parsed [`Config`] and produces the
//! starting `(Layout, panes, active)` triple plus optional snapshot extras
//! (other tabs).
//!
//! Used by both the foreground loop ([`crate::app::event_loop::run`]) and
//! the daemon ([`crate::server::run`]) so the precedence stays in one
//! place. Helpers ([`build_command_launches`], [`try_load_procfile`])
//! live next to it because they're only consumed from this entry point.

use std::collections::HashMap;

use crate::app::lifecycle::{spawn_layout_panes, spawn_project_panes, spawn_snapshot_panes};
use crate::app::state::SnapshotExtra;
use crate::cli::parse::{parse_procfile, Config, LayoutSpec};
use crate::layout::Layout;
use crate::pane::{Pane, PaneLaunch};
use crate::project;
use crate::settings::Settings;
use crate::workspace;

/// Build initial layout, panes, and active pane ID from config.
/// Used by both direct mode and server mode.
/// Returns optional `SnapshotExtra` when restoring a multi-tab snapshot.
#[allow(clippy::type_complexity)]
pub(crate) fn build_initial_state(
    config: &Config,
    default_shell: &mut String,
    settings: &mut Settings,
    restart_policies: &mut HashMap<usize, project::RestartPolicy>,
    scrollback: usize,
    max_scrollback: usize,
    persist_scrollback: &mut bool,
) -> anyhow::Result<(Layout, HashMap<usize, Pane>, usize, Option<SnapshotExtra>)> {
    // Use a default terminal size for initial spawn (server doesn't have a terminal yet).
    // Panes will be resized when a client connects.
    let tw: u16 = 80;
    let th: u16 = 24;

    if let Some(path) = &config.restore {
        let snapshot = workspace::load_snapshot(path)?;
        let active_idx = snapshot.active_tab;
        let tab = &snapshot.tabs[active_idx];
        let layout = tab.layout.clone();
        *default_shell = snapshot.shell.clone();
        settings.border_style = snapshot.border_style;
        settings.show_status_bar = snapshot.show_status_bar;
        settings.show_tab_bar = snapshot.show_tab_bar;
        let effective_scrollback = snapshot.scrollback;

        // Restore restart policies from snapshot
        for ps in &tab.panes {
            if ps.restart != project::RestartPolicy::Never {
                restart_policies.insert(ps.id, ps.restart.clone());
            }
        }

        let panes = spawn_snapshot_panes(
            &layout,
            tab,
            default_shell,
            tw,
            th,
            settings,
            effective_scrollback,
        )?;
        let active = tab.active_pane;

        // Pass all tabs to the caller for full restore
        let extra = Some(SnapshotExtra {
            all_tabs: snapshot.tabs.clone(),
            active_tab_idx: active_idx,
            scrollback: effective_scrollback,
        });

        return Ok((layout, panes, active, extra));
    }

    if config.commands.is_empty() && matches!(config.layout, LayoutSpec::Grid { rows: 1, cols: 2 })
    {
        if let Some(result) = project::load_project() {
            let proj = result.map_err(|e| anyhow::anyhow!("{e}"))?;
            let panes = spawn_project_panes(
                &proj,
                default_shell,
                tw,
                th,
                settings,
                scrollback,
                max_scrollback,
            )?;
            *restart_policies = proj.restarts.clone();
            // Per-project override for persist_scrollback (falls back to global).
            if let Some(override_value) = proj.persist_scrollback {
                *persist_scrollback = override_value;
            }
            let active = *proj.layout.pane_ids().first().unwrap_or(&0);
            return Ok((proj.layout, panes, active, None));
        } else if let Some((layout, launches)) = try_load_procfile() {
            let panes = spawn_layout_panes(
                &layout,
                launches,
                default_shell,
                tw,
                th,
                settings,
                scrollback,
            )?;
            let active = *layout.pane_ids().first().unwrap_or(&0);
            return Ok((layout, panes, active, None));
        } else {
            let layout = Layout::from_grid(1, 2);
            let panes = spawn_layout_panes(
                &layout,
                build_command_launches(&layout, &config.commands),
                default_shell,
                tw,
                th,
                settings,
                scrollback,
            )?;
            let active = *layout.pane_ids().first().unwrap_or(&0);
            return Ok((layout, panes, active, None));
        }
    }

    let layout = match &config.layout {
        LayoutSpec::Grid { rows, cols } => Layout::from_grid(*rows, *cols),
        LayoutSpec::Spec(spec) => {
            Layout::from_spec(spec).map_err(|error| anyhow::anyhow!(error))?
        }
    };
    let panes = spawn_layout_panes(
        &layout,
        build_command_launches(&layout, &config.commands),
        default_shell,
        tw,
        th,
        settings,
        scrollback,
    )?;
    let active = *layout.pane_ids().first().unwrap_or(&0);
    Ok((layout, panes, active, None))
}

/// Try to load a Procfile from the current directory. Returns layout + launches.
pub(crate) fn try_load_procfile() -> Option<(Layout, HashMap<usize, PaneLaunch>)> {
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

pub(crate) fn build_command_launches(
    layout: &Layout,
    commands: &[String],
) -> HashMap<usize, PaneLaunch> {
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
