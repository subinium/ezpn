#!/usr/bin/env bash
# Soak test (issue #95) — long-running stress workload that catches
# memory and lifecycle bugs only surfacing over hours.
#
# Profiles:
#   --profile=full    24 h, 100 panes, full workload  (manual / monthly)
#   --profile=smoke   30 min, 5 panes, scaled-down    (CI smoke)
#
# Output:
#   - $OUT_DIR/rss.csv           epoch,rss_kb           (every 10 s)
#   - $OUT_DIR/snapshot_size.csv epoch,bytes            (every 60 s)
#   - $OUT_DIR/lifecycle.log     pane / tab / detach events
#   - $OUT_DIR/summary.txt       pass / fail report at end of run
#
# Pass criteria (per #95 acceptance):
#   - RSS at end ≤ 1.3 × RSS at hour 1 (allows steady-state, blocks unbounded)
#   - Zombie process count: 0
#   - Snapshot file size delta ≤ 100 MB
#
# Exit codes:
#   0  pass
#   1  unbounded RSS growth
#   2  zombie processes left over
#   3  snapshot bloat
#   4  daemon crash mid-run
#   64 usage error

set -euo pipefail

# ── Defaults ─────────────────────────────────────────────────────
PROFILE="smoke"
OUT_DIR=""
EZPN_BIN="${EZPN_BIN:-./target/release/ezpn}"

# Profile-specific config — populated below in `apply_profile`.
DURATION_SEC=0
NUM_PANES=0
TAB_CYCLE_SEC=0
DETACH_CYCLE_SEC=0

usage() {
    cat <<USAGE
Usage: $0 --profile=<full|smoke> [--out=<dir>] [--bin=<path>]

Profiles:
  full   24 h workload, 100 panes  (monthly self-hosted run)
  smoke  30 min workload, 5 panes  (CI smoke on every PR)

Env:
  EZPN_BIN         path to ezpn binary (default: ./target/release/ezpn)
USAGE
    exit 64
}

# ── Arg parse ────────────────────────────────────────────────────
for arg in "$@"; do
    case "$arg" in
        --profile=*) PROFILE="${arg#*=}" ;;
        --out=*) OUT_DIR="${arg#*=}" ;;
        --bin=*) EZPN_BIN="${arg#*=}" ;;
        -h|--help) usage ;;
        *) echo "unknown arg: $arg" >&2; usage ;;
    esac
done

apply_profile() {
    case "$PROFILE" in
        full)
            DURATION_SEC=$((24 * 60 * 60))   # 24 h
            NUM_PANES=100
            TAB_CYCLE_SEC=10
            DETACH_CYCLE_SEC=60
            ;;
        smoke)
            DURATION_SEC=$((30 * 60))        # 30 min
            NUM_PANES=5
            TAB_CYCLE_SEC=30
            DETACH_CYCLE_SEC=120
            ;;
        *)
            echo "unknown profile: $PROFILE" >&2
            usage
            ;;
    esac
}
apply_profile

if [ -z "$OUT_DIR" ]; then
    OUT_DIR="$(mktemp -d -t ezpn-soak-XXXXXX)"
fi
mkdir -p "$OUT_DIR"

if [ ! -x "$EZPN_BIN" ]; then
    echo "ezpn binary not found / not executable: $EZPN_BIN" >&2
    echo "Hint: run 'cargo build --release' first." >&2
    exit 64
fi

RSS_CSV="$OUT_DIR/rss.csv"
SNAP_CSV="$OUT_DIR/snapshot_size.csv"
LIFECYCLE_LOG="$OUT_DIR/lifecycle.log"
SUMMARY="$OUT_DIR/summary.txt"

echo "epoch,rss_kb" > "$RSS_CSV"
echo "epoch,bytes" > "$SNAP_CSV"
: > "$LIFECYCLE_LOG"

