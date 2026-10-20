"""A server of its own for each scenario: a fresh temporary directory holding its data, a free
port on 127.0.0.1, the scenario's settings (`--set`, over the defaults below) and nothing from the
caller's environment (no NRESE_* variable reaches it, and it starts in its own directory, so no
stray .env file does either). Its log goes to the evidence. `cleanup` stops it and removes the
directory, and runs however the scenario ended; on Linux the server also dies with the runner
(PR_SET_PDEATHSIG), and under fleet the step's scope ends both."""
from __future__ import annotations

import ctypes
import hashlib
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

# Durable, as deployments run (repositories live on disk; a restart reopens them).
DEFAULTS = {"store.mode": "on-disk"}
BIND_ATTEMPTS = 3


class LaunchError(Exception):
    """The server didn't come up; the message says why, with its log's last lines."""


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _die_with_parent() -> None:
    # PR_SET_PDEATHSIG = 1: SIGKILL when the runner goes, however it goes.
    ctypes.CDLL(None, use_errno=True).prctl(1, signal.SIGKILL)


class Server:
    def __init__(self, command: list[str], settings: dict[str, str], env: dict[str, str], log: Path,
                 start_timeout: float = 120.0, scratch: Path | None = None):
        self.command, self.log, self.start_timeout = command, log, start_timeout
        self.settings = {**DEFAULTS, **settings}
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("NRESE_")}
        self.env.update({"RUST_LOG": "info", "RUST_BACKTRACE": "1", **env})
        self.scratch = scratch
        self.root: Path | None = None
        self.process: subprocess.Popen | None = None
        self.port = 0
        self.starts: list[dict] = []

    @property
    def base(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    @property
    def data_dir(self) -> Path:
        assert self.root is not None
        return self.root / "data"

    def argv(self) -> list[str]:
        sets = {**self.settings, "server.bind_address": f"127.0.0.1:{self.port}", "store.data_dir": str(self.data_dir)}
        return [*self.command, *(a for k, v in sets.items() for a in ("--set", f"{k}={v}"))]

    def start(self) -> dict:
        """Starts it (a fresh directory the first time, the same one after) and waits until
        /readyz answers 200. The start's record: argv, port, pid, seconds until ready."""
        if self.root is None:
            if self.scratch:
                self.scratch.mkdir(parents=True, exist_ok=True)
            self.root = Path(tempfile.mkdtemp(prefix="nrese-verify-", dir=self.scratch))
        port = self.port
        for attempt in range(1, BIND_ATTEMPTS + 1):
            self.port = port or free_port()
            began = time.monotonic()
            with open(self.log, "ab") as log:
                log.write(f"=== verify: start {len(self.starts) + 1}, port {self.port}, "
                          f"{time.strftime('%Y-%m-%dT%H:%M:%S%z')}\n".encode())
                log.flush()
                try:
                    self.process = subprocess.Popen(
                        self.argv(), cwd=self.root, env=self.env, stdin=subprocess.DEVNULL, stdout=log,
                        stderr=subprocess.STDOUT, preexec_fn=_die_with_parent if sys.platform.startswith("linux") else None)
                except OSError as e:
                    raise LaunchError(f"can't start {self.command[0]}: {e}") from None
            record = {"argv": self.argv(), "port": self.port, "pid": self.process.pid, "attempt": attempt}
            try:
                record["ready_s"] = self._wait_ready(began)
                self.starts.append(record)
                return record
            except LaunchError as e:
                # The port another process took between our probe and the server's bind: again,
                # on another (a restart keeps its port when it can, else takes another).
                if "failed to bind" in str(e) and attempt < BIND_ATTEMPTS:
                    port = 0
                    continue
                self.stop()
                raise
        raise AssertionError("unreachable")

    def _wait_ready(self, began: float) -> float:
        last = "no answer yet"
        while time.monotonic() - began < self.start_timeout:
            code = self.process.poll()
            if code is not None:
                raise LaunchError(f"the server exited with code {code} before it was ready; its log ends:\n{self.tail()}")
            try:
                with urllib.request.urlopen(f"{self.base}/readyz", timeout=2) as response:
                    if response.status == 200:
                        return round(time.monotonic() - began, 3)
                    last = f"/readyz {response.status}"
            except urllib.error.HTTPError as e:
                last = f"/readyz {e.code}: {e.read(300).decode('utf-8', 'replace')}"
            except OSError as e:
                last = f"/readyz: {e}"
            time.sleep(0.2)
        raise LaunchError(f"not ready after {self.start_timeout:g} s ({last}); its log ends:\n{self.tail()}")

    def restart(self) -> dict:
        self.stop()
        return self.start()

    def exited(self) -> int | None:
        return self.process.poll() if self.process else None

    def stop(self) -> int | None:
        """Stops it: SIGTERM (on Windows TerminateProcess), SIGKILL after ten seconds. Its exit code."""
        if not self.process:
            return None
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(10)
        return self.process.returncode

    def cleanup(self, keep: bool = False) -> dict:
        """Stops it and removes its directory (kept with `keep`, its path in the record)."""
        exit_code, stopping, removing = None, None, None
        try:
            exit_code = self.stop()
        except OSError as e:
            stopping = f"stopping: {e}"
        if self.root and self.root.exists() and not keep:
            for _ in range(10):  # Windows lets go of a dead process's files a little later
                try:
                    shutil.rmtree(self.root)
                    removing = None
                    break
                except OSError as e:
                    removing = f"removing {self.root}: {e}"
                    time.sleep(0.5)
        return {"exit_code": exit_code, "stopped": stopping is None and (self.process is None or self.process.poll() is not None),
                "directory": str(self.root) if self.root else None,
                "removed": bool(self.root) and not self.root.exists(), "kept": keep,
                "error": stopping or removing}

    def tail(self, lines: int = 20) -> str:
        try:
            text = self.log.read_text(encoding="utf-8", errors="replace")
        except OSError:
            return "(no log)"
        return "\n".join(text.splitlines()[-lines:])
