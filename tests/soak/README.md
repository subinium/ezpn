# Runtime Evidence

All harnesses isolate HOME, XDG directories, sockets and working directories
under private temporary directories. They never use existing ezpn sessions.
Python 3.9+ and Unix PTYs are required. Processes and IPC waits are bounded.

```sh
cargo build --locked --release --bins
bash tests/soak/run.sh --profile=smoke --out=target/soak-smoke --build-features=default
bash tests/soak/run.sh --profile=full --out=target/soak-full --build-features=default
# A short diagnostic run is explicitly labeled custom, not a completed soak.
bash tests/soak/run.sh --duration=30 --out=target/soak-diagnostic
python3 scripts/ssh-smoke.py --out=target/ssh-smoke
python3 scripts/preflight.py --mode=ci --ssh=optional
python3 scripts/preflight.py --mode=release --ssh=required
python3 scripts/bench-regression.py --base=origin/main
```

## Soak

Smoke runs 30 minutes with 5 panes; full runs 24 hours with 100 panes.
Every pane runs real bounded bursts of output. The actual interactive client
creates/closes a tab, detaches and reattaches periodically. Each cycle saves
and validates a real snapshot. RSS samples, actual elapsed time, completed
cycles, snapshots and lifecycle events are recorded in the output directory.

Criteria remain final RSS <=1.30 times the hour-one sample (initial workload-ready
sample for shorter runs), zero daemon-child zombies, and snapshot growth <=100 MiB.
Each pane must finish its first 1 MiB burst before the initial sample; setup
duration is recorded separately, avoiding an idle-before-workload RSS baseline.
Missing samples, failed IPC, incomplete cycles and daemon exits fail. No
signal is substituted for tab work; no failure is converted into a warning.
A short run cannot establish 24-hour stability or real-editor compatibility.

Each report includes the executable path, SHA-256, version and explicitly
supplied `--build-features=default|all-features` label (otherwise unspecified).
Do not infer features from `target/release/ezpn`: an all-feature Cargo bench
build can replace it. Preflight builds benches first and materializes the
default-feature release binary last. For both-mode validation, copy each
just-built executable to a distinct artifact path and test both, retaining
failed results; a default-feature pass does not excuse a render-diff failure.

## Terminal And SSH

`cargo test --locked --test integration` runs real client PTYs with
`xterm-256color`, `screen-256color`, `tmux-256color` and `vt100` TERM values.
This checks terminal protocol paths, not those GUI emulators themselves.
Tests cover process/state preservation, kernel resize, readonly/shared
clients, alternate-screen/raw-mode restoration and abrupt socket loss.

The SSH script starts only a private loopback sshd on an ephemeral high port,
with temporary host/client keys, authorized keys, known hosts and config.
It does not enable Remote Login, edit `/etc/ssh`, modify `~/.ssh`, use sudo,
or install missing privilege-separation directories. It makes three real
`ssh -tt` connections, checking the same shell PID and retained variable
after clean detach and a killed SSH transport. Authentication/runtime errors
fail; absent binaries or failed `sshd -t` prerequisites exit 77 with a reason.
`--ssh=required` treats that explicit skip as a failed prerequisite.

An available unprivileged sshd, usable current account and platform-provided
privilege-separation support are requirements for real SSH evidence. GUI
emulator/version, remote host and network-failure matrices still require
separate measured runs. No universal terminal/SSH guarantee is implied.

## Gates

Preflight records commands, durations, exit codes, PASS/FAIL/SKIP and actual
libtest summaries in `status.json` plus per-command logs. It explicitly uses
the manifest's `cargo +<MSRV>`; install that exact toolchain first. Ignored,
filtered, empty or failed test runs cannot pass the evidence parser. Counts
are executions, including mounted-module tests, not unique test definitions.
Release preflight also requires cargo-llvm-cov, cargo-audit and cargo-deny,
release tests and package verification. Coverage floors are 65% overall
source lines and 70% for protocol/layout/workspace; missing records fail.

Run the separate performance gate against the reviewed base revision before
release. It archives the entire baseline source, not just bench files, and
does not touch the current branch. All four Criterion suites must produce
the same metric set. A metric whose mean confidence-interval lower bound
exceeds +5% in at least two of three runs fails. This is an ezpn revision
comparison, not a tmux/Zellij comparison or a live-daemon RSS benchmark.
