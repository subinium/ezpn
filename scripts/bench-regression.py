#!/usr/bin/env python3
"""Compare complete source revisions, requiring all four suites and real estimates."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

BENCHES = ["render_hotpaths", "protocol_codec", "snapshot_io", "rss_proxy"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    parser.add_argument("--out", type=Path, default=Path("target/bench-evidence"))
    parser.add_argument("--baseline-only", action="store_true")
    parser.add_argument("--reuse-baseline", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    base = subprocess.check_output(["git", "rev-parse", "--verify", args.base + "^{commit}"], cwd=root, text=True).strip()
    if args.reuse_baseline and (out / "base.txt").read_text().strip() != base:
        raise ValueError("saved baseline revision does not match requested revision")
    (out / "base.txt").write_text(base + "\n")

    def bench(source, target, arguments, label):
        env = dict(os.environ, CARGO_TARGET_DIR=str(target))
        env["CARGO_HOME"] = env.get("CARGO_HOME", str(Path.home() / ".cargo"))
        env["RUSTUP_HOME"] = env.get("RUSTUP_HOME", str(Path.home() / ".rustup"))
        for key in ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "XDG_RUNTIME_DIR", "EZPN_TEST_SOCKET_DIR"]:
            path = out / "runtime" / key.lower()
            path.mkdir(mode=0o700, parents=True, exist_ok=True)
            env[key] = str(path)
        env.pop("EZPN", None)
        for name in BENCHES:
            with (out / f"{label}-{name}.log").open("w") as log:
                subprocess.run(["cargo", "bench", "--locked", "--all-features", "--bench", name, "--", *arguments], cwd=source, env=env, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=600)

    # Never check out files over the developer's worktree. The baseline needs
    # its src/ as well as benches/ and lockfile, otherwise it is a self-compare.
    with tempfile.TemporaryDirectory(prefix="ezpn-bench-") as scratch:
        source = Path(scratch)
        archive = out / "baseline.tar"
        subprocess.run(["git", "archive", "--format=tar", "-o", str(archive), base], cwd=root, check=True)
        subprocess.run(["tar", "xf", str(archive), "-C", str(source)], check=True)
        baseline_target = out / "base-target"
        candidate_target = out / "candidate-target"
        # Remove only generated Criterion data from this dedicated output tree.
        for target in ([candidate_target] if args.reuse_baseline else [baseline_target, candidate_target]):
            if (target / "criterion").exists():
                shutil.rmtree(target / "criterion")
        if not args.reuse_baseline:
            bench(source, baseline_target, ["--save-baseline", "main"], "baseline")
        if args.baseline_only:
            return
        candidate_source = source / "candidate"
        candidate_source.mkdir()
        tracked = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=root).split(b"\0")
        hashes = {}
        for encoded in tracked:
            if not encoded:
                continue
            relative = Path(os.fsdecode(encoded))
            original = root / relative
            if not original.exists():
                continue
            destination = candidate_source / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(original, destination, follow_symlinks=False)
            hashes[str(relative)] = hashlib.sha256(destination.read_bytes()).hexdigest()
        (out / "candidate-source-sha256.json").write_text(json.dumps(hashes, indent=2))
        (out / "candidate.patch").write_bytes(subprocess.check_output(["git", "diff", "HEAD", "--binary"], cwd=root))
        shutil.copytree(baseline_target / "criterion", candidate_target / "criterion")
        baseline_metrics = {str(path.parent.parent.relative_to(baseline_target / "criterion")) for path in (baseline_target / "criterion").glob("**/main/estimates.json")}
        if not baseline_metrics:
            raise ValueError("baseline produced no Criterion estimates")
        run_paths = []
        for number in range(1, 4):
            for stale in (candidate_target / "criterion").glob("**/change/estimates.json"):
                stale.unlink()
            bench(candidate_source, candidate_target, ["--baseline", "main"], f"candidate-{number}")
            estimates = {str(path.parent.parent.relative_to(candidate_target / "criterion")): json.loads(path.read_text()) for path in (candidate_target / "criterion").glob("**/change/estimates.json")}
            if set(estimates) != baseline_metrics:
                raise ValueError(f"missing/added metrics require reviewed baseline update: {set(estimates) ^ baseline_metrics}")
            path = out / f"run-{number}.json"
            path.write_text(json.dumps(estimates, indent=2))
            run_paths.append(str(path))
        verdict = subprocess.run([sys.executable, str(root / "scripts/check-evidence.py"), "bench", *run_paths], capture_output=True, text=True)
        (out / "verdict.log").write_text(verdict.stdout + verdict.stderr)
        (out / "status.json").write_text(json.dumps({
            "status": "PASS" if verdict.returncode == 0 else "FAIL",
            "exit_code": verdict.returncode, "base": base,
            "runs": 3, "compared_metrics": len(baseline_metrics), "threshold_percent": 5,
        }, indent=2) + "\n")
        print(verdict.stdout + verdict.stderr, end="")
        verdict.check_returncode()


if __name__ == "__main__":
    main()
