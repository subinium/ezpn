"""Small standard-library Unix process/PTY driver for isolated evidence runs."""
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import socket
import struct
import subprocess
import termios
import threading
import time


def isolated_env(root):
    env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "SHELL": "/bin/sh", "TERM": "xterm-256color", "LC_ALL": "C"}
    for key in ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"]:
        path = root / key.lower()
        path.mkdir(mode=0o700, exist_ok=True)
        env[key] = str(path)
    runtime = root / "run"
    runtime.mkdir(mode=0o700, exist_ok=True)
    env["XDG_RUNTIME_DIR"] = env["EZPN_TEST_SOCKET_DIR"] = str(runtime)
    env["ZDOTDIR"] = env["HOME"]
    if "LLVM_PROFILE_FILE" in os.environ:
        env["LLVM_PROFILE_FILE"] = os.environ["LLVM_PROFILE_FILE"]
    return env


def wait_for(predicate, label, terminal=None, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(.02)
    raise RuntimeError(f"timeout: {label}; terminal={terminal.output[-2000:] if terminal else ''!r}")


class TailBuffer:
    """A bounded byte tail whose append cost does not grow with history."""
    def __init__(self, capacity):
        if capacity <= 0:
            raise ValueError("capture capacity must be positive")
        self._buffer = bytearray(capacity)
        self._position = self._used = 0
        self._lock = threading.Lock()

    def append(self, chunk):
        capacity = len(self._buffer)
        chunk = chunk[-capacity:]
        with self._lock:
            first = min(len(chunk), capacity - self._position)
            self._buffer[self._position:self._position + first] = chunk[:first]
            self._buffer[:len(chunk) - first] = chunk[first:]
            self._position = (self._position + len(chunk)) % capacity
            self._used = min(capacity, self._used + len(chunk))

    def snapshot(self):
        with self._lock:
            if self._used < len(self._buffer):
                return bytes(self._buffer[:self._used])
            return bytes(self._buffer[self._position:] + self._buffer[:self._position])


class Terminal:
    def __init__(self, argv, env, cwd, cols=100, rows=32):
        self.status = None
        self._closed = False
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            try:
                fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
                os.chdir(cwd)
                os.execvpe(str(argv[0]), list(map(str, argv)), env)
            finally:
                os._exit(127)
        os.set_blocking(self.fd, False)
        self._capture = TailBuffer(4 * 1024 * 1024)
        self._stop = threading.Event()
        self._reader = threading.Thread(target=self._collect, daemon=True)
        self._reader.start()

    @property
    def output(self):
        return self._capture.snapshot()

    def _collect(self):
        while not self._stop.is_set():
            self._drain()
            self._stop.wait(.005)

    def _drain(self):
        # Bounded per pump even if the workload is continuously producing.
        for _ in range(128):
            if not select.select([self.fd], [], [], 0)[0]:
                break
            try:
                chunk = os.read(self.fd, 65536)
            except OSError as error:
                if error.errno in [errno.EIO, errno.EAGAIN]:
                    break
                raise
            if not chunk:
                break
            self._capture.append(chunk)

    def write(self, text):
        data = memoryview(text.encode())
        deadline = time.monotonic() + 5
        while data:
            if time.monotonic() > deadline:
                raise RuntimeError("PTY write timed out")
            if select.select([], [self.fd], [], .1)[1]:
                data = data[os.write(self.fd, data):]

    def poll(self):
        if self.status is None:
            pid, status = os.waitpid(self.pid, os.WNOHANG)
            if pid:
                self.status = os.waitstatus_to_exitcode(status)
        return self.status

    def expect(self, text):
        return wait_for(lambda: text.encode() in self.output, f"output {text!r}", self)

    def close(self, abrupt=False):
        if self._closed:
            return
        if self.poll() is None:
            os.kill(self.pid, signal.SIGKILL if abrupt else signal.SIGTERM)
            deadline = time.monotonic() + 3
            while self.poll() is None and time.monotonic() < deadline:
                time.sleep(.02)
            if self.poll() is None:
                os.kill(self.pid, signal.SIGKILL)
                os.waitpid(self.pid, 0)
                self.status = -signal.SIGKILL
        self._stop.set()
        self._reader.join(timeout=3)
        os.close(self.fd)
        self._closed = True


class Daemon:
    def __init__(self, binary, root, out, session="evidence", panes=1):
        self.env = isolated_env(root)
        self.root = root
        self.session = session
        self.binary = str(Path(binary).resolve())
        self.log = (out / "daemon.log").open("wb")
        self.process = subprocess.Popen([self.binary, "--server", session, "1", str(panes)], env=self.env, cwd=root, stdin=subprocess.DEVNULL, stdout=self.log, stderr=subprocess.STDOUT, start_new_session=True)
        self.socket = Path(self.env["EZPN_TEST_SOCKET_DIR"]) / f"ezpn-session-{session}.sock"
        try:
            wait_for(lambda: self.socket.exists() or self.process.poll() is not None, "daemon socket")
            self.alive()
        except BaseException:
            self.close()
            raise

    def alive(self):
        if self.process.poll() is not None:
            raise RuntimeError(f"daemon exited with {self.process.returncode}")

    def attach(self):
        terminal = Terminal([self.binary, "attach", self.session, "--shared"], self.env, self.root)
        try:
            terminal.expect("\x1b[?1049h")
            return terminal
        except BaseException:
            terminal.close()
            raise

    def ipc(self, request):
        path = Path(self.env["EZPN_TEST_SOCKET_DIR"]) / f"ezpn-{self.process.pid}.sock"
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(5)
            stream.connect(str(path))
            stream.sendall(json.dumps(request).encode() + b"\n")
            data = bytearray()
            while not data.endswith(b"\n"):
                chunk = stream.recv(65536)
                if not chunk or len(data) > 16 * 1024 * 1024:
                    raise RuntimeError("incomplete/oversized IPC response")
                data.extend(chunk)
            response = json.loads(data)
            if not response.get("ok"):
                raise RuntimeError(f"IPC {request}: {response}")
            return response

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        self.log.close()


def read_complete(path):
    try:
        text = path.read_text()
        return text if text.endswith("\n") else None
    except FileNotFoundError:
        return None
