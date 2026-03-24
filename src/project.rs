//! `.ezpn.toml` project workspace file support.
//!
//! When `ezpn` is run with no layout arguments and a `.ezpn.toml` exists
//! in the current directory, it is automatically loaded.
//!
//! Format:
//! ```toml
//! [workspace]
//! layout = "7:3/1:1"   # ratio spec, OR:
//! # rows = 2
//! # cols = 3            # grid spec
//!
//! [[pane]]
//! command = "cargo watch -x test"
//! cwd = "./backend"
//!
//! [[pane]]
//! command = "npm run dev"
//! cwd = "./frontend"
//! ```

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::layout::Layout;
use crate::pane::PaneLaunch;

/// Top-level `.ezpn.toml` structure.
#[derive(Deserialize)]
pub struct ProjectConfig {
    pub workspace: Option<WorkspaceSection>,
    #[serde(default)]
    pub pane: Vec<PaneSection>,
}

#[derive(Deserialize)]
pub struct WorkspaceSection {
    pub layout: Option<String>,
    pub rows: Option<usize>,
    pub cols: Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy {
    #[default]
    Never,
    OnFailure,
    Always,
}

#[derive(Deserialize)]
pub struct PaneSection {
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub restart: RestartPolicy,
    pub shell: Option<String>,
}

/// Resolved project config ready for launching.
pub struct ResolvedProject {
    pub layout: Layout,
    pub launches: HashMap<usize, PaneLaunch>,
    pub cwds: HashMap<usize, PathBuf>,
    pub names: HashMap<usize, String>,
    pub envs: HashMap<usize, HashMap<String, String>>,
    pub restarts: HashMap<usize, RestartPolicy>,
    pub shells: HashMap<usize, String>,
}

/// Try to find and load `.ezpn.toml` from the current directory.
/// Returns `None` if the file doesn't exist.
pub fn load_project() -> Option<Result<ResolvedProject, String>> {
    let path = Path::new(".ezpn.toml");
    if !path.exists() {
        return None;
    }
    Some(load_project_from(path))
}

fn load_project_from(path: &Path) -> Result<ResolvedProject, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let config: ProjectConfig =
        toml::from_str(&contents).map_err(|e| format!("parse error in {}: {e}", path.display()))?;

    // Determine layout
    let layout = resolve_layout(&config)?;
    let pane_ids = layout.pane_ids();

    // Map pane sections to pane IDs
    let mut launches = HashMap::new();
    let mut cwds = HashMap::new();
    let mut names = HashMap::new();
    let mut envs = HashMap::new();
    let mut restarts = HashMap::new();
    let mut shells = HashMap::new();
    let base_dir = path
        .parent()
        .unwrap_or(Path::new("."))
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."));

    for (i, pid) in pane_ids.iter().enumerate() {
        if let Some(section) = config.pane.get(i) {
            if let Some(cmd) = &section.command {
                launches.insert(*pid, PaneLaunch::Command(cmd.clone()));
            } else {
                launches.insert(*pid, PaneLaunch::Shell);
            }
            if let Some(cwd) = &section.cwd {
                let resolved = base_dir.join(cwd);
                cwds.insert(*pid, resolved);
            }
            if let Some(name) = &section.name {
                names.insert(*pid, name.clone());
            }
            if !section.env.is_empty() {
                envs.insert(*pid, section.env.clone());
            }
            if section.restart != RestartPolicy::Never {
                restarts.insert(*pid, section.restart.clone());
            }
            if let Some(shell) = &section.shell {
                shells.insert(*pid, shell.clone());
            }
        } else {
            launches.insert(*pid, PaneLaunch::Shell);
        }
    }

    Ok(ResolvedProject {
        layout,
        launches,
        cwds,
        names,
        envs,
        restarts,
        shells,
    })
}

fn resolve_layout(config: &ProjectConfig) -> Result<Layout, String> {
    let ws = config.workspace.as_ref();

    if let Some(ws) = ws {
        // layout spec takes priority
        if let Some(spec) = &ws.layout {
            return Layout::from_spec(spec);
        }
        // grid from rows/cols
        let rows = ws.rows.unwrap_or(1);
        let cols = ws.cols.unwrap_or(2);
        if rows == 0 || cols == 0 {
            return Err("rows and cols must be >= 1".into());
        }
        if rows * cols > 100 {
            return Err(format!(
                "maximum 100 panes (got {}x{}={})",
                rows,
                cols,
                rows * cols
            ));
        }
        return Ok(Layout::from_grid(rows, cols));
    }

    // No [workspace] section — infer from pane count
    let pane_count = config.pane.len().max(2);
    Ok(Layout::from_grid(1, pane_count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_toml() {
        let toml_str = r#"
[workspace]
layout = "7:3"

[[pane]]
command = "cargo test"

[[pane]]
command = "npm dev"
cwd = "./frontend"
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.workspace.as_ref().unwrap().layout.as_deref(),
            Some("7:3")
        );
        assert_eq!(config.pane.len(), 2);
        assert_eq!(config.pane[0].command.as_deref(), Some("cargo test"));
        assert_eq!(config.pane[1].cwd.as_deref(), Some("./frontend"));
    }

    #[test]
    fn parse_grid_toml() {
        let toml_str = r#"
[workspace]
rows = 2
cols = 3
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        let ws = config.workspace.unwrap();
        assert_eq!(ws.rows, Some(2));
        assert_eq!(ws.cols, Some(3));
    }

    #[test]
    fn parse_minimal_toml() {
        let toml_str = r#"
[[pane]]
command = "make watch"

[[pane]]
command = "make test"
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert!(config.workspace.is_none());
        assert_eq!(config.pane.len(), 2);
    }

    #[test]
    fn parse_enhanced_fields() {
        let toml_str = r#"
[workspace]
layout = "1:1"

[[pane]]
name = "server"
command = "npm run dev"
cwd = "./frontend"
shell = "/bin/bash"
restart = "on_failure"

[pane.env]
NODE_ENV = "development"
PORT = "3000"

[[pane]]
name = "tests"
command = "cargo watch -x test"
restart = "always"
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.pane.len(), 2);

        let p0 = &config.pane[0];
        assert_eq!(p0.name.as_deref(), Some("server"));
        assert_eq!(p0.shell.as_deref(), Some("/bin/bash"));
        assert_eq!(p0.restart, RestartPolicy::OnFailure);
        assert_eq!(
            p0.env.get("NODE_ENV").map(|s| s.as_str()),
            Some("development")
        );
        assert_eq!(p0.env.get("PORT").map(|s| s.as_str()), Some("3000"));

        let p1 = &config.pane[1];
        assert_eq!(p1.name.as_deref(), Some("tests"));
        assert_eq!(p1.restart, RestartPolicy::Always);
        assert!(p1.env.is_empty());
    }

    #[test]
    fn parse_default_restart() {
        let toml_str = r#"
[[pane]]
command = "echo hello"
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.pane[0].restart, RestartPolicy::Never);
    }
}
