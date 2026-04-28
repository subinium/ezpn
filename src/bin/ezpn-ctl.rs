use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

// `ipc.rs` references `crate::socket_security::*` for its bind/accept
// hardening (issue #65). The control binary doesn't host a listener
// itself, but it shares `ipc.rs` and so needs the same module visible.
// (the inner `#![allow(dead_code)]` in socket_security.rs already covers this)
#[path = "../socket_security.rs"]
mod socket_security;

#[allow(dead_code)]
#[path = "../ipc.rs"]
mod ipc;

/// Wire envelope sent over the IPC socket. Either a legacy
/// [`ipc::IpcRequest`] or one of the v0.12 extended commands
/// ([`ipc::IpcRequestExt`]). Each side serialises with `tag = "cmd"`,
/// so the daemon's [`ipc::handle_client`] interceptor decides which
/// enum to deserialise into based on the `cmd` string.
enum Wire {
    Legacy(ipc::IpcRequest),
    Ext(ipc::IpcRequestExt),
}

impl Wire {
    fn to_json(&self) -> serde_json::Result<String> {
        match self {
            Wire::Legacy(req) => serde_json::to_string(req),
            Wire::Ext(req) => serde_json::to_string(req),
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("ezpn-ctl: {}", error);
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        print_help();
        return Ok(());
    }

    let mut socket_path: Option<PathBuf> = None;
    let mut pid: Option<u32> = None;
    let mut json_output = false;
    let mut command_start = 0usize;

    while command_start < args.len() {
        match args[command_start].as_str() {
            "--socket" => {
                command_start += 1;
                let path = args
                    .get(command_start)
                    .ok_or_else(|| anyhow::anyhow!("--socket requires a path"))?;
                socket_path = Some(PathBuf::from(path));
            }
            "--pid" => {
                command_start += 1;
                let value = args
                    .get(command_start)
                    .ok_or_else(|| anyhow::anyhow!("--pid requires a pid"))?;
                pid = Some(value.parse()?);
            }
            "--json" => {
                json_output = true;
            }
            value if value.starts_with('-') => {
                anyhow::bail!("unknown option: {}", value);
            }
            _ => break,
        }
        command_start += 1;
    }

    args.drain(0..command_start);
    if args.is_empty() {
        anyhow::bail!("missing command");
    }

    // `ls` has a JSON-only payload; force structured output
    // regardless of the global `--json` flag so the human-readable
    // branch never silently strips schema-frozen output. `dump` is
    // bimodal (text by default, JSON when --format json is set) and
    // is handled inline below.
    let parsed = parse_request(&args)?;
    let force_json = matches!(&parsed, Wire::Ext(ipc::IpcRequestExt::LsTree { .. }));
    let dump_format = match &parsed {
        Wire::Ext(ipc::IpcRequestExt::Dump { .. }) => parse_dump_format(&args)?,
        _ => DumpFormat::Text,
    };
    let socket_path = resolve_socket(socket_path, pid)?;
    let mut stream = UnixStream::connect(&socket_path)?;
    writeln!(stream, "{}", parsed.to_json()?)?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        anyhow::bail!("no response from server");
    }

    let response: ipc::IpcResponse = serde_json::from_str(line.trim())?;

    if json_output || force_json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    if !response.ok {
        anyhow::bail!(
            "{}",
            response
                .error
                .unwrap_or_else(|| "request failed".to_string())
        );
    }

    if let Some(panes) = response.panes {
        for pane in panes {
            println!(
                "{} id={} {}x{} {}{}  {}",
                pane.index + 1,
                pane.id,
                pane.cols,
                pane.rows,
                if pane.alive { "alive" } else { "dead" },
                if pane.active { " *" } else { "" },
                pane.command
            );
        }
    } else if let Some(dump) = response.dump {
        match dump_format {
            DumpFormat::Text => {
                for line in &dump.lines {
                    println!("{}", line);
                }
            }
            DumpFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&dump)?);
            }
        }
    } else if let Some(outcome) = response.send_keys {
        // Human-readable summary for `send-keys --await-prompt`. The
        // exit code is mirrored as the process exit status via
        // `send_keys_exit_code`.
        match outcome.status {
            ipc::SendKeysStatus::PromptSeen => {
                println!(
                    "prompt observed in {} ms (exit {})",
                    outcome.waited_ms,
                    outcome
                        .exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "?".to_string())
                );
            }
            ipc::SendKeysStatus::Timeout => {
                eprintln!(
                    "ezpn-ctl: send-keys timed out after {} ms",
                    outcome.waited_ms
                );
            }
            ipc::SendKeysStatus::DetectionUnavailable => {
                eprintln!(
                    "ezpn-ctl: prompt detection not active for this pane (enable OSC 133 in your shell — see docs/shell-integration.md)"
                );
            }
        }
        std::process::exit(send_keys_exit_code(&outcome));
    } else if let Some(message) = response.message {
        println!("{}", message);
    }

    Ok(())
}

