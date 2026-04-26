//! `ezpn --help` text.
//!
//! Kept verbatim out of `main.rs` so the dispatcher stays small. No logic
//! lives here — it only prints. Update this when adding/removing CLI flags
//! or prefix bindings so the help stays in sync with the actual handlers.

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
  ezpn doctor                        Validate .ezpn.toml env interpolation
  ezpn --no-daemon [OPTIONS]         Run in single-process mode (no detach)

EXAMPLES:
  ezpn                              Two panes side by side (or load .ezpn.toml)
  ezpn 2 3                          2x3 grid (6 panes)
  ezpn -l dev                       70/30 split (preset)
  ezpn -l ide                       IDE layout (editor + sidebar + 2 bottom)
  ezpn -l monitor                   3 equal columns
  ezpn -l '7:3/5:5'                 Custom: 2 rows with different ratios
  ezpn -e 'make watch' -e 'npm dev' Per-pane commands via shell -lc
  ezpn --restore .ezpn-session.json Restore a saved workspace
  ezpn a                            Reattach to last session
  Ctrl+B d                          Detach from current session

OPTIONS:
  -l, --layout <SPEC>   Layout spec or preset name (see PRESETS below)
  -e, --exec <CMD>      Command for each pane (repeatable, default: interactive $SHELL)
  -r, --restore <FILE>  Restore a saved workspace snapshot
  -b, --border <STYLE>  single, rounded, heavy, double (default: rounded)
  -d, --direction <DIR> h (horizontal, default) or v (vertical)
  -s, --shell <SHELL>   Default shell path (default: $SHELL)
  -S, --session <NAME>  Custom session name (default: auto from directory)
  --new, --force-new    Always spawn a new session (skip auto-attach to live one)
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

CONTROLS:
  Mouse click       Select pane
  Drag border       Resize panes
  Click [━][┃][×]   Split/close buttons on title bar
  Ctrl+D            Split left|right (auto-equalizes)
  Ctrl+E            Split top/bottom (auto-equalizes)
  Ctrl+N            Next pane
  F2                Equalize all pane sizes
  Ctrl+B <key>      Prefix mode (tmux keys: % \" o x z R q ? {{ }} E B [ d s)
  Ctrl+G / F1       Settings panel (j/k/Enter/1-4/q)
  Alt+Arrow         Navigate (needs Meta key on macOS)
  Double-click      Zoom toggle
  Ctrl+W            Quit

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
  ?                 Help overlay"
    );
}
