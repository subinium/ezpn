# Clipboard Behavior

There are two distinct paths: **a user copying a selection** and **an application
printing OSC 52**. Their authorization and fallback behavior are not identical.
Both ultimately depend on the receiving terminal's clipboard settings when
OSC 52 is used.

## User-Initiated Copy

`Ctrl+B [` enters copy mode. Default bindings include `v` for character
selection, `V` for line selection, `y` or `Enter` to copy and exit, and
`q` or `Esc` to exit without copying. Copy-mode keymaps can remap these actions.

A copy-mode yank:

1. Stores the text in the default internal buffer, subject to its size limit.
2. Attempts the configured or automatically detected system copy command.
3. Queues OSC 52 to attached clients if that command is unavailable or fails.

Mouse drag-selection currently queues OSC 52 directly. It does not use the
copy-mode desktop-command fallback or populate the internal buffer store.

These explicit user copies do not pass back through the child-output OSC 52
confirmation filter. Setting `clipboard.osc52_set = "deny"` blocks application
requests, not the user's own copy action. A successful internal buffer write or
a successfully queued OSC 52 envelope does not prove that a host clipboard changed.

The internal store supports up to 100 named buffers, with a 16 MiB per-buffer
payload cap and oldest-write eviction for new names at capacity. It is runtime
state and is not restored from workspace snapshots. This storage API does not
by itself imply that every tmux buffer command is implemented.

## System Copy Commands and SSH

A non-empty `clipboard.copy_command` array is executed as program plus arguments,
without a shell. An empty array means automatic detection.

For a local daemon, detection checks:

| Environment | Command |
|---|---|
| `WAYLAND_DISPLAY` and an available tool | `wl-copy` |
| `DISPLAY` and an available tool | `xclip -selection clipboard`, otherwise `xsel --clipboard --input` |
| macOS with an available tool | `pbcopy` |

Auto-detection chooses an available command; failure falls back to OSC 52, not
to an endless retry of desktop tools. Commands have a bounded write/wait deadline
of about two seconds on the supported Unix platforms, with failed children
terminated and reaped. A successful command means the daemon-side tool succeeded,
not necessarily that the attached user's local clipboard changed.

If the daemon environment contains `SSH_CONNECTION` or `SSH_TTY`, automatic
desktop clipboard detection is disabled. User yanks prefer OSC 52 over the SSH
connection instead of silently running `pbcopy` or `xclip` on the remote host.
A non-empty command override is an explicit opt-in to running that remote tool:

```toml
[clipboard]
copy_command = ["my-copy-tool", "--clipboard"]
```

Detection uses the daemon environment, not per-attachment origin. A daemon
started locally and attached later through SSH is not automatically reclassified.
There is no per-client desktop clipboard negotiation. The parsed
`paste_command` setting is not a complete system-clipboard read workflow.

## Application OSC 52 Policy

Any process output, including text from an untrusted log, can contain OSC 52.
ezpn cannot distinguish a trusted application request from the same bytes printed
by another source.

For child-emitted writes, the interceptor validates the selection field and
base64-character payload, applies size limits, then resolves the policy:

- Explicit `deny` or a cached denial blocks the write.
- Cached approval allows subsequent writes unless configuration explicitly denies them.
- Otherwise `allow`, `confirm`, or `deny` applies. The default is `confirm`.

Confirmation is per pane, not per application or client. `y` approves the queued
requests and caches approval; `n` drops them and caches denial. `Esc` defers by
requeuing, so the prompt can appear again. A new process in a newly spawned pane
gets fresh state; a terminal reset is not a way for output to clear a denial.

The configured `osc52_max_bytes` defaults to 1 MiB and checks the encoded
payload, not decoded clipboard text. The stream parser also caps the whole OSC
payload at 1 MiB. A larger configured value does not bypass that parser bound.
Application forwarding/confirmation queues are bounded to eight envelopes and
2 MiB per queue; excess requests can be dropped.

Use conservative defaults:

```toml
[clipboard]
osc52_set = "confirm"
osc52_get = "deny"
osc52_max_bytes = 1048576
```

`osc52_set = "allow"` permits displayed process output to overwrite a receiving
clipboard without an ezpn prompt. It is not required for ordinary user yanks.

Reads (`OSC 52 ; c ; ?`) default to denial. Enabling `osc52_get = "allow"`
can forward the query, but there is no complete origin-correlated host reply
relay to the requesting child. Do not rely on clipboard reads working, especially
with multiple clients, and do not enable them as a copy troubleshooting step.

## Delivery and Troubleshooting

OSC 52 requires the receiving host terminal and any outer multiplexer to permit
clipboard writes. ezpn does not receive an acknowledgment that the clipboard was
updated. There is no verified all-emulator support matrix here: emulator versions,
preferences, remote environments, and nested multiplexers can change the result.

This manual probe intentionally requests a clipboard write:

```sh
printf '\033]52;c;%s\007' "$(printf 'ezpn clipboard test' | base64 | tr -d '\r\n')"
```

Run it inside a pane and answer the ezpn confirmation prompt if shown. Inspect
the clipboard yourself. Compare the same probe outside ezpn, then outside SSH,
to locate where behavior changes; consult the host terminal's and outer
multiplexer's own settings. Avoid enabling unrestricted clipboard reads or
weakening unrelated SSH security settings.

Forwarded envelopes are sent to all currently attached clients, including
readonly viewers. A viewer's input restriction is not a clipboard-output filter.
Past clipboard writes are not restored when a new client attaches. See
[multi-client OSC behavior](multi-client-osc.md).

The [release audit](audits/v0.14.0.md) distinguishes tested copy flows from known
parser/selection limitations. In particular, wide-character soft-wrap metadata
was an unresolved audit regression; neither a clipboard setting nor successful
OSC delivery repairs incorrect text extraction.