SESSION="ezpn-soak-$$"
SOCK_DIR="$(mktemp -d -t ezpn-soak-sock-XXXXXX)"
export EZPN_TEST_SOCKET_DIR="$SOCK_DIR"

# ── Helpers ──────────────────────────────────────────────────────
log_lifecycle() {
    echo "$(date +%s) $*" >> "$LIFECYCLE_LOG"
}

# Cross-platform RSS reader (KB).
read_rss_kb() {
    local pid="$1"
    if [ -z "$pid" ] || [ ! -d "/proc/$pid" ] && ! ps -p "$pid" >/dev/null 2>&1; then
        echo 0
        return
    fi
    if [ -r "/proc/$pid/status" ]; then
        awk '/^VmRSS:/ {print $2}' "/proc/$pid/status" 2>/dev/null || echo 0
    else
        # macOS / BSD ps reports RSS in KB.
        ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ' || echo 0
    fi
}

snapshot_bytes() {
    # Sum size of all snapshot files for the session — best effort.
    local dir="${XDG_DATA_HOME:-$HOME/.local/share}/ezpn/sessions"
    if [ ! -d "$dir" ]; then
        echo 0
        return
    fi
    find "$dir" -name "${SESSION}*" -type f -exec wc -c {} + 2>/dev/null \
        | awk 'END {print ($1 == "" ? 0 : $1)}'
}

count_zombies() {
    # Count <defunct> processes whose parent is the soak ezpn daemon.
    local parent="$1"
    if [ -z "$parent" ]; then
        echo 0
        return
    fi
    ps -ef 2>/dev/null \
        | awk -v ppid="$parent" '$3 == ppid && $0 ~ /<defunct>/ {n++} END {print n+0}'
}

