use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::layout::Layout;
use crate::pane::{Pane, PaneLaunch};
use crate::project::RestartPolicy;
use crate::render::BorderStyle;
use crate::tab::TabManager;

const SNAPSHOT_VERSION: u32 = 2;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub version: u32,
    pub shell: String,
    pub border_style: BorderStyle,
    pub show_status_bar: bool,
    #[serde(default = "default_true")]
    pub show_tab_bar: bool,
    #[serde(default = "default_scrollback")]
    pub scrollback: usize,
    pub active_tab: usize,
    pub tabs: Vec<TabSnapshot>,
}

fn default_true() -> bool {
    true
}

fn default_scrollback() -> usize {
    10_000
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TabSnapshot {
    pub name: String,
    pub layout: Layout,
    pub active_pane: usize,
    #[serde(default)]
    pub zoomed_pane: Option<usize>,
    #[serde(default)]
    pub broadcast: bool,
    pub panes: Vec<PaneSnapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneSnapshot {
    pub id: usize,
    pub launch: PaneLaunch,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub restart: RestartPolicy,
    #[serde(default)]
    pub shell: Option<String>,
}

impl WorkspaceSnapshot {
    /// Create a snapshot from live state.
    ///
    /// The active tab is "unpacked" (its state is in separate variables),
    /// while inactive tabs are stored in the `TabManager`.
    #[allow(clippy::too_many_arguments)]
    pub fn from_live(
        tab_mgr: &TabManager,
        tab_name: &str,
        layout: &Layout,
        panes: &HashMap<usize, Pane>,
        active_pane: usize,
        zoomed_pane: Option<usize>,
        broadcast: bool,
        restart_policies: &HashMap<usize, RestartPolicy>,
        shell: &str,
        border_style: BorderStyle,
        show_status_bar: bool,
        show_tab_bar: bool,
        scrollback: usize,
    ) -> Self {
        let mut tabs = Vec::with_capacity(tab_mgr.count);

        for i in 0..tab_mgr.count {
            if i == tab_mgr.active_idx {
                // Active tab: build from unpacked state
                tabs.push(TabSnapshot {
                    name: tab_name.to_string(),
                    layout: layout.clone(),
                    active_pane,
                    zoomed_pane,
                    broadcast,
                    panes: snapshot_panes(layout, panes, restart_policies),
                });
            } else if let Some(tab) = tab_mgr.get_inactive(i) {
                tabs.push(TabSnapshot {
                    name: tab.name.clone(),
                    layout: tab.layout.clone(),
                    active_pane: tab.active_pane,
                    zoomed_pane: tab.zoomed_pane,
                    broadcast: tab.broadcast,
                    panes: snapshot_panes(&tab.layout, &tab.panes, &tab.restart_policies),
                });
            }
        }

        Self {
            version: SNAPSHOT_VERSION,
            shell: shell.to_string(),
            border_style,
            show_status_bar,
            show_tab_bar,
            scrollback,
            active_tab: tab_mgr.active_idx,
            tabs,
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.version != SNAPSHOT_VERSION && self.version != 1 {
            anyhow::bail!(
                "unsupported snapshot version: {} (expected {} or 1)",
                self.version,
                SNAPSHOT_VERSION
            );
        }

        if self.tabs.is_empty() {
            anyhow::bail!("snapshot has no tabs");
        }

        for (ti, tab) in self.tabs.iter().enumerate() {
            let mut snapshot_ids: Vec<usize> = tab.panes.iter().map(|pane| pane.id).collect();
            snapshot_ids.sort_unstable();
            snapshot_ids.dedup();

            let mut layout_ids = tab.layout.pane_ids();
            layout_ids.sort_unstable();

            if snapshot_ids != layout_ids {
                anyhow::bail!("snapshot panes do not match layout leaves in tab {}", ti);
            }

            if !layout_ids.contains(&tab.active_pane) {
                anyhow::bail!(
                    "snapshot active pane does not exist in layout in tab {}",
                    ti
                );
            }
        }

        if self.active_tab >= self.tabs.len() {
            anyhow::bail!("snapshot active_tab index out of range");
        }

        Ok(())
    }
}

/// Build PaneSnapshot vec for a set of panes.
fn snapshot_panes(
    layout: &Layout,
    panes: &HashMap<usize, Pane>,
    restart_policies: &HashMap<usize, RestartPolicy>,
) -> Vec<PaneSnapshot> {
    layout
        .pane_ids()
        .into_iter()
        .map(|id| {
            let pane = panes.get(&id);
            PaneSnapshot {
                id,
                launch: pane
                    .map(|p| p.launch().clone())
                    .unwrap_or(PaneLaunch::Shell),
                name: pane.and_then(|p| p.name().map(|s| s.to_string())),
                cwd: pane
                    .and_then(|p| p.live_cwd())
                    .map(|p| p.to_string_lossy().to_string()),
                env: pane.map(|p| p.initial_env().clone()).unwrap_or_default(),
                restart: restart_policies.get(&id).cloned().unwrap_or_default(),
                shell: pane.and_then(|p| p.initial_shell().map(|s| s.to_string())),
            }
        })
        .collect()
}

/// Migrate a v1 snapshot to v2 format.
fn migrate_v1(v1_json: &serde_json::Value) -> anyhow::Result<WorkspaceSnapshot> {
    // V1 had flat fields: shell, active_pane, border_style, show_status_bar, layout, panes
    let shell = v1_json["shell"].as_str().unwrap_or("/bin/sh").to_string();
    let active_pane = v1_json["active_pane"].as_u64().unwrap_or(0) as usize;
    let border_style: BorderStyle =
        serde_json::from_value(v1_json["border_style"].clone()).unwrap_or(BorderStyle::Rounded);
    let show_status_bar = v1_json["show_status_bar"].as_bool().unwrap_or(true);
    let layout: Layout = serde_json::from_value(v1_json["layout"].clone())?;
    let v1_panes: Vec<serde_json::Value> = v1_json["panes"].as_array().cloned().unwrap_or_default();

    let panes: Vec<PaneSnapshot> = v1_panes
        .into_iter()
        .map(|p| PaneSnapshot {
            id: p["id"].as_u64().unwrap_or(0) as usize,
            launch: serde_json::from_value(p["launch"].clone()).unwrap_or(PaneLaunch::Shell),
            name: None,
            cwd: None,
            env: HashMap::new(),
            restart: RestartPolicy::default(),
            shell: None,
        })
        .collect();

    let tab = TabSnapshot {
        name: "1".to_string(),
        layout,
        active_pane,
        zoomed_pane: None,
        broadcast: false,
        panes,
    };

    Ok(WorkspaceSnapshot {
        version: SNAPSHOT_VERSION,
        shell,
        border_style,
        show_status_bar,
        show_tab_bar: true,
        scrollback: 10_000,
        active_tab: 0,
        tabs: vec![tab],
    })
}

pub fn load_snapshot(path: impl AsRef<Path>) -> anyhow::Result<WorkspaceSnapshot> {
    validate_path(path.as_ref())?;
    let content = std::fs::read_to_string(path)?;
    let raw: serde_json::Value = serde_json::from_str(&content)?;

    let version = raw["version"].as_u64().unwrap_or(0) as u32;
    let snapshot = if version == 1 {
        migrate_v1(&raw)?
    } else {
        serde_json::from_value::<WorkspaceSnapshot>(raw)?
    };

    snapshot.validate()?;
    Ok(snapshot)
}

pub fn save_snapshot(path: impl AsRef<Path>, snapshot: &WorkspaceSnapshot) -> anyhow::Result<()> {
    validate_path(path.as_ref())?;
    save_snapshot_raw(path, snapshot)
}

/// Save without `validate_path`. Used by auto-save where the path is managed
/// by ezpn itself (e.g. `~/.local/share/ezpn/sessions/`).
fn save_snapshot_raw(path: impl AsRef<Path>, snapshot: &WorkspaceSnapshot) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(snapshot)?;

    // Atomic write: write to temp file, then rename
    let path = path.as_ref();
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, &json)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        // Clean up temp file on rename failure
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// Auto-save directory for session snapshots.
pub fn auto_save_dir() -> Option<std::path::PathBuf> {
    let dir = if let Ok(data_dir) = std::env::var("XDG_DATA_HOME") {
        std::path::PathBuf::from(data_dir)
            .join("ezpn")
            .join("sessions")
    } else if let Ok(home) = std::env::var("HOME") {
        std::path::PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("ezpn")
            .join("sessions")
    } else {
        return None;
    };
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Auto-save a snapshot for the given session name.
/// Uses `save_snapshot_raw` to bypass `validate_path` since the auto-save
/// directory is managed by ezpn itself (e.g. `~/.local/share/ezpn/sessions/`).
pub fn auto_save(session_name: &str, snapshot: &WorkspaceSnapshot) {
    if let Some(dir) = auto_save_dir() {
        let path = dir.join(format!("{}.json", session_name));
        if let Err(e) = save_snapshot_raw(&path, snapshot) {
            eprintln!("ezpn: auto-save failed: {e}");
        }
    }
}

/// Load an auto-saved snapshot for the given session name.
#[allow(dead_code)] // Public API for future session resume feature
pub fn auto_load(session_name: &str) -> Option<WorkspaceSnapshot> {
    let dir = auto_save_dir()?;
    let path = dir.join(format!("{}.json", session_name));
    if !path.exists() {
        return None;
    }
    // For auto-load, we skip validate_path since it's our own managed directory
    let content = std::fs::read_to_string(&path).ok()?;
    let raw: serde_json::Value = serde_json::from_str(&content).ok()?;
    let version = raw["version"].as_u64().unwrap_or(0) as u32;
    let snapshot = if version == 1 {
        migrate_v1(&raw).ok()?
    } else {
        serde_json::from_value::<WorkspaceSnapshot>(raw).ok()?
    };
    snapshot.validate().ok()?;
    Some(snapshot)
}

/// Reject paths that could be dangerous when invoked via IPC.
fn validate_path(path: &Path) -> anyhow::Result<()> {
    let s = path.to_string_lossy();
    if s.contains("..") {
        anyhow::bail!("path traversal (..) not allowed: {}", s);
    }

    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            let name = name.to_string_lossy();
            if name.starts_with('.') && !name.contains("ezpn") {
                anyhow::bail!("refusing to use hidden path: {}", s);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Layout;

    fn make_v2_snapshot() -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            version: SNAPSHOT_VERSION,
            shell: "/bin/zsh".to_string(),
            border_style: BorderStyle::Double,
            show_status_bar: false,
            show_tab_bar: true,
            scrollback: 10_000,
            active_tab: 0,
            tabs: vec![TabSnapshot {
                name: "1".to_string(),
                layout: Layout::from_grid(1, 2),
                active_pane: 1,
                zoomed_pane: None,
                broadcast: false,
                panes: vec![
                    PaneSnapshot {
                        id: 0,
                        launch: PaneLaunch::Shell,
                        name: None,
                        cwd: None,
                        env: HashMap::new(),
                        restart: RestartPolicy::Never,
                        shell: None,
                    },
                    PaneSnapshot {
                        id: 1,
                        launch: PaneLaunch::Command("cargo test".to_string()),
                        name: Some("tests".to_string()),
                        cwd: Some("/tmp".to_string()),
                        env: HashMap::new(),
                        restart: RestartPolicy::OnFailure,
                        shell: None,
                    },
                ],
            }],
        }
    }

    #[test]
    fn snapshot_validation_rejects_mismatched_panes() {
        let mut snapshot = make_v2_snapshot();
        snapshot.tabs[0].panes.pop(); // Remove one pane
        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn snapshot_round_trips_json() {
        let snapshot = make_v2_snapshot();
        let json = serde_json::to_string(&snapshot).expect("serialize snapshot");
        let decoded =
            serde_json::from_str::<WorkspaceSnapshot>(&json).expect("deserialize snapshot");

        assert_eq!(decoded.tabs.len(), 1);
        assert_eq!(decoded.tabs[0].active_pane, 1);
        assert_eq!(decoded.tabs[0].panes.len(), 2);
        assert_eq!(
            decoded.tabs[0].panes[1].launch,
            PaneLaunch::Command("cargo test".to_string())
        );
        assert_eq!(decoded.tabs[0].panes[1].name, Some("tests".to_string()));
        assert_eq!(decoded.tabs[0].panes[1].restart, RestartPolicy::OnFailure);
    }

    #[test]
    fn validate_path_rejects_relative_dotfiles() {
        assert!(validate_path(Path::new(".bashrc")).is_err());
        assert!(validate_path(Path::new(".ssh/config")).is_err());
        assert!(validate_path(Path::new(".ezpn-session.json")).is_ok());
        assert!(validate_path(Path::new("sessions/.ezpn/dev.json")).is_ok());
    }

    #[test]
    fn v1_migration_produces_valid_v2() {
        // Use a real Layout to get correct serialization format
        let layout = Layout::from_grid(1, 1);
        let layout_json = serde_json::to_value(&layout).expect("serialize layout");
        let v1_json = serde_json::json!({
            "version": 1,
            "shell": "/bin/bash",
            "active_pane": 0,
            "border_style": "rounded",
            "show_status_bar": true,
            "layout": layout_json,
            "panes": [{ "id": 0, "launch": "shell" }]
        });
        let snapshot = migrate_v1(&v1_json).expect("migration");
        assert_eq!(snapshot.version, SNAPSHOT_VERSION);
        assert_eq!(snapshot.tabs.len(), 1);
        assert_eq!(snapshot.tabs[0].active_pane, 0);
        assert!(snapshot.validate().is_ok());
    }

    #[test]
    fn validate_rejects_out_of_range_active_tab() {
        let mut snapshot = make_v2_snapshot();
        snapshot.active_tab = 99;
        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn multi_tab_round_trip() {
        let snapshot = WorkspaceSnapshot {
            version: SNAPSHOT_VERSION,
            shell: "/bin/zsh".to_string(),
            border_style: BorderStyle::Rounded,
            show_status_bar: true,
            show_tab_bar: false,
            scrollback: 5000,
            active_tab: 1,
            tabs: vec![
                TabSnapshot {
                    name: "editor".to_string(),
                    layout: Layout::from_grid(1, 1),
                    active_pane: 0,
                    zoomed_pane: None,
                    broadcast: false,
                    panes: vec![PaneSnapshot {
                        id: 0,
                        launch: PaneLaunch::Command("nvim .".to_string()),
                        name: Some("nvim".to_string()),
                        cwd: Some("/home/user/project".to_string()),
                        env: HashMap::new(),
                        restart: RestartPolicy::Never,
                        shell: None,
                    }],
                },
                TabSnapshot {
                    name: "server".to_string(),
                    layout: Layout::from_grid(1, 2),
                    active_pane: 1,
                    zoomed_pane: Some(1),
                    broadcast: true,
                    panes: vec![
                        PaneSnapshot {
                            id: 0,
                            launch: PaneLaunch::Command("npm run dev".to_string()),
                            name: Some("dev".to_string()),
                            cwd: Some("/tmp".to_string()),
                            env: [("PORT".to_string(), "3000".to_string())].into(),
                            restart: RestartPolicy::OnFailure,
                            shell: Some("/bin/bash".to_string()),
                        },
                        PaneSnapshot {
                            id: 1,
                            launch: PaneLaunch::Shell,
                            name: None,
                            cwd: None,
                            env: HashMap::new(),
                            restart: RestartPolicy::Never,
                            shell: None,
                        },
                    ],
                },
            ],
        };

        let json = serde_json::to_string_pretty(&snapshot).unwrap();
        let decoded: WorkspaceSnapshot = serde_json::from_str(&json).unwrap();
        decoded.validate().unwrap();

        assert_eq!(decoded.active_tab, 1);
        assert_eq!(decoded.scrollback, 5000);
        assert!(!decoded.show_tab_bar);
        assert_eq!(decoded.tabs.len(), 2);

        // Tab 0
        assert_eq!(decoded.tabs[0].name, "editor");
        assert_eq!(decoded.tabs[0].panes[0].name, Some("nvim".to_string()));
        assert_eq!(
            decoded.tabs[0].panes[0].cwd,
            Some("/home/user/project".to_string())
        );

        // Tab 1
        assert_eq!(decoded.tabs[1].name, "server");
        assert_eq!(decoded.tabs[1].zoomed_pane, Some(1));
        assert!(decoded.tabs[1].broadcast);
        assert_eq!(decoded.tabs[1].panes[0].restart, RestartPolicy::OnFailure);
        assert_eq!(
            decoded.tabs[1].panes[0].shell,
            Some("/bin/bash".to_string())
        );
        assert_eq!(
            decoded.tabs[1].panes[0].env.get("PORT"),
            Some(&"3000".to_string())
        );
    }

    #[test]
    fn pane_metadata_defaults_on_missing_fields() {
        // Simulate a v2 snapshot with minimal pane fields (serde defaults kick in)
        let json = serde_json::json!({
            "version": 2,
            "shell": "/bin/sh",
            "border_style": "single",
            "show_status_bar": true,
            "active_tab": 0,
            "tabs": [{
                "name": "1",
                "layout": serde_json::to_value(Layout::from_grid(1, 1)).unwrap(),
                "active_pane": 0,
                "panes": [{
                    "id": 0,
                    "launch": "shell"
                }]
            }]
        });
        let snapshot: WorkspaceSnapshot = serde_json::from_value(json).unwrap();
        snapshot.validate().unwrap();

        let pane = &snapshot.tabs[0].panes[0];
        assert_eq!(pane.name, None);
        assert_eq!(pane.cwd, None);
        assert!(pane.env.is_empty());
        assert_eq!(pane.restart, RestartPolicy::Never);
        assert_eq!(pane.shell, None);
        assert_eq!(snapshot.scrollback, 10_000); // default_scrollback()
        assert!(snapshot.show_tab_bar); // default_true()
    }
}
