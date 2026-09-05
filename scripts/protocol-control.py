#!/usr/bin/env python3
"""Diagnostic only: compare each unchanged measured ELF against itself."""
import hashlib
import json
import os
from pathlib import Path
import subprocess


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
    report = {"kind": "IDENTICAL_EXECUTABLE_DIAGNOSTIC_NOT_RELEASE_GATE", "runs": []}
    for command in [["uname", "-a"], ["ldd", "--version"], ["lscpu"]]:
        with (out / (command[0] + ".log")).open("w") as log:
            subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, check=True)
    for label, expected in identities.items():
        binary = source / ("protocol-codec-" + label)
        actual = hashlib.sha256(binary.read_bytes()).hexdigest()
        metadata = json.loads(binary.with_suffix(".json").read_text())
        if actual != expected or actual != metadata["sha256"]:
            raise ValueError("measured executable identity mismatch")
        binary.chmod(0o700)
        criterion = out / label
        env["CRITERION_HOME"] = str(criterion)
        for mode in ["--save-baseline", "--baseline"]:
            command = [str(binary), "--bench", mode, "main"]
            entry = {"binary": label, "sha256": actual, "command": command}
            report["runs"].append(entry)
            with (out / (label + mode + ".log")).open("w") as log:
                result = subprocess.run(command, env=env, stdout=log,
                                        stderr=subprocess.STDOUT, timeout=180)
            entry["exit_code"] = result.returncode
            (out / "control.json").write_text(json.dumps(report, indent=2) + "\n")
            result.check_returncode()
        changes = {
            str(path.parent.parent.relative_to(criterion)): json.loads(path.read_text())
            for path in criterion.glob("**/change/estimates.json")
        }
        if len(changes) != 8:
            raise ValueError("missing protocol control estimates")
        (out / (label + "-changes.json")).write_text(json.dumps(changes, indent=2) + "\n")
    report["execution_completed"] = True
    (out / "control.json").write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
