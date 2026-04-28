# Terminal protocol — what ezpn passes through, intercepts, and modifies

ezpn sits between the PTY (where your shell, editor, and tools emit
escape sequences) and the host emulator (the terminal app the user is
actually staring at). For each protocol the question is the same:
**does ezpn forward the bytes verbatim, intercept and act on them, or
modify them on the way through?**

This document is the reference for the v1 behaviour of every protocol
ezpn touches. The implementation lives in:

* [`src/terminal_state.rs`](../src/terminal_state.rs) — per-pane DECSET
  bits, Kitty keyboard stack, OSC 7 cwd cache, OSC 52 decision cache,
  theme palette.
* [`src/pane.rs`](../src/pane.rs) — the byte stream interceptor that
  sees PTY output before vt100 does.

## 1. Quick map

| Protocol            | ezpn behaviour          | Owner issue | See also |
|---------------------|-------------------------|-------------|----------|
| DECSET ?2026 (sync) | Intercept + buffer      | #73         | §2 |
| DECSET ?2004 (bracketed paste) | Track per-pane state | #78    | §3 |
| DECSET ?1004 (focus reporting) | Track per-pane state | #78    | §3 |
| DECSET ?1000/?1002/?1003 (mouse) | Track per-pane state, encode in §3 | #78 | §3 |
| DECSET ?1006 (SGR mouse) | Track per-pane state | #78        | §3 |
| DECSET ?1049 (alt screen) | Vt100 owns it      | —           | §3 |
| Kitty keyboard (`CSI > … u`, `CSI = … ; … u`, `CSI < … u`, `CSI ? u`) | Intercept; per-pane stack | #74 | §4 |
| OSC 4 (palette query) | Multiplexer-side reply when theme set, else pass-through | #77 | §5 |
| OSC 7 (reported cwd) | Intercept per pane, fed into `live_cwd()` | #75 | §6 |
| OSC 8 (hyperlink)    | **Lost end-to-end** (vt100 limitation) | #76 | §7 |
| OSC 10/11/12 (fg/bg/cursor query) | Multiplexer-side reply when theme set, else pass-through | #77 | §5 |
| OSC 52 (clipboard)   | Per-pane policy chain   | #79         | [clipboard.md](./clipboard.md) |
| OSC 133 D (semantic prompt) | Intercept; emit `pane.prompt` event | #81 | §8 |
| Bracketed paste content | Pass-through unchanged | —          | §3 |

## 2. DECSET ?2026 — synchronised output

Apps send `CSI ? 2026 h` to begin a frame and `CSI ? 2026 l` to end it.
The terminal is supposed to buffer everything in between and present it
atomically to the user — no partial repaints during a long redraw.

ezpn intercepts these brackets **before vt100 sees them** ([`Pane::in_sync`](../src/pane.rs)).
While `in_sync` is true, ezpn buffers PTY output in a per-pane staging
buffer and does NOT emit frames to clients. When the closing bracket
arrives (or a sync watchdog fires after 250 ms), ezpn flushes the entire
staged buffer to vt100 and emits a single `S_OUTPUT` frame to clients.

**Why this matters**: applications like helix, neovim with the
`render-incremental` patch, and modern TUI frameworks emit hundreds of
small writes per redraw. Without sync brackets, ezpn would push partial
frames every couple of ms and the user would see tearing. With sync
brackets, the user sees one clean repaint.

ezpn always advertises support for `?2026` in its DA2 reply. There is
no opt-out.

## 3. Per-pane state machine (#78)

The fields in [`PaneTerminalState`](../src/terminal_state.rs) track
DECSET bits the multiplexer needs to respond to **before** vt100 does:

```rust
pub struct PaneTerminalState {
    pub bracketed_paste: bool,        // ?2004
    pub focus_reporting: bool,        // ?1004
    pub mouse_mode: MouseMode,        // ?1000/?1002/?1003 + ?1006
    pub kitty_kbd: KittyKbdStack,     // CSI u stack (§4)
    pub reported_cwd: Option<(PathBuf, Instant)>,  // OSC 7 (§6)
    pub osc52_decision: Osc52Decision,             // OSC 52 (clipboard.md)
    pub osc52_pending_confirm: Vec<Vec<u8>>,
}
```

