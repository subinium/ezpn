//! Environment variable interpolation for `.ezpn.toml` (issue #63).
//!
//! Supports four POSIX-style forms in any string field:
//! - `$VAR`              — bare reference; empty if unset
//! - `${VAR}`            — braced reference; empty if unset
//! - `${VAR:-default}`   — use `default` when `VAR` is unset OR empty
//! - `${VAR:?msg}`       — error with `msg` when `VAR` is unset OR empty
//!
//! Plus a secret-reference form:
//! - `${secret:KEY}`     — read from `$XDG_RUNTIME_DIR/ezpn/secrets.toml`
//!
//! Lookup precedence (highest first) when expanding:
//! 1. per-pane `env =` block
//! 2. `<project>/.env.local`
//! 3. `$XDG_RUNTIME_DIR/ezpn/secrets.toml`  (only via `${secret:..}`)
//! 4. process environment (`std::env::vars`)
//!
//! Secrets are wrapped in [`Redacted`] so they never leak through `Debug` or
//! `Display`. The module provides a [`SecretsFile`] loader that mandates a
//! `0600` permission mask on Unix; loading aborts on wider perms.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// A value that must never appear in `Debug` or `Display` output.
///
/// Wrapping secrets in this newtype is a compile-time-enforced redaction:
/// any field of type `Redacted<String>` will print as `***REDACTED***`,
/// regardless of whether the containing struct uses `#[derive(Debug)]`.
#[derive(Clone, PartialEq, Eq)]
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
    pub fn new(v: T) -> Self {
        Self(v)
    }

    /// Explicit, audit-friendly accessor. Call sites that use this take on
    /// the responsibility of not logging the result.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Redacted(***)")
    }
}

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***REDACTED***")
    }
}

/// Errors emitted during interpolation. `Display` is intentionally terse and
/// never embeds secret values.
#[derive(Debug)]
pub enum ExpandError {
    /// `${VAR:?msg}` triggered: variable unset or empty.
    Required { var: String, message: String },
    /// `${secret:KEY}` referenced an unknown key.
    UnknownSecret { key: String },
    /// Malformed `${...}` syntax (e.g. unterminated brace).
    Syntax(String),
}

impl fmt::Display for ExpandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExpandError::Required { var, message } => {
                write!(f, "required variable `{var}` is unset: {message}")
            }
            ExpandError::UnknownSecret { key } => {
                write!(f, "unknown secret `{key}` referenced via ${{secret:..}}")
            }
            ExpandError::Syntax(msg) => write!(f, "interpolation syntax error: {msg}"),
        }
    }
}

impl std::error::Error for ExpandError {}

/// Errors emitted while loading the secrets file.
#[derive(Debug)]
pub enum SecretsLoadError {
    /// File exists but its mode is wider than 0600.
    InsecurePermissions { path: PathBuf, mode: u32 },
    /// I/O error reading the secrets file.
    Io { path: PathBuf, error: String },
    /// TOML parse error.
    Parse { path: PathBuf, error: String },
}

impl fmt::Display for SecretsLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SecretsLoadError::InsecurePermissions { path, mode } => write!(
                f,
                "refusing to load secrets file {} with insecure mode {:o} (require 0600)",
                path.display(),
                mode & 0o777
            ),
            SecretsLoadError::Io { path, error } => {
                write!(f, "cannot read secrets file {}: {error}", path.display())
            }
            SecretsLoadError::Parse { path, error } => {
                write!(f, "parse error in secrets file {}: {error}", path.display())
            }
        }
    }
}

impl std::error::Error for SecretsLoadError {}

/// Lookup context for [`expand`]. Build once per `.ezpn.toml` parse; the
/// per-pane override map can be swapped per pane via [`Self::with_pane`].
#[derive(Default, Debug)]
pub struct EnvContext {
    /// Per-pane `env =` overrides. Highest priority.
    per_pane: HashMap<String, String>,
    /// Parsed `<project>/.env.local`.
    dotenv: HashMap<String, String>,
    /// Loaded `$XDG_RUNTIME_DIR/ezpn/secrets.toml`. Only consulted via
    /// `${secret:KEY}`, never via `$KEY` / `${KEY}`.
    secrets: HashMap<String, Redacted<String>>,
    /// Process environment snapshot (`std::env::vars`).
    process: HashMap<String, String>,
}

