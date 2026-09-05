#!/usr/bin/env python3
"""Diagnostic only: identical clean-save measurements, then load-only A/A analysis."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess


METRICS = {
    "frame_encode/256b", "frame_encode/4kb", "frame_encode/64kb",
    "frame_decode/256b", "frame_decode/4kb", "frame_decode/64kb",
    "resize_codec/encode", "resize_codec/decode",
}


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def metric_paths(criterion, stage):
    paths = {
        str(path.parent.parent.relative_to(criterion)): path
        for path in criterion.glob(f"**/{stage}/estimates.json")
    }
    if set(paths) != METRICS:
        raise ValueError(f"missing/added protocol metrics in {stage}: {set(paths) ^ METRICS}")
    return paths


def main():
    root = Path(__file__).resolve().parents[1]
    source = root / "target/control-input"
    out = root / "target/protocol-control"
    out.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    for name in ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME",
                 "XDG_CACHE_HOME", "XDG_RUNTIME_DIR", "EZPN_TEST_SOCKET_DIR"]:
        folder = out / "runtime" / name.lower()
        folder.mkdir(parents=True, exist_ok=True, mode=0o700)
        env[name] = str(folder)
    env.pop("EZPN", None)
    identities = {
        "baseline": "aa2b77cea1ccfac7faa3619f1befb070d094e59bef785351b889248c41e8f991",
        "candidate-1": "b24434f7e7eb4da7766fb67ae515489439d51f364346226ff6d40f7295498f49",
    }
    criterion = out / "active"
    env["CRITERION_HOME"] = str(criterion)
    report = {
        "kind": "IDENTICAL_EXECUTABLE_DIAGNOSTIC_NOT_RELEASE_GATE",
        "method": "CLEAN_SAVE_THEN_LOAD_ONLY_AA",
        "criterion_home": str(criterion),
        "expected_metrics": sorted(METRICS),
        "runs": [], "analyses": [], "execution_completed": False,
    }

    def save_report():
        (out / "control.json").write_text(json.dumps(report, indent=2) + "\n")

    def reset_active():
        if criterion.is_symlink():
            raise ValueError("refusing symlink CRITERION_HOME")
        if criterion.exists():
            shutil.rmtree(criterion)
        criterion.mkdir(mode=0o700)

    def archive_active(name, entry):
        archive = out / "raw" / name
        shutil.copytree(criterion, archive)
        hashes = {
            str(path.relative_to(archive)): sha256(path)
            for path in sorted(archive.rglob("*")) if path.is_file()
        }
        hash_path = out / (name + "-raw-sha256.json")
        hash_path.write_text(json.dumps(hashes, indent=2) + "\n")
        entry["raw_archive"] = str(archive.relative_to(out))
        entry["raw_hashes"] = hash_path.name
        save_report()
        return archive

    def invoke(name, label, arguments, phase):
        binary = binaries[label]
        if sha256(binary) != identities[label]:
            raise ValueError("measured executable identity changed")
        command = [str(binary), "--bench", *arguments]
        entry = {"name": name, "binary": label, "sha256": identities[label],
                 "command": command, "phase": phase, "log": name + ".log"}
        report["runs" if phase == "measurement" else "analyses"].append(entry)
        save_report()
        log_path = out / entry["log"]
        try:
            with log_path.open("w") as log:
                result = subprocess.run(command, env=env, stdout=log,
                                        stderr=subprocess.STDOUT, timeout=180)
            entry["exit_code"] = result.returncode
            result.check_returncode()
            if sha256(binary) != identities[label]:
                raise ValueError("measured executable identity changed during invocation")
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            entry["error"] = repr(error)
            entry["timed_out"] = isinstance(error, subprocess.TimeoutExpired)
            raise
        finally:
            if log_path.exists():
                entry["log_sha256"] = sha256(log_path)
            save_report()
            archive_active(name, entry)

    binaries = {}
    for label, expected in identities.items():
        binary = source / ("protocol-codec-" + label)
        actual = sha256(binary)
        metadata = json.loads(binary.with_suffix(".json").read_text())
        if actual != expected or actual != metadata["sha256"]:
            raise ValueError("measured executable identity mismatch")
        binary.chmod(0o700)
        binaries[label] = binary
    names = ["baselineA", "baselineB", "candidateA", "candidateB",
             "baseline-analysis", "candidate-1-analysis"]
    if any((out / "raw" / name).exists() for name in names):
        raise ValueError("refusing to overwrite earlier raw control measurements")
    save_report()
    for command in [["uname", "-a"], ["ldd", "--version"], ["lscpu"]]:
        with (out / (command[0] + ".log")).open("w") as log:
            subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, check=True)
    try:
        # All timing uses identical arguments and the same empty active path.
        for name, label in [("baselineA", "baseline"), ("baselineB", "baseline"),
                            ("candidateA", "candidate-1"), ("candidateB", "candidate-1")]:
            reset_active()
            invoke(name, label, ["--save-baseline", "main"], "measurement")
            for path in metric_paths(out / "raw" / name, "main").values():
                if not (path.parent / "sample.json").is_file():
                    raise ValueError(f"missing raw samples: {path.parent}")

        # Only now stage saved A/B data for comparison by the same ELF.
        for label, first, second in [("baseline", "baselineA", "baselineB"),
                                     ("candidate-1", "candidateA", "candidateB")]:
            reset_active()
            for metric in sorted(METRICS):
                shutil.copytree(out / "raw" / first / metric / "main",
                                criterion / metric / "main")
                shutil.copytree(out / "raw" / second / metric / "main",
                                criterion / metric / "candidate")
            invoke(label + "-analysis", label,
                   ["--baseline", "main", "--load-baseline", "candidate"], "load-only-analysis")
            changes = {
                metric: json.loads(path.read_text())
                for metric, path in metric_paths(criterion, "change").items()
            }
            (out / (label + "-changes.json")).write_text(json.dumps(changes, indent=2) + "\n")
        report["execution_completed"] = True
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        report["error"] = repr(error)
        raise
    finally:
        save_report()


if __name__ == "__main__":
    main()
