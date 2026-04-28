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

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::env_interp::{self, EnvContext, ExpandError, Redacted, SecretsLoadError};
use crate::hooks::{Hook, RawHook};
use crate::layout::Layout;
use crate::pane::PaneLaunch;

/// Top-level `.ezpn.toml` structure.
#[derive(Deserialize)]
pub struct ProjectConfig {
    pub workspace: Option<WorkspaceSection>,
    #[serde(default)]
    pub pane: Vec<PaneSection>,
    /// Project-level `[[hooks]]`. Merged with global hooks from
    /// `~/.config/ezpn/config.toml`. See `crate::hooks` (issue #83) for the
    /// security model and event vocabulary.
    #[serde(default)]
    pub hooks: Vec<RawHook>,
}

#[derive(Deserialize)]
pub struct WorkspaceSection {
    pub layout: Option<String>,
    pub rows: Option<usize>,
    pub cols: Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
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
    /// Per-pane override for snapshot scrollback persistence (#69).
    /// `None` (omitted) means "fall back to `[global] persist_scrollback`";
    /// `Some(true)` / `Some(false)` force the pane on or off regardless
    /// of the global default. Resolution lives in [`ResolvedProject`]
    /// so the daemon can consult a single map at save time.
    pub persist_scrollback: Option<bool>,
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
    /// Per-pane scrollback-persistence overrides (#69). `Some(true)` /
    /// `Some(false)` force the pane on or off; missing entries fall
    /// back to `[global] persist_scrollback` from `EzpnConfig`. Storing
    /// only the explicit overrides keeps the map empty for the typical
    /// case where the user just sets the global flag.
    pub persist_scrollback: HashMap<usize, bool>,
    /// Validated project-level hooks. Already passed through
    /// `Hook::from_raw`, so unknown events / empty exec arrays are
    /// rejected here at load time. Merge into the global executor at
    /// server boot (`HookExecutor::new(global ++ project.hooks)`).
    pub hooks: Vec<Hook>,
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
    let mut persist_scrollback = HashMap::new();
    let base_dir = path
        .parent()
        .unwrap_or(Path::new("."))
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."));

    // Build the interpolation context once per `.ezpn.toml` parse:
    //   per-pane env > .env.local > secrets > process env
    // Secrets are gated behind ${secret:KEY} and never leak through Debug.
    let dotenv = env_interp::load_dotenv(&base_dir).map_err(|e| {
        // Error message comes from load_dotenv; never includes secret values.
        format!("loading .env.local: {e}")
    })?;
    let secrets_path = env_interp::default_secrets_path();
    let secrets: HashMap<String, Redacted<String>> = env_interp::load_secrets(&secrets_path)
        .map_err(|e: SecretsLoadError| format!("loading secrets: {e}"))?;
    let base_ctx = EnvContext::build(dotenv, secrets);

    for (i, pid) in pane_ids.iter().enumerate() {
        if let Some(section) = config.pane.get(i) {
            // Resolve the pane's own env block first, using the base context
            // (no pane overrides yet — the pane's env values are the source
            // for that precedence layer, so they cannot reference themselves).
            let mut resolved_env: HashMap<String, String> = HashMap::new();
            for (k, v) in &section.env {
                let expanded = env_interp::expand(v, &base_ctx)
                    .map_err(|e| format_expand_err("env", k, &e))?;
                resolved_env.insert(k.clone(), expanded);
            }
            // Build a pane-scoped context that layers the resolved per-pane
            // env on top of .env.local + process env, used for the pane's
            // command/cwd/name/shell expansions.
            let pane_ctx = base_ctx.with_pane(resolved_env.clone());

            if let Some(cmd) = &section.command {
                let expanded = env_interp::expand(cmd, &pane_ctx)
                    .map_err(|e| format_expand_err("command", "", &e))?;
                launches.insert(*pid, PaneLaunch::Command(expanded));
            } else {
                launches.insert(*pid, PaneLaunch::Shell);
            }
            if let Some(cwd) = &section.cwd {
                let expanded = env_interp::expand(cwd, &pane_ctx)
                    .map_err(|e| format_expand_err("cwd", "", &e))?;
                let resolved = base_dir.join(expanded);
                cwds.insert(*pid, resolved);
            }
            if let Some(name) = &section.name {
                let expanded = env_interp::expand(name, &pane_ctx)
                    .map_err(|e| format_expand_err("name", "", &e))?;
                names.insert(*pid, expanded);
            }
            if !resolved_env.is_empty() {
                envs.insert(*pid, resolved_env);
            }
            if section.restart != RestartPolicy::Never {
                restarts.insert(*pid, section.restart.clone());
            }
            if let Some(shell) = &section.shell {
                let expanded = env_interp::expand(shell, &pane_ctx)
                    .map_err(|e| format_expand_err("shell", "", &e))?;
                shells.insert(*pid, expanded);
            }
            if let Some(flag) = section.persist_scrollback {
                persist_scrollback.insert(*pid, flag);
            }
        } else {
            launches.insert(*pid, PaneLaunch::Shell);
        }
    }

