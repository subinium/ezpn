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

use crate::layout::Layout;
use crate::pane::PaneLaunch;

/// Maximum recursion depth allowed when expanding `${...}` references in env values.
/// Hard cap to detect cycles like `A=${env:B}` + `B=${env:A}`.
pub const ENV_RESOLVE_MAX_DEPTH: u32 = 8;

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
    /// Directory containing `.ezpn.toml`. Used to resolve `${file:.env.local}`
    /// and to locate `.env.local` for auto-merge. Surfaced for `ezpn doctor`
    /// and snapshot restore — not read by current spawn path (envs are already
    /// resolved at load time), but kept on the public struct so callers can
    /// re-run [`resolve_env`] without re-parsing the TOML.
    #[allow(dead_code)]
    pub base_dir: PathBuf,
    /// Per-pane env resolution errors, surfaced by `ezpn doctor`.
    /// Empty when every reference in every pane resolved successfully.
    #[allow(dead_code)]
    pub env_errors: HashMap<usize, Vec<String>>,
}

/// Errors returned by [`resolve_env`].
#[derive(Debug)]
pub enum ResolveError {
    TooDeep,
    MissingRef(String),
    Io(String),
}

// Hand-rolled Display + Error so we don't pull in `thiserror` as a dep.
impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::TooDeep => write!(
                f,
                "env interpolation exceeded depth {} (cycle?)",
                ENV_RESOLVE_MAX_DEPTH
            ),
            ResolveError::MissingRef(s) => write!(f, "Missing reference: {s}"),
            ResolveError::Io(s) => write!(f, "{s}"),
        }
    }
}
impl std::error::Error for ResolveError {}

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
    let mut env_errors: HashMap<usize, Vec<String>> = HashMap::new();
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
            // Resolve `${...}` interpolation + merge `.env.local` (overrides).
            // On error, keep best-effort values (literal raw) and surface to doctor.
            match resolve_env(&base_dir, &section.env, 0) {
                Ok(resolved) => {
                    if !resolved.is_empty() {
                        envs.insert(*pid, resolved);
                    }
                }
                Err(e) => {
                    env_errors.entry(*pid).or_default().push(e.to_string());
                    if !section.env.is_empty() {
                        envs.insert(*pid, section.env.clone());
                    }
                }
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
        base_dir,
        env_errors,
    })
}

/// Resolve `${...}` references and merge `.env.local` into `raw`.
///
/// Supported reference forms:
///   - `${HOME}`              -> `std::env::var("HOME")`
///   - `${env:NODE_ENV}`      -> `std::env::var("NODE_ENV")`
///   - `${file:.env.local}`   -> dotenv-style lookup of the *current key* in that file
///   - `${secret:keychain:K}` -> macOS `security`, Linux `secret-tool`, then env fallback
///
/// `.env.local` (in `base_dir`) is merged AFTER per-pane resolution and **overrides**
/// `.ezpn.toml` literal values. Missing `.env.local` is not an error.
///
/// `depth` is the recursion counter; pass 0 for top-level callers. Cap is
/// [`ENV_RESOLVE_MAX_DEPTH`].
pub fn resolve_env(
    base_dir: &Path,
    raw: &HashMap<String, String>,
    depth: u32,
) -> Result<HashMap<String, String>, ResolveError> {
    if depth > ENV_RESOLVE_MAX_DEPTH {
        return Err(ResolveError::TooDeep);
    }
    let mut out = HashMap::with_capacity(raw.len());
    for (k, v) in raw {
        out.insert(k.clone(), expand_value(v, k, base_dir, depth + 1)?);
    }
    // Merge `.env.local` (highest precedence below `${secret:...}`).
    if let Some(local) = dotenv_load(&base_dir.join(".env.local"))? {
        for (k, v) in local {
            let resolved = expand_value(&v, &k, base_dir, depth + 1)?;
            out.insert(k, resolved);
        }
    }
    Ok(out)
}