Every pane gets a fresh state on spawn. When a pane slot is reused for
a new shell, `PaneTerminalState::reset()` zeroes everything — no leak
of the previous occupant's state.

**Not tracked here**:

* `?1049` alternate-screen — vt100's `Screen::alternate_screen()` is
  authoritative.
* `?2026` sync brackets — owned by [`Pane::in_sync`](../src/pane.rs)
  (§2).

### 3.1 Mouse encoding

| Wire request   | Effective `MouseProtocol` |
|----------------|---------------------------|
| `?1000 h`      | `X10`                     |
| `?1002 h`      | `Btn`                     |
| `?1003 h`      | `Any`                     |

| Wire request   | Effective `MouseEncoding` |
|----------------|---------------------------|
| `?1006 h`      | `Sgr`                     |
| (default)      | `X10` (legacy 6-byte)     |

ezpn re-emits mouse events to the child using the **child's requested
encoding**, even if the host emulator forwards them in a different
encoding. Crossterm's mouse-event abstraction is used in between.

## 4. Kitty keyboard protocol (#74)

The Kitty progressive enhancement protocol (https://sw.kovidgoyal.net/kitty/keyboard-protocol/)
adds modifier-aware key reports via `CSI u`. ezpn implements the **flag
stack** semantics:

| Wire request                  | Action |
|-------------------------------|--------|
| `CSI > <flags> u`             | Push `flags` on the stack (becomes new top). |
| `CSI = <flags> ; <mode> u`    | Modify top entry. `mode = 1` set, `2` OR (enable bits), `3` AND-NOT (disable bits). Modes outside `1..=3` are ignored. |
| `CSI < <n> u`                 | Pop `n` entries (default `1`). Saturates at empty. |
| `CSI ? u`                     | Query: ezpn replies with `CSI ? <flags> u` for the current top. |

The flag bits (low 5 bits, see `KittyKbdFlags`):

* `0b00001` — disambiguate
* `0b00010` — report events
* `0b00100` — report alternates
* `0b01000` — report all as escapes
* `0b10000` — report associated text

Stack depth is capped at 32 entries (apps that push without ever
popping silently rotate out the oldest). ezpn forwards key reports in
the encoding the **active flags** require — modifier-only key presses,
key releases, alternate keysyms — without translating them to legacy
crossterm reports.

The client must advertise the `kitty-kbd-stack` capability in
`ClientHello.supported_features` (see
[`docs/protocol/v1.md`](./protocol/v1.md#5-capability-strings-frozen))
before ezpn forwards Kitty-encoded key reports; older clients see only
the legacy `Esc + char` form.

## 5. OSC 4 / 10 / 11 / 12 — colour queries (#77)

Apps query the terminal's colour palette to auto-detect light vs dark
backgrounds (`bat`, `delta`, `fzf` respect this).

| OSC | Subject |
|-----|---------|
| 4   | Indexed-colour query (`OSC 4 ; <n> ; ?`). |
| 10  | Foreground query. |
| 11  | Background query. |
| 12  | Cursor colour query. |

ezpn answers from its own theme palette **only when a theme override is
active** (any field of `ThemePalette` is `Some`). Otherwise the query
passes through to the host emulator unchanged so the host can answer
authoritatively.

The xterm-format response uses 4 hex digits per channel, byte-doubled
to fill 16-bit channels:

```
ESC ] 11 ; rgb:1d1d/2020/2424 ESC \
```

This matches what real xterms send and is the form `bat` and friends
parse.

## 6. OSC 7 — reported cwd (#75)

Modern shells emit `OSC 7 ; file://<host><path>` on every directory
change. ezpn intercepts the sequence per pane and stores the path in
`PaneTerminalState::reported_cwd`. `live_cwd()` then prefers this value
over `/proc/<pid>/cwd` polling — and over SSH that's the **only** way
to know the remote shell's cwd.

Stale values are detected via the timestamp paired with the cwd. If
the value is older than the procfs poll interval and procfs disagrees,
procfs wins (handles `cd` from a child shell that didn't emit OSC 7).

See [shell-integration.md](./shell-integration.md) for shell snippets.

## 7. OSC 8 — hyperlinks (#76)

OSC 8 lets terminal apps emit clickable hyperlinks:

```
ESC ] 8 ; id=link1 ; https://example.com ESC \
Anchor text
ESC ] 8 ; ; ESC \
```

ezpn does NOT intercept OSC 8 — the bytes pass through to vt100. But
**`vt100` v0.15 does not preserve OSC 8 in cell state**. The hyperlink
metadata is dropped on the floor at vt100 parse time. End-to-end:

* In live mode the hyperlink is **lost** (vt100 drops it; clients only
  see the cell-grid output, not the original byte stream).
* In copy mode the selection contains the visible text only, no URL.
* In snapshot replay the hyperlink is gone (it was never stored).
* In scrollback re-render it's gone.

Apps that fall back gracefully (printing the URL inline when hyperlinks
aren't supported) work fine. Apps that emit hyperlinks unconditionally
show the anchor text only.

**Path forward**: cell-by-cell preservation requires either forking
vt100 to add a per-cell hyperlink ID or rewriting the cell store. Both
are out of scope for v0.12 — see issue #76 for the v0.14 plan.

## 8. OSC 133 D — semantic prompt (#81)

`OSC 133 ; D ; <exit_code>` marks the boundary between a command's
output and the next prompt. ezpn intercepts D markers and emits a
`pane.prompt` event on the event bus
([`docs/scripting.md`](./scripting.md)). The `send-keys --await-prompt`
flag in `ezpn-ctl` filters these events to detect when a scripted
command has finished.

ezpn does not synthesise OSC 133 — the shell must emit it. Snippets
for bash, zsh, and fish are in
[shell-integration.md](./shell-integration.md).

## 9. Pass-through, unmodified

These protocols pass through ezpn unchanged. ezpn never inspects the
content; the host emulator handles them.

* Bracketed paste content (between `?2004` brackets).
* SGR colours and attributes (FG/BG, bold, italic, underline, strike).
* Cursor movement (`CSI A/B/C/D`, `CSI H`, `CSI <r>;<c>H`, scroll
  regions).
* Title-bar OSC (OSC 0/1/2). The host emulator's window title reflects
  whichever pane wrote last; ezpn does not synthesise titles.
* Bell (`BEL`).
* DA1/DA2 device attribute queries — the host emulator answers, except
  ezpn injects `?2026` into its DA2 advertisement so apps that probe
  for sync support see it.

## 10. Off — never forwarded

* OSC 9 / 777 (iTerm2-style notifications) — currently dropped. A
  multiplexer-aware notification API is on the roadmap (#90 family).
* DCS sequences other than the few above — currently dropped by vt100.
  Apps that depend on Sixel or kitty graphics need to attach to the
  host emulator directly, outside ezpn.

## 11. Verification

For each protocol, a one-liner that exercises it from inside an ezpn
pane:

```sh
# DECSET 2026 sync (no visible effect; check `Pane::in_sync` toggle in trace logs)
printf '\e[?2026h\e[2J\e[H[redrawing]\e[?2026l'

# Kitty kbd stack push, query, pop
printf '\e[>1u'         # push DISAMBIGUATE
printf '\e[?u'          # query → server replies with current top
printf '\e[<1u'         # pop

# OSC 7
printf '\e]7;file://%s/tmp\e\\' "$(hostname)"

# OSC 52 (subject to clipboard policy)
printf '\e]52;c;%s\e\\' "$(printf 'hello' | base64)"

# OSC 4 / 10 / 11 / 12 (responses appear inline as `\e]N;rgb:…\e\\`)
printf '\e]11;?\e\\'

# OSC 133 D
printf '\e]133;D;0\e\\'
```

If a sequence misbehaves, enable trace logging:

```sh
EZPN_LOG=trace ezpn 2>ezpn.log
grep -E '(decset|osc[0-9]+|kitty)' ezpn.log
```