/// Map a [`ipc::SendKeysOutcome`] to the per-#81-spec process exit
/// code: real exit code on `PromptSeen`, `-1` on `Timeout`, `-2` on
/// `DetectionUnavailable`. Negatives are encoded as their `u8`-wrap
/// (255 / 254) by `std::process::exit`.
fn send_keys_exit_code(outcome: &ipc::SendKeysOutcome) -> i32 {
    match outcome.status {
        ipc::SendKeysStatus::PromptSeen => outcome.exit_code.unwrap_or(0),
        ipc::SendKeysStatus::Timeout => -1,
        ipc::SendKeysStatus::DetectionUnavailable => -2,
    }
}

/// Output format for `ezpn-ctl dump --format` (#88).
#[derive(Clone, Copy)]
enum DumpFormat {
    Text,
    Json,
}

/// Re-scan the pre-command argv looking for `--format text|json`.
/// `parse_request` already consumed it but kept its semantics in
/// `parsed`; this helper extracts the user-facing format choice that
/// drives stdout shape (the IPC payload is identical for both).
fn parse_dump_format(args: &[String]) -> anyhow::Result<DumpFormat> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--format" {
            i += 1;
            let value = args
                .get(i)
                .ok_or_else(|| anyhow::anyhow!("--format requires text|json"))?;
            return match value.as_str() {
                "text" => Ok(DumpFormat::Text),
                "json" => Ok(DumpFormat::Json),
                other => anyhow::bail!("invalid --format: {} (want text|json)", other),
            };
        }
        i += 1;
    }
    Ok(DumpFormat::Text)
}

fn parse_request(args: &[String]) -> anyhow::Result<Wire> {
    match args.first().map(String::as_str) {
        Some("split") => {
            let direction = match args.get(1).map(String::as_str).unwrap_or("horizontal") {
                "horizontal" | "h" => ipc::SplitDirection::Horizontal,
                "vertical" | "v" => ipc::SplitDirection::Vertical,
                other => anyhow::bail!("invalid split direction: {}", other),
            };
            let pane = args.get(2).map(|value| value.parse()).transpose()?;
            Ok(Wire::Legacy(ipc::IpcRequest::Split { direction, pane }))
        }
        Some("close") => Ok(Wire::Legacy(ipc::IpcRequest::Close {
            pane: parse_required_usize(args, 1, "close <pane>")?,
        })),
        Some("focus") => Ok(Wire::Legacy(ipc::IpcRequest::Focus {
            pane: parse_required_usize(args, 1, "focus <pane>")?,
        })),
        Some("equalize") => Ok(Wire::Legacy(ipc::IpcRequest::Equalize)),
        Some("list") => Ok(Wire::Legacy(ipc::IpcRequest::List)),
        Some("ls") => parse_ls(&args[1..]),
        Some("dump") => parse_dump(&args[1..]),
        Some("send-keys") => parse_send_keys(&args[1..]),
        Some("layout") => Ok(Wire::Legacy(ipc::IpcRequest::Layout {
            spec: args
                .get(1)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("layout <spec>"))?,
        })),
        Some("exec") => Ok(Wire::Legacy(ipc::IpcRequest::Exec {
            pane: parse_required_usize(args, 1, "exec <pane> <command>")?,
            command: args
                .get(2)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("exec <pane> <command>"))?,
        })),
        Some("save") => Ok(Wire::Legacy(ipc::IpcRequest::Save {
            path: args
                .get(1)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("save <path>"))?,
        })),
        Some("load") => Ok(Wire::Legacy(ipc::IpcRequest::Load {
            path: args
                .get(1)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("load <path>"))?,
        })),
        Some(other) => anyhow::bail!("unknown command: {}", other),
        None => anyhow::bail!("missing command"),
    }
}

