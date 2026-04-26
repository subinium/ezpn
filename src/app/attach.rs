//! Top-level subcommand handlers (`cmd_*`).
//!
//! These are the side doors `main` short-circuits to before deciding to
//! spawn the daemon: `ls`, `kill`, `rename`, `attach`, `init`, `from`,
//! `doctor`. Each handler is self-contained and never touches the event
//! loop — they exit the process directly when done.
//!
//! Pane lifecycle helpers (split/spawn/resize/snapshot) and the IPC
//! command dispatcher live in [`super::lifecycle`] and
//! [`super::ipc_dispatch`] so this file stays focused on the CLI surface.

use crate::cli::parse::parse_procfile;
use crate::client;
use crate::project;
use crate::protocol;
use crate::session;

pub(crate) fn cmd_ls() -> anyhow::Result<()> {
    let sessions = session::list();
    if sessions.is_empty() {
        println!("No active sessions.");
    } else {
        for (name, path) in &sessions {
            // Show creation time from socket mtime
            let age = std::fs::metadata(path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|d| {
                    let secs = d.as_secs();
                    if secs < 60 {
                        format!("{}s", secs)
                    } else if secs < 3600 {
                        format!("{}m", secs / 60)
                    } else if secs < 86400 {
                        format!("{}h", secs / 3600)
                    } else {
                        format!("{}d", secs / 86400)
                    }
                })
                .unwrap_or_else(|| "?".to_string());
            println!("{}: (created {})", name, age);
        }
    }
    Ok(())
}

pub(crate) fn cmd_kill(name: Option<&str>) -> anyhow::Result<()> {
    let (session_name, path) = session::find(name).ok_or_else(|| {
        anyhow::anyhow!(
            "no session found{}",
            name.map(|n| format!(": {}", n)).unwrap_or_default()
        )
    })?;

    // Connect to the server and send kill command
    if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&path) {
        let _ = protocol::write_msg(&mut stream, protocol::C_KILL, &[]);
        // Give the server a moment to shut down gracefully
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    // Clean up socket file in case server didn't
    session::cleanup(&session_name);
    println!("Killed session: {}", session_name);
    Ok(())
}

pub(crate) fn cmd_rename(old: Option<&str>, new: Option<&str>) -> anyhow::Result<()> {
    let new_name = new.ok_or_else(|| anyhow::anyhow!("usage: ezpn rename <old> <new>"))?;
    let (old_name, old_path) = session::find(old).ok_or_else(|| {
        anyhow::anyhow!(
            "no session found{}",
            old.map(|n| format!(": {}", n)).unwrap_or_default()
        )
    })?;
    let new_path = session::socket_path(new_name);
    if new_path.exists() {
        anyhow::bail!("session '{}' already exists", new_name);
    }
    std::fs::rename(&old_path, &new_path)?;
    println!("Renamed session: {} → {}", old_name, new_name);
    Ok(())
}

pub(crate) fn cmd_attach(args: &[String]) -> anyhow::Result<()> {
    let mut name: Option<&str> = None;
    let mut attach_mode = protocol::AttachMode::Steal;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--shared" => attach_mode = protocol::AttachMode::Shared,
            "--readonly" => attach_mode = protocol::AttachMode::Readonly,
            other if !other.starts_with('-') && name.is_none() => name = Some(other),
            other => anyhow::bail!("unknown attach option: {}", other),
        }
        i += 1;
    }

    let (session_name, path) = session::find(name).ok_or_else(|| {
        anyhow::anyhow!(
            "no session found{}",
            name.map(|n| format!(": {}", n)).unwrap_or_default()
        )
    })?;

    if std::env::var("EZPN").is_ok() {
        eprintln!("ezpn: cannot attach from inside an existing ezpn session");
        std::process::exit(1);
    }

    client::run_with_mode(&path, &session_name, attach_mode)
}

pub(crate) fn cmd_init() -> anyhow::Result<()> {
    let path = std::path::Path::new(".ezpn.toml");
    if path.exists() {
        eprintln!("ezpn: .ezpn.toml already exists");
        std::process::exit(1);
    }

    let template = r#"# ezpn project workspace
# Run `ezpn` in this directory to auto-load this config.

[workspace]
# Layout spec: ratios separated by : (cols) and / (rows)
# Examples: "7:3", "1:1:1", "7:3/5:5", "1/1:1"
layout = "1:1"
# Or use grid: rows = 2, cols = 3

[[pane]]
name = "editor"
# command = "nvim ."
# cwd = "."
# shell = "/bin/zsh"
# restart = "never"  # never | on_failure | always
# [pane.env]
# NODE_ENV = "development"

[[pane]]
name = "shell"
# command = "npm run dev"
# cwd = "./frontend"
"#;

    std::fs::write(path, template)?;
    println!("Created .ezpn.toml — edit it and run `ezpn` to launch.");
    Ok(())
}