impl EnvContext {
    /// Build a context layered as:
    /// `.env.local` over process env, plus the loaded secrets map.
    pub fn build(
        dotenv: HashMap<String, String>,
        secrets: HashMap<String, Redacted<String>>,
    ) -> Self {
        let process = std::env::vars().collect();
        Self {
            per_pane: HashMap::new(),
            dotenv,
            secrets,
            process,
        }
    }

    /// Build a context using an explicit process-env snapshot (for tests).
    pub fn build_with_process(
        dotenv: HashMap<String, String>,
        secrets: HashMap<String, Redacted<String>>,
        process: HashMap<String, String>,
    ) -> Self {
        Self {
            per_pane: HashMap::new(),
            dotenv,
            secrets,
            process,
        }
    }

    /// Return a clone of `self` with the given per-pane overrides.
    pub fn with_pane(&self, per_pane: HashMap<String, String>) -> Self {
        Self {
            per_pane,
            dotenv: self.dotenv.clone(),
            secrets: self.secrets.clone(),
            process: self.process.clone(),
        }
    }

    /// Resolve `VAR` against per-pane → .env.local → process env.
    /// Secrets are NOT consulted here; use `${secret:KEY}` to opt in.
    fn lookup_plain(&self, name: &str) -> Option<&str> {
        if let Some(v) = self.per_pane.get(name) {
            return Some(v.as_str());
        }
        if let Some(v) = self.dotenv.get(name) {
            return Some(v.as_str());
        }
        self.process.get(name).map(|s| s.as_str())
    }

    fn lookup_secret(&self, key: &str) -> Option<&str> {
        self.secrets.get(key).map(|r| r.expose().as_str())
    }
}

/// Parse `KEY=VALUE` lines (POSIX-ish; supports `#` comments, leading
/// `export `, single/double quoted values). Empty lines and lines without
/// `=` are skipped silently.
pub fn parse_dotenv(contents: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim().to_string();
        if key.is_empty() {
            continue;
        }
        let v = v.trim();
        let value = if (v.starts_with('"') && v.ends_with('"') && v.len() >= 2)
            || (v.starts_with('\'') && v.ends_with('\'') && v.len() >= 2)
        {
            v[1..v.len() - 1].to_string()
        } else {
            v.to_string()
        };
        out.insert(key, value);
    }
    out
}

/// Load `<project>/.env.local` if present. Returns an empty map if the file
/// does not exist; surfaces I/O errors otherwise.
pub fn load_dotenv(project_dir: &Path) -> Result<HashMap<String, String>, String> {
    let path = project_dir.join(".env.local");
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let contents = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(parse_dotenv(&contents))
}

/// Resolved location of the secrets file: `$XDG_RUNTIME_DIR/ezpn/secrets.toml`.
/// Falls back to `/tmp/ezpn/secrets.toml` (matching the rest of the codebase)
/// when `XDG_RUNTIME_DIR` is unset.
pub fn default_secrets_path() -> PathBuf {
    let dir = std::env::var("EZPN_TEST_SECRETS_DIR")
        .or_else(|_| std::env::var("XDG_RUNTIME_DIR"))
        .unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(dir).join("ezpn").join("secrets.toml")
}

