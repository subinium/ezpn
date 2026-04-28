//! Session-management subcommands run from the user-facing CLI.
//!
//! Each command here connects to (or queries) an existing daemon socket
//! rather than starting one up, so they share a "client-side" flavour:
//! - [`cmd_ls`] enumerates `$XDG_RUNTIME_DIR/ezpn-*.sock` sockets.
//! - [`cmd_kill`] sends `C_KILL` over the IPC protocol.
//! - [`cmd_rename`] moves the socket inode atomically.
//! - [`cmd_attach`] picks an attach mode (`--shared` / `--readonly`) and
//!   dispatches into [`crate::client::run_with_mode`] — the actual
//!   prefix-mode key dispatch lives inside that client loop.
//!
//! Pure structural extraction from `src/main.rs` — no behaviour change.

use std::path::PathBuf;

use crate::{client, protocol, session, workspace};

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

/// `ezpn upgrade-snapshot <path> [--out PATH] [--force]`
///
/// Reads any supported snapshot version (v1, v2, v3 today) and writes
/// it back as the latest format. By default the upgrade happens
/// in-place; pass `--out` to write somewhere else. The CLI refuses to
/// clobber an existing target unless `--force` is given.
///
/// Idempotency (#70 AC): running this twice is the same as running it
/// once — the loader migrates lazily and the writer always emits the
/// current `SNAPSHOT_VERSION`.
pub(crate) fn cmd_upgrade_snapshot(args: &[String]) -> anyhow::Result<()> {
    let opts = parse_upgrade_args(args)?;

    let (snapshot, on_disk_version) = workspace::load_snapshot_with_meta(&opts.input)?;
    let target = opts.output.as_ref().unwrap_or(&opts.input);

    // Refuse to overwrite by default. The in-place case is exempt
    // because the user explicitly named the same file as the input.
    let writing_elsewhere = opts.output.is_some();
    if writing_elsewhere && target.exists() && !opts.force {
        anyhow::bail!(
            "refusing to overwrite existing file {} (pass --force)",
            target.display()
        );
    }

    workspace::save_snapshot(target, &snapshot)?;

    if on_disk_version == workspace::SNAPSHOT_VERSION {
        println!(
            "ezpn: {} already at v{} (re-wrote in place; no schema change)",
            target.display(),
            workspace::SNAPSHOT_VERSION
        );
    } else {
        println!(
            "ezpn: upgraded {} v{} → v{}",
            target.display(),
            on_disk_version,
            workspace::SNAPSHOT_VERSION
        );
    }
    Ok(())
}

#[derive(Debug)]
struct UpgradeOpts {
    input: PathBuf,
    output: Option<PathBuf>,
    force: bool,
}

fn parse_upgrade_args(args: &[String]) -> anyhow::Result<UpgradeOpts> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut force = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--out requires a path"))?;
                output = Some(PathBuf::from(value));
            }
            "--force" | "-f" => force = true,
            "-h" | "--help" => {
                anyhow::bail!("usage: ezpn upgrade-snapshot <path> [--out PATH] [--force]");
            }
            other if !other.starts_with('-') && input.is_none() => {
                input = Some(PathBuf::from(other));
            }
            other => anyhow::bail!("unknown upgrade-snapshot option: {}", other),
        }
        i += 1;
    }

    let input = input.ok_or_else(|| {
        anyhow::anyhow!("usage: ezpn upgrade-snapshot <path> [--out PATH] [--force]")
    })?;
    Ok(UpgradeOpts {
        input,
        output,
        force,
    })
}