    // Validate project-level hooks. A bad event name or empty exec is a
    // hard error — the project config refused to start the daemon.
    let mut hooks = Vec::with_capacity(config.hooks.len());
    for (i, raw) in config.hooks.into_iter().enumerate() {
        let hook = Hook::from_raw(raw)
            .map_err(|e| format!("[[hooks]][{i}] in {}: {e}", path.display()))?;
        hooks.push(hook);
    }

    Ok(ResolvedProject {
        layout,
        launches,
        cwds,
        names,
        envs,
        restarts,
        shells,
        persist_scrollback,
        hooks,
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

/// Format an `ExpandError` with the offending field/key context. The error
/// message intentionally never contains resolved secret values — it relies
/// on `ExpandError`'s redaction-safe `Display` impl.
fn format_expand_err(field: &str, key: &str, err: &ExpandError) -> String {
    if key.is_empty() {
        format!("interpolation error in `{field}`: {err}")
    } else {
        format!("interpolation error in `{field}.{key}`: {err}")
    }
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

    // ─── persist_scrollback override (#69) ─────────────────────

    #[test]
    fn parse_persist_scrollback_override() {
        let toml_str = r#"
[[pane]]
command = "tail -F /var/log/syslog"
persist_scrollback = true

[[pane]]
command = "htop"
persist_scrollback = false

[[pane]]
command = "echo neutral"
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.pane[0].persist_scrollback, Some(true));
        assert_eq!(config.pane[1].persist_scrollback, Some(false));
        // Omitted means "fall back to global default".
        assert_eq!(config.pane[2].persist_scrollback, None);
    }

    #[test]
    fn resolved_persist_scrollback_only_records_explicit_overrides() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        isolate_secrets_dir(tmp.path());
        let toml_str = r#"
[workspace]
layout = "1:1"

[[pane]]
command = "echo a"
persist_scrollback = true

[[pane]]
command = "echo b"
"#;
        let path = write_project(tmp.path(), toml_str);
        let resolved = load_project_from(&path).expect("load");

        // Exactly one explicit override stored. The omitted pane is
        // absent from the map so the daemon can fall through to the
        // global flag without false `false` entries shadowing it.
        assert_eq!(resolved.persist_scrollback.len(), 1);
        assert_eq!(resolved.persist_scrollback.values().next(), Some(&true));

        std::env::remove_var("EZPN_TEST_SECRETS_DIR");
    }

    // --- env interpolation integration (issue #63) --------------------------

    use crate::pane::PaneLaunch;
    use std::sync::Mutex;

    /// Serialize tests that mutate `std::env`. `cargo test` runs tests in
    /// parallel by default, and `std::env::set_var` is process-global.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn write_project(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
        let path = dir.join(".ezpn.toml");
        std::fs::write(&path, body).unwrap();
        path
    }

    fn isolate_secrets_dir(dir: &std::path::Path) {
        // Prevents tests from picking up a real $XDG_RUNTIME_DIR/ezpn/secrets.toml.
        // Safe to set per-process; serialized by ENV_LOCK callers.
        std::env::set_var("EZPN_TEST_SECRETS_DIR", dir);
    }

    #[test]
    fn command_with_dollar_var_resolves() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        isolate_secrets_dir(tmp.path());
        std::env::set_var("EZPN_TEST_EDITOR_VAR", "nvim");

        let toml = r#"
[workspace]
layout = "1"

[[pane]]
command = "$EZPN_TEST_EDITOR_VAR ."
"#;
        let path = write_project(tmp.path(), toml);
        let resolved = load_project_from(&path).expect("load");