/// Load secrets from the given path. Returns an empty map if the file does
/// not exist (secrets are optional). On Unix, refuses to load if the file's
/// mode is wider than `0600`.
pub fn load_secrets(path: &Path) -> Result<HashMap<String, Redacted<String>>, SecretsLoadError> {
    if !path.exists() {
        return Ok(HashMap::new());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).map_err(|e| SecretsLoadError::Io {
            path: path.to_path_buf(),
            error: e.to_string(),
        })?;
        let mode = meta.mode() & 0o777;
        if mode != 0o600 {
            return Err(SecretsLoadError::InsecurePermissions {
                path: path.to_path_buf(),
                mode,
            });
        }
    }

    let contents = std::fs::read_to_string(path).map_err(|e| SecretsLoadError::Io {
        path: path.to_path_buf(),
        error: e.to_string(),
    })?;
    let table: toml::Table = toml::from_str(&contents).map_err(|e| SecretsLoadError::Parse {
        path: path.to_path_buf(),
        error: e.to_string(),
    })?;

    let mut out = HashMap::new();
    for (k, v) in table {
        if let toml::Value::String(s) = v {
            out.insert(k, Redacted::new(s));
        }
        // Non-string entries are skipped silently; only flat string maps are
        // valid for the current spec.
    }
    Ok(out)
}

/// Expand `$VAR`, `${VAR}`, `${VAR:-default}`, `${VAR:?msg}`, and
/// `${secret:KEY}` in `template` against `ctx`.
///
/// Errors carry the offending variable name so the caller can produce
/// human-readable diagnostics.
pub fn expand(template: &str, ctx: &EnvContext) -> Result<String, ExpandError> {
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];

        // Escape: `$$` -> literal `$`.
        if b == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'$' {
            out.push('$');
            i += 2;
            continue;
        }

        if b == b'$' {
            // `${...}` form (including `${secret:KEY}`).
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                let close = find_matching_brace(bytes, i + 1)?;
                let inner = &template[i + 2..close];
                let resolved = expand_braced(inner, ctx)?;
                out.push_str(&resolved);
                i = close + 1;
                continue;
            }

            // `$VAR` form (no braces).
            let start = i + 1;
            let end = scan_identifier_end(bytes, start);
            if end > start {
                let name = &template[start..end];
                if let Some(v) = ctx.lookup_plain(name) {
                    out.push_str(v);
                }
                // Unset bare `$VAR` expands to empty (POSIX behavior).
                i = end;
                continue;
            }

            // `$` not followed by an identifier: emit literal `$`.
            out.push('$');
            i += 1;
            continue;
        }

        out.push(b as char);
        i += 1;
    }

    Ok(out)
}

/// Find the index of the `}` matching the `{` at `bytes[open_idx]`.
fn find_matching_brace(bytes: &[u8], open_idx: usize) -> Result<usize, ExpandError> {
    debug_assert_eq!(bytes[open_idx], b'{');
    let mut j = open_idx + 1;
    while j < bytes.len() {
        if bytes[j] == b'}' {
            return Ok(j);
        }
        j += 1;
    }
    Err(ExpandError::Syntax(format!(
        "unterminated `${{` starting at byte {open_idx}"
    )))
}

/// Scan a POSIX-ish env-var identifier `[A-Za-z_][A-Za-z0-9_]*`.
fn scan_identifier_end(bytes: &[u8], start: usize) -> usize {
    if start >= bytes.len() {
        return start;
    }
    let first = bytes[start];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return start;
    }
    let mut j = start + 1;
    while j < bytes.len() {
        let c = bytes[j];
        if c.is_ascii_alphanumeric() || c == b'_' {
            j += 1;
        } else {
            break;
        }
    }
    j
}

