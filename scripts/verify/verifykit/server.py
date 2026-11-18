"""A server of its own for each scenario: a fresh temporary directory holding its data, a free
port on 127.0.0.1, the scenario's settings (`--set`, over the defaults below) and nothing from the
caller's environment (no NRESE_* variable reaches it, and it starts in its own directory, so no
stray .env file does either). Its log goes to the evidence. `cleanup` stops it and removes the
directory, and runs however the scenario ended; on Linux the server also dies with the runner
(PR_SET_PDEATHSIG), and under fleet the step's scope ends both. A stop ends the server with all it
started (suitekit/processes.py: its own process group on POSIX), so a command that runs the
server as its child (a Windows venv's python.exe) frees the port too.

Every attempt to start it is recorded (`attempts`), failed ones too. An attempt that fails is
stopped and reaped before anything else happens; only one whose own log says it couldn't bind
its port (another process took the port between our probe and its bind) is tried again, on
another port."""
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
from pathlib import Path

from suitekit.processes import stop as stop_tree

from . import client

# Durable, as deployments run (repositories live on disk; a restart reopens them).
DEFAULTS = {"store.mode": "on-disk"}
BIND_ATTEMPTS = 3
BIND_FAILED = "failed to bind"  # nrese-server's own words (main.rs) when its port is taken


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
        self.stopped: subprocess.Popen | None = None  # the process whose tree was stopped
        self.port = 0
        self.attempts: list[dict] = []

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
        /readyz answers 200. The start's record (also in `attempts`): argv, port, pid, the
        attempt's number and outcome, seconds until ready."""
        if self.root is None:
            if self.scratch:
                self.scratch.mkdir(parents=True, exist_ok=True)
            self.root = Path(tempfile.mkdtemp(prefix="nrese-verify-", dir=self.scratch))
        port = self.port  # a restart keeps its port while it can
        for attempt in range(1, BIND_ATTEMPTS + 1):
            self.port = port or free_port()
            began = time.monotonic()
            with open(self.log, "ab") as log:
                offset = log.tell()  # where this attempt's own log begins
                log.write(f"=== verify: start attempt {len(self.attempts) + 1}, port {self.port}, "
                          f"{time.strftime('%Y-%m-%dT%H:%M:%S%z')}\n".encode())
                log.flush()
                record = {"argv": self.argv(), "port": self.port, "pid": None, "attempt": attempt, "outcome": None}
                self.attempts.append(record)
                try:
                    self.process = subprocess.Popen(
                        self.argv(), cwd=self.root, env=self.env, stdin=subprocess.DEVNULL, stdout=log,
                        stderr=subprocess.STDOUT, start_new_session=os.name != "nt",
                        preexec_fn=_die_with_parent if sys.platform.startswith("linux") else None)
                except OSError as e:
                    record["outcome"] = f"can't start {self.command[0]}: {e}"
                    raise LaunchError(record["outcome"]) from None
            record["pid"] = self.process.pid
            try:
                record["ready_s"] = self._wait_ready(began, offset)
                record["outcome"] = "ready"
                return record
            except LaunchError as e:
                record["exit_code"] = self.stop()  # stopped and reaped, whatever went wrong
                bind_failed = BIND_FAILED in self.since(offset)
                record["outcome"] = BIND_FAILED if bind_failed else str(e).splitlines()[0]
                if bind_failed and attempt < BIND_ATTEMPTS:
                    port = 0
                    continue
                raise
            except BaseException as e:  # interrupted: the cleanup stops it
                record["outcome"] = f"interrupted ({type(e).__name__})"
                raise
        raise AssertionError("unreachable")

    def _wait_ready(self, began: float, offset: int) -> float:
        """Seconds until /readyz answered 200, every probe within what is left of start_timeout."""
        last = "no answer yet"
        while (left := self.start_timeout - (time.monotonic() - began)) > 0:
            code = self.process.poll()
            if code is not None:
                raise LaunchError(f"the server exited with code {code} before it was ready; "
                                  f"its log ends:\n{self.tail(offset=offset)}")
            probe = client.get(f"{self.base}/readyz", timeout_s=min(2.0, left), read_limit=300)
            if probe["response"] and probe["response"]["status"] == 200:
                return round(time.monotonic() - began, 3)
            last = (probe["error"] or f"/readyz {probe['response']['status']}: "
                    + probe["body"].kept.decode("utf-8", "replace"))
            time.sleep(min(0.2, max(0.0, self.start_timeout - (time.monotonic() - began))))
        raise LaunchError(f"not ready after {self.start_timeout:g} s ({last}); "
                          f"its log ends:\n{self.tail(offset=offset)}")

    def restart(self) -> dict:
        self.stop()
        return self.start()

    def exited(self) -> int | None:
        return self.process.poll() if self.process else None

    def stop(self) -> int | None:
        """Stops it with all it started: SIGTERM to its process group, SIGKILL after ten seconds
        to what is left of it (on Windows taskkill /T /F). Its exit code. Also when the process
        itself has gone (a launcher that left its child), and once per process, since its pid
        may be another's after that."""
        if not self.process:
            return None
        if self.stopped is not self.process:
            stop_tree(self.process, grace_s=10)
            self.stopped = self.process
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
        stopped = stopping is None and (self.process is None or self.process.poll() is not None)
        return {"exit_code": exit_code, "stopped": stopped,
                "directory": str(self.root) if self.root else None,
                "removed": bool(self.root) and not self.root.exists(), "kept": keep,
                "error": stopping or removing}

    def since(self, offset: int = 0) -> str:
        """The log from `offset` on (an attempt's own, from where it began)."""
        try:
            with open(self.log, "rb") as f:
                f.seek(offset)
                return f.read().decode("utf-8", errors="replace")
        except OSError:
            return "(no log)"

    def tail(self, lines: int = 20, offset: int = 0) -> str:
        return "\n".join(self.since(offset).splitlines()[-lines:])