/// Convenience wrapper used by tests so they can drive the same code
/// path without round-tripping through `Vec<String>`.
#[cfg(test)]
fn run_upgrade(
    input: &std::path::Path,
    output: Option<&std::path::Path>,
    force: bool,
) -> anyhow::Result<()> {
    let mut args: Vec<String> = vec![input.to_string_lossy().into_owned()];
    if let Some(o) = output {
        args.push("--out".to_string());
        args.push(o.to_string_lossy().into_owned());
    }
    if force {
        args.push("--force".to_string());
    }
    cmd_upgrade_snapshot(&args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn write_v2_doc(path: &Path) {
        // Smallest valid v2 snapshot the loader will accept. The exact
        // layout JSON shape is owned by `Layout::serialize`; constructing
        // it via the type keeps this test resilient to internal changes.
        let layout = crate::layout::Layout::from_grid(1, 1);
        let doc = serde_json::json!({
            "version": 2,
            "shell": "/bin/sh",
            "border_style": "single",
            "show_status_bar": true,
            "show_tab_bar": true,
            "scrollback": 10000,
            "active_tab": 0,
            "tabs": [{
                "name": "1",
                "layout": serde_json::to_value(&layout).unwrap(),
                "active_pane": 0,
                "panes": [{ "id": 0, "launch": "shell" }]
            }]
        });
        fs::write(path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    }

    #[test]
    fn parse_upgrade_args_requires_input_path() {
        let err = parse_upgrade_args(&[]).unwrap_err();
        assert!(
            err.to_string().contains("upgrade-snapshot"),
            "error must point at the subcommand: {err}"
        );
    }

    #[test]
    fn parse_upgrade_args_handles_out_and_force() {
        let opts = parse_upgrade_args(&[
            "in.ezpn-session.json".to_string(),
            "--out".to_string(),
            "out.ezpn-session.json".to_string(),
            "--force".to_string(),
        ])
        .unwrap();
        assert_eq!(opts.input, PathBuf::from("in.ezpn-session.json"));
        assert_eq!(opts.output, Some(PathBuf::from("out.ezpn-session.json")));
        assert!(opts.force);
    }

    #[test]
    fn upgrade_snapshot_v2_to_v3_in_place() {
        // `tempfile::tempdir()` defaults to a `.tmpXXX` prefix, which
        // `workspace::validate_path` refuses (hidden, no "ezpn" in the
        // name). Prefix the dir so it survives the path guard.
        let tmp = tempfile::Builder::new()
            .prefix("ezpn-upgrade-test-")
            .tempdir()
            .unwrap();
        let path = tmp.path().join("a.ezpn-session.json");
        write_v2_doc(&path);

        run_upgrade(&path, None, false).expect("upgrade");

        let raw = fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            parsed["version"].as_u64().unwrap() as u32,
            workspace::SNAPSHOT_VERSION,
            "version must be bumped to current after upgrade"
        );
        // v3 fields default to None and are skipped on serialize, so the
        // doc stays byte-compatible with a v2 reader on roundtrip.
        let pane0 = &parsed["tabs"][0]["panes"][0];
        assert!(pane0.get("scrollback").is_none());
        assert!(pane0.get("cursor_pos").is_none());
    }

    #[test]
    fn upgrade_snapshot_is_idempotent() {
        // `tempfile::tempdir()` defaults to a `.tmpXXX` prefix, which
        // `workspace::validate_path` refuses (hidden, no "ezpn" in the
        // name). Prefix the dir so it survives the path guard.
        let tmp = tempfile::Builder::new()
            .prefix("ezpn-upgrade-test-")
            .tempdir()
            .unwrap();
        let path = tmp.path().join("b.ezpn-session.json");
        write_v2_doc(&path);

        run_upgrade(&path, None, false).expect("first upgrade");
        let after_first = fs::read_to_string(&path).unwrap();

        run_upgrade(&path, None, false).expect("second upgrade");
        let after_second = fs::read_to_string(&path).unwrap();

        // Byte-identical: the writer is deterministic and the migration
        // ran exactly once on the first call.
        assert_eq!(after_first, after_second);
    }

    #[test]
    fn upgrade_snapshot_refuses_to_overwrite_without_force() {
        // `tempfile::tempdir()` defaults to a `.tmpXXX` prefix, which
        // `workspace::validate_path` refuses (hidden, no "ezpn" in the
        // name). Prefix the dir so it survives the path guard.
        let tmp = tempfile::Builder::new()
            .prefix("ezpn-upgrade-test-")
            .tempdir()
            .unwrap();
        let input = tmp.path().join("c.ezpn-session.json");
        let output = tmp.path().join("d.ezpn-session.json");
        write_v2_doc(&input);
        fs::write(&output, "preexisting\n").unwrap();

        let err = run_upgrade(&input, Some(&output), false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("refusing to overwrite"), "{msg}");
        // Original output preserved.
        assert_eq!(fs::read_to_string(&output).unwrap(), "preexisting\n");
    }

    #[test]
    fn upgrade_snapshot_overwrites_with_force() {
        // `tempfile::tempdir()` defaults to a `.tmpXXX` prefix, which
        // `workspace::validate_path` refuses (hidden, no "ezpn" in the
        // name). Prefix the dir so it survives the path guard.
        let tmp = tempfile::Builder::new()
            .prefix("ezpn-upgrade-test-")
            .tempdir()
            .unwrap();
        let input = tmp.path().join("e.ezpn-session.json");
        let output = tmp.path().join("f.ezpn-session.json");
        write_v2_doc(&input);
        fs::write(&output, "preexisting\n").unwrap();

        run_upgrade(&input, Some(&output), true).expect("force upgrade");
        let raw = fs::read_to_string(&output).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            parsed["version"].as_u64().unwrap() as u32,
            workspace::SNAPSHOT_VERSION
        );
    }

    #[test]
    fn upgrade_snapshot_rejects_unknown_version_with_pointer_to_cli() {
        // `tempfile::tempdir()` defaults to a `.tmpXXX` prefix, which
        // `workspace::validate_path` refuses (hidden, no "ezpn" in the
        // name). Prefix the dir so it survives the path guard.
        let tmp = tempfile::Builder::new()
            .prefix("ezpn-upgrade-test-")
            .tempdir()
            .unwrap();
        let path = tmp.path().join("g.ezpn-session.json");
        let layout = crate::layout::Layout::from_grid(1, 1);
        let doc = serde_json::json!({
            "version": 99,
            "shell": "/bin/sh",
            "border_style": "single",
            "show_status_bar": true,
            "show_tab_bar": true,
            "scrollback": 10000,
            "active_tab": 0,
            "tabs": [{
                "name": "1",
                "layout": serde_json::to_value(&layout).unwrap(),
                "active_pane": 0,
                "panes": [{ "id": 0, "launch": "shell" }]
            }]
        });
        fs::write(&path, serde_json::to_string(&doc).unwrap()).unwrap();
        let err = run_upgrade(&path, None, false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("99"), "error must echo bad version: {msg}");
    }
}