/// Resolve the contents of `${...}`. Handles plain names, `:-default`,
/// `:?error`, and the `secret:KEY` prefix.
fn expand_braced(inner: &str, ctx: &EnvContext) -> Result<String, ExpandError> {
    if let Some(key) = inner.strip_prefix("secret:") {
        let key = key.trim();
        if key.is_empty() {
            return Err(ExpandError::Syntax(
                "empty key in `${secret:..}`".to_string(),
            ));
        }
        return ctx
            .lookup_secret(key)
            .map(|s| s.to_string())
            .ok_or_else(|| ExpandError::UnknownSecret {
                key: key.to_string(),
            });
    }

    // `${VAR:?msg}` — required (unset OR empty triggers).
    if let Some(idx) = inner.find(":?") {
        let name = &inner[..idx];
        let msg = &inner[idx + 2..];
        validate_name(name)?;
        return match ctx.lookup_plain(name) {
            Some(v) if !v.is_empty() => Ok(v.to_string()),
            _ => Err(ExpandError::Required {
                var: name.to_string(),
                message: msg.to_string(),
            }),
        };
    }

    // `${VAR:-default}` — default on unset OR empty.
    if let Some(idx) = inner.find(":-") {
        let name = &inner[..idx];
        let default = &inner[idx + 2..];
        validate_name(name)?;
        return match ctx.lookup_plain(name) {
            Some(v) if !v.is_empty() => Ok(v.to_string()),
            _ => Ok(default.to_string()),
        };
    }

    // `${VAR}` — empty when unset.
    validate_name(inner)?;
    Ok(ctx.lookup_plain(inner).unwrap_or("").to_string())
}