/// Recursively expand `${...}` references in `value`. `current_key` is the
/// outer env key being resolved — needed for `${file:.env.local}` which
/// looks up the same key in the dotenv file.
fn expand_value(
    value: &str,
    current_key: &str,
    base_dir: &Path,
    depth: u32,
) -> Result<String, ResolveError> {
    if depth > ENV_RESOLVE_MAX_DEPTH {
        return Err(ResolveError::TooDeep);
    }
    let mut out = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Look for `${`
        if i + 1 < bytes.len() && bytes[i] == b'$' && bytes[i + 1] == b'{' {
            // Find matching `}` (no nested braces supported — keep grammar simple).
            if let Some(end_rel) = bytes[i + 2..].iter().position(|&b| b == b'}') {
                let inner = &value[i + 2..i + 2 + end_rel];
                let resolved = resolve_ref(inner, current_key, base_dir)?;
                // Recursively expand the resolved value (handles nested refs).
                let expanded = expand_value(&resolved, current_key, base_dir, depth + 1)?;
                out.push_str(&expanded);
                i += 2 + end_rel + 1;
                continue;
            }
            // Unterminated `${` — treat literally.
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    Ok(out)
}

/// Resolve a single `${...}` reference body (the part between braces).
/// Forms accepted:
///   `HOME`              -> env var lookup
///   `env:VAR`           -> env var lookup
///   `file:./relative`   -> dotenv lookup, key = `current_key`
///   `secret:keychain:K` -> OS keyring (macOS/linux) with env fallback
fn resolve_ref(body: &str, current_key: &str, base_dir: &Path) -> Result<String, ResolveError> {
    if let Some(rest) = body.strip_prefix("env:") {
        std::env::var(rest)
            .map_err(|_| ResolveError::MissingRef(format!("${{env:{rest}}} (env var unset)")))
    } else if let Some(rest) = body.strip_prefix("file:") {
        let file_path = base_dir.join(rest);
        let map = dotenv_load(&file_path)?.ok_or_else(|| {
            ResolveError::MissingRef(format!("${{file:{rest}}} (file not found)"))
        })?;
        map.get(current_key).cloned().ok_or_else(|| {
            ResolveError::MissingRef(format!(
                "${{file:{rest}}} (key '{current_key}' not in file)"
            ))
        })
    } else if let Some(rest) = body.strip_prefix("secret:") {
        // Form: `secret:<backend>:<KEY>`
        let mut parts = rest.splitn(2, ':');
        let backend = parts.next().unwrap_or("");
        let key = parts
            .next()
            .ok_or_else(|| ResolveError::MissingRef(format!("${{secret:{rest}}} (missing key)")))?;
        match backend {
            "keychain" => keychain_lookup(key)
                .ok_or_else(|| ResolveError::MissingRef(format!("${{secret:keychain:{key}}}"))),
            other => Err(ResolveError::MissingRef(format!(
                "${{secret:{other}:{key}}} (unknown backend)"
            ))),
        }
    } else {
        // Bare `${HOME}` form -> env var
        std::env::var(body)
            .map_err(|_| ResolveError::MissingRef(format!("${{{body}}} (env var unset)")))
    }
}

/// macOS: `security find-generic-password -s ezpn -a <key> -w` (500ms timeout).
/// Linux: `secret-tool lookup ezpn <key>`. On either, fall back to env var with a warn.
/// Returns `None` if the secret is not found anywhere.
fn keychain_lookup(key: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        if let Some(out) = run_with_timeout(
            "security",
            &["find-generic-password", "-s", "ezpn", "-a", key, "-w"],
            std::time::Duration::from_millis(500),
        ) {
            return Some(out);
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(out) = run_with_timeout(
            "secret-tool",
            &["lookup", "ezpn", key],
            std::time::Duration::from_millis(500),
        ) {
            return Some(out);
        }
    }
    // Fallback: process env (with warn).
    if let Ok(v) = std::env::var(key) {
        eprintln!("ezpn: secret '{key}' not found in OS keychain, falling back to ${{env:{key}}}");
        return Some(v);
    }
    None
}

/// Spawn a command, wait up to `timeout`, and return trimmed stdout on exit-0.
/// Returns `None` on timeout, non-zero exit, or any I/O error.
#[cfg(unix)]
fn run_with_timeout(program: &str, args: &[&str], timeout: std::time::Duration) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut buf = String::new();
                child.stdout.as_mut()?.read_to_string(&mut buf).ok()?;
                return Some(buf.trim_end_matches(['\n', '\r']).to_string());
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
}

