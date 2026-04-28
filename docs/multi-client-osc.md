# Multi-client OSC semantics

This document covers how ezpn handles OSC (Operating System Command) escape
sequences when multiple clients are attached to the same session, and the
limitations in v0.12.x.

## Overview

| OSC code | Subject              | ezpn behaviour                     | Owner issue |
|---------:|----------------------|------------------------------------|-------------|
| OSC 4    | Indexed colour query | Multiplexer-side reply when theme set, else pass-through to host | #77 |
| OSC 7    | Reported cwd         | Captured per pane, used by `live_cwd()` | #75 |
| OSC 8    | Hyperlink            | Pass-through (limitations below)   | #76 |
| OSC 10/11/12 | fg / bg / cursor query | Multiplexer-side reply when theme set, else pass-through | #77 |
| OSC 52   | Clipboard set/get    | Per-pane policy (`allow`/`confirm`/`deny`) + 1 MiB hard cap | #79 |

## OSC 8 — hyperlinks

OSC 8 lets terminal apps emit clickable hyperlinks:

```
\e]8;id=link1;https://example.com\e\\Anchor text\e]8;;\e\\
```

ezpn does **not** intercept OSC 8 — the bytes pass through to the host
emulator. However, ezpn's vt100 backend (`vt100` crate v0.15) does **not**
preserve OSC 8 in screen state. The hyperlink is invisible in:

- copy mode (selection contains the visible text only — no URL)
- snapshot replay (hyperlink metadata is lost on detach/reattach)
- scrollback re-render

In live mode, hyperlinks generally work for apps that emit them in a single
write, because vt100 sees the bytes once, drops them on the floor, and the
host emulator (running underneath ezpn) has already received the bytes via
its own input channel… **wait, no — that's not how it works.** ezpn reads
PTY output, runs it through vt100, and re-renders cells to clients. OSC 8
is therefore **lost end-to-end** in v0.12.x.

### Workarounds

- Apps that fall back gracefully (printing the URL inline when hyperlinks
  aren't supported) are unaffected.
- Apps that emit hyperlinks unconditionally show the anchor text only.

### Path forward

Preserving OSC 8 cell-by-cell requires either forking `vt100` to store a
per-cell hyperlink ID or rewriting the cell store. Both are out of scope
for v0.12 — see issue #76.

## Multi-client OSC 8 ID collisions

OSC 8 supports an optional `id=…` parameter so that adjacent runs of the
same hyperlink can be drawn as a single visual link. Different clients
attached to the same session share the same byte stream, but each client's
host emulator tracks IDs in its own namespace. There is no collision risk
between clients today **because** ezpn drops OSC 8 entirely (see above).

If/when OSC 8 gets full pass-through, two attached clients may see
visually-merged links if the underlying app reuses an ID across logically
distinct anchors. Fixing this requires per-client ID renumbering — also
deferred.

## OSC 52 — clipboard

OSC 52 lets apps write to (and, less commonly, read from) the user's
system clipboard. ezpn applies a per-pane policy chain:

1. **Hard cap** (`clipboard.osc52_max_bytes`, default 1 MiB) — anything
   larger is dropped with a `warn`-level log line. Defends against memory
   exhaustion via crafted output.
2. **Effective policy** — cached per-pane decision from a previous prompt
   (`Allowed` / `Denied`) overrides the configured `confirm` policy.
3. **Configured policy** (`clipboard.osc52_set`):
   - `allow` — pass through unchanged. Documented as insecure.
   - `confirm` (default) — park the sequence, show a status-bar prompt.
   - `deny` — drop silently, log.

Reads (`OSC 52 ; c ; ?`) default to `deny` because read is the dominant
attack vector — apps that legitimately need the clipboard contents are
extremely rare, while malware exfiltrating recent clipboard contents
(e.g. password manager output) is not.

### Multi-client behaviour

When a clipboard set is allowed, the OSC 52 envelope is forwarded to
**every** attached client. This matches user expectation: detaching from
laptop A and attaching from laptop B should still result in the laptop-B
host emulator seeing the clipboard set. The downside is that on
multi-client detached/reattach scenarios, the clipboard write happens on
both clients' machines.

## OSC 4 / 10 / 11 / 12 — colour queries

When ezpn has a theme override active (palette set on `Pane.theme_palette`),
queries are answered multiplexer-side with the theme's colours. When no
theme override is active, queries pass through to the host emulator.

This avoids the failure mode where `bat`/`delta` auto-detect light vs dark
based on the host emulator's bg, but the user has applied a dark theme
inside ezpn — the apps would render with the wrong colour scheme.

Per-tab themes are out of scope for v0.12.x. The active palette is
session-wide.

## OSC 7 — cwd

When a shell emits OSC 7 (`\e]7;file://host/path\e\\`), ezpn parses the
URI, percent-decodes the path, and stores `(path, instant_now)` on the
pane. `live_cwd()` consults this first; if it's older than 30 s, falls
back to procfs polling.

Shell snippets to emit OSC 7 — `bash`, `zsh`, `fish` — are documented in
each shell's wiki entry; `fish >= 3.x` and `zsh` with `vcs_info` already
emit OSC 7 by default in many distros. ezpn does not modify the user's
shell config.

### Forgery safety

OSC 7 input is treated as untrusted: the path is only used for status-bar
display and the "open new pane here" action. It is not passed to system
calls without normalisation, and a hostile process printing a fake OSC 7
can at most mislead the user about the cwd of its own pane.
