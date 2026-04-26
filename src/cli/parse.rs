//! Argument parsing for the user-facing `ezpn` CLI.
//!
//! Two entry points:
//! - [`parse_args`]: reads `std::env::args()` for the foreground process.
//! - [`parse_args_from`]: reparses an explicit slice for the daemon
//!   subprocess so the server inherits the same flags the user typed.
//!
//! Both produce a [`Config`] consumed by `app::event_loop::run` and the
//! daemon. The two parsers are intentionally near-duplicates — keeping the
//! daemon's parser tolerant of unknown flags isolates "user typo on the
//! command line" from "server crashed because flag list drifted".
//!
//! [`parse_procfile`] lives here so the `from <Procfile>` codegen stays
//! next to the CLI surface that exposes it.

use crate::cli::help::print_help;
use crate::render::BorderStyle;

pub(crate) enum LayoutSpec {
    Grid { rows: usize, cols: usize },
    Spec(String),
}

pub(crate) struct Config {
    pub layout: LayoutSpec,
    pub border: BorderStyle,
    pub has_border_override: bool,
    pub shell: String,
    pub has_shell_override: bool,
    pub commands: Vec<String>,
    pub restore: Option<String>,
}

pub(crate) fn parse_args() -> anyhow::Result<Config> {
    let args: Vec<String> = std::env::args().collect();
    let mut rows = 1usize;
    let mut cols = 2usize;
    let mut border = BorderStyle::Rounded;
    let mut shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut vertical = false;
    let mut layout_spec: Option<String> = None;
    let mut commands = Vec::new();
    let mut restore = None;
    let mut positional = Vec::new();

    let mut border_set = false;
    let mut shell_set = false;
    let mut direction_set = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-b" | "--border" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--border requires a style"))?;
                border = BorderStyle::from_str(value).ok_or_else(|| {
                    anyhow::anyhow!(
                        "Unknown border style: '{}'. Options: single, rounded, heavy, double",
                        value
                    )
                })?;
                border_set = true;
            }
            "-s" | "--shell" => {
                i += 1;
                shell = args
                    .get(i)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("--shell requires a path"))?;
                shell_set = true;
            }
            "-d" | "--direction" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--direction requires h|v"))?;
                match value.as_str() {
                    "v" | "vertical" => vertical = true,
                    "h" | "horizontal" => vertical = false,
                    other => anyhow::bail!("Unknown direction: '{}'. Options: h, v", other),
                }
                direction_set = true;
            }
            "-l" | "--layout" => {
                i += 1;
                layout_spec = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--layout requires a spec"))?,
                );
            }
            "-e" | "--exec" => {
                i += 1;
                commands.push(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--exec requires a command"))?,
                );
            }
            "-r" | "--restore" => {
                i += 1;
                restore = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--restore requires a file path"))?,
                );
            }
            "-S" | "--session" => {
                i += 1; // Skip value — handled in main()
            }
            "--new" | "--force-new" => {} // Handled in main()
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("ezpn {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--no-daemon" => {} // Handled by main() before parse_args
            other if other.starts_with('-') => anyhow::bail!("Unknown option: {}", other),
            _ => positional.push(args[i].clone()),
        }
        i += 1;
    }

    if layout_spec.is_some() && !positional.is_empty() {
        anyhow::bail!("--layout cannot be combined with positional rows/cols");
    }

    if restore.is_some()
        && (layout_spec.is_some()
            || !commands.is_empty()
            || !positional.is_empty()
            || border_set
            || shell_set
            || direction_set)
    {
        anyhow::bail!("--restore cannot be combined with layout, command, shell, or grid flags");
    }

    let layout = if let Some(spec) = layout_spec {
        LayoutSpec::Spec(spec)
    } else {
        match positional.len() {
            0 => {}
            1 => {
                let n: usize = positional[0].parse()?;
                if vertical {
                    rows = n;
                    cols = 1;
                } else {
                    rows = 1;
                    cols = n;
                }
            }
            2 => {
                rows = positional[0].parse()?;
                cols = positional[1].parse()?;
            }
            _ => anyhow::bail!("Too many arguments. See: ezpn --help"),
        }
        if rows == 0 || cols == 0 {
            anyhow::bail!("Rows and cols must be >= 1");
        }
        if rows * cols > 100 {
            anyhow::bail!("Maximum 100 panes (got {}x{}={})", rows, cols, rows * cols);
        }
        LayoutSpec::Grid { rows, cols }
    };

    Ok(Config {
        layout,
        border,
        has_border_override: border_set,
        shell,
        has_shell_override: shell_set,
        commands,
        restore,
    })
}