        let (_, launch) = resolved.launches.iter().next().expect("one pane");
        match launch {
            PaneLaunch::Command(cmd) => assert_eq!(cmd, "nvim ."),
            other => panic!("expected Command, got {other:?}"),
        }

        std::env::remove_var("EZPN_TEST_EDITOR_VAR");
        std::env::remove_var("EZPN_TEST_SECRETS_DIR");
    }

    #[test]
    fn command_with_default_uses_default_when_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        isolate_secrets_dir(tmp.path());
        std::env::remove_var("EZPN_TEST_PORT_VAR");

        let toml = r#"
[workspace]
layout = "1"

[[pane]]
command = "${EZPN_TEST_PORT_VAR:-3000}"
"#;
        let path = write_project(tmp.path(), toml);
        let resolved = load_project_from(&path).expect("load");

        let (_, launch) = resolved.launches.iter().next().unwrap();
        match launch {
            PaneLaunch::Command(cmd) => assert_eq!(cmd, "3000"),
            other => panic!("expected Command, got {other:?}"),
        }

        std::env::remove_var("EZPN_TEST_SECRETS_DIR");
    }

    #[test]
    fn dotenv_overrides_process_env_at_project_level() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        isolate_secrets_dir(tmp.path());
        std::env::set_var("EZPN_TEST_HOST_VAR", "from-process");
        std::fs::write(
            tmp.path().join(".env.local"),
            "EZPN_TEST_HOST_VAR=from-dotenv\n",
        )
        .unwrap();

        let toml = r#"
[workspace]
layout = "1"

