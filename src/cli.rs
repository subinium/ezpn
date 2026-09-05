//! Command-line argument parsing.
//!
//! Centralises the `Config` / `LayoutSpec` / `SocketKind` types plus the
//! two parser entry points used by the binary:
//! - [`parse_args`] reads the live `std::env::args()` (used by the user
//!   facing CLI in [`crate::main`]).
//! - [`parse_args_from`] reparses an explicit `&[String]` (used by the
//!   server subprocess so it can recover the original config from
//!   `--server <session> <args…>`).
//!
//! Pure structural extraction — no behaviour change.

use crate::render::BorderStyle;

#[derive(Debug)]
pub(crate) enum LayoutSpec {
    Grid { rows: usize, cols: usize },
    Spec(String),
}

/// How the daemon should bind its session socket.
///
/// `Path` (default) is the standard pathname-based Unix socket under
/// `$XDG_RUNTIME_DIR` or `/tmp`. `Abstract` is the Linux-only abstract
/// namespace at `\0ezpn-<uid>-<session>` — useful when the runtime dir
/// lives on NFS or has surprising perms. On non-Linux platforms the
/// flag is accepted and a warning is logged; bind falls back to `Path`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SocketKind {
    #[default]
    Path,
    Abstract,
}

#[derive(Debug)]
pub(crate) struct Config {
    pub layout: LayoutSpec,
    pub border: BorderStyle,
    pub has_border_override: bool,
    pub shell: String,
    pub has_shell_override: bool,
    pub commands: Vec<String>,
    pub restore: Option<String>,
    pub socket_kind: SocketKind,
    pub session_name: Option<String>,
    pub trust_project: bool,
    pub has_layout_override: bool,
}

pub(crate) fn parse_args() -> anyhow::Result<Config> {
    parse_args_from(&std::env::args().skip(1).collect::<Vec<_>>())
}

pub(crate) fn parse_args_from(input: &[String]) -> anyhow::Result<Config> {
    let mut args = vec!["ezpn".to_owned()];
    args.extend_from_slice(input);
    let mut session_name = None;
    let mut trust_project = false;
    let mut rows = 1usize;
    let mut cols = 2usize;
    let mut border = BorderStyle::Rounded;
    let mut shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut vertical = false;
    let mut layout_spec: Option<String> = None;
    let mut commands = Vec::new();
    let mut restore = None;
    let mut socket_kind = SocketKind::default();
    let mut positional = Vec::new();

    let mut border_set = false;
    let mut shell_set = false;
    let mut direction_set = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--socket" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--socket requires path|abstract"))?;
                socket_kind = match value.as_str() {
                    "path" => SocketKind::Path,
                    "abstract" => SocketKind::Abstract,
                    other => anyhow::bail!(
                        "Unknown --socket value: '{}'. Options: path, abstract",
                        other
                    ),
                };
            }
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
                i += 1;
                session_name = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--session requires a name"))?,
                );
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("ezpn {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--trust-project" => trust_project = true,
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

    let has_layout_override = layout_spec.is_some() || !positional.is_empty() || direction_set;
    let layout = if let Some(spec) = layout_spec {
        let parsed = crate::layout::Layout::from_spec(&spec).map_err(anyhow::Error::msg)?;
        if parsed.pane_count() > 100 {
            anyhow::bail!("Maximum 100 panes");
        }
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
        if rows.checked_mul(cols).is_none_or(|count| count > 100) {
            anyhow::bail!("Maximum 100 panes (got {}x{})", rows, cols);
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
        socket_kind,
        session_name,
        trust_project,
        has_layout_override,
    })
}

