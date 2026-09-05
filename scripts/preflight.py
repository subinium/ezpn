#!/usr/bin/env python3
"""Repeatable pre-CI/release checks with real exit codes and isolated runtime state."""
import argparse
import datetime
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=["ci", "release"], default="ci")
    parser.add_argument("--out", type=Path, default=Path("target/preflight"))
    parser.add_argument("--ssh", choices=["optional", "required", "skip"], default="optional")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    os.chdir(root)
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    results = []

    def run(name, command, env=None, timeout=1200, optional=False):
        start = time.monotonic()
        print(f"RUN {name}: {' '.join(map(str, command))}", flush=True)
        with (out / f"{name}.log").open("w") as log:
            try:
                process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, env=env, start_new_session=True)
                try:
                    code = process.wait(timeout=timeout)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
                    code = 124
            except OSError as error:
                log.write(str(error))
                code = 127
        status = "PASS" if code == 0 else "SKIP" if optional and code == 77 else "FAIL"
        results.append({"name": name, "status": status, "exit_code": code, "seconds": round(time.monotonic() - start, 3), "command": list(map(str, command))})
        (out / "status.json").write_text(json.dumps({"mode": args.mode, "timestamp_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(), "checks": results}, indent=2) + "\n")
        print(f"{status} {name} (exit {code})", flush=True)
        return code

    run("rustc", ["rustc", "--version", "--verbose"])
    run("revision", ["git", "rev-parse", "HEAD"])
    run("worktree", ["git", "status", "--short", "--branch"])
    run("script-tests", [sys.executable, "-m", "unittest", "discover", "-s", "scripts", "-p", "test_*.py"])
    run("fmt", ["cargo", "fmt", "--all", "--", "--check"])
    run("check-all", ["cargo", "check", "--locked", "--all-targets", "--all-features"])
    run("clippy", ["cargo", "clippy", "--locked", "--all-targets", "--all-features", "--", "-D", "warnings"])
    metadata_code = run("metadata", ["cargo", "metadata", "--locked", "--format-version=1", "--no-deps"])
    if metadata_code == 0:
        metadata = json.loads((out / "metadata.log").read_text())
        package = next(p for p in metadata["packages"] if p["name"] == "ezpn")
        msrv = package["rust_version"]
        if not msrv:
            raise ValueError("Cargo.toml does not declare an MSRV")
        # Explicit +toolchain overrides rust-toolchain.toml; installation is a
        # prerequisite, never an implicit replacement with the pinned toolchain.
        run("msrv-version", ["cargo", f"+{msrv}", "--version"])
        run("msrv", ["cargo", f"+{msrv}", "check", "--locked", "--all-targets", "--all-features"])
    with tempfile.TemporaryDirectory(prefix="ezpn-preflight-", dir="/tmp") as scratch:
        env = os.environ.copy()
        env["CARGO_HOME"] = env.get("CARGO_HOME", str(Path.home() / ".cargo"))
        env["RUSTUP_HOME"] = env.get("RUSTUP_HOME", str(Path.home() / ".rustup"))
        for key in ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "XDG_RUNTIME_DIR", "EZPN_TEST_SOCKET_DIR"]:
            path = Path(scratch) / key.lower()
            path.mkdir(mode=0o700)
            env[key] = str(path)
        env.pop("EZPN", None)
        for name, flags in [("default", []), ("all-features", ["--all-features"])]:
            run(f"discovery-{name}", ["cargo", "test", "--locked", *flags, "--", "--list"], env)
            run(f"tests-{name}", ["cargo", "test", "--locked", "--no-fail-fast", *flags], env)
            run(f"counts-{name}", [sys.executable, "scripts/check-evidence.py", "tests", str(out / f"tests-{name}.log")])
        if args.mode == "release":
            run("release-tests", ["cargo", "test", "--locked", "--no-fail-fast", "--release", "--all-features"], env)
            run("release-counts", [sys.executable, "scripts/check-evidence.py", "tests", str(out / "release-tests.log")])
            run("coverage", ["bash", "scripts/coverage.sh"], env)
    run("bench-build", ["cargo", "bench", "--locked", "--all-features", "--no-run"])
    # Cargo bench can replace target/release/ezpn with the all-feature binary.
    # Materialize the default-feature artifact last, immediately before SSH.
    run("release-build", ["cargo", "build", "--locked", "--release", "--bins"])
    if args.ssh != "skip":
        run("ssh", [sys.executable, "scripts/ssh-smoke.py", "--out", str(out / "ssh")], timeout=120, optional=args.ssh == "optional")
    else:
        run("ssh", [sys.executable, "-c", "print('SKIP: explicitly requested; no real SSH evidence'); raise SystemExit(77)"], optional=True)
    if args.mode == "release":
        run("audit", ["cargo", "audit", "--deny", "warnings"])
        run("deny", ["cargo", "deny", "--locked", "--all-features", "check"])
        run("package", ["cargo", "package", "--locked"])
    run("diff-check", ["git", "diff", "--check"])
    print(f"Evidence: {out / 'status.json'}")
    return int(any(result["status"] == "FAIL" for result in results))


if __name__ == "__main__":
    sys.exit(main())
