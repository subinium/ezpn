# Terminal Protocol and Compatibility

This document describes the current implementation, not a certification of every
terminal, shell, editor, or SSH configuration. See the [release audit](audits/v0.14.0.md)
for test evidence and unresolved regressions.

## Two Separate Terminal Boundaries

Output follows this path:

```text
child process -> pane PTY -> interceptor -> vt100 screen -> renderer -> attached client -> host terminal
```

ezpn is a cell-based terminal multiplexer, not a transparent ANSI proxy. Most
child output is interpreted and redrawn. A sequence reaching the pane parser
does not mean its original bytes reach the host terminal.

Input travels in the opposite direction as decoded crossterm events. The client
requests host mouse, focus, bracketed-paste, and keyboard-disambiguation reporting.
The server then encodes input according to the **child application's** requested
modes. Host support and child negotiation are separate.

The client/server version handshake and its capability strings describe ezpn's
transport. They do not negotiate the host emulator's complete terminal feature
set or replace a child's Kitty keyboard requests.

## Input Contract

| Input | Current behavior |
|---|---|
| Legacy keyboard | Applications that have not requested Kitty enhancements receive legacy text, control bytes, CSI/SS3 navigation and function-key sequences. Application-cursor mode is respected. |
| Kitty keyboard | Per-pane push, pop, modify and query handling. Accepted bits are disambiguation (1), event reporting (2), and report-all-as-escapes (8). Alternate key codes (4) and associated text (16) are masked out. |
| Key releases | Forwarded only when the child's active encoding requests event reports and that key is encoded as an enhanced report. ezpn cannot recover events the host did not report. |
| Paste | In normal mode, decoded paste text goes to the active pane, or live broadcast targets. Each child gets bracketed-paste delimiters only if it enabled `?2004`. This is not byte-for-byte forwarding of host paste envelopes. |
| Focus | Focus-gained/lost reports go to the active child only if it enabled `?1004`. |
| Mouse | Child modes include `?9`, `?1000`, `?1002`, and `?1003`; encodings include legacy, UTF-8 (`?1005`), SGR (`?1006`), and urxvt (`?1015`). Events are filtered by the requested mode and translated to pane-relative coordinates. |
| Multiplexer selection | Pane chrome is handled by ezpn. Shift-modified mouse input retains ezpn selection instead of being sent to the child. |

The Kitty flag stack is bounded to 32 entries and separated between normal and
alternate screens. Unsupported flags do not become supported merely because the
host terminal implements them. In particular, crossterm events do not retain
every field of the full Kitty protocol.

Legacy mouse encodings have coordinate limits. Unrepresentable reports are
dropped rather than wrapped or clamped to a different cell. There is no promise
of pixel-mouse or every terminal-specific mouse extension.

## Output Contract

| Output | Current behavior |
|---|---|
| Text, cursor movement, scrolling and alternate screens | Interpreted by the pane parser and redrawn within pane geometry. This is not raw pass-through or complete terminal emulation. |
| Cell styles | Foreground/background, bold, italic, underline and inverse are represented. Do not assume every SGR attribute, underline style, font selection or decoration survives. |
| OSC 0/2 titles | Bounded, control-filtered per-pane title metadata. The attached client's host window title identifies the ezpn session; it does not simply follow whichever child writes last. |
| OSC 7 cwd | A fresh, accepted local-host file URI can supply the pane's working directory. Details below. |
| OSC 4/10/11/12 queries | A pane-side responder can answer populated `ThemePalette` entries. Unanswered queries are not relayed to the host. Normal UI theme initialization does not populate this separate pane query palette. |
| OSC 52 | Selected envelopes use a separate forwarding queue and clipboard policy. See [clipboard behavior](clipboard.md). |
| OSC 8 hyperlinks | Hyperlink metadata is not preserved or forwarded end-to-end. Visible anchor text remains; copy and history contain text, not the hidden URL. A host may independently detect a URL printed literally. |
| OSC 133 semantic markers | No complete prompt-boundary implementation. An event type or CLI flag is not evidence that the PTY interceptor publishes prompt events. Do not rely on `--await-prompt` as a command-completion guarantee. |
| Sixel, Kitty graphics and other DCS/APC payloads | No supported graphics pass-through, cell storage, or replay contract. |
| Other terminal queries, bells and notifications | No general raw relay to the host. Only specifically implemented parser/interceptor behavior is available; ezpn does not advertise universal support through DA1/DA2. |

