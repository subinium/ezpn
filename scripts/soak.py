#!/usr/bin/env python3
"""Real multi-pane workload, tab lifecycle, detach/reattach and snapshot checks."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import shutil
import sys
import tempfile
import time
import traceback

from runtime_driver import Daemon, read_complete, wait_for


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=["smoke", "full"], default="smoke")
    parser.add_argument("--duration", type=int, help="Short diagnostic override; reported as custom, never a completed smoke/full soak")
    parser.add_argument("--out", type=Path, default=Path("target/soak"))
    parser.add_argument("--bin", type=Path, default=Path(os.environ.get("EZPN_BIN", "target/release/ezpn")))
    parser.add_argument("--build-features", choices=["default", "all-features", "unspecified"], default="unspecified",
                        help="Feature set used by the recorded build command, not inferred from the filename")
    args = parser.parse_args()
    duration = args.duration if args.duration is not None else 86400 if args.profile == "full" else 1800
    if duration < 10:
        parser.error("duration must be at least 10 seconds")
    count = 100 if args.profile == "full" else 5
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    summary = {"status": "FAIL", "profile": "custom" if args.duration is not None else args.profile,
               "requested_duration_seconds": duration, "panes": count, "build_features": args.build_features}
    daemon = client = None
    scratch = tempfile.TemporaryDirectory(prefix="ezpn-soak-", dir="/tmp")
    start = time.monotonic()
    try:
        root = Path(scratch.name)
        summary["binary_path"] = str(args.bin.resolve())
        summary["binary_sha256"] = hashlib.sha256(args.bin.read_bytes()).hexdigest()
        daemon = Daemon(args.bin, root, out, session="soak", panes=count)
        summary["binary_version"] = subprocess.check_output([daemon.binary, "--version"], env=daemon.env, text=True, timeout=5).strip()
        client = daemon.attach()
        listed = daemon.ipc({"cmd": "list"})["panes"]
        if len(listed) != count:
            raise RuntimeError(f"expected {count} live panes, got {len(listed)}")
        workload = "while :; do head -c 1048576 /dev/zero | tr '\\000' x; sleep 1; done"
        for pane in listed:
            command = f"head -c 1048576 /dev/zero | tr '\\000' x; printf 'ready\\n' > {root / ('ready-' + str(pane['id']))}; " + workload
            daemon.ipc({"cmd": "exec", "pane": pane["id"], "command": command})
        for pane in listed:
            wait_for(lambda pane=pane: read_complete(root / f"ready-{pane['id']}"), "first workload burst completed", client)
        summary["setup_seconds"] = round(time.monotonic() - start, 3)
        start = time.monotonic()
        rss_values = []
        hour_one = None
        snapshots = []
        cycles = 0
        next_cycle = 0.0
        with (out / "rss.csv").open("w") as rss_log, (out / "snapshot_size.csv").open("w") as snapshot_log, (out / "lifecycle.log").open("w") as lifecycle:
            rss_log.write("elapsed_seconds,rss_kb\n")
            snapshot_log.write("elapsed_seconds,bytes\n")
            while time.monotonic() - start < duration:
                daemon.alive()
                elapsed = time.monotonic() - start
                rss = int(subprocess.check_output(["ps", "-o", "rss=", "-p", str(daemon.process.pid)], text=True, timeout=5).strip())
                if rss <= 0:
                    raise RuntimeError("missing or zero RSS measurement")
                rss_values.append(rss)
                if elapsed >= 3600 and hour_one is None:
                    hour_one = rss
                rss_log.write(f"{elapsed:.3f},{rss}\n")
                rss_log.flush()
                if elapsed >= next_cycle:
                    cycles += 1
                    client.write("\x02c")
                    wait_for(lambda: len(daemon.ipc({"cmd": "list"})["panes"]) == 1, "new tab active", client)
                    marker = root / f"tab-{cycles}"
                    client.write(f"printf 'tab-alive\\n' > {marker}\r")
                    wait_for(lambda: read_complete(marker), "new tab shell executed", client)
                    client.write("\x02&y")
                    wait_for(lambda: len(daemon.ipc({"cmd": "list"})["panes"]) == count, "close tab restored workload", client)
                    client.write("\x02d")
                    wait_for(lambda: client.poll() is not None, "detach exit", client)
                    if client.status != 0:
                        raise RuntimeError(f"detach exit {client.status}")
                    client.close()
                    client = daemon.attach()
                    snapshot = root / "snapshot.json"
                    daemon.ipc({"cmd": "save", "path": str(snapshot)})
                    parsed = json.loads(snapshot.read_text())
                    if len(parsed["tabs"]) != 1 or len(parsed["tabs"][0]["panes"]) != count:
                        raise RuntimeError("saved snapshot lost workload panes")
                    size = snapshot.stat().st_size + sum(p.stat().st_size for p in Path(daemon.env["XDG_DATA_HOME"]).rglob("*.json"))
                    snapshots.append(size)
                    snapshot_log.write(f"{elapsed:.3f},{size}\n")
                    snapshot_log.flush()
                    lifecycle.write(f"{elapsed:.3f} tab-created-and-closed detach-reattach snapshot-validated\n")
                    lifecycle.flush()
                    next_cycle = elapsed + (10 if args.duration is not None or args.profile == "full" else 30)
                deadline = min(time.monotonic() + 1, start + duration)
                while time.monotonic() < deadline:
                    time.sleep(.02)
        daemon.alive()
        final_rss = int(subprocess.check_output(["ps", "-o", "rss=", "-p", str(daemon.process.pid)], text=True, timeout=5))
        processes = subprocess.check_output(["ps", "-axo", "ppid=,stat="], text=True, timeout=5)
        zombies = sum(int(parts[0]) == daemon.process.pid and parts[1].startswith("Z") for line in processes.splitlines() if len(parts := line.split()) == 2)
        baseline = hour_one if hour_one is not None else rss_values[0]
        growth = final_rss / baseline
        snapshot_growth = snapshots[-1] - snapshots[0]
        summary.update(measured_duration_seconds=round(time.monotonic() - start, 3), samples=len(rss_values), cycles=cycles,
                       baseline_rss_kb=baseline, final_rss_kb=final_rss, peak_rss_kb=max(rss_values), rss_growth_ratio=growth,
                       zombie_count=zombies, snapshot_growth_bytes=snapshot_growth)
        if growth > 1.30 or zombies or snapshot_growth > 100 * 1024 * 1024 or not cycles:
            raise RuntimeError("RSS <=1.30x / zero zombies / snapshot growth <=100MiB criteria failed")
        summary["status"] = "PASS"
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        summary["reason"] = str(error)
        (out / "failure.txt").write_text(traceback.format_exc())
    finally:
        if client:
            summary["client_exit_code_before_cleanup"] = client.poll()
            summary["client_tail"] = client.output[-2048:].decode("utf-8", "replace")
            (out / "terminal.log").write_bytes(client.output)
            client.close(abrupt=True)
        if daemon:
            summary["daemon_exit_code_before_cleanup"] = daemon.process.poll()
            daemon.close()
            shutil.copytree(Path(daemon.env["XDG_STATE_HOME"]), out / "daemon-state", dirs_exist_ok=True)
        scratch.cleanup()
        summary.setdefault("measured_duration_seconds", round(time.monotonic() - start, 3))
        (out / "status.json").write_text(json.dumps(summary, indent=2) + "\n")
        (out / "summary.txt").write_text(json.dumps(summary, indent=2) + "\n")
        print(json.dumps(summary, indent=2))
    return int(summary["status"] != "PASS")


if __name__ == "__main__":
    sys.exit(main())
