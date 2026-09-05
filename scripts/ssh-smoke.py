#!/usr/bin/env python3
"""Isolated loopback sshd + PTY. Exit 77 means unavailable, never tested/pass."""
import argparse
import contextlib
import getpass
import json
import os
from pathlib import Path
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile

from runtime_driver import Daemon, Terminal, read_complete, wait_for


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, default=Path("target/release/ezpn"))
    parser.add_argument("--out", type=Path, default=Path("target/ssh-smoke"))
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    result = {"status": "FAIL", "scenario": "loopback ssh -tt detach/reattach and abrupt disconnect"}
    daemon = sshd_process = terminal = scratch = None
    code = 1
    try:
        sshd = shutil.which("sshd") or ("/usr/sbin/sshd" if Path("/usr/sbin/sshd").is_file() else None)
        if not sshd or not shutil.which("ssh") or not shutil.which("ssh-keygen"):
            result.update(status="SKIP", reason="sshd, ssh and ssh-keygen are required; no system SSH settings changed")
            code = 77
            return code
        scratch = tempfile.TemporaryDirectory(prefix="ezpn-ssh-", dir="/tmp")
        with contextlib.nullcontext() as _:
            root = Path(scratch.name)
            for name in ["host", "client"]:
                subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(root / name)], check=True, timeout=10, stdin=subprocess.DEVNULL)
            with socket.socket() as reservation:
                reservation.bind(("127.0.0.1", 0))
                port = reservation.getsockname()[1]
            config = root / "sshd_config"
            home = root / "home"
            home.mkdir(mode=0o700)
            config.write_text(f"""Port {port}
ListenAddress 127.0.0.1
HostKey {root / 'host'}
PidFile {root / 'sshd.pid'}
AuthorizedKeysFile none
AuthorizedKeysCommand /bin/cat {root / 'client.pub'}
AuthorizedKeysCommandUser {getpass.getuser()}
StrictModes yes
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
UsePAM no
PermitRootLogin prohibit-password
AllowUsers {getpass.getuser()}
AllowTcpForwarding no
X11Forwarding no
PermitTunnel no
PrintMotd no
PrintLastLog no
SetEnv HOME={home} ZDOTDIR={home}
""")
            check = subprocess.run([sshd, "-t", "-f", str(config)], capture_output=True, text=True, timeout=10)
            if check.returncode:
                result.update(status="SKIP", reason=f"isolated sshd preflight unavailable: {check.stderr.strip()}")
                code = 77
                return code
            with (out / "sshd.log").open("wb") as log:
                sshd_process = subprocess.Popen([sshd, "-D", "-e", "-f", str(config)], stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
                def listening():
                    if sshd_process.poll() is not None:
                        raise RuntimeError("isolated sshd exited; see sshd.log")
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=.2):
                            return True
                    except OSError:
                        return False
                wait_for(listening, "loopback sshd listening")
                daemon = Daemon(args.bin, root, out, session="ssh")
                remote = "exec env -i " + " ".join(f"{key}={shlex.quote(value)}" for key, value in daemon.env.items())
                remote += f" {shlex.quote(daemon.binary)} attach ssh --shared"
                argv = ["ssh", "-F", "/dev/null", "-tt", "-p", str(port), "-i", str(root / "client"),
                        "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes", "-o", "StrictHostKeyChecking=accept-new",
                        "-o", f"UserKnownHostsFile={root / 'known_hosts'}", "-o", "GlobalKnownHostsFile=/dev/null",
                        "-o", "ConnectTimeout=5", "127.0.0.1", remote]
                before = None
                for index in range(3):
                    terminal = Terminal(argv, daemon.env, root)
                    terminal.expect("\x1b[?1049h")
                    if index == 0:
                        terminal.write("RETAINED=ssh-state\r")
                    terminal.write(f"printf '%s:%s\\n' \"$$\" \"$RETAINED\" > {root / ('probe-' + str(index))}\r")
                    value = wait_for(lambda: read_complete(root / f"probe-{index}"), "same shell over SSH", terminal)
                    if index == 0:
                        before = value
                        if not before.endswith(":ssh-state\n"):
                            raise RuntimeError(f"unexpected shell state: {before!r}")
                    elif value != before:
                        raise RuntimeError(f"SSH reconnect changed shell state: {before!r} -> {value!r}")
                    if index == 1:
                        terminal.close(abrupt=True)
                    else:
                        terminal.write("\x02d")
                        wait_for(lambda: terminal.poll() is not None, "SSH detach exits", terminal)
                        if terminal.status != 0:
                            raise RuntimeError(f"SSH detach failed: {terminal.status}")
                        terminal.close()
                    (out / f"client-{index}.log").write_bytes(terminal.output)
                    terminal = None
                    daemon.alive()
                result.update(status="PASS", shell_pid=before.split(":")[0], connections=3)
                code = 0
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        result["reason"] = str(error)
    finally:
        if terminal:
            terminal.close(abrupt=True)
        if daemon:
            daemon.close()
        if sshd_process and sshd_process.poll() is None:
            sshd_process.terminate()
            try:
                sshd_process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                sshd_process.kill()
                sshd_process.wait(timeout=5)
        if scratch:
            scratch.cleanup()
        (out / "status.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result))
    return code


if __name__ == "__main__":
    sys.exit(main())
