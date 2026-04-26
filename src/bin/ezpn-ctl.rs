use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

#[allow(dead_code)]
#[path = "../ipc.rs"]
mod ipc;

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

    let request = parse_request(&args)?;
    let socket_path = resolve_socket(socket_path, pid)?;
    let mut stream = UnixStream::connect(&socket_path)?;
    writeln!(stream, "{}", serde_json::to_string(&request)?)?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        anyhow::bail!("no response from server");
    }

    let response: ipc::IpcResponse = serde_json::from_str(line.trim())?;
    if json_output {
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
    } else if let Some(message) = response.message {
        println!("{}", message);
    }

    Ok(())
}

fn parse_request(args: &[String]) -> anyhow::Result<ipc::IpcRequest> {
    match args.first().map(String::as_str) {
        Some("split") => {
            let direction = match args.get(1).map(String::as_str).unwrap_or("horizontal") {
                "horizontal" | "h" => ipc::SplitDirection::Horizontal,
                "vertical" | "v" => ipc::SplitDirection::Vertical,
                other => anyhow::bail!("invalid split direction: {}", other),
            };
            let pane = args.get(2).map(|value| value.parse()).transpose()?;
            Ok(ipc::IpcRequest::Split { direction, pane })
        }
        Some("close") => Ok(ipc::IpcRequest::Close {
            pane: parse_required_usize(args, 1, "close <pane>")?,
        }),
        Some("focus") => Ok(ipc::IpcRequest::Focus {
            pane: parse_required_usize(args, 1, "focus <pane>")?,
        }),
        Some("equalize") => Ok(ipc::IpcRequest::Equalize),
        Some("list") => Ok(ipc::IpcRequest::List),
        Some("layout") => Ok(ipc::IpcRequest::Layout {
            spec: args
                .get(1)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("layout <spec>"))?,
        }),
        Some("exec") => Ok(ipc::IpcRequest::Exec {
            pane: parse_required_usize(args, 1, "exec <pane> <command>")?,
            command: args
                .get(2)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("exec <pane> <command>"))?,
        }),
        Some("save") => Ok(ipc::IpcRequest::Save {
            path: args
                .get(1)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("save <path>"))?,
        }),
        Some("load") => Ok(ipc::IpcRequest::Load {
            path: args
                .get(1)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("load <path>"))?,
        }),
        Some("clear-history") => Ok(ipc::IpcRequest::ClearHistory {
            pane: parse_flag_pane(args)?,
        }),
        Some("set-scrollback") => Ok(ipc::IpcRequest::SetHistoryLimit {
            pane: parse_flag_pane(args)?,
            lines: parse_flag_lines(args)?,
        }),
        Some("send-keys") => parse_send_keys(args),
        Some(other) => anyhow::bail!("unknown command: {}", other),
        None => anyhow::bail!("missing command"),
    }
}

/// Parse `ezpn-ctl send-keys [--pane N | --target current] [--literal] -- <key>...`.
///
/// Per SPEC 06 §5: the `--` separator is mandatory once any `<key>` could
/// look like a flag. We require it unconditionally to keep the contract
/// simple and unambiguous (matches `git`'s convention).
fn parse_send_keys(args: &[String]) -> anyhow::Result<ipc::IpcRequest> {
    // Find the `--` separator; everything after it is `<key>` tokens.
    let dash_dash = args.iter().position(|a| a == "--").ok_or_else(|| {
        anyhow::anyhow!(
            "send-keys requires `--` before key tokens (e.g. \
             `ezpn-ctl send-keys --pane 0 -- 'echo hi' Enter`)"
        )
    })?;
    let opts = &args[1..dash_dash];
    let keys: Vec<String> = args[dash_dash + 1..].to_vec();
    if keys.is_empty() {
        anyhow::bail!("send-keys requires at least one key after `--`");
    }

    let mut pane_id: Option<usize> = None;
    let mut current = false;
    let mut literal = false;
    let mut i = 0;
    while i < opts.len() {
        match opts[i].as_str() {
            "--pane" => {
                i += 1;
                let v = opts
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--pane requires a number"))?;
                pane_id = Some(v.parse()?);
            }
            "--target" => {
                i += 1;
                let v = opts
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--target requires a value"))?;
                if v != "current" {
                    anyhow::bail!("only --target current is supported in v0.10");
                }
                current = true;
            }
            "--literal" => literal = true,
            other => anyhow::bail!("unknown send-keys option: {}", other),
        }
        i += 1;
    }
    if pane_id.is_some() && current {
        anyhow::bail!("--pane and --target current are mutually exclusive");
    }
    let target = match (pane_id, current) {
        (Some(id), false) => ipc::PaneTarget::Id { value: id },
        (None, true) => ipc::PaneTarget::Current,
        (None, false) => anyhow::bail!("send-keys requires --pane <N> or --target current"),
        (Some(_), true) => unreachable!(),
    };
    Ok(ipc::IpcRequest::SendKeys {
        target,
        keys,
        literal,
    })
}

/// Parse a required `--pane N` flag from the args after the subcommand.
/// Returns the pane id; bails if the flag is missing or not a usize.
fn parse_flag_pane(args: &[String]) -> anyhow::Result<usize> {
    parse_named_usize(args, "--pane").ok_or_else(|| {
        anyhow::anyhow!("missing --pane <N>; example: ezpn-ctl clear-history --pane 0")
    })
}

/// Parse a required `--lines N` flag.
fn parse_flag_lines(args: &[String]) -> anyhow::Result<usize> {
    parse_named_usize(args, "--lines").ok_or_else(|| {
        anyhow::anyhow!(
            "missing --lines <N>; example: ezpn-ctl set-scrollback --pane 0 --lines 5000"
        )
    })
}

fn parse_named_usize(args: &[String], flag: &str) -> Option<usize> {
    args.iter().enumerate().find_map(|(i, a)| {
        if a == flag {
            args.get(i + 1).and_then(|v| v.parse().ok())
        } else {
            None
        }
    })
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
  list                       List panes
  layout <spec>              Reset to layout spec
  exec <pane> <command>      Run command in a pane
  save <path>                Save workspace snapshot
  load <path>                Load workspace snapshot
  clear-history --pane N     Drop scrollback above the visible screen
  set-scrollback --pane N --lines L
                             Resize a pane's scrollback ring (max from
                             [scrollback] max_lines, default 100000)
  send-keys [--pane N | --target current] [--literal] -- <key>...
                             Deliver keystrokes/text into a pane's PTY.
                             Each <key> is a chord token: 'C-a', 'Enter',
                             'F5', or literal text like 'echo hi'.
                             --literal writes bytes verbatim (no parsing).

EXAMPLES:
  ezpn-ctl list
  ezpn-ctl exec 0 'cargo test'
  ezpn-ctl save .ezpn-session.json
  ezpn-ctl --pid 12345 load .ezpn-session.json
  ezpn-ctl clear-history --pane 0
  ezpn-ctl set-scrollback --pane 0 --lines 5000
  ezpn-ctl send-keys --pane 0 -- 'echo hello' Enter
  ezpn-ctl send-keys --target current -- C-c
  ezpn-ctl send-keys --pane 2 -- C-x C-s"
    );
}