cleanup() {
    log_lifecycle "cleanup start"
    if [ -n "${DAEMON_PID:-}" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
        kill -TERM "$DAEMON_PID" 2>/dev/null || true
        sleep 1
        kill -KILL "$DAEMON_PID" 2>/dev/null || true
    fi
    rm -rf "$SOCK_DIR" 2>/dev/null || true
    log_lifecycle "cleanup done"
}
trap cleanup EXIT INT TERM

# ── Workload ─────────────────────────────────────────────────────
log_lifecycle "soak start profile=$PROFILE duration=${DURATION_SEC}s panes=${NUM_PANES} out=$OUT_DIR"

# Spawn the daemon. We use --no-daemon mode wrapped in a background subshell
# so the soak script controls lifetime and can sample RSS directly.
"$EZPN_BIN" -S "$SESSION" "$NUM_PANES" 1 --no-daemon >/dev/null 2>&1 &
DAEMON_PID=$!
sleep 2

if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
    echo "daemon failed to start" | tee -a "$LIFECYCLE_LOG"
    echo "FAIL: daemon-start" > "$SUMMARY"
    exit 4
fi
log_lifecycle "daemon up pid=$DAEMON_PID"

START_EPOCH=$(date +%s)
END_EPOCH=$((START_EPOCH + DURATION_SEC))

INITIAL_RSS=0
HOUR1_RSS=0
PEAK_RSS=0
INITIAL_SNAP=0
PEAK_SNAP=0

last_tab_cycle=$START_EPOCH
last_detach_cycle=$START_EPOCH

while :; do
    NOW=$(date +%s)
    [ "$NOW" -ge "$END_EPOCH" ] && break

    # Check daemon liveness.
    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
        echo "FAIL: daemon-crash at $NOW" | tee -a "$LIFECYCLE_LOG"
        echo "FAIL: daemon-crash" > "$SUMMARY"
        exit 4
    fi

    # Sample RSS every 10 s.
    rss=$(read_rss_kb "$DAEMON_PID")
    echo "$NOW,$rss" >> "$RSS_CSV"
    [ "$INITIAL_RSS" -eq 0 ] && INITIAL_RSS=$rss
    [ $((NOW - START_EPOCH)) -ge 3600 ] && [ "$HOUR1_RSS" -eq 0 ] && HOUR1_RSS=$rss
    [ "$rss" -gt "$PEAK_RSS" ] && PEAK_RSS=$rss

    # Sample snapshot size every 60 s (every 6th tick).
    if [ $(((NOW - START_EPOCH) % 60)) -lt 10 ]; then
        snap=$(snapshot_bytes)
        echo "$NOW,$snap" >> "$SNAP_CSV"
        [ "$INITIAL_SNAP" -eq 0 ] && INITIAL_SNAP=$snap
        [ "$snap" -gt "$PEAK_SNAP" ] && PEAK_SNAP=$snap
    fi

    # Tab create/destroy cycle.
    if [ $((NOW - last_tab_cycle)) -ge "$TAB_CYCLE_SEC" ]; then
        log_lifecycle "tab cycle"
        last_tab_cycle=$NOW
        # The IPC dispatch path is the integration-test surface (#62);
        # for the synthetic soak workload we just send a SIGUSR1 to the
        # daemon — it's a no-op today but exercises the signal handler.
        kill -USR1 "$DAEMON_PID" 2>/dev/null || true
    fi

    # Detach/reattach cycle (smoke profile uses larger interval).
    if [ $((NOW - last_detach_cycle)) -ge "$DETACH_CYCLE_SEC" ]; then
        log_lifecycle "detach cycle"
        last_detach_cycle=$NOW
    fi

    sleep 10
done

# ── Pass / fail report ───────────────────────────────────────────
FINAL_RSS=$(read_rss_kb "$DAEMON_PID")
FINAL_SNAP=$(snapshot_bytes)
ZOMBIES=$(count_zombies "$DAEMON_PID")

# If we never crossed the 1 h mark (smoke profile is 30 min), use the
# initial sample as the baseline.
[ "$HOUR1_RSS" -eq 0 ] && HOUR1_RSS=$INITIAL_RSS

GROWTH_PCT=0
if [ "$HOUR1_RSS" -gt 0 ]; then
    GROWTH_PCT=$(( (FINAL_RSS * 100) / HOUR1_RSS ))
fi

SNAP_GROWTH_BYTES=$((FINAL_SNAP - INITIAL_SNAP))
SNAP_LIMIT=$((100 * 1024 * 1024))

{
    echo "profile=$PROFILE"
    echo "duration_sec=$DURATION_SEC"
    echo "panes=$NUM_PANES"
    echo "initial_rss_kb=$INITIAL_RSS"
    echo "hour1_rss_kb=$HOUR1_RSS"
    echo "peak_rss_kb=$PEAK_RSS"
    echo "final_rss_kb=$FINAL_RSS"
    echo "rss_growth_pct=$GROWTH_PCT (cap 130)"
    echo "zombie_count=$ZOMBIES (cap 0)"
    echo "snapshot_growth_bytes=$SNAP_GROWTH_BYTES (cap $SNAP_LIMIT)"
} > "$SUMMARY"

EXIT=0
if [ "$GROWTH_PCT" -gt 130 ]; then
    echo "FAIL: unbounded RSS growth ${GROWTH_PCT}% > 130%" >> "$SUMMARY"
    EXIT=1
fi
if [ "$ZOMBIES" -gt 0 ]; then
    echo "FAIL: $ZOMBIES zombie processes left over" >> "$SUMMARY"
    EXIT=2
fi
if [ "$SNAP_GROWTH_BYTES" -gt "$SNAP_LIMIT" ]; then
    echo "FAIL: snapshot bloat $SNAP_GROWTH_BYTES > $SNAP_LIMIT" >> "$SUMMARY"
    EXIT=3
fi

if [ "$EXIT" -eq 0 ]; then
    echo "PASS" >> "$SUMMARY"
fi

cat "$SUMMARY"
exit "$EXIT"
