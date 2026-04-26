# SPEC 06 — `send-keys` IPC + CLI

**Status:** Draft
**Related issue:** TBD
**PRD:** [v0.10.0](../../prd/v0.10.0.md)
**Category:** B. Automation & Scripting

## 1. Background

tmux ships `send-keys -t target 'C-a' 'echo hello' Enter` as the load-bearing
primitive that every editor integration, AI agent, CI pipeline, and `tmuxp`
config in the wild leans on. Without an equivalent, ezpn cannot be driven by
anything other than a human at a keyboard — Goal #2 of the PRD
(`docs/prd/v0.10.0.md:23-25`) is unreachable.

Today, the closest thing ezpn has is `IpcRequest::Exec { pane, command }` in
`src/ipc.rs:32-36`, which spawns a *new* shell-like process via the daemon
rather than feeding bytes into the existing PTY. That is fundamentally the
wrong primitive: it cannot type into a running REPL, cannot send `Ctrl-C`,
cannot drive `vim`, and cannot replay a chord like `C-x C-s`.

The pieces ezpn needs already exist server-side:

- `Pane::write_bytes(&[u8])` and `Pane::write_key(KeyEvent)` are already
  what `process_key` (`src/daemon/keys.rs:602-654`) and the paste handler
  (`src/daemon/dispatch.rs:127-140`) call — the same code path real
  keystrokes take.
- `broadcast`-mode iteration (`src/daemon/keys.rs:601-611`) shows the
  existing pattern for "for every pane, write".
- The IPC channel (`src/ipc.rs:108-149`) already serializes requests
  through the main loop, so a `SendKeys` variant gets `&mut HashMap<usize, Pane>`
  for free.

This SPEC fills the gap with a single new IPC variant, a `ezpn-ctl send-keys`
subcommand, and a small key-spec parser shared with SPEC 09 (custom keymap).

## 2. Goal

`ezpn-ctl send-keys --pane N -- 'echo hello' Enter` delivers exactly the
bytes that pressing those keys interactively would deliver, into pane `N`,
via a single IPC round-trip, with deterministic semantics defined by a
documented key-spec grammar — and is the *only* mechanism scripts need to
drive any pane in any session.

## 3. Non-goals

- **Cross-session targeting.** Targets are pane IDs in the *current*
  session (the one `ezpn-ctl` resolved a socket for). A `--session` selector
  is deferred to v0.11 along with the multi-session model.
- **Recording / playback.** No `tmux capture-keys` equivalent.
- **Macros.** Users compose macros at the shell level (`for i in …; do ezpn-ctl send-keys …; done`).
- **Asynchronous queueing.** `send-keys` is synchronous: the IPC response
  acknowledges the bytes were enqueued on the PTY write half. There is no
  "wait for shell to finish" primitive; that's `wait-for` from tmux and is
  out of scope for v0.10.
- **Chord/timeout sequences.** A single `send-keys` invocation pushes its
  bytes contiguously; ezpn does not insert delays between chord components.

## 4. Design

### 4.1 Pipeline

```
ezpn-ctl send-keys --pane 3 -- 'echo hi' Enter
        │
        ▼ (1) parse argv → KeySpec list
KeySpec::Literal("echo hi"), KeySpec::Named(Enter)
        │
        ▼ (2) JSON over Unix socket  ipc::IpcRequest::SendKeys{...}
{"cmd":"send_keys","target":{"pane":3},"keys":"echo hi\rEnter","literal":false}
        │
        ▼ (3) main loop: handle_ipc_command → keymap::keyspec::compile
Vec<u8> = b"echo hi\r"            // \r = Enter named key, NOT \n
        │
        ▼ (4) Pane::write_bytes
PTY write half
```

### 4.2 Key-spec grammar (PEG-ish)

This grammar lives in a new module `src/keymap/keyspec.rs` and is shared by
SPEC 09 (custom keymap TOML).

```
KeySpec    ← Chord (WS Chord)*           // whitespace-separated chords
Chord      ← (Modifier '-')* Atom        // C-M-S- prefixes
Modifier   ← 'C' / 'M' / 'S'             // Ctrl / Alt / Shift
Atom       ← Named / Char
Named      ← 'Enter' / 'Tab' / 'Esc' / 'Space' / 'Backspace' / 'Delete'
           / 'Up' / 'Down' / 'Left' / 'Right'
           / 'Home' / 'End' / 'PageUp' / 'PageDown'
           / 'F1' .. 'F12'
Char       ← <single Unicode scalar that is not whitespace and not '-'>

WS         ← ' '+
```

