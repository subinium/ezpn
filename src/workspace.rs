use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::layout::Layout;
use crate::pane::{Pane, PaneLaunch};
use crate::render::BorderStyle;

const SNAPSHOT_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub version: u32,
    pub shell: String,
    pub active_pane: usize,
    pub border_style: BorderStyle,
    pub show_status_bar: bool,
    pub layout: Layout,
    pub panes: Vec<PaneSnapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneSnapshot {
    pub id: usize,
    pub launch: PaneLaunch,
}

impl WorkspaceSnapshot {
    pub fn from_live(
        layout: &Layout,
        panes: &HashMap<usize, Pane>,
        active_pane: usize,
        shell: &str,
        border_style: BorderStyle,
        show_status_bar: bool,
    ) -> Self {
        let panes = layout
            .pane_ids()
            .into_iter()
            .map(|id| PaneSnapshot {
                id,
                launch: panes
                    .get(&id)
                    .map(|pane| pane.launch().clone())
                    .unwrap_or(PaneLaunch::Shell),
            })
            .collect();

        Self {
            version: SNAPSHOT_VERSION,
            shell: shell.to_string(),
            active_pane,
            border_style,
            show_status_bar,
            layout: layout.clone(),
            panes,
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.version != SNAPSHOT_VERSION {
            anyhow::bail!(
                "unsupported snapshot version: {} (expected {})",
                self.version,
                SNAPSHOT_VERSION
            );
        }

        let mut snapshot_ids: Vec<usize> = self.panes.iter().map(|pane| pane.id).collect();
        snapshot_ids.sort_unstable();
        snapshot_ids.dedup();

        let mut layout_ids = self.layout.pane_ids();
        layout_ids.sort_unstable();

        if snapshot_ids != layout_ids {
            anyhow::bail!("snapshot panes do not match layout leaves");
        }

        if !layout_ids.contains(&self.active_pane) {
            anyhow::bail!("snapshot active pane does not exist in layout");
        }

        Ok(())
    }
}

pub fn load_snapshot(path: impl AsRef<Path>) -> anyhow::Result<WorkspaceSnapshot> {
    validate_path(path.as_ref())?;
    let snapshot = serde_json::from_str::<WorkspaceSnapshot>(&std::fs::read_to_string(path)?)?;
    snapshot.validate()?;
    Ok(snapshot)
}

pub fn save_snapshot(path: impl AsRef<Path>, snapshot: &WorkspaceSnapshot) -> anyhow::Result<()> {
    validate_path(path.as_ref())?;
    let json = serde_json::to_string_pretty(snapshot)?;
    std::fs::write(path, json)?;
    Ok(())
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

    #[test]
    fn snapshot_validation_rejects_mismatched_panes() {
        let snapshot = WorkspaceSnapshot {
            version: SNAPSHOT_VERSION,
            shell: "/bin/sh".to_string(),
            active_pane: 0,
            border_style: BorderStyle::Rounded,
            show_status_bar: true,
            layout: Layout::from_grid(1, 2),
            panes: vec![PaneSnapshot {
                id: 0,
                launch: PaneLaunch::Shell,
            }],
        };

        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn snapshot_round_trips_json() {
        let snapshot = WorkspaceSnapshot {
            version: SNAPSHOT_VERSION,
            shell: "/bin/zsh".to_string(),
            active_pane: 1,
            border_style: BorderStyle::Double,
            show_status_bar: false,
            layout: Layout::from_grid(1, 2),
            panes: vec![
                PaneSnapshot {
                    id: 0,
                    launch: PaneLaunch::Shell,
                },
                PaneSnapshot {
                    id: 1,
                    launch: PaneLaunch::Command("cargo test".to_string()),
                },
            ],
        };

        let decoded = serde_json::from_str::<WorkspaceSnapshot>(
            &serde_json::to_string(&snapshot).expect("serialize snapshot"),
        )
        .expect("deserialize snapshot");

        assert_eq!(decoded.active_pane, 1);
        assert_eq!(decoded.panes.len(), 2);
        assert_eq!(
            decoded.panes[1].launch,
            PaneLaunch::Command("cargo test".to_string())
        );
    }

    #[test]
    fn validate_path_rejects_relative_dotfiles() {
        assert!(validate_path(Path::new(".bashrc")).is_err());
        assert!(validate_path(Path::new(".ssh/config")).is_err());
        assert!(validate_path(Path::new(".ezpn-session.json")).is_ok());
        assert!(validate_path(Path::new("sessions/.ezpn/dev.json")).is_ok());
    }
}
