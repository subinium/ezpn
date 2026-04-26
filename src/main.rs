//! `ezpn` binary entry point.
//!
//! Dispatcher only — see `cli/`, `app/`, `direct.rs` for the actual logic.
//! The first positional arg picks one of:
//! - subcommand (`ls`, `kill`, `attach`, `init`, `from`, `doctor`, `rename`)
//! - `--server` (internal daemon entrypoint, used by `session::spawn_server`)
//! - `--help` / `--version` short circuit
//!
//! Anything else falls through to the daemon spawn / auto-attach path.

use std::env;

mod app;
mod cli;
mod client;
mod config;
mod copy_mode;
mod daemon;
mod direct;
mod ipc;
mod layout;
mod pane;
mod project;
mod protocol;
mod render;
mod server;
mod session;
mod settings;
mod signals;
mod snapshot_blob;
mod tab;
mod theme;
mod workspace;

use app::attach::{cmd_attach, cmd_doctor, cmd_from, cmd_init, cmd_kill, cmd_ls, cmd_rename};
use cli::{parse_args, print_help};

fn main() -> anyhow::Result<()> {
    // Handle subcommands before anything else
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("init") => return cmd_init(),
        Some("from") => return cmd_from(args.get(2).map(|s| s.as_str())),
        Some("ls") => return cmd_ls(),
        Some("kill") => return cmd_kill(args.get(2).map(|s| s.as_str())),
        Some("a") | Some("attach") => return cmd_attach(&args[2..]),
        Some("doctor") => return cmd_doctor(),
        Some("rename") => {
            return cmd_rename(
                args.get(2).map(|s| s.as_str()),
                args.get(3).map(|s| s.as_str()),
            )
        }
        Some("-h") | Some("--help") => {
            print_help();
            return Ok(());
        }
        Some("-V") | Some("--version") => {
            println!("ezpn {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--server") => {
            // Internal: run as server daemon
            let session_name = args
                .get(2)
                .ok_or_else(|| anyhow::anyhow!("--server requires session name"))?;
            let remaining: Vec<String> = args[3..].to_vec();
            return server::run(session_name, &remaining);
        }
        _ => {}
    }

    if env::var("EZPN").is_ok() {
        eprintln!("ezpn: cannot run inside an existing ezpn session");
        std::process::exit(1);
    }

    // Validate args BEFORE spawning daemon — catch errors like invalid flags,
    // conflicting options, etc. early so the user sees them immediately.
    let config = parse_args()?;

    // Check for --no-daemon flag for legacy single-process mode
    let original_args: Vec<String> = args[1..].to_vec();
    if original_args.iter().any(|a| a == "--no-daemon") {
        return direct::run_direct(&config);
    }

    // Resolve session name with precedence:
    //   1. CLI `-S/--session NAME` (highest)
    //   2. `.ezpn.toml [session].name` pin
    //   3. Auto: sanitized basename of cwd
    //
    // Then run the chosen "preferred" name through `resolve_session_name`,
    // which handles atomic collision counters and dead-socket cleanup. The
    // `force_new` flag (`--new` / `--force-new`) disables the auto-attach
    // shortcut so users can deterministically spawn a fresh session even when
    // a live one already owns the preferred slot.
    let mut cli_session: Option<String> = None;
    let mut force_new = false;
    {
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "-S" | "--session" if i + 1 < args.len() => {
                    cli_session = Some(args[i + 1].clone());
                    i += 1;
                }
                "--new" | "--force-new" => {
                    force_new = true;
                }
                _ => {}
            }
            i += 1;
        }
    }

    let preferred = cli_session
        .or_else(project::pinned_session_name)
        .unwrap_or_else(session::auto_base_name);

    match session::resolve_session_name(&preferred, !force_new) {
        session::SessionResolution::AttachExisting(name) => {
            // Live session under the preferred name — attach instead of
            // spawning a duplicate. Matches the historical `cd repo && ezpn`
            // behavior so existing scripts keep working.
            let path = session::socket_path(&name);
            match client::run(&path, &name) {
                Ok(()) => Ok(()),
                Err(_) => {
                    // Connection died mid-attach (server crashed between
                    // is_alive probe and our connect). Clean up and spawn
                    // fresh under the same name.
                    session::cleanup(&name);
                    let sock_path = session::spawn_server(&name, &original_args)?;
                    client::run(&sock_path, &name)
                }
            }
        }
        session::SessionResolution::New(name) => {
            let sock_path = session::spawn_server(&name, &original_args)?;
            client::run(&sock_path, &name)
        }
    }
}
