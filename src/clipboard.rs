//! System-clipboard fallback chain (#92).
//!
//! v0.5 only emitted OSC 52 — many host emulators silently drop those
//! sequences (especially on Wayland). This module detects an in-process
//! `copy_command` to spawn instead and exposes a small façade
//! ([`copy`]) that the yank path calls. When no tool is available the
//! caller is expected to fall back to OSC 52 (current behaviour).
//!
//! Detection order, first available wins:
//!   1. `$WAYLAND_DISPLAY` set + `wl-copy` on `PATH` → `wl-copy`.
//!   2. `$DISPLAY` set + `xclip` on `PATH` → `xclip -selection clipboard`.
//!   3. `$DISPLAY` set + `xsel` on `PATH` → `xsel --clipboard --input`.
//!   4. `cfg(target_os = "macos")` (or `uname -s == Darwin`) → `pbcopy`.
//!   5. None — caller falls back to OSC 52.
//!
//! User overrides come from `[clipboard]` in the config file:
//!
//! ```toml
//! [clipboard]
//! copy_command  = ["wl-copy"]
//! paste_command = ["wl-paste"]
//! ```
//!
//! An explicit empty array means "auto-detect" (same as omitting the
//! key); any non-empty array bypasses detection. This module performs
//! NO PATH lookup on user-supplied overrides — if the user said
//! `["my-tool"]`, that's what we run.
//!
//! ## Daemon vs client trap
//! Detection runs in the **daemon** process. When the daemon was
//! launched over SSH, environment variables and PATH refer to the
//! remote machine, so the clipboard exec lands in the SSH server's
//! clipboard — almost never what the user wanted. The per-attach
//! `--clipboard-mode local|daemon` flag is the eventual mitigation;
//! see `docs/clipboard.md` (TODO).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// A resolved clipboard command: program path + leading argv tail.
///
/// We split the command into `program` + `args` so the spawn site uses
/// `Command::new(program).args(args)` directly — no shell escape, no
/// quoting bugs. Empty `args` is fine (e.g., `pbcopy`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyCommand {
    pub program: String,
    pub args: Vec<String>,
    /// Provenance for logging. `Detected(name)` for the auto-chain,
    /// `Override` for a user-supplied `[clipboard] copy_command`.
    pub source: CopySource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopySource {
    /// Detected by the runtime fallback chain. The string is the
    /// human-readable label (`wl-copy`, `xclip`, `xsel`, `pbcopy`).
    Detected(&'static str),
    /// User-supplied via `[clipboard] copy_command = [...]`.
    Override,
}

impl CopyCommand {
    fn new_detected(program: &str, args: &[&str], label: &'static str) -> Self {
        Self {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            source: CopySource::Detected(label),
        }
    }

    fn new_override(program: String, args: Vec<String>) -> Self {
        Self {
            program,
            args,
            source: CopySource::Override,
        }
    }

    /// Human-readable description for logs.
    pub fn label(&self) -> String {
        match &self.source {
            CopySource::Detected(name) => (*name).to_string(),
            CopySource::Override => format!("override({})", self.program),
        }
    }
}

/// Resolve the active copy command. `override_argv` is the parsed
/// `[clipboard] copy_command = [...]` from the config (None or empty =
/// auto-detect).
///
/// Detection result is **cached for the daemon process lifetime** —
/// re-resolving on every yank would fork three child processes for the
/// PATH lookup. The cache is keyed on the override slice so calls with
/// `None`/empty share a single slot.
pub fn resolve(override_argv: Option<&[String]>) -> Option<CopyCommand> {
    if let Some(argv) = override_argv {
        if !argv.is_empty() {
            // No PATH check — trust user. Empty program is rejected.
            let mut iter = argv.iter().cloned();
            let program = iter.next()?;
            if program.is_empty() {
                return None;
            }
            let args: Vec<String> = iter.collect();
            return Some(CopyCommand::new_override(program, args));
        }
    }

    static AUTO: OnceLock<Option<CopyCommand>> = OnceLock::new();
    AUTO.get_or_init(detect_chain).clone()
}

/// Run the detection chain once. Visible for tests via the env-driven
/// hooks below.
fn detect_chain() -> Option<CopyCommand> {
    if env_set("WAYLAND_DISPLAY") && which("wl-copy").is_some() {
        return Some(CopyCommand::new_detected("wl-copy", &[], "wl-copy"));
    }
    if env_set("DISPLAY") {
        if which("xclip").is_some() {
            return Some(CopyCommand::new_detected(
                "xclip",
                &["-selection", "clipboard"],
                "xclip",
            ));
        }
        if which("xsel").is_some() {
            return Some(CopyCommand::new_detected(
                "xsel",
                &["--clipboard", "--input"],
                "xsel",
            ));
        }
    }
    if cfg!(target_os = "macos") && which("pbcopy").is_some() {
        return Some(CopyCommand::new_detected("pbcopy", &[], "pbcopy"));
    }
    None
}

fn env_set(name: &str) -> bool {
    std::env::var_os(name)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// Lightweight `which` — walks `PATH` and returns the first executable
/// match. Avoids depending on the `which` crate for one call site.
fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(program);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

/// Pipe `text` into the resolved copy command. Returns the command
/// label on success so the caller can log it (typically once per
/// session; the caller is responsible for dedup).
///
/// On any spawn / pipe error we return `Err` and the caller is expected
/// to fall back to OSC 52. Errors here are non-fatal.
pub fn copy(text: &str, override_argv: Option<&[String]>) -> Result<String, ClipboardError> {
    let cmd = resolve(override_argv).ok_or(ClipboardError::NoCommand)?;
    let label = cmd.label();
    let mut child = Command::new(&cmd.program)
        .args(&cmd.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| ClipboardError::Spawn {
            program: cmd.program.clone(),
            source: e,
        })?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or(ClipboardError::StdinUnavailable)?;
        stdin
            .write_all(text.as_bytes())
            .map_err(ClipboardError::Write)?;
    }
    let status = child.wait().map_err(ClipboardError::Wait)?;
    if !status.success() {
        return Err(ClipboardError::ExitStatus {
            program: cmd.program.clone(),
            code: status.code(),
        });
    }
    Ok(label)
}

/// Errors surfaced by [`copy`]. All variants are non-fatal — the
/// caller falls back to OSC 52.
#[derive(Debug)]
pub enum ClipboardError {
    /// Detection chain found nothing AND the user did not override.
    NoCommand,
    /// `Command::spawn` failed (program missing, permission denied …).
    Spawn {
        program: String,
        source: std::io::Error,
    },
    /// `child.stdin` was `None` after spawn — should never happen.
    StdinUnavailable,
    /// Writing the payload into the child's stdin failed.
    Write(std::io::Error),
    /// `child.wait` failed.
    Wait(std::io::Error),
    /// Child exited non-zero.
    ExitStatus { program: String, code: Option<i32> },
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCommand => write!(f, "no clipboard command available"),
            Self::Spawn { program, source } => {
                write!(f, "failed to spawn {program}: {source}")
            }
            Self::StdinUnavailable => write!(f, "child stdin unavailable"),
            Self::Write(e) => write!(f, "failed to write to clipboard: {e}"),
            Self::Wait(e) => write!(f, "wait on clipboard child failed: {e}"),
            Self::ExitStatus { program, code } => match code {
                Some(c) => write!(f, "{program} exited with {c}"),
                None => write!(f, "{program} terminated by signal"),
            },
        }
    }
}

