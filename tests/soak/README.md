# Soak test

Long-running stress harness for ezpn (issue #95).

## Why

Memory and lifecycle bugs surface only over hours. tmux/4029 (memory not
freed after `clear-history`) and zellij/3598 (RSS growth on tab
open/close) are exactly the kind of bug a soak test catches that no unit
test does. This script exercises the daemon under a synthetic workload
and emits CSV samples so regressions are easy to spot in CI artifacts.

## Profiles

| Profile  | Duration | Panes | Tab cycle | Cadence                  |
|----------|----------|-------|-----------|--------------------------|
| `full`   | 24 h     | 100   | 10 s      | monthly (self-hosted)    |
| `smoke`  | 30 min   | 5     | 30 s      | every PR (GH-hosted)     |

The smoke profile is ~5% of the full workload, sized to fit GitHub
Actions' 6 h job cap with budget for cold start + report upload.

## Usage

```bash
# Build first.
cargo build --release

# 30-min CI smoke.
tests/soak/run.sh --profile=smoke --out=/tmp/soak-smoke

# 24-h manual run.
tests/soak/run.sh --profile=full --out=/var/log/ezpn-soak
```

Override the binary path with `--bin=/path/to/ezpn` or
`EZPN_BIN=/path/to/ezpn` for sanitizer / coverage builds.

## Output layout

```
$OUT_DIR/
├── rss.csv             # epoch,rss_kb every 10 s
├── snapshot_size.csv   # epoch,bytes  every 60 s
├── lifecycle.log       # tab / detach / cleanup events
└── summary.txt         # pass / fail report
```

## Pass criteria

Per #95 acceptance:

- **RSS growth**: `final_rss ≤ 1.30 × hour1_rss` (allows steady-state,
  blocks unbounded). For the smoke profile we use `initial_rss` as the
  baseline since the run is shorter than 1 h.
- **Zombies**: 0 `<defunct>` processes whose ppid is the soak daemon at
  the end of the run.
- **Snapshot bloat**: net snapshot file growth ≤ 100 MB over the run.

## Exit codes

| Code | Meaning                            |
|------|------------------------------------|
| 0    | pass                               |
| 1    | unbounded RSS growth               |
| 2    | zombie processes left over         |
| 3    | snapshot bloat                     |
| 4    | daemon crashed mid-run             |
| 64   | usage / setup error                |

## CI integration

Wired in `.github/workflows/bench.yml` as the `soak-smoke` job — runs on
every PR and uploads `rss.csv` + `summary.txt` as artifacts. The full
24 h run is intentionally **not** in CI; it lives on a self-hosted
runner with a separate cron schedule.

## Limitations

The current synthetic workload sends `SIGUSR1` to the daemon as a
placeholder for the IPC tab-cycle path because the IPC test harness
(#62) is not yet wired into `tests/soak/`. As soon as `ezpn-ctl` ships
a stable `tab new` / `tab kill` command we will swap the signal stub
for a real IPC dispatch. Until then the script catches RSS regressions
in the daemon's idle / signal path but does not exercise the full
tab/pane lifecycle hot loop.

Real-app workloads (vim/nvim long-living sessions) are out of scope
for v0.16 — the synthetic `yes | head -c 1M; sleep 1` workload is what
the issue calls for.