fn validate_name(name: &str) -> Result<(), ExpandError> {
    if name.is_empty() {
        return Err(ExpandError::Syntax("empty variable name in `${}`".into()));
    }
    let bytes = name.as_bytes();
    let first = bytes[0];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return Err(ExpandError::Syntax(format!(
            "invalid variable name `{name}`"
        )));
    }
    for &c in &bytes[1..] {
        if !(c.is_ascii_alphanumeric() || c == b'_') {
            return Err(ExpandError::Syntax(format!(
                "invalid variable name `{name}`"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with(process: &[(&str, &str)]) -> EnvContext {
        let process: HashMap<String, String> = process
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        EnvContext::build_with_process(HashMap::new(), HashMap::new(), process)
    }

    // --- $VAR / ${VAR} -------------------------------------------------------

    #[test]
    fn bare_dollar_var_set() {
        let ctx = ctx_with(&[("EDITOR", "nvim")]);
        assert_eq!(expand("$EDITOR .", &ctx).unwrap(), "nvim .");
    }

    #[test]
    fn bare_dollar_var_unset_expands_empty() {
        let ctx = ctx_with(&[]);
        assert_eq!(expand("[$MISSING]", &ctx).unwrap(), "[]");
    }

    #[test]
    fn braced_var_set() {
        let ctx = ctx_with(&[("PORT", "9000")]);
        assert_eq!(expand("port=${PORT}", &ctx).unwrap(), "port=9000");
    }

    #[test]
    fn braced_var_unset_expands_empty() {
        let ctx = ctx_with(&[]);
        assert_eq!(expand("[${MISSING}]", &ctx).unwrap(), "[]");
    }

    // --- ${VAR:-default} -----------------------------------------------------

    #[test]
    fn default_used_when_unset() {
        let ctx = ctx_with(&[]);
        assert_eq!(expand("${PORT:-3000}", &ctx).unwrap(), "3000");
    }

    #[test]
    fn default_used_when_empty() {
        let ctx = ctx_with(&[("PORT", "")]);
        assert_eq!(expand("${PORT:-3000}", &ctx).unwrap(), "3000");
    }

    #[test]
    fn default_skipped_when_set() {
        let ctx = ctx_with(&[("PORT", "8080")]);
        assert_eq!(expand("${PORT:-3000}", &ctx).unwrap(), "8080");
    }

    // --- ${VAR:?msg} ---------------------------------------------------------

    #[test]
    fn required_errors_when_unset() {
        let ctx = ctx_with(&[]);
        let err = expand("${API_KEY:?must be set}", &ctx).unwrap_err();
        match err {
            ExpandError::Required { var, message } => {
                assert_eq!(var, "API_KEY");
                assert_eq!(message, "must be set");
            }
            other => panic!("expected Required, got {other:?}"),
        }
    }

    #[test]
    fn required_errors_when_empty() {
        let ctx = ctx_with(&[("API_KEY", "")]);
        let err = expand("${API_KEY:?must be set}", &ctx).unwrap_err();
        assert!(matches!(err, ExpandError::Required { .. }));
    }

    #[test]
    fn required_passes_when_set() {
        let ctx = ctx_with(&[("API_KEY", "abc")]);
        assert_eq!(expand("${API_KEY:?must be set}", &ctx).unwrap(), "abc");
    }

    // --- $$ escape -----------------------------------------------------------

    #[test]
    fn dollar_dollar_is_literal() {
        let ctx = ctx_with(&[]);
        assert_eq!(expand("price: $$5", &ctx).unwrap(), "price: $5");
    }

    // --- precedence ----------------------------------------------------------

    #[test]
    fn dotenv_overrides_process_env() {
        let process: HashMap<String, String> = [("PORT".to_string(), "9999".to_string())]
            .into_iter()
            .collect();
        let dotenv: HashMap<String, String> = [("PORT".to_string(), "3000".to_string())]
            .into_iter()
            .collect();
        let ctx = EnvContext::build_with_process(dotenv, HashMap::new(), process);
        assert_eq!(expand("${PORT}", &ctx).unwrap(), "3000");
    }

    #[test]
    fn per_pane_overrides_dotenv_and_process() {
        let process: HashMap<String, String> = [("HOST".to_string(), "p".to_string())]
            .into_iter()
            .collect();
        let dotenv: HashMap<String, String> = [("HOST".to_string(), "d".to_string())]
            .into_iter()
            .collect();
        let base = EnvContext::build_with_process(dotenv, HashMap::new(), process);
        let per: HashMap<String, String> = [("HOST".to_string(), "pane".to_string())]
            .into_iter()
            .collect();
        let ctx = base.with_pane(per);
        assert_eq!(expand("${HOST}", &ctx).unwrap(), "pane");
    }

    // --- secrets -------------------------------------------------------------

    #[test]
    fn secret_resolves_from_map() {
        let secrets: HashMap<String, Redacted<String>> = [(
            "DB_PASSWORD".to_string(),
            Redacted::new("hunter2".to_string()),
        )]
        .into_iter()
        .collect();
        let ctx = EnvContext::build_with_process(HashMap::new(), secrets, HashMap::new());
        assert_eq!(expand("${secret:DB_PASSWORD}", &ctx).unwrap(), "hunter2");
    }

    #[test]
    fn secret_unknown_errors() {
        let ctx = ctx_with(&[]);
        let err = expand("${secret:NOPE}", &ctx).unwrap_err();
        assert!(matches!(err, ExpandError::UnknownSecret { ref key } if key == "NOPE"));
    }

    #[test]
    fn secret_not_resolved_via_plain_var() {
        // `${secret:KEY}` must be the only path to secrets.
        let secrets: HashMap<String, Redacted<String>> =
            [("API".to_string(), Redacted::new("abc".to_string()))]
                .into_iter()
                .collect();
        let ctx = EnvContext::build_with_process(HashMap::new(), secrets, HashMap::new());
        // Plain `${API}` does not see secrets — expands to empty.
        assert_eq!(expand("${API}", &ctx).unwrap(), "");
    }

    // --- secrets file loading & permissions ---------------------------------

    #[cfg(unix)]
    #[test]
    fn load_secrets_with_mode_0600_succeeds() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.toml");
        std::fs::write(&path, "DB_PASSWORD = \"hunter2\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let secrets = load_secrets(&path).expect("load");
        assert_eq!(secrets.get("DB_PASSWORD").unwrap().expose(), "hunter2");

        let ctx = EnvContext::build_with_process(HashMap::new(), secrets, HashMap::new());
        assert_eq!(expand("${secret:DB_PASSWORD}", &ctx).unwrap(), "hunter2");
    }

    #[cfg(unix)]
    #[test]
    fn load_secrets_with_mode_0644_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.toml");
        std::fs::write(&path, "DB_PASSWORD = \"hunter2\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let err = load_secrets(&path).expect_err("must refuse wider perms");
        match err {
            SecretsLoadError::InsecurePermissions { mode, .. } => {
                assert_eq!(mode & 0o777, 0o644);
            }
            other => panic!("expected InsecurePermissions, got {other}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn load_secrets_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let secrets = load_secrets(&path).expect("missing file is not an error");
        assert!(secrets.is_empty());
    }

    // --- redaction -----------------------------------------------------------

    #[test]
    fn redacted_debug_does_not_leak() {
        let r = Redacted::new("hunter2".to_string());
        let dbg = format!("{:?}", r);
        let disp = format!("{}", r);
        assert!(!dbg.contains("hunter2"), "Debug leaked: {dbg}");
        assert!(!disp.contains("hunter2"), "Display leaked: {disp}");
    }

    #[test]
    fn env_context_debug_does_not_leak_secrets() {
        let secrets: HashMap<String, Redacted<String>> = [(
            "DB_PASSWORD".to_string(),
            Redacted::new("supersecret-token-xyz".to_string()),
        )]
        .into_iter()
        .collect();
        let ctx = EnvContext::build_with_process(HashMap::new(), secrets, HashMap::new());
        let dbg = format!("{:?}", ctx);
        assert!(
            !dbg.contains("supersecret-token-xyz"),
            "EnvContext Debug leaked secret: {dbg}"
        );
    }

    #[test]
    fn expand_error_does_not_embed_value() {
        // Confirms that even when a secret lookup fails, the error message
        // does not include any value (only the key name).
        let secrets: HashMap<String, Redacted<String>> = [(
            "PRESENT".to_string(),
            Redacted::new("real-secret".to_string()),
        )]
        .into_iter()
        .collect();
        let ctx = EnvContext::build_with_process(HashMap::new(), secrets, HashMap::new());
        let err = expand("${secret:MISSING}", &ctx).unwrap_err();
        let s = format!("{err}");
        assert!(s.contains("MISSING"));
        assert!(!s.contains("real-secret"));
    }

    // --- dotenv parsing ------------------------------------------------------

    #[test]
    fn parse_dotenv_basic() {
        let m = parse_dotenv("FOO=bar\n# comment\nBAZ=\"quoted\"\nexport QUX='single'\n\n");
        assert_eq!(m.get("FOO"), Some(&"bar".to_string()));
        assert_eq!(m.get("BAZ"), Some(&"quoted".to_string()));
        assert_eq!(m.get("QUX"), Some(&"single".to_string()));
    }

    #[test]
    fn parse_dotenv_skips_malformed_lines() {
        let m = parse_dotenv("not an assignment\n=novalue\nGOOD=ok\n");
        assert_eq!(m.len(), 1);
        assert_eq!(m.get("GOOD"), Some(&"ok".to_string()));
    }

    // --- syntax errors -------------------------------------------------------

    #[test]
    fn unterminated_brace_errors() {
        let ctx = ctx_with(&[]);
        let err = expand("${UNTERMINATED", &ctx).unwrap_err();
        assert!(matches!(err, ExpandError::Syntax(_)));
    }

    #[test]
    fn invalid_name_errors() {
        let ctx = ctx_with(&[]);
        let err = expand("${1BAD}", &ctx).unwrap_err();
        assert!(matches!(err, ExpandError::Syntax(_)));
    }

    #[test]
    fn empty_secret_key_errors() {
        let ctx = ctx_with(&[]);
        let err = expand("${secret:}", &ctx).unwrap_err();
        assert!(matches!(err, ExpandError::Syntax(_)));
    }

    // --- complex strings -----------------------------------------------------

    #[test]
    fn mixed_template() {
        let ctx = ctx_with(&[("USER", "alice"), ("HOME", "/home/alice")]);
        assert_eq!(
            expand("${USER}@host:$HOME/${SUBDIR:-projects}", &ctx).unwrap(),
            "alice@host:/home/alice/projects"
        );
    }

    #[test]
    fn lone_dollar_at_end_is_literal() {
        let ctx = ctx_with(&[]);
        assert_eq!(expand("price: $", &ctx).unwrap(), "price: $");
    }

    #[test]
    fn dollar_followed_by_non_ident_is_literal() {
        let ctx = ctx_with(&[]);
        assert_eq!(expand("a $1 b", &ctx).unwrap(), "a $1 b");
    }
}