impl std::error::Error for ClipboardError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_with_program_and_args_is_used_verbatim() {
        let argv = vec!["my-tool".to_string(), "--clip".to_string()];
        let cmd = resolve(Some(&argv)).expect("override resolves");
        assert_eq!(cmd.program, "my-tool");
        assert_eq!(cmd.args, vec!["--clip".to_string()]);
        assert_eq!(cmd.source, CopySource::Override);
    }

    #[test]
    fn override_empty_array_falls_through_to_auto_detect() {
        let empty: Vec<String> = Vec::new();
        // We cannot assert the auto-detect *result* (depends on host),
        // but we can assert the override path did NOT short-circuit.
        // Callers passing `Some(&empty)` get the same answer as `None`.
        let a = resolve(Some(&empty));
        let b = resolve(None);
        assert_eq!(a, b);
    }

    #[test]
    fn override_empty_program_is_rejected() {
        let argv = vec!["".to_string()];
        assert!(resolve(Some(&argv)).is_none());
    }

    #[test]
    fn override_label_marks_user_supplied_source() {
        let argv = vec!["pbcopy".to_string()];
        let cmd = resolve(Some(&argv)).unwrap();
        assert_eq!(cmd.label(), "override(pbcopy)");
    }

    #[test]
    fn copy_with_missing_program_returns_spawn_error() {
        let argv = vec!["this-binary-definitely-does-not-exist-zzz".to_string()];
        let err = copy("hello", Some(&argv)).unwrap_err();
        assert!(matches!(err, ClipboardError::Spawn { .. }));
    }
}
