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


def protocol_compiler_artifact(log_path, target_dir):
    """Select only the benchmark executable reported by this Cargo invocation."""
    target_dir = target_dir.resolve(strict=True)
    artifacts = {}
    with log_path.open(errors="replace") as log:
        for line in log:
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if not isinstance(message, dict) or message.get("reason") != "compiler-artifact":
                continue
            target = message.get("target")
            if not isinstance(target, dict) or target.get("name") != "protocol_codec" or target.get("kind") != ["bench"]:
                continue
            executable = message.get("executable")
            if executable is None:
                continue
            if not isinstance(executable, str) or not executable or not Path(executable).is_absolute():
                raise ValueError("protocol_codec executable must be an absolute Cargo-reported path")
            path = Path(executable).resolve(strict=True)
            if not path.is_relative_to(target_dir) or not path.is_file() or not os.access(path, os.X_OK):
                raise ValueError(f"invalid protocol_codec executable outside the dedicated target or not executable: {path}")
            artifacts[path] = message
    if len(artifacts) != 1:
        raise ValueError(f"expected exactly one Cargo-reported protocol_codec executable, found {len(artifacts)}")
    return next(iter(artifacts.items()))


def preserve_protocol_executable(log_path, target_dir, out, label, command, failure=None):
    cargo_options = command[:command.index("--")] if "--" in command else command
    unmeasured = "--no-run" in cargo_options
    if label not in ["baseline", "candidate-1", "candidate-2", "candidate-3"] and not (label == "smoke-unmeasured" and unmeasured):
        raise ValueError(f"unexpected benchmark label: {label}")
    executable, cargo_artifact = protocol_compiler_artifact(log_path, target_dir)
    digest = hashlib.sha256(executable.read_bytes()).hexdigest()

    def copy_with_metadata(artifact_name):
        destination = out / artifact_name
        if destination.is_symlink():
            raise ValueError(f"refusing symlink artifact destination: {destination}")
        shutil.copy2(executable, destination)
        if hashlib.sha256(destination.read_bytes()).hexdigest() != digest:
            raise ValueError("protocol_codec executable changed while copying")
        metadata = {
            "label": label,
            "artifact": destination.name,
            "source_executable": str(executable),
            "dedicated_target": str(target_dir.resolve()),
            "sha256": digest,
            "size_bytes": destination.stat().st_size,
            "cargo_log": log_path.name,
            "command": list(map(str, command)),
            "execution_kind": "unmeasured-no-run" if unmeasured else "benchmark",
            "cargo_exit_code": getattr(failure, "returncode", None) if failure else 0,
            "timed_out": isinstance(failure, subprocess.TimeoutExpired),
            "package_id": cargo_artifact.get("package_id"),
            "target": cargo_artifact["target"],
            "profile": cargo_artifact.get("profile"),
            "features": cargo_artifact.get("features"),
            "fresh": cargo_artifact.get("fresh"),
        }
        (out / f"{artifact_name}.json").write_text(json.dumps(metadata, indent=2) + "\n")

    # Preserve each candidate run even if a future Cargo invocation relinks it.
    copy_with_metadata(f"protocol-codec-{label}")
    if label == "candidate-1":
        copy_with_metadata("protocol-codec-candidate")


def run_benchmark(command, *, source, env, target, log_path, out, label, capture_protocol):
    failure = None
    try:
        with log_path.open("w") as log:
            subprocess.run(command, cwd=source, env=env, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=600)
    except (subprocess.SubprocessError, OSError) as error:
        failure = error
    if capture_protocol:
        try:
            preserve_protocol_executable(log_path, target, out, label, command, failure)
        except (ValueError, OSError) as error:
            try:
                (out / f"protocol-codec-{label}-capture-error.json").write_text(json.dumps({
                    "label": label, "error": str(error), "cargo_log": log_path.name,
                }, indent=2) + "\n")
            except OSError as report_error:
                print(f"could not record protocol artifact error: {report_error}", file=sys.stderr)
            if failure is None:
                raise
            print(f"protocol artifact capture also failed: {error}", file=sys.stderr)
    if failure is not None:
        raise failure


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
            command = ["cargo", "bench", "--locked", "--all-features", "--bench", name,
                       "--message-format=json-render-diagnostics", "--", *arguments]
            run_benchmark(command, source=source, env=env, target=target,
                          log_path=out / f"{label}-{name}.log", out=out,
                          label=label, capture_protocol=name == "protocol_codec")

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
