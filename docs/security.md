# Security and Trust

ezpn multiplexes processes you explicitly start. It is not a sandbox for those
processes and cannot make arbitrary shell commands safe.

## Project execution

Automatic .ezpn.toml / Procfile execution requires `--trust-project`.
Review the repository before granting it. An explicit grid (`ezpn 1 2`)
starts plain shells. `ezpn doctor` performs read-only syntax diagnostics;
it does not execute project commands, resolve secrets or test a remote host.

An explicit `--restore FILE` or control `load FILE` starts commands stored in
that snapshot. Treat snapshots as executable configuration, not harmless logs.

## Secrets and snapshots

Secret references use `${secret:KEY}` from the private runtime secrets file.
Global OS keychain backends are not implemented. Secret files are opened without
following symlinks and checked for owner/private permissions.

External environment/dotenv/secret reads mark a project pane as sensitive.
Its executable metadata, cwd, name, env and history are omitted from snapshots;
it restores as a clean shell. Literal configuration can itself contain secrets:
do not embed credentials in literal commands or env fields.

Snapshots are private, written through exclusive temporary files and rename,
and bounded before and after decompression. Unknown history codecs skip history
rather than execute it. History replay writes only into the display parser,
never into shell stdin. Live detached sessions retain processes independently
of disk snapshots.

## Program-controlled terminal output

OSC 52 set requests follow `[clipboard]` policy (confirm by default); reads are
denied by default. A per-pane decision does not authenticate the program that
later writes to the same pane. Choosing allow trusts that pane's output.
User-initiated copies are separate from program-initiated clipboard requests.

OSC 7 is accepted only as a local cwd hint. A path reported by a remote process
must not become a local split's working directory. Unsupported graphics and
hyperlink metadata are not transparently passed through.

## Local transport

Session and control sockets are local Unix sockets. Connections verify peer UID.
Socket creation checks paths, symlinks, ownership and permissions. Probing a slow
server does not authorize unlinking its socket. Handshakes, frames, queues and
writers are bounded; a slow client is disconnected rather than stalling peers.

Readonly is an input/geometry policy for that attached client, not isolation
between processes belonging to the same OS user. Unix permissions and OpenSSH
authentication are the outer security boundary.

## Hooks

Hooks execute configured argv with finite worker/queue capacity, timeouts and
bounded private logs. Invoking `sh -c` explicitly introduces shell evaluation:
do not interpolate untrusted values into shell source. Prefer argv substitution
as separate arguments. Cancellation is bounded best effort; a hostile process
that deliberately escapes its process group is outside the supervisor guarantee.

## Supply chain

Strict audit and deny checks run before release. Existing unmaintained-library
exceptions are documented in the audit configuration; new safety advisories
must be fixed, not silently ignored. The bundled parser's MIT license and narrow
boundary patches are recorded in [its provenance](../src/vt100/UPSTREAM.md).

See the [release audit](audits/v0.14.0.md) for tested evidence and remaining limits.