Implementation pointers: [pane parsing and input encoding](../src/pane.rs),
[per-pane modes](../src/terminal_state.rs), [input routing](../src/server/input_modes.rs),
and [rendering](../src/render.rs).

## Synchronized Output

`CSI ? 2026 h` opens a per-pane synchronization window; `CSI ? 2026 l` closes it.
Repeated opens do not create a nesting counter.

The daemon continues processing output into the pane's vt100 screen while the
window is open. It coalesces output-driven dirty updates until close, EOF, or a
roughly **33 ms** watchdog. It does **not** maintain a private, complete staging
screen that is committed atomically afterward.

A resize, focus change, overlay, or another user-forced redraw can expose the
already-updated screen before the child closes its window. Host-side
synchronized-update envelopes reduce visible intermediate writes when supported
by the host; they do not turn this into transaction isolation or a timing
guarantee. There is no special DA2 advertisement of `?2026`.

## Color and Unicode Limits

The renderer emits actual basic/bright ANSI-16 SGR for palette indices 0..15,
rather than assuming crossterm named colors imply basic ANSI output. Higher
indices and RGB have separate output paths. Child cell colors are preserved
independently of the daemon's `NO_COLOR` UI preference.

UI `ColorDepth` is detected from the **daemon's startup environment**. Theme
reload retains that depth. It is not renegotiated for each attached terminal:
a later SSH client or a mixed 16-color/truecolor client set does not automatically
get independent palettes. Do not infer child RGB down-conversion from the UI
theme's fallback.

CJK width, combining text, selections and clipping have regression coverage.
That is not complete grapheme-cluster support across all emulators. The parser
has bounded cell text storage, and terminals can disagree on emoji and ambiguous
character widths.

The optional `--features render-diff` path uses a bounded virtual screen to emit
ANSI deltas. Complete redraws establish a baseline; unsupported controls,
unmodelled text widths and oversized grids retain original output and disable
diffing until a new full redraw. Clipboard side effects are outside that frame
model. This is not a universal speed or memory guarantee.

## Working Directories and History

OSC 7 accepts `file://` URIs for an empty host, `localhost`, or the daemon's own
hostname. The decoded path must be absolute and contain no control characters.
A report is preferred for 30 seconds, then OS process lookup or the launch
directory is used. A local ezpn pane running SSH does not acquire an arbitrary
remote filesystem cwd; a daemon running on that remote host is a different case.

The reported directory is untrusted process output and can influence the
working directory used for a new pane. It is neither process authentication nor
a filesystem permission boundary.

A live detached session keeps its daemon and processes. Disk or named workspace
snapshots instead recreate processes. Opt-in history replay restores sanitized
text, not process memory, executable terminal controls, hyperlink/graphics
metadata, or an exact live alternate-screen application. Internal copy buffers
are runtime state, not snapshot-backed clipboard history.

## Audit Status

The reliability audit reproduced upstream vt100 0.16.2 failures involving
one-column wide output, one-row wrapping, and shrinking a wide cell before
erase, plus missing soft-wrap metadata at a wide-character boundary. These
are covered by passing real regression tests using the private MIT-licensed
parser in `src/vt100/`. Its upstream revision and narrow boundary patches are
documented in `src/vt100/UPSTREAM.md` and shipped in the crate. Passing these
regressions does not establish compatibility with every terminal emulator.

Geometry tests, a simulated terminal, a real Unix PTY, loopback SSH, and a GUI
emulator session are different kinds of evidence. Consult the
[release audit](audits/v0.14.0.md) and [preflight checks](../scripts/preflight.py)
rather than treating this protocol description as a 100% compatibility claim.