/// Parse `ezpn-ctl dump --pane <id> [--session NAME] [--since LINE]
/// [--last N] [--include-scrollback | --no-scrollback] [--strip-ansi]
/// [--format text|json]` (issue #88).
///
/// `--format` is consumed here (CLI-side) AND in `parse_dump_format`
/// (which re-scans for the same flag) — the IPC payload is identical
/// for both formats; only the human-readable layer differs.
fn parse_dump(args: &[String]) -> anyhow::Result<Wire> {
    let mut pane: Option<usize> = None;
    let mut session: Option<String> = None;
    let mut since: Option<usize> = None;
    let mut last: Option<usize> = None;
    let mut include_scrollback = true;
    let mut strip_ansi = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--pane" => {
                i += 1;
                pane = Some(
                    args.get(i)
                        .ok_or_else(|| anyhow::anyhow!("--pane requires an id"))?
                        .parse()?,
                );
            }
            "--session" => {
                i += 1;
                session = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--session requires a name"))?,
                );
            }
            "--since" => {
                i += 1;
                since = Some(
                    args.get(i)
                        .ok_or_else(|| anyhow::anyhow!("--since requires a line number"))?
                        .parse()?,
                );
            }
            "--last" => {
                i += 1;
                last = Some(
                    args.get(i)
                        .ok_or_else(|| anyhow::anyhow!("--last requires a count"))?
                        .parse()?,
                );
            }
            "--include-scrollback" => include_scrollback = true,
            "--no-scrollback" => include_scrollback = false,
            "--strip-ansi" => strip_ansi = true,
            "--format" => {
                // Validated separately by `parse_dump_format`. Skip
                // the value here so unknown-option detection doesn't
                // trip.
                i += 1;
                if args.get(i).is_none() {
                    anyhow::bail!("--format requires text|json");
                }
            }
            other => anyhow::bail!("unknown dump option: {}", other),
        }
        i += 1;
    }
    let pane = pane.ok_or_else(|| anyhow::anyhow!("dump requires --pane <id>"))?;
    Ok(Wire::Ext(ipc::IpcRequestExt::Dump {
        pane,
        session,
        since,
        last,
        include_scrollback,
        strip_ansi,
    }))
}

/// Parse `ezpn-ctl send-keys [--pane <id>] [--await-prompt]
/// [--timeout SECONDS] [--no-newline] -- TEXT...` (issue #81).
///
/// `--pane` is required for now (multi-pane sessions need an explicit
/// target). The `--` separator is recommended but optional; everything
/// after `--` (or after the last recognised flag) is concatenated with
/// single spaces and shipped as the `text` payload.
fn parse_send_keys(args: &[String]) -> anyhow::Result<Wire> {
    let mut pane: Option<usize> = None;
    let mut await_prompt = false;
    let mut timeout_ms: Option<u64> = None;
    let mut no_newline = false;
    let mut text_parts: Vec<String> = Vec::new();
    let mut i = 0;
    let mut after_separator = false;
    while i < args.len() {
        if after_separator {
            text_parts.push(args[i].clone());
            i += 1;
            continue;
        }
        match args[i].as_str() {
            "--" => after_separator = true,
            "--pane" => {
                i += 1;
                pane = Some(
                    args.get(i)
                        .ok_or_else(|| anyhow::anyhow!("--pane requires an id"))?
                        .parse()?,
                );
            }
            "--await-prompt" => await_prompt = true,
            "--no-newline" => no_newline = true,
            "--timeout" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--timeout requires SECONDS"))?;
                timeout_ms = Some(parse_timeout_seconds(value)?);
            }
            other if other.starts_with("--") => {
                anyhow::bail!("unknown send-keys option: {}", other);
            }
            other => text_parts.push(other.to_string()),
        }
        i += 1;
    }
    let pane = pane.ok_or_else(|| anyhow::anyhow!("send-keys requires --pane <id>"))?;
    if text_parts.is_empty() {
        anyhow::bail!("send-keys requires TEXT to send");
    }
    let text = text_parts.join(" ");
    Ok(Wire::Ext(ipc::IpcRequestExt::SendKeys {
        pane,
        text,
        await_prompt,
        timeout_ms,
        no_newline,
    }))
}

