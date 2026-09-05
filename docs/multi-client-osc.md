# Multi-Client OSC Behavior

Clients share the daemon's active session view and rendered output. They do not
receive independent raw child-terminal streams. This distinction determines
which OSC effects are visible, shared, discarded, or not implemented.

See [terminal protocols](terminal-protocol.md) for the input/output boundary and
[clipboard behavior](clipboard.md) for user-copy versus application authorization.

## Current Routing

| Sequence | Routing |
|---|---|
| OSC 0/2 title | Stored per pane and used as pane metadata. Client host window titles identify the ezpn session, not the last emitting child. |
| OSC 7 cwd | Consumed by the daemon as bounded, local-host pane metadata. Not forwarded as a host terminal cwd update. |
| OSC 4/10/11/12 query | Answered directly to the child only when the pane query palette has the requested entry. No generic host-query fallback or per-client response arbitration. |
| OSC 8 hyperlink | Metadata is discarded; clients receive visible text only. No live, history, selection or snapshot hyperlink guarantee. |
| OSC 52 write | Approved application requests and explicit user-copy envelopes use a separate output queue and fan out to attached clients. |
| OSC 52 read | Denied by default. Allowing a query does not provide a complete reply-routing implementation. |
| OSC 133 markers and terminal graphics | No complete semantic-prompt, hyperlink-ID, Sixel or Kitty-graphics forwarding/replay contract. |

## Clipboard Fan-Out and Consent

When the daemon drains an approved OSC 52 queue, it sends the envelopes to every
client attached at that time. This includes readonly clients. Each host terminal
independently decides whether to accept the request, so one clipboard may change
while another does not.

The confirmation prompt and cached decision are **per pane**, shared by the
session. The first accepted answer from an input-capable client resolves that
prompt for everyone. A readonly viewer cannot supply the answer but can receive
the resulting write. There is no per-client clipboard consent or destination
selection in this path.

An explicit user copy bypasses the child-output confirmation filter; this does
not bypass the receiving terminal's own clipboard policy. Copy-mode yanks may
instead succeed through a daemon-side desktop command, in which case they do
not also emit fallback OSC 52. On SSH-launched daemons, automatic desktop
commands are disabled unless explicitly overridden.

A write is an event, not persistent clipboard state. A new or reattached client
does not receive past writes simply because it gets a full screen redraw.
Bounded queues, disconnections and terminal policy can prevent delivery.
Neither an enqueue nor a successful socket write confirms a changed clipboard.

## Color and Other Terminal State

The daemon detects UI color depth at startup and shares its resulting rendering
choices. It does not negotiate separate 16-color, 256-color or truecolor output
for heterogeneous attached terminals. Actual basic ANSI-16 UI codes are available,
but that is not per-client capability negotiation or automatic down-conversion
of all child output.

The pane-side OSC color-query palette is separate from the resolved UI palette.
Normal UI theme setup does not populate the former, so selecting a theme is not
a guarantee that an application's OSC 4/10/11/12 probe receives a response.
Unanswered probes are not broadcast to whichever host might answer first.

Host keyboard reporting is also separate from the child's mode. The client asks
the host for keyboard disambiguation; the daemon sends legacy or supported
enhanced reports according to the child. The transport's `kitty-kbd-stack`
feature string is not proof of complete Kitty support in every attached emulator.

Mouse and focus input represent events from individual clients routed into the
shared session. They do not establish independent per-client pane focus or
application terminal state.

## Cwd, Hyperlinks and Snapshots

OSC 7 accepts only an empty host, `localhost`, or the daemon's own hostname.
Its percent-decoded absolute path is untrusted input used for pane cwd display
and new-pane working-directory selection, with a freshness limit and OS fallback.
Do not treat it as authenticated remote-process identity or assume arbitrary
remote-host paths are usable on a local daemon.

There is no OSC 8 ID-renumbering problem to solve in the current output path:
hyperlink metadata is not retained or emitted. The visible label is preserved;
a literal URL can still be copied or detected independently by a host terminal.

Runtime copy buffers, live detached processes, and disk snapshots are different
state. Snapshots recreate processes; opt-in history restores sanitized text.
They do not replay clipboard events, preserve graphics or hidden hyperlinks,
or resume a live editor's process memory.

## Evidence and Limits

The same frame model can be shared only while clients receive the same output
at the shared dimensions. Attach, resize and lost-output handling must establish
a valid redraw baseline; optional render diffing is not a capability-discovery
layer.

Child `?2026` windows coalesce output-driven redraws until close or approximately
33 ms. The parser continues updating during that interval, so an unrelated
forced redraw can reveal partial application output. Shared-client delivery does
not make this a privately staged, atomic application frame.

The [release audit](audits/v0.14.0.md) records unresolved tiny/wide parser
regressions and the actual PTY/SSH coverage. These contracts are not a claim of
universal GUI-emulator support, successful clipboard delivery to every client,
or a fully passing compatibility certification.
