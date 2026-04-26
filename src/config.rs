use crate::render::BorderStyle;
use crate::settings::Settings;
use std::path::PathBuf;

/// Runtime config merged from: defaults < config file < CLI args.
pub struct EzpnConfig {
    pub border: BorderStyle,
    pub shell: String,
    /// Default scrollback line count applied to every pane unless overridden.
    /// Mapped from either the flat `scrollback = N` (back-compat) or the new
    /// `[scrollback] default_lines = N` (SPEC 02).
    pub scrollback: usize,
    /// Hard ceiling enforced when applying per-pane overrides or runtime
    /// `set-scrollback` requests. Defaults to 100_000.
    pub scrollback_max_lines: usize,
    /// Estimated-bytes threshold above which the daemon emits a one-shot
    /// per-pane warning. Defaults to 50 MiB. Hint, not a hard cap.
    pub scrollback_warn_bytes: usize,
    pub show_status_bar: bool,
    pub show_tab_bar: bool,
    /// Prefix key character (default: 'b' for Ctrl+B).
    pub prefix_key: char,
    /// Whether to persist pane scrollback into auto-saved snapshots.
    /// Off by default (snapshots stay small); enable to restore terminal
    /// contents on reattach. May be overridden per-project via `.ezpn.toml`'s
    /// `[workspace] persist_scrollback`.
    pub persist_scrollback: bool,
    /// Theme name (resolved against `~/.config/ezpn/themes/<name>.toml`
    /// then the embedded built-in palettes).  Defaults to `"default"`.
    pub theme: String,
}

impl Default for EzpnConfig {
    fn default() -> Self {
        Self {
            border: BorderStyle::Rounded,
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
            scrollback: 10_000,
            scrollback_max_lines: 100_000,
            scrollback_warn_bytes: 50 * 1024 * 1024,
            show_status_bar: true,
            show_tab_bar: true,
            prefix_key: 'b',
            persist_scrollback: false,
            theme: "default".to_string(),
        }
    }
}

/// Load config from ~/.config/ezpn/config.toml (simple key=value, no toml dep).
/// Format:
///   border = rounded
///   shell = /bin/zsh
///   scrollback = 10000
///   status_bar = true
///   tab_bar = true
///   theme = tokyo-night
///
/// `[ui]` section headers are accepted but ignored — every key is global.
pub fn load_config() -> EzpnConfig {
    let mut config = EzpnConfig::default();
    if let Some(path) = existing_config_path() {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            parse_config_into(&contents, &mut config);
        }
    }
    config
}

fn parse_config_into(contents: &str, config: &mut EzpnConfig) {
    // Section state: `Some("scrollback")` while inside `[scrollback]`, etc.
    // Other section names (e.g. `[ui]`) are accepted but their keys are still
    // routed through the global key table — preserves legacy behaviour where
    // `[ui]\ntheme = "..."` worked.
    let mut section: Option<String> = None;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = Some(name.trim().to_string());
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = normalize_value(value);

        // `[scrollback]` table — per SPEC 02. Keys here populate the new
        // EzpnConfig fields; values are capped against `scrollback_max_lines`
        // when applied, not here, so users can lower the cap at runtime.
        if section.as_deref() == Some("scrollback") {
            match key {
                "default_lines" => {
                    if let Ok(n) = value.parse::<usize>() {
                        config.scrollback = n;
                    }
                }
                "max_lines" => {
                    if let Ok(n) = value.parse::<usize>() {
                        config.scrollback_max_lines = n;
                    }
                }
                "warn_bytes" => {
                    if let Ok(n) = value.parse::<usize>() {
                        config.scrollback_warn_bytes = n;
                    }
                }
                _ => {}
            }
            continue;
        }

        match key {
            "border" => {
                if let Some(style) = BorderStyle::from_str(value) {
                    config.border = style;
                }
            }
            "shell" => config.shell = value.to_string(),
            "scrollback" => {
                // Flat key — keep back-compat. Users mixing `scrollback = N`
                // with `[scrollback] default_lines = M` get last-write-wins.
                if let Ok(n) = value.parse::<usize>() {
                    config.scrollback = n;
                }
            }
            "status_bar" => config.show_status_bar = value == "true",
            "tab_bar" => config.show_tab_bar = value == "true",
            "persist_scrollback" => {
                config.persist_scrollback = value == "true";
            }
            "prefix" => {
                let ch = value.to_lowercase();
                if let Some(c) = ch.chars().next() {
                    if c.is_ascii_lowercase() {
                        config.prefix_key = c;
                    }
                }
            }
            "theme" if !value.is_empty() => {
                config.theme = value.to_string();
            }
            _ => {} // ignore unknown keys
        }
    }

    // Apply max-lines cap to whichever default value won. Do this once at the
    // end so the `[scrollback] max_lines` key can lower the effective default
    // even when listed after `default_lines`.
    if config.scrollback > config.scrollback_max_lines {
        config.scrollback = config.scrollback_max_lines;
    }
}