[[pane]]
command = "${EZPN_TEST_HOST_VAR}"
"#;
        let path = write_project(tmp.path(), toml);
        let resolved = load_project_from(&path).expect("load");

        let (_, launch) = resolved.launches.iter().next().unwrap();
        match launch {
            PaneLaunch::Command(cmd) => assert_eq!(cmd, "from-dotenv"),
            other => panic!("expected Command, got {other:?}"),
        }

        std::env::remove_var("EZPN_TEST_HOST_VAR");
        std::env::remove_var("EZPN_TEST_SECRETS_DIR");
    }

    #[cfg(unix)]
    #[test]
    fn secret_ref_resolves_from_0600_secrets_file() {
        use std::os::unix::fs::PermissionsExt;
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let secrets_dir = tmp.path().join("ezpn");
        std::fs::create_dir_all(&secrets_dir).unwrap();
        let secrets_path = secrets_dir.join("secrets.toml");
        std::fs::write(&secrets_path, "DB_PASSWORD = \"hunter2\"\n").unwrap();
        std::fs::set_permissions(&secrets_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::env::set_var("EZPN_TEST_SECRETS_DIR", tmp.path());

        let toml = r#"
[workspace]
layout = "1"

[[pane]]
command = "psql --password=${secret:DB_PASSWORD}"
"#;
        let path = write_project(tmp.path(), toml);
        let resolved = load_project_from(&path).expect("load");

        let (_, launch) = resolved.launches.iter().next().unwrap();
        match launch {
            PaneLaunch::Command(cmd) => assert_eq!(cmd, "psql --password=hunter2"),
            other => panic!("expected Command, got {other:?}"),
        }

        std::env::remove_var("EZPN_TEST_SECRETS_DIR");
    }

    #[cfg(unix)]
    #[test]
    fn secret_file_with_mode_0644_refused_at_load() {
        use std::os::unix::fs::PermissionsExt;
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let secrets_dir = tmp.path().join("ezpn");
        std::fs::create_dir_all(&secrets_dir).unwrap();
        let secrets_path = secrets_dir.join("secrets.toml");
        std::fs::write(&secrets_path, "DB_PASSWORD = \"hunter2\"\n").unwrap();
        std::fs::set_permissions(&secrets_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::env::set_var("EZPN_TEST_SECRETS_DIR", tmp.path());

        let toml = r#"
[workspace]
layout = "1"

[[pane]]
command = "echo ok"
"#;
        let path = write_project(tmp.path(), toml);
        let err = match load_project_from(&path) {
            Ok(_) => panic!("must refuse 0644 secrets"),
            Err(e) => e,
        };
        assert!(
            err.contains("0600"),
            "error should mention required mode: {err}"
        );
        // The error must not embed any secret value.
        assert!(
            !err.contains("hunter2"),
            "secret value leaked into error: {err}"
        );

        std::env::remove_var("EZPN_TEST_SECRETS_DIR");
    }

    #[cfg(unix)]
    #[test]
    fn secrets_never_appear_in_resolved_project_debug() {
        // ResolvedProject does not derive Debug, but the secrets HashMap
        // we feed into EnvContext must not leak its values via Debug.
        use std::os::unix::fs::PermissionsExt;
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let secrets_dir = tmp.path().join("ezpn");
        std::fs::create_dir_all(&secrets_dir).unwrap();
        let secrets_path = secrets_dir.join("secrets.toml");
        std::fs::write(&secrets_path, "TOKEN = \"do-not-print-me\"\n").unwrap();
        std::fs::set_permissions(&secrets_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let secrets = crate::env_interp::load_secrets(&secrets_path).expect("load secrets");
        let dbg = format!("{:?}", secrets);
        assert!(
            !dbg.contains("do-not-print-me"),
            "secrets HashMap Debug leaked value: {dbg}"
        );
    }

    #[test]
    fn required_var_unset_errors_with_offending_name() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        isolate_secrets_dir(tmp.path());
        std::env::remove_var("EZPN_TEST_REQUIRED_VAR");

        let toml = r#"
[workspace]
layout = "1"

[[pane]]
command = "${EZPN_TEST_REQUIRED_VAR:?must be set}"
"#;
        let path = write_project(tmp.path(), toml);
        let err = match load_project_from(&path) {
            Ok(_) => panic!("must error on unset required var"),
            Err(e) => e,
        };
        assert!(
            err.contains("EZPN_TEST_REQUIRED_VAR"),
            "error missing var name: {err}"
        );
        assert!(err.contains("command"), "error missing field name: {err}");

        std::env::remove_var("EZPN_TEST_SECRETS_DIR");
    }

    #[test]
    fn per_pane_env_value_itself_is_expanded() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        isolate_secrets_dir(tmp.path());
        std::env::set_var("EZPN_TEST_BASE_URL", "https://api.example.com");

        let toml = r#"
[workspace]
layout = "1"

[[pane]]
command = "echo ${API_URL}"

[pane.env]
API_URL = "${EZPN_TEST_BASE_URL}/v1"
"#;
        let path = write_project(tmp.path(), toml);
        let resolved = load_project_from(&path).expect("load");

        // The pane's env value should have been expanded.
        let (_, env) = resolved.envs.iter().next().expect("one pane env map");
        assert_eq!(
            env.get("API_URL").map(|s| s.as_str()),
            Some("https://api.example.com/v1")
        );

        // And the command should pick up the per-pane env (highest precedence).
        let (_, launch) = resolved.launches.iter().next().unwrap();
        match launch {
            PaneLaunch::Command(cmd) => assert_eq!(cmd, "echo https://api.example.com/v1"),
            other => panic!("expected Command, got {other:?}"),
        }

        std::env::remove_var("EZPN_TEST_BASE_URL");
        std::env::remove_var("EZPN_TEST_SECRETS_DIR");
    }

    // ─── project-level [[hooks]] (#83) ─────────────────────────

    #[test]
    fn project_hooks_validated_on_load() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        isolate_secrets_dir(tmp.path());
        let toml_str = r#"
[[pane]]
command = "echo a"

[[pane]]
command = "echo b"

[[hooks]]
event = "after_pane_exit"
exec = ["true"]
"#;
        let path = write_project(tmp.path(), toml_str);
        let resolved = load_project_from(&path).expect("load");
        assert_eq!(resolved.hooks.len(), 1);
        assert_eq!(
            resolved.hooks[0].event,
            crate::hooks::HookEvent::AfterPaneExit
        );

        std::env::remove_var("EZPN_TEST_SECRETS_DIR");
    }

    #[test]
    fn project_hooks_unknown_event_refuses_load() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        isolate_secrets_dir(tmp.path());
        let toml_str = r#"
[[pane]]
command = "echo a"

[[hooks]]
event = "after_typo"
exec = ["true"]
"#;
        let path = write_project(tmp.path(), toml_str);
        let err = match load_project_from(&path) {
            Ok(_) => panic!("must reject unknown hook event"),
            Err(e) => e,
        };
        assert!(err.contains("unknown hook event"), "{err}");

        std::env::remove_var("EZPN_TEST_SECRETS_DIR");
    }
}