pub(crate) fn print_help() {
    println!(
        "\
ezpn — Dead simple terminal pane splitting

USAGE:
  ezpn [OPTIONS] [COLS]              Create session + attach (daemon mode)
  ezpn [OPTIONS] [ROWS] [COLS]       Create session + attach
  ezpn [OPTIONS] --layout <SPEC>     Create session with layout
  ezpn a|attach [SESSION]            Attach to existing session
  ezpn ls                            List active sessions
  ezpn kill [SESSION]                Kill a session
  ezpn rename <OLD> <NEW>            Rename a session
  ezpn --restore <FILE>              Restore workspace snapshot
  ezpn init                          Generate .ezpn.toml template
  ezpn from [FILE]                   Generate .ezpn.toml from Procfile
  ezpn doctor                        Read-only configuration diagnostics
  ezpn --no-daemon [OPTIONS]         Run in single-process mode (no detach)

EXAMPLES:
  ezpn                              Two panes side by side
  ezpn --trust-project              Launch trusted .ezpn.toml / Procfile
  ezpn 2 3                          2x3 grid (6 panes)
  ezpn -l dev                       70/30 split (preset)
  ezpn -l ide                       IDE layout (editor + sidebar + 2 bottom)
  ezpn -l monitor                   3 equal columns
  ezpn -l '7:3/5:5'                 Custom: 2 rows with different ratios
  ezpn -e 'make watch' -e 'npm dev' Per-pane commands via shell -lc
  ezpn --restore workspace.json    Recreate a saved workspace
  ezpn a                            Reattach to last session
  Ctrl+B d                          Detach from current session

OPTIONS:
  -l, --layout <SPEC>   Layout spec or preset name (see PRESETS below)
  -e, --exec <CMD>      Command for each pane (repeatable, default: interactive $SHELL)
  -r, --restore <FILE>  Restore a saved workspace snapshot
  -b, --border <STYLE>  single, rounded, heavy, double (default: rounded)
  -d, --direction <DIR> h (horizontal, default) or v (vertical)
  -s, --shell <SHELL>   Default shell path (default: $SHELL)
  -S, --session <NAME>  Create or reattach to this named session
      --new              Create a new session instead of auto-attaching
      --trust-project    Allow commands and env from .ezpn.toml / Procfile
      --socket <KIND>   path (default); abstract is reserved, not implemented
  -h, --help            Show this help
  -V, --version         Show version

LAYOUT PRESETS:
  dev       7:3         Main editor + side terminal
  ide       7:3/1:1     Editor + sidebar + 2 bottom panes
  monitor   1:1:1       3 equal columns
  quad      2x2         4 panes in a grid
  stack     1/1/1       3 stacked rows
  main      6:4/1       Wide top pair + full-width bottom
  trio      1/1:1       Full top + 2 bottom panes

PROJECT CONFIG (.ezpn.toml):
  Place .ezpn.toml in your project root. Run `ezpn init` to generate a template.
  Supports: layout, per-pane commands, cwd, env vars, custom names,
  auto-restart (never/on_failure/always), per-pane shell override.

DEFAULT CONTROLS:
  Mouse click       Select pane
  Drag border       Resize panes
  Click [━][┃][×]   Split/close buttons on title bar
  F2                Equalize all pane sizes
  Ctrl+B <key>      Prefix mode (tmux keys: % \" o x z R q ? {{ }} E B [ d s)
  F1                Settings panel (j/k/Enter/1-4/q)
  Alt+Arrow         Navigate (needs Meta key on macOS)
  Double-click      Zoom toggle
  Shell control keys pass through unless explicitly rebound.

PREFIX KEYS (Ctrl+B then):
  TABS (tmux windows):
  c                 New tab
  n                 Next tab
  p                 Previous tab
  0-9               Go to tab by number
  &                 Close current tab

  PANES:
  %                 Split left|right
  \"                 Split top/bottom
  o                 Next pane
  Arrow             Navigate directional
  x                 Close pane
  ;                 Last active pane (toggle back)
  Space             Equalize layout
  z                 Zoom toggle
  B                 Broadcast mode (type in all panes)
  R                 Resize mode (arrow/hjkl, q to exit)
  q                 Show pane numbers + quick jump
  {{ }}               Swap pane with prev/next
  [                 Copy mode (vi keys, v select, y copy, / search)
  ,                 Rename current tab
  :                 Command palette
  d                 Detach (session continues in background)
  s                 Toggle status bar
  r                 Reload global config
  ?                 Help overlay"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(args: &[&str]) -> anyhow::Result<Config> {
        parse_args_from(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }
    #[test]
    fn rejects_invalid_or_overflowing_grids() {
        assert!(parse(&[&usize::MAX.to_string(), "2"]).is_err());
        assert!(parse(&["0", "2"]).is_err());
        assert!(parse(&["11", "10"]).is_err());
    }
    #[test]
    fn internal_and_interactive_arguments_are_strict() {
        assert!(parse(&["--bogus"]).is_err());
        assert!(parse(&["-S"]).is_err());
        assert!(parse(&["--restore", "saved.json", "2"]).is_err());
        assert!(parse(&["--layout", "invalid"]).is_err());
    }
    #[test]
    fn project_trust_is_explicit() {
        assert!(!parse(&[]).unwrap().trust_project);
        let cfg = parse(&["--trust-project", "-S", "dev"]).unwrap();
        assert!(cfg.trust_project);
        assert_eq!(cfg.session_name.as_deref(), Some("dev"));
        assert!(parse(&["1", "2"]).unwrap().has_layout_override);
    }
}
