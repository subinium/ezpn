//! `ezpn` binary entry point.
//!
//! `main.rs` is intentionally thin (<200 LOC) — its only jobs are to
//! declare the module tree, re-export crate-root symbols that
//! `crate::server` reaches via `super::*`, and dispatch the very first
//! argv slot to a subcommand handler.
//!
//! Heavier responsibilities live next door:
//! - [`cli`] — argv parsing, `Config`, help text.
//! - [`attach`] — `ls` / `kill` / `rename` / `attach` subcommands.
//! - [`bootstrap`] — `init` / `from` subcommands, `--no-daemon` direct
//!   mode (`run_direct` + the giant prefix-mode `run` loop), the IPC
//!   command dispatcher, and the pane-lifecycle helpers shared with
//!   [`server::run`].

mod attach;
mod bootstrap;
// `buffers` is the v0.16 named copy buffer store (issue #91). The
// command palette wiring (`:set-buffer` / `:paste-buffer` /
// `:list-buffers` / `:save-buffer` / `:delete-buffer`) lands in a
// follow-up since `server/input_modes.rs` is off-limits for this
// slice. The module is reachable via `crate::copy_mode::yank_to_buffer`
// today.
#[allow(dead_code)]
mod buffers;
mod cli;
mod client;
// `clipboard` is the v0.16 system-clipboard fallback chain (issue #92).
// Detection is self-contained today; copy_mode and the OSC-52 emit path
// (server/input_modes.rs, server/mouse.rs) opt in by calling
// `clipboard::copy(...)` before falling back to the OSC 52 sequence.
#[allow(dead_code)]
mod clipboard;
mod commands;
mod config;
mod copy_mode;
mod env_interp;
// `events` is the IPC event bus introduced for issue #82. The producer
// side (server.rs hooks that call `events::publish(...)`) lands as a
// deferred follow-up — until then the bus is reachable through
// `ezpn-ctl events` only via the IPC subscribe path.
#[allow(dead_code)]
mod events;
// `fuzzy` is the v0.15 command palette backend (issue #86). Renderer
// integration lands separately; the module is self-contained today so
// its tests cover scoring + history persistence without a live render.
#[allow(dead_code)]
mod fuzzy;
#[allow(dead_code)]
mod hooks;
mod ipc;
mod keymap;
mod layout;
mod observability;
mod pane;
mod project;
mod protocol;
mod render;
// `render_diff` is the cell-grid delta machinery from issue #93. Gated
// behind the `render-diff` cargo feature so the legacy full-frame path
// stays the default until the bench suite proves the tradeoff.
#[cfg(feature = "render-diff")]
mod render_diff;
mod server;
mod session;
mod settings;
#[cfg(unix)]
mod signals;
#[cfg(unix)]
mod socket_security;
mod tab;
mod terminal_state;
// `theme` is the v0.15 palette + downgrade matrix (issue #85). Renderer
// wiring is intentionally separate; today the module is consumed via
// `EzpnConfig::theme` only.
#[allow(dead_code)]
mod theme;
mod workspace;

// Crate-root re-exports so the `super::Foo` references in `server.rs`
// keep resolving after the structural split. Everything stays
// `pub(crate)` — no new public surface.
pub(crate) use bootstrap::{
    base64_encode, build_initial_state, close_pane, collect_render_targets, do_split,
    extract_selected_text, handle_ipc_command, kill_all_panes, make_inner, replace_pane,
    reset_render_targets, resize_all, resize_zoomed_pane, selection_char_count_from_synced,
    spawn_layout_panes, spawn_pane, spawn_snapshot_panes, sync_render_targets, RenderUpdate,
};
pub(crate) use cli::{parse_args_from, SocketKind};

fn main() -> anyhow::Result<()> {
    // Handle subcommands before anything else
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("init") => return bootstrap::cmd_init(),
        Some("from") => return bootstrap::cmd_from(args.get(2).map(|s| s.as_str())),
        Some("ls") => return attach::cmd_ls(),
        Some("kill") => return attach::cmd_kill(args.get(2).map(|s| s.as_str())),
        Some("a") | Some("attach") => return attach::cmd_attach(&args[2..]),
        Some("rename") => {
            return attach::cmd_rename(
                args.get(2).map(|s| s.as_str()),
                args.get(3).map(|s| s.as_str()),
            )
        }
        Some("upgrade-snapshot") => return attach::cmd_upgrade_snapshot(&args[2..]),
        Some("-h") | Some("--help") => {
            cli::print_help();
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

    if std::env::var("EZPN").is_ok() {
        eprintln!("ezpn: cannot run inside an existing ezpn session");
        std::process::exit(1);
    }

    // Validate args BEFORE spawning daemon — catch errors like invalid flags,
    // conflicting options, etc. early so the user sees them immediately.
    let config = cli::parse_args()?;

    // Check for --no-daemon flag for legacy single-process mode
    let original_args: Vec<String> = args[1..].to_vec();
    if original_args.iter().any(|a| a == "--no-daemon") {
        return bootstrap::run_direct(&config);
    }

    // Create a new session and attach
    // Check for -S/--session flag to set custom session name
    let session_name = {
        let mut custom = None;
        let mut i = 1;
        while i < args.len() {
            if (args[i] == "-S" || args[i] == "--session") && i + 1 < args.len() {
                custom = Some(args[i + 1].clone());
                break;
            }
            i += 1;
        }
        custom.unwrap_or_else(session::auto_name)
    };

    // Auto-attach: if a session with this name already exists, attach to it
    // instead of creating a new one. (Like `cd` into a tmux project.)
    if let Some((existing_name, existing_path)) = session::find(Some(&session_name)) {
        match client::run(&existing_path, &existing_name) {
            Ok(()) => return Ok(()),
            Err(e) => {
                // Incompatible-server errors surface to the user with exit
                // code 2 — never silently fall through to spawning a fresh
                // daemon, since that would mask a genuine version mismatch.
                if let Some(ic) = e.downcast_ref::<client::IncompatibleServerError>() {
                    eprintln!("ezpn: {ic}");
                    std::process::exit(2);
                }
                // Session was stale — clean up and fall through to create new
                session::cleanup(&existing_name);
            }
        }
    }

    let sock_path = session::spawn_server(&session_name, &original_args)?;
    client::run(&sock_path, &session_name)
}
