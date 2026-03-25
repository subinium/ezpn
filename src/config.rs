use crate::render::BorderStyle;
use std::path::PathBuf;

/// Runtime config merged from: defaults < config file < CLI args.
pub struct EzpnConfig {
    pub border: BorderStyle,
    pub shell: String,
    pub scrollback: usize,
    pub show_status_bar: bool,
    pub show_tab_bar: bool,
    /// Prefix key character (default: 'b' for Ctrl+B).
    pub prefix_key: char,
}

impl Default for EzpnConfig {
    fn default() -> Self {
        Self {
            border: BorderStyle::Rounded,
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
            scrollback: 10_000,
            show_status_bar: true,
            show_tab_bar: true,
            prefix_key: 'b',
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
pub fn load_config() -> EzpnConfig {
    let mut config = EzpnConfig::default();
    if let Some(path) = config_path() {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            for line in contents.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    let key = key.trim();
                    let value = normalize_value(value);
                    match key {
                        "border" => {
                            if let Some(style) = BorderStyle::from_str(value) {
                                config.border = style;
                            }
                        }
                        "shell" => config.shell = value.to_string(),
                        "scrollback" => {
                            if let Ok(n) = value.parse::<usize>() {
                                config.scrollback = n.min(100_000);
                            }
                        }
                        "status_bar" => config.show_status_bar = value == "true",
                        "tab_bar" => config.show_tab_bar = value == "true",
                        "prefix" => {
                            // Accept single char like "a" or "b"
                            let ch = value.to_lowercase();
                            if let Some(c) = ch.chars().next() {
                                if c.is_ascii_lowercase() {
                                    config.prefix_key = c;
                                }
                            }
                        }
                        _ => {} // ignore unknown keys
                    }
                }
            }
        }
    }
    config
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

fn config_path() -> Option<PathBuf> {
    // Try XDG_CONFIG_HOME, then ~/.config
    let dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let mut home = dirs_fallback();
            home.push(".config");
            home
        });
    let path = dir.join("ezpn").join("config.toml");
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

fn dirs_fallback() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

#[cfg(test)]
mod tests {
    use super::normalize_value;

    #[test]
    fn normalize_value_trims_quotes() {
        assert_eq!(normalize_value("rounded"), "rounded");
        assert_eq!(normalize_value(" \"rounded\" "), "rounded");
        assert_eq!(normalize_value(" '/bin/zsh' "), "/bin/zsh");
    }
}