fn normalize_value(value: &str) -> &str {
    let value = value.trim();
    if value.len() >= 2 {
        let first = value.as_bytes()[0];
        let last = value.as_bytes()[value.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}

/// Resolve the config file path, regardless of whether it currently exists.
/// Used for both reads and writes.
pub fn config_path() -> anyhow::Result<PathBuf> {
    let dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let mut home = dirs_fallback();
            home.push(".config");
            home
        });
    Ok(dir.join("ezpn").join("config.toml"))
}

/// Convenience for callers that only care about an *existing* config.
pub fn existing_config_path() -> Option<PathBuf> {
    let p = config_path().ok()?;
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

/// User-friendly display path for the config file (with leading "~/" when
/// inside $HOME). Used by the settings panel header to show users where
/// their changes are saved.
pub fn display_config_path() -> String {
    let path = match config_path() {
        Ok(p) => p,
        Err(_) => return "~/.config/ezpn/config.toml".to_string(),
    };
    if let Ok(home) = std::env::var("HOME") {
        if let Ok(stripped) = path.strip_prefix(&home) {
            return format!("~/{}", stripped.display());
        }
    }
    path.display().to_string()
}

fn dirs_fallback() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

/// Serialize the live settings panel state to the same key=value format
/// `load_config` understands. Only the knobs the panel can change are
/// emitted; other keys (shell, scrollback, prefix) are not present here
/// since the panel does not expose them.
fn serialize_settings(s: &Settings) -> String {
    let border = match s.border_style {
        BorderStyle::Single => "single",
        BorderStyle::Rounded => "rounded",
        BorderStyle::Heavy => "heavy",
        BorderStyle::Double => "double",
        BorderStyle::None => "none",
    };
    let mut out = String::new();
    out.push_str("# Written by ezpn settings panel.\n");
    out.push_str("# Edit by hand or via Ctrl+B Shift+, — reload with Ctrl+B r.\n");
    out.push_str(&format!("border = {border}\n"));
    out.push_str(&format!("status_bar = {}\n", s.show_status_bar));
    out.push_str(&format!("tab_bar = {}\n", s.show_tab_bar));
    out
}

/// Persist settings panel state to `~/.config/ezpn/config.toml` using an
/// atomic write (tmp file + rename). Creates the parent directory if it
/// doesn't exist. The temp filename includes the current pid so concurrent
/// daemons don't clobber each other's tmp files.
pub fn save_settings(s: &Settings) -> anyhow::Result<()> {
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let pid = std::process::id();
    let tmp_name = format!(
        "{}.tmp.{pid}",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "config.toml".to_string())
    );
    let tmp = path.with_file_name(tmp_name);
    let contents = serialize_settings(s);
    if let Err(e) = std::fs::write(&tmp, contents) {
        // Best-effort cleanup; rename never happened.
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// Apply file-loaded config knobs to a live `Settings` value. Only the
/// fields the settings panel manages are touched.
pub fn apply_config_to_settings(cfg: &EzpnConfig, s: &mut Settings) {
    s.border_style = cfg.border;
    s.show_status_bar = cfg.show_status_bar;
    s.show_tab_bar = cfg.show_tab_bar;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::BorderStyle;
    use crate::settings::Settings;
    use std::sync::Mutex;

    /// Serializes tests that mutate the `XDG_CONFIG_HOME` env var so they
    /// don't race when cargo runs them in parallel within the same process.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn normalize_value_trims_quotes() {
        assert_eq!(normalize_value("rounded"), "rounded");
        assert_eq!(normalize_value(" \"rounded\" "), "rounded");
        assert_eq!(normalize_value(" '/bin/zsh' "), "/bin/zsh");
    }

    #[test]
    fn save_then_parse_roundtrips_panel_state() {
        let mut s = Settings::new(BorderStyle::Heavy);
        s.show_status_bar = false;
        s.show_tab_bar = true;

        let serialized = serialize_settings(&s);
        let mut cfg = EzpnConfig::default();
        parse_config_into(&serialized, &mut cfg);

        assert_eq!(cfg.border, BorderStyle::Heavy);
        assert!(!cfg.show_status_bar);
        assert!(cfg.show_tab_bar);
    }

    #[test]
    fn save_settings_writes_atomically_and_leaves_no_tmp() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmpdir = tempfile::tempdir().expect("tempdir");
        // Point XDG_CONFIG_HOME at our scratch dir for the duration of this test.
        let prev = std::env::var("XDG_CONFIG_HOME").ok();
        std::env::set_var("XDG_CONFIG_HOME", tmpdir.path());

        let mut s = Settings::new(BorderStyle::Double);
        s.show_status_bar = true;
        s.show_tab_bar = false;

        save_settings(&s).expect("save should succeed");

        let path = tmpdir.path().join("ezpn").join("config.toml");
        assert!(path.exists(), "config.toml should exist after save");

        // No leftover *.tmp.* siblings.
        let dir = path.parent().unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(dir)
            .expect("read_dir")
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("config.toml.tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "expected no .tmp.* files, found {leftovers:?}"
        );

        // Round-trip via the public loader.
        let loaded = std::fs::read_to_string(&path).expect("read");
        let mut cfg = EzpnConfig::default();
        parse_config_into(&loaded, &mut cfg);
        assert_eq!(cfg.border, BorderStyle::Double);
        assert!(cfg.show_status_bar);
        assert!(!cfg.show_tab_bar);

        // Restore env.
        match prev {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    #[test]
    fn flat_scrollback_back_compat() {
        let mut cfg = EzpnConfig::default();
        parse_config_into("scrollback = 7777\n", &mut cfg);
        assert_eq!(cfg.scrollback, 7777);
        assert_eq!(cfg.scrollback_max_lines, 100_000);
    }

    #[test]
    fn scrollback_section_parses_all_keys() {
        let mut cfg = EzpnConfig::default();
        parse_config_into(
            "[scrollback]\n\
             default_lines = 5000\n\
             max_lines = 50000\n\
             warn_bytes = 1048576\n",
            &mut cfg,
        );
        assert_eq!(cfg.scrollback, 5000);
        assert_eq!(cfg.scrollback_max_lines, 50_000);
        assert_eq!(cfg.scrollback_warn_bytes, 1_048_576);
    }

    #[test]
    fn scrollback_section_caps_default_above_max() {
        // default_lines 200_000 with max_lines 100_000 should clamp to 100_000.
        let mut cfg = EzpnConfig::default();
        parse_config_into(
            "[scrollback]\ndefault_lines = 200000\nmax_lines = 100000\n",
            &mut cfg,
        );
        assert_eq!(cfg.scrollback, 100_000);
        assert_eq!(cfg.scrollback_max_lines, 100_000);
    }

    #[test]
    fn unknown_section_does_not_swallow_global_keys() {
        // Legacy `[ui]` header followed by a global key should still parse.
        let mut cfg = EzpnConfig::default();
        parse_config_into("[ui]\nshell = /bin/zsh\n", &mut cfg);
        assert_eq!(cfg.shell, "/bin/zsh");
    }

    #[test]
    fn save_settings_creates_missing_parent_dir() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmpdir = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var("XDG_CONFIG_HOME").ok();
        std::env::set_var("XDG_CONFIG_HOME", tmpdir.path().join("does-not-exist-yet"));

        let s = Settings::new(BorderStyle::Single);
        save_settings(&s).expect("save should create parent dir");

        let path = tmpdir
            .path()
            .join("does-not-exist-yet")
            .join("ezpn")
            .join("config.toml");
        assert!(path.exists());

        match prev {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }
}
