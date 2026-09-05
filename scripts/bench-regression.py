#!/usr/bin/env python3
"""Compare complete source revisions, requiring all four suites and real estimates."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

BENCHES = ["render_hotpaths", "protocol_codec", "snapshot_io", "rss_proxy"]
MEASUREMENT_MODE = "criterion-save-main-empty-v1"
SAVED_FILES = ("benchmark.json", "sample.json", "estimates.json", "tukey.json")
EXPECTED_METRICS = 22


def compiler_artifact(log_path, target_dir, name):
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
            if not isinstance(target, dict) or target.get("name") != name or target.get("kind") != ["bench"]:
                continue
            executable = message.get("executable")
            if executable is None:
                continue
            if not isinstance(executable, str) or not executable or not Path(executable).is_absolute():
                raise ValueError(f"{name} executable must be an absolute Cargo-reported path")
            path = Path(executable).resolve(strict=True)
            if not path.is_relative_to(target_dir) or not path.is_file() or not os.access(path, os.X_OK):
                raise ValueError(f"invalid {name} executable outside the dedicated target or not executable: {path}")
            artifacts[path] = message
    if len(artifacts) != 1:
        raise ValueError(f"expected exactly one Cargo-reported {name} executable, found {len(artifacts)}")
    return next(iter(artifacts.items()))


def protocol_compiler_artifact(log_path, target_dir):
    return compiler_artifact(log_path, target_dir, "protocol_codec")


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


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def tree_hashes(root):
    hashes = {}
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise ValueError(f"symlink in Criterion evidence: {path}")
        if path.is_file():
            hashes[str(path.relative_to(root))] = digest(path)
    return hashes


def saved_metrics(root, baseline="main"):
    metrics = {}
    for directory in sorted(root.glob(f"**/{baseline}")):
        if not directory.is_dir() or directory.is_symlink():
            raise ValueError(f"invalid saved baseline directory: {directory}")
        metric = str(directory.parent.relative_to(root))
        for name in SAVED_FILES:
            path = directory / name
            if path.is_symlink() or not path.is_file():
                raise ValueError(f"missing/invalid Criterion file: {path}")
            json.loads(path.read_text())
        benchmark = json.loads((directory / "benchmark.json").read_text())
        if benchmark["directory_name"] != metric:
            raise ValueError(f"Criterion metric identity mismatch: {metric}")
        sample = json.loads((directory / "sample.json").read_text())
        iters, times = sample["iters"], sample["times"]
        if (sample["sampling_mode"] not in ("Linear", "Flat") or not iters or len(iters) != len(times)
                or any(isinstance(value, bool) or not isinstance(value, (int, float))
                       or not math.isfinite(value) or value <= 0 for value in [*iters, *times])):
            raise ValueError(f"invalid Criterion sample: {metric}")
        metrics[metric] = directory
    if not metrics:
        raise ValueError("no saved Criterion metrics")
    return metrics


def compiler_identity(source):
    return subprocess.check_output([os.environ.get("RUSTC", "rustc"), "-Vv"], cwd=source, text=True)


def source_contract(source):
    files = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "rust-toolchain", ".cargo/config.toml", ".cargo/config"]
    return {
        "bench_source_sha256": {name: digest(source / "benches" / f"{name}.rs") for name in BENCHES},
        "configuration_sha256": {name: digest(source / name) if (source / name).is_file() else None for name in files},
        "rustc_verbose": compiler_identity(source),
        "compiler_environment": {name: os.environ.get(name) for name in (
            "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTUP_TOOLCHAIN", "RUSTC", "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_TARGET",
        )},
    }


def runtime_environment(out):
    env = dict(os.environ)
    env["CARGO_HOME"] = env.get("CARGO_HOME", str(Path.home() / ".cargo"))
    env["RUSTUP_HOME"] = env.get("RUSTUP_HOME", str(Path.home() / ".rustup"))
    for key in ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "XDG_RUNTIME_DIR", "EZPN_TEST_SOCKET_DIR"]:
        path = out / "runtime" / key.lower()
        path.mkdir(mode=0o700, parents=True, exist_ok=True)
        env[key] = str(path)
    env.pop("EZPN", None)
    env.pop("CARGO_CRITERION_PORT", None)
    return env


def measure(source, target, out, label, revision):
    active = out / "criterion-current"
    archive = out / "raw" / label
    manifest_path = out / f"measurement-{label}.json"
    if active.exists() or archive.exists() or manifest_path.exists():
        raise ValueError("measurement evidence already exists; use a fresh --out")
    archive.parent.mkdir(parents=True, exist_ok=True)
    active.mkdir()
    env = dict(runtime_environment(out), CARGO_TARGET_DIR=str(target), CRITERION_HOME=str(active))
    manifest = {"mode": MEASUREMENT_MODE, "status": "RUNNING", "label": label,
                "revision": revision, "execution_kind": "benchmark", "empty_before_measurement": True,
                "criterion_home": str(active), "source_contract": source_contract(source),
                "commands": [], "executables": {}}
    failure = None
    try:
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
        for name in BENCHES:
            command = ["cargo", "bench", "--locked", "--all-features", "--bench", name,
                       "--message-format=json-render-diagnostics", "--", "--save-baseline", "main"]
            log_path = out / f"{label}-{name}.log"
            invocation = {"command": command, "cargo_log": log_path.name}
            manifest["commands"].append(invocation)
            manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
            run_benchmark(command, source=source, env=env, target=target, log_path=log_path,
                          out=out, label=label, capture_protocol=name == "protocol_codec")
            invocation["exit_code"] = 0
            executable, artifact = compiler_artifact(log_path, target, name)
            copied = out / "executables" / label / name
            copied.parent.mkdir(parents=True, exist_ok=True)
            if copied.exists() or copied.is_symlink():
                raise ValueError(f"refusing existing executable evidence: {copied}")
            sha256 = digest(executable)
            shutil.copy2(executable, copied)
            if digest(copied) != sha256:
                raise ValueError("measured executable changed while copying")
            manifest["executables"][name] = {
                "path": str(copied.relative_to(out)), "sha256": sha256, "source_executable": str(executable),
                "command": command, "cargo_log": log_path.name, "cargo_exit_code": 0,
                "target": artifact["target"], "profile": artifact.get("profile"),
                "features": artifact.get("features"), "package_id": artifact.get("package_id"),
                "fresh": artifact.get("fresh"), "execution_kind": "benchmark",
            }
            manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
        metrics = saved_metrics(active)
        if len(metrics) != EXPECTED_METRICS:
            raise ValueError(f"expected {EXPECTED_METRICS} measured metrics, found {len(metrics)}")
        if set(saved_metrics(active, "new")) != set(metrics):
            raise ValueError("new/main Criterion metric sets differ")
        for metric, directory in metrics.items():
            if tree_hashes(directory) != tree_hashes(active / metric / "new"):
                raise ValueError(f"new/main Criterion files differ: {metric}")
        if list(active.glob("**/change/estimates.json")):
            raise ValueError("unexpected online comparison during symmetric measurement")
        manifest["metrics"] = sorted(metrics)
        manifest["status"] = "PASS"
    except BaseException as error:
        failure = error
        manifest.update(status="FAIL", error=str(error), exit_code=getattr(error, "returncode", None),
                        timed_out=isinstance(error, subprocess.TimeoutExpired))
    finally:
        try:
            # Archive, never delete, even partially generated samples on failure.
            active.rename(archive)
            manifest["raw_sha256"] = tree_hashes(archive)
            manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
        except (OSError, ValueError) as error:
            if failure is None:
                raise
            print(f"measurement evidence archival also failed: {error}", file=sys.stderr)
    if failure is not None:
        raise failure
    return manifest


def prepare_output(out, reuse, base):
    if out.is_symlink():
        raise ValueError("refusing symlink output directory")
    if not reuse:
        if out.exists() and any(out.iterdir()):
            raise ValueError("refusing existing evidence; use a fresh --out")
        out.mkdir(parents=True, exist_ok=True)
        return
    path = out / "measurement-baseline.json"
    if not path.is_file():
        raise ValueError("legacy/unproven baseline reuse is not supported")
    manifest = json.loads(path.read_text())
    if (manifest.get("mode") != MEASUREMENT_MODE or manifest.get("status") != "PASS"
            or manifest.get("revision") != base or manifest.get("empty_before_measurement") is not True
            or manifest.get("execution_kind") != "benchmark"):
        raise ValueError("baseline measurement mode/revision proof is invalid")
    if (out / "base.txt").read_text().strip() != base:
        raise ValueError("saved baseline revision does not match requested revision")
    if any((out / name).exists() for name in ["candidate-source-sha256.json", "analysis", "criterion-current"]):
        raise ValueError("candidate/partial evidence already exists; use a fresh --out")
    if list(out.glob("candidate-*")) or list(out.glob("measurement-candidate-*")) or list((out / "raw").glob("candidate-*")):
        raise ValueError("candidate evidence already exists; use a fresh --out")
    if manifest.get("raw_sha256") != tree_hashes(out / "raw" / "baseline"):
        raise ValueError("saved baseline raw evidence hash mismatch")
    metrics = saved_metrics(out / "raw" / "baseline")
    if len(metrics) != EXPECTED_METRICS or sorted(metrics) != manifest.get("metrics"):
        raise ValueError("incomplete saved baseline metric set")
    if set(manifest.get("executables", {})) != set(BENCHES):
        raise ValueError("incomplete saved baseline executable provenance")
    for name, record in manifest["executables"].items():
        expected = ["cargo", "bench", "--locked", "--all-features", "--bench", name,
                    "--message-format=json-render-diagnostics", "--", "--save-baseline", "main"]
        if record.get("command") != expected or record.get("cargo_exit_code") != 0:
            raise ValueError("saved baseline command provenance mismatch")
        path = (out / record["path"]).resolve(strict=True)
        if not path.is_relative_to(out.resolve()) or digest(path) != record["sha256"]:
            raise ValueError("saved baseline executable hash mismatch")


def offline_analysis(baseline, candidate, executables, out, label, env, expected_hashes):
    sources = {"baseline": tree_hashes(baseline), "candidate": tree_hashes(candidate)}
    if sources != expected_hashes:
        raise ValueError("raw evidence changed since measurement manifest was recorded")
    directory = out / "analysis" / label
    directory.mkdir(parents=True, exist_ok=False)
    staged = directory / "criterion"
    base_metrics, candidate_metrics = saved_metrics(baseline), saved_metrics(candidate)
    if set(base_metrics) != set(candidate_metrics):
        raise ValueError("missing/added metrics require reviewed baseline update")
    for metric in base_metrics:
        shutil.copytree(base_metrics[metric], staged / metric / "main")
        shutil.copytree(candidate_metrics[metric], staged / metric / label)
    staged_inputs = tree_hashes(staged)
    record = {"execution_kind": "unmeasured-analysis", "status": "RUNNING", "commands": [],
              "input_sha256": sources, "staged_input_sha256": staged_inputs}
    record_path = directory / "status.json"
    record_path.write_text(json.dumps(record, indent=2) + "\n")
    failure = None
    estimates = None
    try:
        for name, executable in executables.items():
            path = (out / executable["path"]).resolve(strict=True)
            if not path.is_relative_to(out.resolve()) or digest(path) != executable["sha256"]:
                raise ValueError("analysis executable differs from measured executable")
            command = [str(path), "--bench", "--baseline", "main", "--load-baseline", label]
            invocation = {"command": command, "sha256": executable["sha256"], "suite": name,
                          "execution_kind": "unmeasured-analysis",
                          "executable_origin": executable.get("execution_kind")}
            record["commands"].append(invocation)
            record_path.write_text(json.dumps(record, indent=2) + "\n")
            analysis_env = dict(env, CRITERION_HOME=str(staged))
            analysis_env.pop("CARGO_CRITERION_PORT", None)
            with (directory / f"{name}.log").open("x") as log:
                result = subprocess.run(command, env=analysis_env, stdout=log, stderr=subprocess.STDOUT, timeout=600)
            invocation["exit_code"] = result.returncode
            result.check_returncode()
        estimates = {str(path.parent.parent.relative_to(staged)): json.loads(path.read_text())
                     for path in staged.glob("**/change/estimates.json")}
        if set(estimates) != set(base_metrics):
            raise ValueError("missing/added offline Criterion comparison metrics")
        record["status"] = "PASS"
    except BaseException as error:
        failure = error
        record.update(status="FAIL", error=str(error), exit_code=getattr(error, "returncode", None),
                      timed_out=isinstance(error, subprocess.TimeoutExpired))
    finally:
        try:
            after = {"baseline": tree_hashes(baseline), "candidate": tree_hashes(candidate)}
            staged_after = {name: value for name, value in tree_hashes(staged).items()
                            if any(name.startswith(f"{metric}/{side}/")
                                   for metric in base_metrics for side in ("main", label))}
            record["input_sha256_after"] = after
            record["staged_input_sha256_after"] = staged_after
            if after != sources or staged_after != staged_inputs:
                raise ValueError("Criterion raw samples changed during offline analysis")
        except (OSError, ValueError) as error:
            record.update(status="FAIL", input_verification_error=str(error))
            if failure is None:
                failure = error
        try:
            record_path.write_text(json.dumps(record, indent=2) + "\n")
        except OSError as error:
            if failure is None:
                raise
            print(f"analysis evidence recording also failed: {error}", file=sys.stderr)
    if failure is not None:
        raise failure
    return estimates


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    parser.add_argument("--out", type=Path, default=Path("target/bench-evidence"))
    parser.add_argument("--baseline-only", action="store_true")
    parser.add_argument("--reuse-baseline", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    if args.out.is_symlink():
        raise ValueError("refusing symlink output directory")
    out = args.out.resolve()
    base = subprocess.check_output(["git", "rev-parse", "--verify", args.base + "^{commit}"], cwd=root, text=True).strip()
    prepare_output(out, args.reuse_baseline, base)
    if not args.reuse_baseline:
        (out / "base.txt").write_text(base + "\n")

    # Never check out files over the developer's worktree. The baseline needs
    # its src/ as well as benches/ and lockfile, otherwise it is a self-compare.
    with tempfile.TemporaryDirectory(prefix="ezpn-bench-") as scratch:
        source = Path(scratch)
        archive = source / "baseline.tar"
        subprocess.run(["git", "archive", "--format=tar", "-o", str(archive), base], cwd=root, check=True)
        subprocess.run(["tar", "xf", str(archive), "-C", str(source)], check=True)
        baseline_target = out / "base-target"
        candidate_target = out / "candidate-target"
        if not args.reuse_baseline:
            measure(source, baseline_target, out, "baseline", base)
        elif json.loads((out / "measurement-baseline.json").read_text())["source_contract"] != source_contract(source):
            raise ValueError("saved baseline source/configuration proof does not match")
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
        baseline_metrics = set(saved_metrics(out / "raw" / "baseline"))
        baseline_manifest = json.loads((out / "measurement-baseline.json").read_text())
        measurements = []
        revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
        for number in range(1, 4):
            manifest = measure(candidate_source, candidate_target, out, f"candidate-{number}", revision)
            if set(manifest["metrics"]) != baseline_metrics:
                raise ValueError("missing/added metrics require reviewed baseline update")
            measurements.append(manifest)
        # All timing is complete before any relative comparison allocation occurs.
        run_paths = []
        for number, manifest in enumerate(measurements, 1):
            label = f"candidate-{number}"
            estimates = offline_analysis(out / "raw" / "baseline", out / "raw" / label,
                                         manifest["executables"], out, label, runtime_environment(out),
                                         {"baseline": baseline_manifest["raw_sha256"], "candidate": manifest["raw_sha256"]})
            path = out / f"run-{number}.json"
            path.write_text(json.dumps(estimates, indent=2))
            run_paths.append(str(path))
        verdict = subprocess.run([sys.executable, str(root / "scripts/check-evidence.py"), "bench", *run_paths], capture_output=True, text=True)
        (out / "verdict.log").write_text(verdict.stdout + verdict.stderr)
        (out / "status.json").write_text(json.dumps({
            "status": "PASS" if verdict.returncode == 0 else "FAIL",
            "exit_code": verdict.returncode, "base": base,
            "measurement_mode": MEASUREMENT_MODE, "comparison_kind": "unmeasured-analysis",
            "runs": 3, "compared_metrics": len(baseline_metrics), "threshold_percent": 5,
        }, indent=2) + "\n")
        print(verdict.stdout + verdict.stderr, end="")
        verdict.check_returncode()


if __name__ == "__main__":
    main()
