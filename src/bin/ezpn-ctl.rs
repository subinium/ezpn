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
        Some(other) => anyhow::bail!("unknown command: {}", other),
        None => anyhow::bail!("missing command"),
    }
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

EXAMPLES:
  ezpn-ctl list
  ezpn-ctl exec 0 'cargo test'
  ezpn-ctl save .ezpn-session.json
  ezpn-ctl --pid 12345 load .ezpn-session.json"
    );
}