#[cfg(not(unix))]
fn run_with_timeout(
    _program: &str,
    _args: &[&str],
    _timeout: std::time::Duration,
) -> Option<String> {
    None
}

/// Parse a dotenv-style file. Returns `Ok(None)` if the file does not exist.
/// Grammar (intentionally minimal — no shell expansion):
///   - `KEY=value`
///   - `KEY="quoted value"` (handles `\"`, `\\`, `\n`, `\t`, `\r`)
///   - `KEY='single-quoted'` (no escapes inside)
///   - `# comment` lines and trailing `# comment` after value
///   - blank lines
///   - leading whitespace on lines is trimmed; `export KEY=...` is accepted
pub fn dotenv_load(path: &Path) -> Result<Option<HashMap<String, String>>, ResolveError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ResolveError::Io(format!("read {}: {e}", path.display()))),
    };
    let mut out = HashMap::new();
    for raw_line in contents.lines() {
        let line = raw_line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, rest)) = line.split_once('=') else {
            continue; // malformed line — silently skip
        };
        let key = key.trim().to_string();
        if key.is_empty() {
            continue;
        }
        let value = parse_dotenv_value(rest);
        out.insert(key, value);
    }
    Ok(Some(out))
}

fn parse_dotenv_value(rest: &str) -> String {
    let s = rest.trim_start();
    if let Some(rest) = s.strip_prefix('"') {
        // Double-quoted: collect up to unescaped `"`, process backslash escapes.
        let mut out = String::with_capacity(rest.len());
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return out,
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => break,
                },
                _ => out.push(c),
            }
        }
        out
    } else if let Some(rest) = s.strip_prefix('\'') {
        // Single-quoted: literal until next `'`.
        match rest.find('\'') {
            Some(end) => rest[..end].to_string(),
            None => rest.to_string(),
        }
    } else {
        // Unquoted: stop at `#` (comment) or end-of-line; trim trailing whitespace.
        let end = s.find(" #").or_else(|| s.find('#')).unwrap_or(s.len());
        s[..end].trim_end().to_string()
    }
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

    // -------- env interpolation tests (.env.local, ${...}, depth, errors) --------

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn env_interpolation_bare_var() {
        // Use a known-set variable (PATH is always present on unix).
        let raw = map(&[("MY_PATH", "${PATH}")]);
        let dir = temp_dir();
        let out = resolve_env(dir.path(), &raw, 0).unwrap();
        let path = std::env::var("PATH").unwrap();
        assert_eq!(out.get("MY_PATH").map(|s| s.as_str()), Some(path.as_str()));
    }

    #[test]
    fn env_interpolation_env_prefix() {
        // SAFETY: process-level mutation in tests; key is unique to this test.
        unsafe {
            std::env::set_var("EZPN_TEST_VAR_X", "hello");
        }
        let raw = map(&[("FOO", "x=${env:EZPN_TEST_VAR_X}-end")]);
        let dir = temp_dir();
        let out = resolve_env(dir.path(), &raw, 0).unwrap();
        assert_eq!(out.get("FOO").map(|s| s.as_str()), Some("x=hello-end"));
    }

    #[test]
    fn env_interpolation_file_ref() {
        let dir = temp_dir();
        std::fs::write(dir.path().join(".env.shared"), "API_KEY=secret123\n").unwrap();
        let raw = map(&[("API_KEY", "${file:.env.shared}")]);
        let out = resolve_env(dir.path(), &raw, 0).unwrap();
        assert_eq!(out.get("API_KEY").map(|s| s.as_str()), Some("secret123"));
    }

    #[test]
    fn env_local_auto_merge_and_override() {
        let dir = temp_dir();
        // .env.local declares NODE_ENV (override) + adds DB_URL.
        std::fs::write(
            dir.path().join(".env.local"),
            "# local secrets\nNODE_ENV=production\nDB_URL=\"postgres://localhost/x\"\n",
        )
        .unwrap();
        let raw = map(&[("NODE_ENV", "development"), ("PORT", "3000")]);
        let out = resolve_env(dir.path(), &raw, 0).unwrap();
        assert_eq!(out.get("NODE_ENV").map(|s| s.as_str()), Some("production"));
        assert_eq!(out.get("PORT").map(|s| s.as_str()), Some("3000"));
        assert_eq!(
            out.get("DB_URL").map(|s| s.as_str()),
            Some("postgres://localhost/x")
        );
    }

    #[test]
    fn env_local_missing_is_silent() {
        let dir = temp_dir();
        let raw = map(&[("PORT", "3000")]);
        let out = resolve_env(dir.path(), &raw, 0).unwrap();
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn env_resolve_depth_cap_errors() {
        let dir = temp_dir();
        let raw = map(&[("A", "x")]);
        // Depth 9 is over the cap (8) -> immediate error.
        let err = resolve_env(dir.path(), &raw, 9).unwrap_err();
        assert!(matches!(err, ResolveError::TooDeep));
    }

    #[test]
    fn env_resolve_missing_ref_errors() {
        let dir = temp_dir();
        // Use a name unlikely to exist.
        let raw = map(&[("X", "${env:EZPN_DEFINITELY_NOT_SET_12345}")]);
        let err = resolve_env(dir.path(), &raw, 0).unwrap_err();
        assert!(matches!(err, ResolveError::MissingRef(_)));
    }

    #[test]
    fn env_resolve_cycle_via_env_local() {
        // .env.local: A=${env:LOOP_A} and process env has LOOP_A=${env:LOOP_A}
        // Since expand_value recurses on resolved values, this hits the depth cap.
        unsafe {
            std::env::set_var("EZPN_CYCLE_LOOP", "${env:EZPN_CYCLE_LOOP}");
        }
        let dir = temp_dir();
        let raw = map(&[("X", "${env:EZPN_CYCLE_LOOP}")]);
        let err = resolve_env(dir.path(), &raw, 0).unwrap_err();
        assert!(
            matches!(err, ResolveError::TooDeep),
            "expected TooDeep, got {err:?}"
        );
    }

    #[test]
    fn dotenv_parse_quoted_and_comments() {
        let dir = temp_dir();
        let body = r#"
# top comment
export PLAIN=raw
QUOTED="hello world"
ESCAPED="line1\nline2"
SINGLE='no $expand'
WITH_COMMENT=foo # trailing
"#;
        std::fs::write(dir.path().join(".env.local"), body).unwrap();
        let map = dotenv_load(&dir.path().join(".env.local"))
            .unwrap()
            .unwrap();
        assert_eq!(map.get("PLAIN").map(|s| s.as_str()), Some("raw"));
        assert_eq!(map.get("QUOTED").map(|s| s.as_str()), Some("hello world"));
        assert_eq!(map.get("ESCAPED").map(|s| s.as_str()), Some("line1\nline2"));
        assert_eq!(map.get("SINGLE").map(|s| s.as_str()), Some("no $expand"));
        assert_eq!(map.get("WITH_COMMENT").map(|s| s.as_str()), Some("foo"));
    }
}
