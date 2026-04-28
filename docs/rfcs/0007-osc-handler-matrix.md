# RFC 0007 — OSC handler coverage matrix

| | |
|---|---|
| **Status** | Proposed |
| **Tracks issue** | #107 |
| **Implements** | #75, #76, #77, #79 (per-row dispositions) |
| **Owner** | @subinium |

## Summary

OSC handling is fragmented across four open issues, each defining its own multi-client behaviour informally: OSC 52 broadcasts on `Allow` (#79), OSC 7 is intercepted (#75), OSC 8 passes through but is lost end-to-end (#76, see `docs/multi-client-osc.md`), OSC 4/10/11/12 reply from the multiplexer when a theme is set (#77). There is no single document that says, for an OSC code N, what the daemon does and which clients see what.

This RFC defines the policy matrix once, audits every existing handler against it, and establishes a CI invariant that prevents drift.

## Motivation

### Today's audit

The grounding docs (`docs/multi-client-osc.md`, `docs/terminal-protocol.md`) cover **5 of the 9** OSC codes ezpn touches. The implementation in `src/pane.rs:802-987` and `src/terminal_state.rs` handles each on a per-issue basis without a unifying contract.

`docs/multi-client-osc.md` lists OSC 4, 7, 8, 10/11/12, 52. Missing from that table: OSC 0/1/2 (window/icon title), OSC 133 (semantic prompts), OSC 633 (VS Code shell integration), OSC 1337 (iTerm proprietary). All four are encountered in the wild; ezpn's handling is currently ad-hoc.

`src/pane.rs:619-622` shows the OSC 0 handling (vt100 owns it, ezpn reads `parser.screen().title()` for tab labels) — undocumented in the multi-client doc.

`src/pane.rs:927-933` notes OSC 8 is pass-through "(#76). See docs/multi-client-osc.md" — but the doc says OSC 8 is **lost** end-to-end. The discrepancy is a documentation drift waiting to bite a contributor.

### Why a matrix

Without a single reference, every new OSC handler reinvents the wheel:
- "Should this broadcast or stay client-local?"
- "What if no client is focused?"
- "Can I respond from the daemon, or must I forward?"

The intercept matrix collapses these to a 2-axis decision: `{intercept, forward, gated}` × `{active-client, all-clients, none}`. Every OSC code goes in exactly one cell. Reviewers challenge the cell, not the implementation.

## Design

### The matrix

| OSC | Purpose | Disposition | Multi-client routing | Status | Tracking | Notes |
|---:|---|---|---|---|---|---|
| **0 / 1 / 2** | Set window title / icon name | intercept-and-respond (no reply needed; daemon stores title for tab label) | none — daemon state | Shipped | (built-in) | `parser.screen().title()` consumed by `Pane::display_name` (`src/pane.rs:619-622`); broadcast happens implicitly via tab-bar render |
| **4** | Set/query indexed palette colour | intercept (query, when theme set) / pass (set) | active-client only (passive set) | Shipped | #77 | Multiplexer reply when `theme_palette` non-empty; else pass-through (`docs/terminal-protocol.md` §5) |
| **7** | Reported cwd | intercept | none — daemon state | Shipped | #75 | `PaneTerminalState.reported_cwd` cached per pane; consumed by `live_cwd()` |
| **8** | Hyperlink | forward | active-client only (intent) | Lost end-to-end | #76 | Today: forwarded but vt100 0.15 drops the hyperlink ID before re-render; **effectively lost**. Per `docs/multi-client-osc.md` §"OSC 8 — hyperlinks". Real fix blocked on RFC 0002 fork (cell-level hyperlink storage) |
| **10 / 11 / 12** | Set/query default fg / bg / cursor | intercept (query, when theme set) / pass (set) | active-client only (passive set) | Shipped | #77 | Same logic as OSC 4 |
| **52** | Set/get clipboard | gated by `Osc52Policy` | confirm overlay → broadcast on `Allow` | Shipped (set); read denied | #79 | Per-pane policy chain (`allow` / `confirm` / `deny`); 1 MiB hard cap; reads default `deny`. UI confirm landed in v0.13.0 wiring |
| **133** | Shell prompt markers (A/B/C/D) | intercept | none — daemon state, exposed via events bus | Shipped (intercept); events publish wired | #82 | OSC 133 D triggers `pane.prompt` event; consumed by `send-keys --await-prompt` (#81) |
| **633** | VS Code shell integration | forward | active-client only | Pass-through | (passive) | Daemon does not parse; passed verbatim to clients |
| **1337** | iTerm proprietary | forward | active-client only | Pass-through | (passive) | Daemon does not parse; passed verbatim to clients |
| **(unknown)** | any other OSC | forward | active-client only | Default policy | — | Allow-list for `intercept`; default `forward (active-client)` for safety |

### Cell legend

- **intercept-and-respond**: daemon parses, handles, optionally writes a reply OSC back into the pane PTY (so the requesting program receives a response). Bytes are not forwarded to clients (the host emulator does not need to render the reply).
- **intercept**: daemon parses, updates internal state, does not forward. The host emulator never sees the OSC.
- **forward (active-client)**: daemon writes the OSC verbatim only to the currently focused client's writer.
- **forward (broadcast)**: daemon writes to all attached clients.
- **gated**: per-policy decision (e.g., OSC 52 confirm overlay) determines forward/drop at runtime.

### "Active client" — disambiguation

When **no client is focused** (transient state during attach/detach), or when **multiple clients are foregrounded simultaneously**, "active" needs a tie-breaker. Definition:

- For **input-driven OSC** (a program responds to user input that came from a specific client): the client whose input event triggered the response.
- For **daemon-initiated OSC** (the daemon writes an OSC into a pane's stream — e.g., to tell the shell to clear hyperlinks): broadcast.
- For **passive forwards** (OSC 633, OSC 1337): the most-recently-focused client. If none, drop.

The "most-recently-focused" rule is implemented as a single `Option<ClientId>` on the daemon (`last_focused_client`) updated on every focus event. Clearing on detach prevents writes to dead sockets.

### Implementation surface today

`src/pane.rs:802-987` is the OSC interceptor. The current dispatch is a series of `if buf.starts_with(b"OSC ...; ")` arms:

```rust
// src/pane.rs:907-933 (paraphrased; see file for exact code)
fn handle_one_osc(ctx, payload) {
    if payload.starts_with(b"52;") { handle_osc52(...); return; }
    if payload.starts_with(b"7;")  { handle_osc7(...);  return; }
    for slot in [10, 11, 12]       { if matches { handle_osc_color(slot, ...); return; } }
    if payload.starts_with(b"4;")  { handle_osc4(...);  return; }
    // OSC 8: pass-through (#76); see docs/multi-client-osc.md
    // (no early return — falls through to default forward)
    // (unknown codes fall through to default forward)
}
```

This RFC does not refactor the dispatch — it codifies the table that the dispatch is already implementing (or supposed to be). The audit in step 1 below verifies each branch matches a matrix row.

### Code-level invariant — the matrix mirror

A unit test walks the dispatcher's branches and asserts every handled code has a matching entry in a Rust-side table:

```rust
// src/pane.rs (test module addition)
const OSC_MATRIX: &[(OscCode, Disposition, MultiClient)] = &[
    (OscCode::Title,        Disposition::InterceptRespond, MultiClient::None),
    (OscCode::PaletteColor, Disposition::InterceptOrPass,  MultiClient::ActiveClient),
    (OscCode::Cwd,          Disposition::Intercept,        MultiClient::None),
    // ... one row per handled code
];

#[test]
fn matrix_covers_all_handlers() {
    // Walks every handler arm; asserts a matching row exists.
}
```

Drift is caught at `cargo test`. Adding a new OSC handler without a matrix row fails CI.

### Extension policy

> Any new OSC handler MUST justify its row in PR review.

Practically: PR template (or commit body) cites the row's disposition + multi-client cell with one-sentence rationale. Reviewers challenge the cell, not the implementation. Captured in `docs/spec/osc-handler-matrix.md` as the canonical doc.

## Risks & Mitigations

| Risk | Impact | Mitigation | Verify In Step |
|---|---|---|---|
| "Active client" ambiguous when no client is focused | Lost OSC writes | Most-recently-focused fallback; broadcast for daemon-initiated; drop on no-clients | step 2 |
| OSC handlers proliferate | Matrix bloats | Default disposition for unknown codes is `forward (active-client)`; explicit allow-list for intercept; rejects undocumented OSC interception in PR review | step 4 |
| Matrix doc drifts from code | Reviewers trust stale doc | `OSC_MATRIX` Rust mirror + unit test (above) | step 3 |
| OSC 8 lost end-to-end stays "lost" indefinitely | UX regression vs raw terminal | Blocked on RFC 0002 fork; document the limitation prominently in `docs/spec/osc-handler-matrix.md` so users understand the gap | step 1 |
| OSC 52 broadcast leaks clipboard data to detached-but-not-yet-cleaned-up clients | Security | Use `last_focused_client` rule + verify socket aliveness before each write; fall back to drop | step 5 |

## Implementation Steps

| # | Step | Files | Depends On | Scope |
|---|------|-------|------------|-------|
| 1 | Write `docs/spec/osc-handler-matrix.md` with the full table + extension policy | `docs/spec/osc-handler-matrix.md` | — | M |
| 2 | Audit `src/pane.rs:802-987` against the matrix; fix deviations or update matrix with rationale | `src/pane.rs` | 1 | M |
| 3 | Add `OSC_MATRIX` Rust mirror + drift unit test | `src/pane.rs` (tests module) | 2 | S |
| 4 | Update `docs/multi-client-osc.md` and `docs/terminal-protocol.md` to point at the matrix as canonical | `docs/multi-client-osc.md`, `docs/terminal-protocol.md` | 1 | S |
| 5 | Multi-client integration tests for at least three rows (OSC 0 broadcast, OSC 8 forward-active, OSC 52 gated) | `tests/integration/osc_matrix.rs` | 2 | M |
| 6 | Implement #75 (already shipped — verify) | (audit) | 2 | S |
| 7 | Implement #76 cell-level hyperlink storage (blocked on RFC 0002 fork) | `src/pane.rs`, fork APIs | RFC 0002 | M |
| 8 | Implement #77 (already shipped — verify) | (audit) | 2 | S |
| 9 | Implement #79 confirm UI policy-respect verification (already shipped — verify) | (audit) | 2 | S |

Steps 1 and 5 are independent (different files) and parallelisable. Step 7 (real OSC 8) is gated on RFC 0002.

## Acceptance criteria (per issue #107)

- [ ] `docs/spec/osc-handler-matrix.md` published with the table above + extension policy.
- [ ] Each existing OSC handler in `pane.rs` audited against the matrix; deviations fixed or matrix updated with explicit reason.
- [ ] Multi-client integration test for at least three OSC codes from different rows.
- [ ] #75 implemented per matrix (already shipped — verified by audit).
- [ ] #76 implemented per matrix (blocked on RFC 0002).
- [ ] #77 implemented per matrix (already shipped — verified by audit).
- [ ] #79 confirm UI implementation respects matrix policy (already shipped — verified by audit).

## Open Questions

- **DCS / SOS / APC / PM strings** — out of scope for this RFC. Separate matrix when those become load-bearing (e.g., Sixel).
- **Sixel / Kitty graphics protocol** — own RFC; tangentially OSC-adjacent (OSC 1337 carries some image data) but big enough to deserve a dedicated doc.
- **Broadcast vs round-robin for OSC 52 across clients** — current default is broadcast (every client's clipboard updated on `Allow`). Edge case: two clients on different machines with different clipboards, user copies on machine A, expects paste on machine A only. Today's behaviour is "all or nothing" — punt to a future RFC if user feedback demands per-client policy.
- **OSC 633 / OSC 1337 deeper integration** — VS Code's shell integration emits OSC 633 to convey command boundaries; mirroring its semantics into ezpn's event bus (#82) is plausible but not required. Defer.

## Decision Path / Recommendation

**Adopt.** Lock the matrix in `docs/spec/osc-handler-matrix.md`; enforce via `OSC_MATRIX` mirror unit test. Implementation deviations land per the audit step.

### Numbers

- **OSC codes covered**: 9 (OSC 0/1/2 collapsed, OSC 4, 7, 8, 10/11/12 collapsed, 52, 133, 633, 1337).
- **Default disposition for unknown codes**: forward (active-client). Defensive: unknown codes do not silently break apps that expect them.
- **OSC 8 fidelity**: cell-level lossy until RFC 0002 fork ships hyperlink-aware cell storage. Documented limitation; not a blocker for v1.0 if `docs/multi-client-osc.md` § "Workarounds" stays accurate.

### Reversibility

Matrix rows are not API; they are policy. Changing a row (e.g., flipping OSC 633 from forward-active to broadcast) requires a follow-up RFC and a CHANGELOG note, but does not break the wire format.

## References

- Issue #107 — this RFC's tracking issue
- Issue #75 — OSC 7 cwd intercept (matrix row "7")
- Issue #76 — OSC 8 hyperlinks (matrix row "8"; blocked on RFC 0002)
- Issue #77 — OSC 4/10/11/12 colour queries (rows "4" and "10/11/12")
- Issue #79 — OSC 52 paste-injection guard (row "52")
- Issue #82 — events bus (consumes OSC 133 → `pane.prompt` event)
- RFC 0002 — vt100 strategy commitment (gates OSC 8 cell-level fix)
- `docs/multi-client-osc.md` — current OSC 4/7/8/10/11/12/52 doc (will point at the matrix as canonical)
- `docs/terminal-protocol.md` — broader protocol reference (DECSET + OSC quick map)
- `src/pane.rs:619-622` — OSC 0 title consumption
- `src/pane.rs:802-987` — OSC + Kitty CSI interceptor
- `src/terminal_state.rs:62-95` — `PaneTerminalState` fields backing OSC 7 / 52 / palette
- `docs/clipboard.md` — OSC 52 policy chain detail

Closes #107