/// Parse args from a given slice (used by server process).
pub(crate) fn parse_args_from(args: &[String]) -> anyhow::Result<Config> {
    // Build a fake argv with program name
    let mut full_args = vec!["ezpn".to_string()];
    full_args.extend_from_slice(args);
    // Temporarily override std::env::args by reparsing
    let mut rows = 1usize;
    let mut cols = 2usize;
    let mut border = BorderStyle::Rounded;
    let mut shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut vertical = false;
    let mut layout_spec: Option<String> = None;
    let mut commands = Vec::new();
    let mut restore = None;
    let mut positional = Vec::new();
    let mut border_set = false;
    let mut shell_set = false;
    let mut direction_set = false;

    let mut i = 1;
    while i < full_args.len() {
        match full_args[i].as_str() {
            "-b" | "--border" => {
                i += 1;
                let value = full_args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--border requires a style"))?;
                border = BorderStyle::from_str(value)
                    .ok_or_else(|| anyhow::anyhow!("Unknown border style: '{}'", value))?;
                border_set = true;
            }
            "-s" | "--shell" => {
                i += 1;
                shell = full_args
                    .get(i)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("--shell requires a path"))?;
                shell_set = true;
            }
            "-d" | "--direction" => {
                i += 1;
                let value = full_args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--direction requires h|v"))?;
                match value.as_str() {
                    "v" | "vertical" => vertical = true,
                    "h" | "horizontal" => vertical = false,
                    other => anyhow::bail!("Unknown direction: '{}'", other),
                }
                direction_set = true;
            }
            "-l" | "--layout" => {
                i += 1;
                layout_spec = Some(
                    full_args
                        .get(i)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--layout requires a spec"))?,
                );
            }
            "-e" | "--exec" => {
                i += 1;
                commands.push(
                    full_args
                        .get(i)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--exec requires a command"))?,
                );
            }
            "-r" | "--restore" => {
                i += 1;
                restore = Some(
                    full_args
                        .get(i)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--restore requires a file path"))?,
                );
            }
            "--no-daemon" | "-h" | "--help" | "-V" | "--version" => {
                // Skip flags not relevant to server
            }
            "-S" | "--session" => {
                i += 1; // Skip value — handled by main()
            }
            other if other.starts_with('-') => {
                // Skip unknown flags silently in server mode
            }
            _ => positional.push(full_args[i].clone()),
        }
        i += 1;
    }

    let layout = if let Some(spec) = layout_spec {
        LayoutSpec::Spec(spec)
    } else {
        match positional.len() {
            0 => {}
            1 => {
                let n: usize = positional[0].parse()?;
                if vertical {
                    rows = n;
                    cols = 1;
                } else {
                    rows = 1;
                    cols = n;
                }
            }
            2 => {
                rows = positional[0].parse()?;
                cols = positional[1].parse()?;
            }
            _ => anyhow::bail!("Too many arguments"),
        }
        if rows == 0 || cols == 0 {
            anyhow::bail!("Rows and cols must be >= 1");
        }
        if rows * cols > 100 {
            anyhow::bail!("Maximum 100 panes");
        }
        LayoutSpec::Grid { rows, cols }
    };

    let _ = (direction_set, restore.as_ref());

    Ok(Config {
        layout,
        border,
        has_border_override: border_set,
        shell,
        has_shell_override: shell_set,
        commands,
        restore,
    })
}

/// Parse Procfile format: `name: command`
pub(crate) fn parse_procfile(contents: &str) -> Vec<(String, String)> {
    contents
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (name, cmd) = line.split_once(':')?;
            let name = name.trim();
            let cmd = cmd.trim();
            if name.is_empty() || cmd.is_empty() {
                return None;
            }
            Some((name.to_string(), cmd.to_string()))
        })
        .collect()
}