/// Accept `--timeout 30`, `--timeout 30s`, `--timeout 1500ms`. Fails
/// for negative or non-numeric values.
fn parse_timeout_seconds(value: &str) -> anyhow::Result<u64> {
    if let Some(stripped) = value.strip_suffix("ms") {
        return stripped
            .parse::<u64>()
            .map_err(|e| anyhow::anyhow!("invalid --timeout: {}", e));
    }
    let stripped = value.strip_suffix('s').unwrap_or(value);
    let secs: u64 = stripped
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid --timeout: {}", e))?;
    Ok(secs.saturating_mul(1000))
}

/// Parse `ezpn-ctl ls [--json] [--session NAME]` (issue #89).
///
/// `--json` is accepted (and required for the structured tree output)
/// but the CLI also forces JSON automatically — see `force_json` in
/// `run`. The legacy positional `--json` global flag also works.
fn parse_ls(args: &[String]) -> anyhow::Result<Wire> {
    let mut session: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {
                // Accepted for parity with the documented surface; the
                // tree variant always emits JSON.
            }
            "--session" => {
                i += 1;
                session = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--session requires a name"))?,
                );
            }
            other => anyhow::bail!("unknown ls option: {}", other),
        }
        i += 1;
    }
    Ok(Wire::Ext(ipc::IpcRequestExt::LsTree { session }))
}

fn parse_required_usize(args: &[String], index: usize, usage: &str) -> anyhow::Result<usize> {
    args.get(index)
        .ok_or_else(|| anyhow::anyhow!(usage.to_string()))?
        .parse()
        .map_err(Into::into)
}

fn resolve_socket(explicit: Option<PathBuf>, pid: Option<u32>) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    if let Some(pid) = pid {
        return Ok(ipc::socket_path_for_pid(pid));
    }

    find_latest_socket().ok_or_else(|| anyhow::anyhow!("no running ezpn instance found"))
}

fn find_latest_socket() -> Option<PathBuf> {
    // Search XDG_RUNTIME_DIR first, then /tmp
    let dirs: Vec<String> = std::env::var("XDG_RUNTIME_DIR")
        .into_iter()
        .chain(std::iter::once("/tmp".to_string()))
        .collect();

    let mut sockets: Vec<PathBuf> = dirs
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    // Match IPC sockets (ezpn-{PID}.sock) but NOT session sockets
                    // (ezpn-session-*.sock) which use a different binary protocol.
                    name.starts_with("ezpn-")
                        && name.ends_with(".sock")
                        && !name.starts_with("ezpn-session-")
                })
        })
        .filter(|path| {
            // Validate socket is connectable (rejects stale sockets from dead processes)
            UnixStream::connect(path).is_ok()
        })
        .collect();

    sockets.sort_by(|a, b| {
        let a_time = a.metadata().and_then(|meta| meta.modified()).ok();
        let b_time = b.metadata().and_then(|meta| meta.modified()).ok();
        b_time.cmp(&a_time)
    });

    sockets.into_iter().next()
}

fn print_help() {
    println!(
        "\
ezpn-ctl — Control a running ezpn instance

USAGE:
  ezpn-ctl [--pid <PID> | --socket <PATH>] [--json] <command> [args...]

COMMANDS:
  split horizontal [pane]    Split pane left|right
  split vertical [pane]      Split pane top/bottom
  close <pane>               Close a pane
  focus <pane>               Focus a pane
  equalize                   Equalize all pane sizes
  list                       List panes (line-formatted)
  ls [--json] [--session NAME]
                             Frozen v1 session/tab/pane tree (JSON)
  dump --pane <id> [--session NAME] [--since LINE] [--last N]
       [--include-scrollback | --no-scrollback] [--strip-ansi]
       [--format text|json]
                             Capture pane output (16 MiB hard cap)
  layout <spec>              Reset to layout spec
  exec <pane> <command>      Run command in a pane
  send-keys --pane <id> [--await-prompt] [--timeout SECONDS]
            [--no-newline] -- TEXT...
                             Write TEXT to a pane; with --await-prompt
                             block until OSC 133 D semantic-prompt
                             arrives (default 30s timeout)
  save <path>                Save workspace snapshot
  load <path>                Load workspace snapshot

EXAMPLES:
  ezpn-ctl list
  ezpn-ctl ls --json
  ezpn-ctl dump --pane 0 --last 50
  ezpn-ctl send-keys --pane 0 --await-prompt -- 'cargo test'
  ezpn-ctl exec 0 'cargo test'
  ezpn-ctl save .ezpn-session.json
  ezpn-ctl --pid 12345 load .ezpn-session.json"
    );
}