/// Generate .ezpn.toml from Procfile or docker-compose.yml
pub(crate) fn cmd_from(source: Option<&str>) -> anyhow::Result<()> {
    let source = source.unwrap_or("Procfile");
    let path = std::path::Path::new(source);
    if !path.exists() {
        eprintln!("ezpn: {} not found", source);
        std::process::exit(1);
    }

    let out_path = std::path::Path::new(".ezpn.toml");
    if out_path.exists() {
        eprintln!("ezpn: .ezpn.toml already exists (delete it first or edit manually)");
        std::process::exit(1);
    }

    let contents = std::fs::read_to_string(path)?;
    let entries = parse_procfile(&contents);

    if entries.is_empty() {
        eprintln!("ezpn: no processes found in {}", source);
        std::process::exit(1);
    }

    let mut toml = String::new();
    toml.push_str(&format!("# Generated from {}\n\n", source));

    // Auto-select layout based on count
    let layout = match entries.len() {
        1 => "1",
        2 => "1:1",
        3 => "1:1:1",
        4 => "1:1/1:1",
        n if n <= 6 => "1:1:1/1:1:1",
        _ => "1:1:1/1:1:1",
    };
    toml.push_str(&format!("[workspace]\nlayout = \"{}\"\n\n", layout));

    for (name, command) in &entries {
        toml.push_str("[[pane]]\n");
        toml.push_str(&format!("name = \"{}\"\n", name));
        toml.push_str(&format!(
            "command = \"{}\"\n\n",
            command.replace('"', "\\\"")
        ));
    }

    std::fs::write(out_path, &toml)?;
    println!(
        "Created .ezpn.toml from {} ({} processes). Run `ezpn` to launch.",
        source,
        entries.len()
    );
    Ok(())
}

/// Validate `.ezpn.toml` env interpolation (`ezpn doctor`).
///
/// Reads `.ezpn.toml` from cwd, resolves every pane's env via [`project::resolve_env`],
/// and prints per-pane status. Exits 1 if any reference fails to resolve.
pub(crate) fn cmd_doctor() -> anyhow::Result<()> {
    let path = std::path::Path::new(".ezpn.toml");
    if !path.exists() {
        eprintln!("ezpn doctor: .ezpn.toml not found in current directory");
        std::process::exit(1);
    }
    println!("Reading .ezpn.toml... OK");

    let contents = std::fs::read_to_string(path)?;
    let config: project::ProjectConfig =
        toml::from_str(&contents).map_err(|e| anyhow::anyhow!("parse error in .ezpn.toml: {e}"))?;
    let base_dir = path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));

    println!("Resolving env...");
    let mut error_count = 0usize;
    for (i, pane) in config.pane.iter().enumerate() {
        let label = pane.name.as_deref().unwrap_or("(unnamed)");
        println!("  pane[{i}] ({label}):");
        if pane.env.is_empty() {
            // Still surface .env.local merges for empty-section panes.
            match project::resolve_env(&base_dir, &pane.env, 0) {
                Ok(resolved) if resolved.is_empty() => {
                    println!("    (no env)");
                }
                Ok(resolved) => {
                    let mut keys: Vec<&String> = resolved.keys().collect();
                    keys.sort();
                    for k in keys {
                        let v = &resolved[k];
                        println!("    {k} = {} ✓ (from .env.local)", redact(v));
                    }
                }
                Err(e) => {
                    println!("    ✗ {e}");
                    error_count += 1;
                }
            }
            continue;
        }
        match project::resolve_env(&base_dir, &pane.env, 0) {
            Ok(resolved) => {
                let mut keys: Vec<&String> = resolved.keys().collect();
                keys.sort();
                for k in keys {
                    let v = &resolved[k];
                    let source = if pane.env.contains_key(k) {
                        ""
                    } else {
                        " (from .env.local)"
                    };
                    println!("    {k} = {}{} ✓", redact(v), source);
                }
            }
            Err(e) => {
                println!("    ✗ {e}");
                error_count += 1;
            }
        }
    }
    if error_count > 0 {
        eprintln!("\n{error_count} error(s). See above.");
        std::process::exit(1);
    }
    println!("\nAll pane env resolved successfully.");
    Ok(())
}

/// Mask values that look like secrets so doctor output is safe to share.
/// Heuristic: long opaque strings, or keys named like *TOKEN/SECRET/KEY/PASSWORD.
fn redact(value: &str) -> String {
    // Conservative: redact values >= 12 chars that are mostly non-space.
    // Doctor is for validation, not exfiltration.
    if value.len() >= 12 && !value.contains(' ') && !value.contains('/') {
        "********".to_string()
    } else {
        value.to_string()
    }
}