Examples:

| Input              | Compiles to (bytes / KeyEvent) |
|--------------------|--------------------------------|
| `'a'`              | `b"a"` |
| `'C-a'`            | `0x01` (SOH) |
| `'C-M-x'`          | `b"\x1bx"` with Ctrl on x → `0x1b 0x18` |
| `'Enter'`          | `b"\r"` (CR — what real Enter sends) |
| `'Tab'`            | `b"\t"` |
| `'Esc'`            | `b"\x1b"` |
| `'F5'`             | `b"\x1b[15~"` (xterm sequence) |
| `'Up'`             | `b"\x1b[A"` |
| `'Space'`          | `b" "` |

### 4.3 Literal vs interpreted: the contract

Two flags decide how the daemon handles the payload:

| `literal` flag | `keys` payload | Behaviour |
|---|---|---|
| `false` (default) | KeySpec text (`'echo hi' Enter`) | Parse via grammar above. `\n` inside a quoted string is the literal LF byte. |
| `true` | Raw UTF-8 bytes | No parsing. The bytes are written verbatim. `'C-a'` is the four ASCII bytes `C`, `-`, `a`, … not the SOH control code. |

**Multi-line contract** (called out because it's the most common footgun):

- `--literal -- $'line1\nline2\n'` writes two LF-terminated lines.
- (non-literal) `'line1' Enter 'line2' Enter` writes `b"line1\rline2\r"`.
- Mixing in non-literal mode is allowed: `'echo' Space 'hi' Enter` →
  `b"echo hi\r"`.
- `\n` inside a non-literal quoted argument is **rejected** at parse time —
  shells can disagree on whether `\n` is a literal LF or the two ASCII bytes
  `\` and `n`. Use `Enter` for newlines or `--literal` for raw bytes.

### 4.4 Targeting

`PaneTarget` is an enum, not a bare `usize`, to keep the wire format
forward-compatible with `current` and (later) `name`-based targeting:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaneTarget {
    /// Numeric pane ID as shown by `ezpn-ctl list`.
    Id(usize),
    /// The active pane on the active tab — resolved server-side.
    Current,
}
```

`Current` resolves to the daemon's `active: usize` at the moment the IPC
command runs (`src/daemon/event_loop.rs:96` & `:582-605`). Resolution
happens inside the main loop, so there is no race between resolution and
delivery.

### 4.5 Error semantics

| Error                                          | Wire response                                    | Exit |
|------------------------------------------------|--------------------------------------------------|------|
| Pane not found (`!panes.contains_key(&pid)`)   | `{"ok":false,"error":"pane <N> not found"}`      | 1    |
| Pane dead (`!pane.is_alive()`)                 | `{"ok":false,"error":"pane <N> not alive"}`      | 1    |
| Daemon not running (no socket)                 | (already handled in `ezpn-ctl`: stderr + exit 1) | 1    |
| `--literal` with key-spec args (`Enter`, etc.) | `{"ok":false,"error":"--literal forbids named keys"}` | 1 |
| Parse error (e.g. `'C-X-y'` — unknown mod)     | `{"ok":false,"error":"parse: unknown modifier 'X'"}` | 1 |
| Empty `keys` payload                           | `{"ok":false,"error":"no keys to send"}`         | 1    |

Errors are reported by the existing `IpcResponse::error` shape in
`src/ipc.rs:86-93` — no new response variant needed.

## 5. Surface changes

### IPC / wire protocol

`SendKeys` is a new variant on `IpcRequest` in `src/ipc.rs`. It does **not**
need a new tag in `src/protocol.rs`: the JSON-RPC IPC channel
(`/run/user/<uid>/ezpn-<pid>.sock`) is separate from the binary client
protocol. `protocol.rs` constants stay untouched by this SPEC. (SPEC 07
introduces `S_EVENT` over the binary protocol; this one stays on the IPC
channel because requests are fire-and-acknowledge.)

```rust
// src/ipc.rs additions
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IpcRequest {
    // ... existing variants ...
    SendKeys {
        target: PaneTarget,
        /// Either KeySpec text (literal=false) or raw bytes (literal=true).
        keys: String,
        #[serde(default)]
        literal: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaneTarget {
    Id(usize),
    Current,
}
```

JSON example:

```json
{"cmd":"send_keys","target":{"kind":"id","value":3},"keys":"echo hi Enter","literal":false}
{"cmd":"send_keys","target":{"kind":"current"},"keys":"","literal":true}
```

### CLI (`ezpn-ctl`)

```
ezpn-ctl send-keys [--pane <N> | --target current] [--literal] -- <key>...

OPTIONS
  --pane <N>          Numeric pane ID (mutually exclusive with --target).
  --target current    Send to the daemon's active pane.
  --literal           Treat each <key> as raw UTF-8 bytes; no parsing.

ARGUMENTS after --:
  Each <key> is parsed as a KeySpec (chord) unless --literal is set.
  Multiple <key> arguments are concatenated with no separator between them
  (so 'echo' Space 'hi' yields "echo hi", not "echo  hi").

EXAMPLES
  ezpn-ctl send-keys --pane 3 -- 'echo hello' Enter
  ezpn-ctl send-keys --target current -- C-c
  ezpn-ctl send-keys --pane 0 --literal -- $'#!/bin/sh\nexit 0\n'
  ezpn-ctl send-keys --pane 2 -- C-x C-s            # vim-style chord
```

The `--` separator is mandatory whenever any `<key>` would otherwise look
like a flag (e.g. `-` itself, `--`). This matches `git`'s convention and
sidesteps the entire "is `-c` a flag or a key?" ambiguity.

### Config (TOML)

None — this is a runtime API. Defaults baked into the daemon's parser.

## 6. Touchpoints

| File | Lines | Change |
|---|---|---|
| `src/ipc.rs` | 16-43 | Add `SendKeys` variant + `PaneTarget` enum |
| `src/keymap/keyspec.rs` | new | Key-spec parser + `compile_to_bytes` (shared with SPEC 09) |
| `src/keymap/mod.rs` | new | `pub mod keyspec;` |
| `src/lib.rs` (or `src/main.rs`) | n/a | `mod keymap;` |
| `src/app/input_dispatch.rs` | (existing `handle_ipc_command`) | New arm: resolve target, parse, write bytes, return `IpcResponse` |
| `src/bin/ezpn-ctl.rs` | 107-153 | Add `send-keys` arm in `parse_request`; add help text 213-238 |
| `src/daemon/event_loop.rs` | 1050-1062 | No code change — existing IPC dispatch already routes through `handle_ipc_command` |
| `tests/send_keys.rs` | new | Integration test (see §8) |

## 7. Migration / backwards-compat

- **Wire**: `IpcRequest` is a serde-tagged enum; adding a variant is
  backward-compatible — older `ezpn-ctl` binaries simply never emit
  `SendKeys`. New `ezpn-ctl` against an old daemon will get the existing
  error `"invalid request: unknown variant 'send_keys'"` from
  `serde_json::from_str` at `src/ipc.rs:182-188`. We accept this — the
  user-facing message is good enough.
- **No protocol bump.** `PROTOCOL_VERSION` (binary protocol) unchanged.
- **Existing scripts.** `ezpn-ctl exec <pane> <cmd>` still works; we do
  **not** deprecate it in v0.10. Documentation will steer users toward
  `send-keys` for new code.

## 8. Test plan

1. **Parser unit tests** (`src/keymap/keyspec.rs`):
   - Round-trip every `Named` key.
   - Modifier permutations: `C-a`, `M-a`, `C-M-a`, `S-Tab`, `C-M-S-x`.
   - Reject: `'X-a'`, `'C-'`, `'C--a'`, empty chord, embedded `\n` (non-literal).
   - Multi-chord: `'echo' Space 'hi' Enter` → `b"echo hi\r"`.
2. **Compiler golden tests**: snapshot `compile_to_bytes("C-c") == [0x03]`,
   `compile_to_bytes("F5") == b"\x1b[15~"` etc.
3. **IPC unit test**: serialize/deserialize each `PaneTarget` variant; verify
   `serde(tag="kind", rename_all="snake_case")` produces stable JSON.
4. **Integration test** (`tests/send_keys.rs`):
   ```rust
   // 1. spawn daemon with --no-attach in a tmpdir
   // 2. ezpn-ctl split horizontal, ezpn-ctl list → grab pane id
   // 3. ezpn-ctl send-keys --pane <id> -- 'echo SENDKEYS_OK' Enter
   // 4. sleep 200ms (PTY drain)
   // 5. ezpn-ctl save snapshot.json (uses existing snapshot path)
   // 6. assert snapshot pane scrollback contains "SENDKEYS_OK"
   ```
   Snapshot path is the cheapest "capture pane state" we already have; SPEC
   06 does **not** ship a separate `--snapshot` flag (that's SPEC 02's
   `clear-history` neighbourhood — see PRD §5 Initiative 02).
5. **Negative**: `ezpn-ctl send-keys --pane 99999 -- 'x'` exits 1 with
   `"pane 99999 not found"`.
6. **Soak**: 1000 `ezpn-ctl send-keys` calls in a loop; `ps -o nlwp` on the
   daemon stays constant (validates SPEC 01's IPC thread-pool fix).

## 9. Acceptance criteria

- [ ] `IpcRequest::SendKeys` defined; round-trips through serde.
- [ ] `src/keymap/keyspec.rs` parses & compiles every entry in §4.2's table.
- [ ] `ezpn-ctl send-keys --pane N -- 'echo hello' Enter` delivers
      `b"echo hello\r"` and the running shell echoes the line.
- [ ] `--target current` resolves to the daemon's active pane.
- [ ] `--literal` writes bytes verbatim; named keys are forbidden in
      literal mode and rejected with a clear error.
- [ ] All 6 test categories in §8 pass.
- [ ] Daemon `ps -o nlwp` constant across 1000 invocations (PRD release gate).
- [ ] `cargo clippy --all-targets -- -D warnings` clean.
- [ ] CHANGELOG entry under v0.10.0 / Automation.

## 10. Risks

| Risk | Mitigation |
|---|---|
| Key-spec grammar drift between SPEC 06 (send-keys) and SPEC 09 (custom keymap) | Single `src/keymap/keyspec.rs` module owns both directions (parse → bytes for send-keys, parse → `KeyEvent` matcher for keymap). Shared test suite. |
| `--literal` lets a script flood a pane with megabytes in one call | Cap `keys.len()` at the existing `MAX_PAYLOAD = 16 MB` in `src/protocol.rs:64` (IPC inherits the same cap via JSON line length — enforce at parse time with a friendly error). |
| Sending `C-c` to a pane that has scrolled into copy-mode shouldn't accidentally exit copy-mode for the *user's* attached client | `send-keys` writes to the **pane PTY**, not the *client* event channel. Copy-mode lives in `InputMode` (`src/daemon/state.rs`), which is per-server-state, not per-pane. So `C-c` reaches the child process; the user's copy-mode is unaffected. Verified by reading `src/daemon/keys.rs:232-294`. |
| Daemon main loop blocks on a slow pane's PTY write | Already addressed by SPEC 01 (slow-client + IPC backpressure). `send-keys` sits behind the same write path; if SPEC 01 lands first, `send-keys` inherits the fix for free. We mark SPEC 01 as a soft dependency, not a hard one — `Pane::write_bytes` is non-blocking on the PTY master fd in practice. |
| `Enter` ambiguity (CR vs LF vs CRLF) | Document explicitly: `Enter` = `\r` (0x0D), matching what a real terminal emits. `Ctrl-J` = `\n` (0x0A). Most shells accept either; vim and curses-based apps care about the difference. |

## 11. Open questions

1. Should `send-keys` to a *broadcast*-mode pane also fan out to all panes
   in that broadcast group? **Default proposal**: no — `send-keys` targets
   exactly one pane. If a script wants fan-out it can `ezpn-ctl list` and
   loop. Broadcast mode is for human ergonomics, not API semantics.
2. Should we add `--prefix` to inject the configured prefix key first
   (so `--prefix -- d` sends `Ctrl-B d`)? **Default proposal**: no — adds
   coupling between this SPEC and the prefix-key config. Users can write
   `--target current -- C-b d` themselves.
3. Should the IPC response carry the *number of bytes written*?
   **Default proposal**: yes — populate `IpcResponse::message` with
   `"sent N bytes"`; cheap, useful for debugging, no schema change.
