"""Where the systems run: Docker containers, Apptainer images, or plain processes.

An adapter describes a step as a `Spec` (image, command, mounts, environment, the port a
server listens on) in container terms; a runtime carries it out and measures it: wall
time, peak memory (Docker: the stream of `docker stats`, about one reading a second, so a step
shorter than that may have none; Apptainer and processes: the kernel's peak resident
memory of the process and the descendants it waited for (wait4), and the process tree
sampled from /proc), exit code. The process runtime maps
container paths in the command and environment to the host directories mounted there.

Everything a runtime creates is named in the job's namespace (ns.py), labelled with it, and
removed by `cleanup()`, also when a run fails; nothing else is touched.
"""
from __future__ import annotations

import json
import math
import os
import re
import shlex
import shutil
import signal
import subprocess
import sys
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path

try:
    from . import ns
except ImportError:  # run as a script (the scope entry below): the kit's module beside it
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import ns

OWNER = "suite"
ROOT = Path(__file__).resolve().parents[3]  # the checkout the suite runs from
SAMPLE_S = 0.25


@dataclass
class Mount:
    source: str  # a host path, or (Docker only) a volume name
    target: str  # the path inside the container
    readonly: bool = True


@dataclass
class Spec:
    name: str
    image: str | None  # None: a host command (process runtime only)
    command: list[str]
    env: dict[str, str] = field(default_factory=dict)
    mounts: list[Mount] = field(default_factory=list)
    port: int | None = None  # the port a server listens on
    workdir: str | None = None
    # The command is arguments to the image's own entrypoint (Docker) or runscript
    # (Apptainer); otherwise command[0] is the program.
    entrypoint: bool = False
    user: str | None = None
    keep_stdin: bool = False  # servers that read commands from stdin (RDFox's shell)
    memory: str | None = None  # a hard memory cap (Docker `--memory`), e.g. "18g"


@dataclass
class Measured:
    ms: float
    peak_mib: int | None
    rc: int
    timed_out: bool = False

    @property
    def ok(self) -> bool:
        return self.rc == 0 and not self.timed_out


@dataclass
class Server:
    spec: Spec
    handle: object = None  # a Popen, or None for Docker (the container name is the handle)
    log: Path | None = None
    peak_mib: int | None = None
    sampler: "Sampler | DockerStats | None" = None


def shell(command: list[str]) -> str:
    return " ".join(shlex.quote(c) for c in command)


def run_quiet(command: list[str], timeout: float | None = None) -> subprocess.CompletedProcess:
    # UTF-8 whatever the console's code page (Windows): a container's log is the system's bytes.
    return subprocess.run(command, capture_output=True, text=True, encoding="utf-8", errors="replace",
                          timeout=timeout)


def mib(text: str) -> int | None:
    """Docker's "1.23GiB / 31GiB" (the used part) in MiB."""
    used = text.split("/")[0].strip()
    number = "".join(c for c in used if c.isdigit() or c == ".")
    unit = used[len(number):].strip()
    factor = {"GiB": 1024, "MiB": 1, "KiB": 1 / 1024, "B": 1 / 1048576,
              "GB": 1000**3 / 1048576, "MB": 1000**2 / 1048576, "kB": 1000 / 1048576}.get(unit)
    if not number or factor is None:
        return None
    return int(float(number) * factor)


class Sampler:
    """Samples a memory figure every SAMPLE_S seconds until stopped; keeps the peak."""

    def __init__(self, sample):
        self.sample = sample
        self.peak: int | None = None
        self.stopped = threading.Event()
        self.thread = threading.Thread(target=self.loop, daemon=True)
        try:
            self.thread.start()
        except BaseException:
            self.stopped.set()
            if self.thread.ident is not None:
                self.thread.join(timeout=10)
            raise

    def loop(self):
        while not self.stopped.is_set():
            try:
                value = self.sample()
            except Exception:  # a process that just ended
                value = None
            if value is not None:
                self.peak = value if self.peak is None else max(self.peak, value)
            self.stopped.wait(SAMPLE_S)

    def stop(self) -> int | None:
        self.stopped.set()
        self.thread.join(timeout=10)
        return self.peak


ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")


class DockerStats:
    """The peak of a container's memory from one streaming `docker stats` (a reading per
    second or so). A stopped container reads 0: not a reading."""

    def __init__(self, name: str):
        self.peak: int | None = None
        self.process = subprocess.Popen(["docker", "stats", "--format", "{{.MemUsage}}", name],
                                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
                                        errors="replace")
        self.thread = threading.Thread(target=self.read, daemon=True)
        try:
            self.thread.start()
        except BaseException:
            self.process.kill()
            self.process.wait(timeout=10)
            if self.thread.ident is not None:
                self.thread.join(timeout=10)
            raise

    def read(self):
        for line in self.process.stdout:
            for part in ANSI.sub("\n", line).splitlines():
                value = mib(part) if part.strip() else None
                if value:
                    self.peak = value if self.peak is None else max(self.peak, value)

    def stop(self) -> int | None:
        if self.process.poll() is None:
            self.process.terminate()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=10)
        self.thread.join(timeout=10)
        return self.peak


def reap(process: subprocess.Popen, timeout: float) -> tuple[int | None, int | None]:
    """Waits for a process: its exit code (None on timeout) and, where wait4 exists, the peak
    resident memory in MiB of it and the descendants it waited for (ru_maxrss)."""
    if not hasattr(os, "wait4") or process.returncode is not None:
        try:
            return process.wait(timeout=timeout), None
        except subprocess.TimeoutExpired:
            return None, None
    deadline = time.monotonic() + timeout
    while True:
        try:
            pid, status, usage = os.wait4(process.pid, os.WNOHANG)
        except ChildProcessError:  # reaped elsewhere (poll)
            return process.poll(), None
        if pid:
            process.returncode = os.waitstatus_to_exitcode(status)
            return process.returncode, usage.ru_maxrss // 1024  # KiB on Linux
        if time.monotonic() > deadline:
            return None, None
        time.sleep(0.05)


def larger(a: int | None, b: int | None) -> int | None:
    return b if a is None else a if b is None else max(a, b)


def scope_argv(unit: str, memory: str, environ=os.environ, *, interval_s=None, stop_s=None) -> list[str]:
    """A command's prefix to run it in its own systemd user scope capped as Docker's --memory
    and --memory-swap cap a container: `memory` in Docker's notation (12g, 900m), which
    systemd reads with upper-case units only. At the cap the kernel kills the command alone
    (exit 137, as in a container), not systemd the whole scope, so what runs after it in the
    scope (fast's wrapper) can still read the peak. Under fleet the scope goes into the step's
    slice (FLEET_SLICE), so it counts against what the job declared (14 Oct 2026: a scope beside
    them, counted nowhere, took Office into a whole-host OOM). The rootfs runtime and fast's
    native runtime share it."""
    # An explicit interval includes both TERM/KILL waits of parent and bound workers.
    parent = environ.get("SUITE_SCOPE_PARENT", "")
    stop_s = float(stop_s if stop_s is not None else environ.get("SUITE_SCOPE_STOP_S", 30))
    if not re.fullmatch(r"[A-Za-z0-9_-]+", unit) or not math.isfinite(stop_s) or stop_s <= 0:
        raise ValueError("invalid scope name or shutdown allowance")
    placed = [f"--slice={environ['FLEET_SLICE']}"] if environ.get("FLEET_SLICE") else []
    if parent:
        if not re.fullmatch(r"[A-Za-z0-9_-]+", parent) or parent == unit:
            raise ValueError("invalid parent scope")
        placed += ["-p", f"BindsTo={parent}.scope", "-p", f"After={parent}.scope"]
    if parent or interval_s is not None:
        placed += ["-p", f"TimeoutStopSec={stop_s:g}", "-p", "KillMode=control-group", "-p", "SendSIGKILL=yes"]
    if interval_s is not None:
        if parent or not math.isfinite(interval_s) or interval_s <= 4 * stop_s:
            raise ValueError("interval must reserve bounded parent and worker shutdown")
        placed += ["-p", f"RuntimeMaxSec={interval_s - 4 * stop_s:g}"]
    return ["systemd-run", "--user", "--scope", "-q", *placed, f"--unit={unit}", "-p", f"MemoryMax={memory.upper()}",
            "-p", "MemorySwapMax=0", "-p", "OOMPolicy=continue", "--"]


def end_scope(unit: str) -> None:
    """Nothing outlives its task: whatever the scope still holds after its command ended or was
    stopped (a child its wrapper started, reparented when the wrapper died) is killed. A scope
    already gone is no error."""
    observed = run_quiet(["systemctl", "--user", "show", "-p", "ControlGroup", "--value", f"{unit}.scope"], 30)
    group = observed.stdout.strip()
    cgroup = Path("/sys/fs/cgroup") / group.lstrip("/") if group else None
    if observed.returncode or (cgroup and (not group.startswith("/") or ".." in cgroup.parts)):
        raise RuntimeError(f"scope {unit}: cgroup ownership unverified")
    killed = run_quiet(["systemctl", "--user", "kill", "--signal=KILL", f"{unit}.scope"], 30)
    deadline = time.monotonic() + 30
    while True:
        shown = run_quiet(["systemctl", "--user", "show", "-p", "LoadState", "-p", "ActiveState",
                           f"{unit}.scope"], 30)
        if shown.returncode == 0 and ("LoadState=not-found" in shown.stdout.splitlines()
                                      or "ActiveState=inactive" in shown.stdout.splitlines()
                                      or "ActiveState=failed" in shown.stdout.splitlines()):
            if cgroup and cgroup.exists() and "populated 0" not in (cgroup / "cgroup.events").read_text().splitlines():
                raise RuntimeError(f"scope {unit}: cgroup still populated")
            return
        if shown.returncode or time.monotonic() >= deadline:
            raise RuntimeError(f"scope {unit}: termination unverified: {killed.stderr or shown.stderr}")
        time.sleep(0.05)


def slice_limit_mib(environ=os.environ) -> int | None:
    """The memory limit of the fleet step's slice (FLEET_SLICE: what the job declared, plus a
    tenth), in MiB; None outside fleet or without a limit."""
    name = environ.get("FLEET_SLICE")
    if not name or not shutil.which("systemctl"):
        return None
    shown = run_quiet(["systemctl", "--user", "show", "-p", "MemoryMax", "--value", name], 30).stdout.strip()
    return int(shown) // 1048576 if shown.isdigit() else None


class ScopePeak:
    """A running scope's memory.peak in MiB: everything the cap counts, the page cache charged
    to it included, so that a run killed at the cap shows the cap. Its cgroup is asked of
    systemd until the scope exists, then only the file is read (a sample costs no process)."""

    def __init__(self, unit: str):
        self.unit, self.peak_file = unit, None

    def __call__(self) -> int | None:
        if self.peak_file is None:
            shown = run_quiet(["systemctl", "--user", "show", "-p", "ControlGroup", "--value", f"{self.unit}.scope"],
                              timeout=10)
            group = shown.stdout.strip() if shown.returncode == 0 else ""
            if not group:
                return None
            self.peak_file = Path("/sys/fs/cgroup" + group) / "memory.peak"
        try:
            return int(self.peak_file.read_text()) // (1024 * 1024)
        except (OSError, ValueError):
            return None


def tree_rss_mib(pid: int) -> int | None:
    """The resident memory of a process and its descendants, in MiB (Linux)."""
    proc = Path("/proc")
    if not proc.exists():
        return None
    children: dict[int, list[int]] = {}
    rss: dict[int, int] = {}
    page = os.sysconf("SC_PAGE_SIZE")
    for entry in proc.iterdir():
        if not entry.name.isdigit():
            continue
        try:
            stat = (entry / "stat").read_text()
            fields = stat[stat.rindex(")") + 2:].split()
            ppid, resident = int(fields[1]), int(fields[21])
        except (OSError, ValueError, IndexError):
            continue
        children.setdefault(ppid, []).append(int(entry.name))
        rss[int(entry.name)] = resident * page
    total, stack = 0, [pid]
    while stack:
        p = stack.pop()
        total += rss.get(p, 0)
        stack.extend(children.get(p, []))
    return total // 1048576 if total else None


class Runtime:
    """The interface; `DockerRuntime`, `ApptainerRuntime` and `ProcessRuntime` implement it."""

    kind = "?"

    def __init__(self, scratch: Path, port: int, echo=print):
        self.scratch = scratch.resolve()
        self.port = port
        self.echo = echo
        self.dry = False
        self.created_stores: list[str] = []
        self.cleanup_errors: list[str] = []

    def attempt(self, action, *args) -> bool:
        try:
            action(*args)
            return True
        except Exception as error:
            self.cleanup_errors.append(f"{getattr(action, '__name__', type(action).__name__)}: {error}")
            self.echo(self.cleanup_errors[-1])
            return False

    def residuals(self) -> dict:
        return {"stores": list(self.created_stores), "containers": sorted(getattr(self, "containers", ())),
                "processes": list(getattr(self, "processes", ())),
                "scopes": list(getattr(self, "units", {}).values())}

    def workers(self) -> bool:
        return any(v for k, v in self.residuals().items() if k != "stores")

    def owned_path(self, source: str) -> Path:
        path = Path(source)
        if (path.parent != self.scratch or path.is_symlink() or path.resolve() != path
                or not path.resolve().is_relative_to(self.scratch)):
            raise ValueError(f"unsafe store path: {source}")
        return path

    # --- steps ---
    def run(self, spec: Spec, log: Path, timeout_s: float) -> Measured:
        raise NotImplementedError

    def start(self, spec: Spec, log: Path) -> Server:
        raise NotImplementedError

    def alive(self, server: Server) -> bool:
        raise NotImplementedError

    def stop(self, server: Server) -> int | None:
        """Stops a server; returns its peak memory in MiB."""
        raise NotImplementedError

    def exec(self, server: Server, command: list[str], timeout_s: float = 3600) -> tuple[int, str]:
        """Runs a command next to a running server (in its container)."""
        raise NotImplementedError

    def url(self, spec: Spec) -> str:
        return f"http://127.0.0.1:{self.port}"

    # --- stores: where a system keeps its data for one run ---
    def new_store(self, name: str) -> str:
        path = self.owned_path(str(self.scratch / name))
        path.mkdir(parents=True)
        self.created_stores.append(str(path))
        return str(path)

    def store_bytes(self, source: str) -> int | None:
        total = 0
        for root, _, files in os.walk(source):
            for f in files:
                try:
                    total += os.path.getsize(os.path.join(root, f))
                except OSError:
                    pass
        return total

    def remove_store(self, source: str):
        if source not in self.created_stores or self.workers():
            raise RuntimeError(f"store not owned or workers remain: {source}")
        path = self.owned_path(source)
        if path.exists():
            shutil.rmtree(path)
        if os.path.lexists(path):
            raise RuntimeError(f"store remains: {source}")
        self.created_stores.remove(source)

    def image_present(self, image: str) -> bool:
        return True

    def cleanup(self):
        if not self.workers():
            for source in list(self.created_stores):
                self.attempt(self.remove_store, source)


class DockerRuntime(Runtime):
    kind = "docker"

    def __init__(self, scratch: Path, port: int, echo=print):
        super().__init__(scratch, port, echo)
        self.containers: dict[str, Server] = {}

    def args(self, spec: Spec, detached: bool) -> list[str]:
        args = [ns.executable(), "run", *ns.name_args(spec.name, ROOT, OWNER), "--init"]
        # A measurement's own container: the quiet slot's watcher pauses the others.
        if os.environ.get("NRESE_QUIET_TOKEN"):
            args += ["--label", f"nrese.quiet={os.environ['NRESE_QUIET_TOKEN']}"]
        # A queue job's containers: its host's guard stops them with the job (suite/queue.py).
        if os.environ.get("QUEUE_JOB"):
            args += ["--label", f"nrese.queue={os.environ['QUEUE_JOB']}"]
        if detached:
            args.append("-d")
        if spec.keep_stdin:
            args.append("-i")
        if spec.port is not None:
            args += ["-p", f"{self.port}:{spec.port}"]
        if spec.user:
            args += ["-u", spec.user]
        if spec.memory:
            args += ["--memory", spec.memory, "--memory-swap", spec.memory]
        # Every system on the same CPUs when a run asks for it (DOCKER_CPUS, e.g. "8").
        if os.environ.get("DOCKER_CPUS"):
            args += ["--cpus", os.environ["DOCKER_CPUS"]]
        for key, value in spec.env.items():
            args += ["-e", f"{key}={value}"]
        for m in spec.mounts:
            args += ["-v", f"{docker_path(m.source)}:{m.target}{':ro' if m.readonly else ''}"]
        if spec.workdir:
            args += ["-w", spec.workdir]
        command = list(spec.command)
        if not spec.entrypoint:
            args += ["--entrypoint", command.pop(0)]
        # The image by the ID the job first resolved its tag to (ns.pin_image): a retag while the
        # job runs can't change what a later system or repetition runs.
        return args + [ns.pin_image(spec.image, ROOT)] + command

    def remove(self, name: str):
        if name not in self.containers:
            raise RuntimeError(f"container not owned: {name}")
        server = self.containers[name]
        self.attempt(self.save_logs, server)
        if server.sampler:
            server.sampler.stop()
        self.remove_resource("container", name)
        self.containers.pop(name)

    def names(self, kind):
        # Request names explicitly; daemon/list errors must never mean absence.
        out = run_quiet(["docker", kind, "ls", *(["-a"] if kind == "container" else []),
                         "--format", "{{.Names}}" if kind == "container" else "{{.Name}}"])
        out.check_returncode()
        return out.stdout.splitlines()

    def remove_resource(self, kind, name):
        out = run_quiet(["docker", kind, "rm", "-f", name])
        if out.returncode and name in self.names(kind):
            out.check_returncode()

    def sample(self, name: str) -> DockerStats:
        return DockerStats(name)

    def save_logs(self, server):
        if server.log:
            server.log.write_text(self.logs(server), encoding="utf-8", errors="replace")

    def run(self, spec, log, timeout_s):
        if spec.name in self.containers or spec.name in self.names("container"):
            raise FileExistsError(spec.name)
        server = self.containers[spec.name] = Server(spec, spec.name, log)
        started = time.monotonic()
        try:
            subprocess.run(self.args(spec, detached=True), check=True, capture_output=True)
            sampler = server.sampler = self.sample(spec.name)
            timed_out = False
            try:
                waited = subprocess.run(["docker", "wait", spec.name], capture_output=True, text=True,
                                        timeout=timeout_s)
                rc = int(waited.stdout.strip() or 1)
            except subprocess.TimeoutExpired:
                run_quiet(["docker", "kill", spec.name])
                timed_out, rc = True, 124
            ms = (time.monotonic() - started) * 1000
            peak = sampler.stop()
            return Measured(ms, peak, rc, timed_out)
        finally:
            self.attempt(self.remove, spec.name)

    def start(self, spec, log):
        if spec.name in self.containers or spec.name in self.names("container"):
            raise FileExistsError(spec.name)
        server = self.containers[spec.name] = Server(spec, spec.name, log)
        subprocess.run(self.args(spec, detached=True), check=True, capture_output=True)
        server.sampler = self.sample(spec.name)
        return server

    def alive(self, server):
        out = run_quiet(["docker", "inspect", "-f", "{{.State.Running}}", server.spec.name])
        return out.stdout.strip() == "true"

    def logs(self, server) -> str:
        out = run_quiet(["docker", "logs", server.spec.name])
        out.check_returncode()
        return out.stdout + out.stderr

    def stop(self, server):
        # One reading of the running server as a floor: a short run may end before the
        # stream's first.
        now = run_quiet(["docker", "stats", "--no-stream", "--format", "{{.MemUsage}}", server.spec.name], 30)
        peak = server.sampler.stop() if server.sampler else None
        reading = mib(ANSI.sub("", now.stdout)) if now.returncode == 0 and now.stdout.strip() else None
        if reading:
            peak = max(peak or 0, reading)
        run_quiet(["docker", "stop", "-t", "60", server.spec.name], 120)
        self.remove(server.spec.name)
        return peak

    def exec(self, server, command, timeout_s=3600):
        out = subprocess.run(["docker", "exec", server.spec.name] + command, capture_output=True,
                             text=True, timeout=timeout_s)
        return out.returncode, out.stdout + out.stderr

    def new_store(self, name):
        if name in self.created_stores or name in self.names("volume"):
            raise FileExistsError(name)
        ns.docker(ns.volume("create", name, ROOT, OWNER), check=True)
        self.created_stores.append(name)
        return name

    def store_bytes(self, source):
        out = run_quiet(["docker", "run", "--rm", "-v", f"{source}:/v:ro", "alpine", "du", "-sb", "/v"], 600)
        try:
            return int(out.stdout.split()[0])
        except (IndexError, ValueError):
            return None

    def remove_store(self, source):
        if source not in self.created_stores or self.workers():
            raise RuntimeError(f"store not owned or workers remain: {source}")
        self.remove_resource("volume", source)
        self.created_stores.remove(source)

    def image_present(self, image):
        return run_quiet(["docker", "image", "inspect", image]).returncode == 0

    def cleanup(self):
        for name in list(self.containers):
            self.attempt(self.remove, name)
        super().cleanup()


def docker_path(source: str) -> str:
    """A host path as Docker Desktop on Windows wants it (C:/...); volume names unchanged."""
    if os.name == "nt" and len(source) > 1 and source[1] == ":":
        return source.replace("\\", "/")
    return source


class ProcessRuntime(Runtime):
    """Commands on the host; container paths are mapped to the mounted host directories."""

    kind = "process"

    def __init__(self, scratch, port, echo=print):
        super().__init__(scratch, port, echo)
        self.processes: dict[str, Server] = {}

    def translate(self, spec: Spec, text: str) -> str:
        for m in sorted(spec.mounts, key=lambda m: -len(m.target)):
            if text == m.target or text.startswith(m.target + "/"):
                return m.source + text[len(m.target):]
            text = text.replace("=" + m.target + "/", "=" + m.source + "/")
            text = text.replace("file://" + m.target + "/", "file://" + m.source + "/")
        return text

    def argv(self, spec: Spec) -> list[str]:
        if spec.image is not None:
            raise RuntimeError(f"{spec.name}: runs from the image {spec.image}; the process runtime runs host commands only")
        return [self.translate(spec, c) for c in spec.command]

    def environment(self, spec: Spec) -> dict[str, str]:
        env = dict(os.environ)
        env.update({k: self.translate(spec, v) for k, v in spec.env.items()})
        return env

    def cwd(self, spec: Spec) -> str | None:
        return self.translate(spec, spec.workdir) if spec.workdir else None

    def popen(self, argv, spec, log):
        kwargs = {}
        if os.name == "posix":
            kwargs["start_new_session"] = True
        with open(log, "w", encoding="utf-8", errors="replace") as out:
            return subprocess.Popen(argv, stdout=out, stderr=subprocess.STDOUT, env=self.environment(spec),
                                    cwd=self.cwd(spec), stdin=subprocess.PIPE if spec.keep_stdin else subprocess.DEVNULL,
                                    **kwargs)

    def kill(self, process: subprocess.Popen, sig=signal.SIGTERM):
        if os.name != "posix" and process.poll() is not None:
            return
        try:
            if os.name == "posix":
                os.killpg(process.pid, sig)
            else:
                process.kill()
        except ProcessLookupError:
            pass

    def run(self, spec, log, timeout_s):
        if spec.name in self.processes:
            raise FileExistsError(spec.name)
        started = time.monotonic()
        server = None
        try:
            server = self.start(spec, log)
            rc, rss = reap(server.handle, timeout_s)
            # A completed workload's time excludes sampling and harness teardown.
            finished = time.monotonic() if rc is not None else None
            peak = server.sampler.stop() if server.sampler else None
            peak = self.measured_peak(spec, peak, rss)
        finally:
            server = server or self.processes.get(spec.name)
            if server:
                self.attempt(self.stop, server)
        ms = ((finished if finished is not None else time.monotonic()) - started) * 1000
        return Measured(ms, peak, 124 if rc is None else rc, rc is None)

    def start(self, spec, log):
        if spec.name in self.processes or spec.name in getattr(self, "units", {}):
            raise FileExistsError(spec.name)
        process = self.popen(self.argv(spec), spec, log)
        server = self.processes[spec.name] = Server(spec, process, log)
        server.sampler = self.peak(spec)
        return server

    def peak(self, spec):
        return Sampler(lambda: tree_rss_mib(self.processes[spec.name].handle.pid))

    def measured_peak(self, spec, sampled, rss):
        return larger(sampled, rss)

    def alive(self, server):
        return server.handle.poll() is None

    def stop(self, server):
        peak = server.sampler.stop() if server.sampler else None
        process = server.handle
        self.kill(process)
        rc, rss = reap(process, 60)
        if rc is None:
            self.kill(process, signal.SIGKILL if os.name == "posix" else signal.SIGTERM)
            rc, rss = reap(process, 30)
        if rc is None:
            raise RuntimeError(f"process not reaped: {server.spec.name}")
        if os.name == "posix":
            self.kill(process, signal.SIGKILL)
            deadline = time.monotonic() + 30
            while True:
                try:
                    os.killpg(process.pid, 0)
                except ProcessLookupError:
                    break
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"process group remains: {server.spec.name}")
                time.sleep(0.05)
        self.processes.pop(server.spec.name, None)
        return self.measured_peak(server.spec, peak, rss)

    def exec(self, server, command, timeout_s=3600):
        spec = server.spec
        out = subprocess.run([self.translate(spec, c) for c in command], capture_output=True, text=True,
                             env=self.environment(spec), timeout=timeout_s)
        return out.returncode, out.stdout + out.stderr

    def url(self, spec):
        return f"http://127.0.0.1:{spec.port}"

    def cleanup(self):
        for server in list(self.processes.values()):
            self.attempt(self.stop, server)
        super().cleanup()


class ApptainerRuntime(ProcessRuntime):
    """Images as SIF files (`<sif_dir>/<image with / and : as _>.sif`), run with the host's
    network: a server listens on its own port. Stores are host directories."""

    kind = "apptainer"

    def __init__(self, scratch, port, sif_dir: Path, echo=print):
        super().__init__(scratch, port, echo)
        self.sif_dir = sif_dir

    def sif(self, image: str) -> str:
        return str(self.sif_dir / (image.replace("/", "_").replace(":", "_") + ".sif"))

    def prefix(self, spec: Spec, verb: str) -> list[str]:
        args = ["apptainer", verb, "--cleanenv", "--no-home", "--writable-tmpfs"]
        for m in spec.mounts:
            args += ["--bind", f"{m.source}:{m.target}{':ro' if m.readonly else ''}"]
        for key, value in spec.env.items():
            args += ["--env", f"{key}={value}"]
        if spec.workdir:
            args += ["--pwd", spec.workdir]
        return args + [self.sif(spec.image)]

    def argv(self, spec):
        if spec.image is None:
            return ProcessRuntime.argv(self, spec)
        return self.prefix(spec, "run" if spec.entrypoint else "exec") + list(spec.command)

    def environment(self, spec):
        return dict(os.environ) if spec.image is not None else ProcessRuntime.environment(self, spec)

    def cwd(self, spec):
        return None if spec.image is not None else ProcessRuntime.cwd(self, spec)

    def exec(self, server, command, timeout_s=3600):
        spec = server.spec
        argv = self.prefix(spec, "exec") + command if spec.image else command
        out = subprocess.run(argv, capture_output=True, text=True, timeout=timeout_s)
        return out.returncode, out.stdout + out.stderr

    def image_present(self, image):
        return Path(self.sif(image)).exists()


def inside(rootfs: Path, target: str) -> str:
    """A mount target with the image's own symlinks resolved, as Docker resolves them
    (Virtuoso's /database links to /opt/virtuoso-opensource/database); bubblewrap doesn't
    follow an absolute link inside the new root."""
    parts, resolved, hops = [p for p in target.split("/") if p], [], 0
    while parts:
        part = parts.pop(0)
        candidate = rootfs.joinpath(*resolved, part)
        if candidate.is_symlink() and hops < 40:
            hops += 1
            link = os.readlink(candidate)
            parts = [p for p in link.split("/") if p] + parts
            if link.startswith("/"):
                resolved = []
            continue
        if part == "..":
            resolved = resolved[:-1]
        elif part != ".":
            resolved.append(part)
    return "/" + "/".join(resolved)


class RootfsRuntime(ProcessRuntime):
    """The systems' Docker images unpacked into directories and run with bubblewrap, for a
    measure host without Docker (phuoc-yu): every system from the same image as under Docker,
    on the host's network and kernel, under a systemd scope with the container's memory cap.
    `<rootfs_dir>/<image with / and : as _>/` holds the image's filesystem in `rootfs/` and its
    `docker image inspect` in `config.json` (benches/sparql/images.sh export). The image is
    read-only below a throwaway overlay; the mounts are bound as Docker binds them; the user is
    root inside a user namespace (files written are the caller's outside)."""

    kind = "rootfs"

    def measured_peak(self, spec, sampled, rss):
        # The scope owns this measurement even after its unit has been removed.
        return sampled if spec.memory else super().measured_peak(spec, sampled, rss)

    def __init__(self, scratch, port, rootfs_dir: Path, echo=print):
        super().__init__(scratch, port, echo)
        self.rootfs_dir = rootfs_dir
        self.units: dict[str, str] = {}  # spec name -> its scope, while it runs

    def image_dir(self, image: str) -> Path:
        return self.rootfs_dir / image.replace("/", "_").replace(":", "_")

    def config(self, image: str) -> dict:
        data = json.loads((self.image_dir(image) / "config.json").read_text(encoding="utf-8"))
        entry = data[0] if isinstance(data, list) else data
        return entry.get("Config", entry)

    def prefix(self, spec: Spec, command: list[str]) -> list[str]:
        config = self.config(spec.image)
        if spec.entrypoint:
            command = list(config.get("Entrypoint") or []) + (command or list(config.get("Cmd") or []))
        args = []
        if spec.memory:
            unit = f"nrese-{spec.name}-{os.getpid()}-{int(time.monotonic() * 1000)}"
            self.units[spec.name] = unit
            args += scope_argv(unit, spec.memory)
        args += ["bwrap", "--overlay-src", str(self.image_dir(spec.image) / "rootfs"), "--tmp-overlay", "/",
                 "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp", "--unshare-user", "--uid", "0", "--gid", "0",
                 "--die-with-parent", "--clearenv"]
        for pair in config.get("Env") or []:
            key, _, value = pair.partition("=")
            args += ["--setenv", key, value]
        for key, value in spec.env.items():
            args += ["--setenv", key, value]
        rootfs = self.image_dir(spec.image) / "rootfs"
        for m in spec.mounts:
            if not m.readonly:
                # Docker creates a missing bind source (a store the system makes on first start).
                Path(m.source).mkdir(parents=True, exist_ok=True)
            args += ["--ro-bind" if m.readonly else "--bind", m.source, inside(rootfs, m.target)]
        args += ["--chdir", spec.workdir or config.get("WorkingDir") or "/"]
        return args + ["--", *command]

    def argv(self, spec):
        if spec.image is None:
            # Host binaries need the same scope and cap as image-backed contenders.
            command = ProcessRuntime.argv(self, spec)
            if not spec.memory:
                return command
            unit = f"nrese-{spec.name}-{os.getpid()}-{int(time.monotonic() * 1000)}"
            self.units[spec.name] = unit
            return scope_argv(unit, spec.memory) + command
        return self.prefix(spec, list(spec.command))

    def environment(self, spec):
        return dict(os.environ) if spec.image is not None else ProcessRuntime.environment(self, spec)

    def cwd(self, spec):
        return None if spec.image is not None else ProcessRuntime.cwd(self, spec)

    def peak(self, spec) -> "Sampler | None":
        """The scope's memory.peak, sampled while it lives (the scope goes when its command ends)."""
        unit = self.units.get(spec.name)
        return Sampler(ScopePeak(unit)) if unit else super().peak(spec)

    def run(self, spec, log, timeout_s):
        if spec.name in self.units or spec.name in self.processes:
            raise FileExistsError(spec.name)
        try:
            return ProcessRuntime.run(self, spec, log, timeout_s)
        finally:
            self.attempt(self.end_unit, spec.name)

    def stop(self, server):
        try:
            return ProcessRuntime.stop(self, server)
        finally:
            self.attempt(self.end_unit, server.spec.name)

    def end_unit(self, name):
        unit = self.units.get(name)
        if unit:
            end_scope(unit)
            self.units.pop(name)

    def cleanup(self):
        for name in list(self.units):
            self.attempt(self.end_unit, name)
        super().cleanup()

    def exec(self, server, command, timeout_s=3600):
        spec = server.spec
        if spec.image is None:
            return ProcessRuntime.exec(self, server, command, timeout_s)
        plain = Spec(spec.name, spec.image, [], spec.env, spec.mounts, workdir=spec.workdir)
        out = subprocess.run(self.prefix(plain, command), capture_output=True, text=True, timeout=timeout_s)
        return out.returncode, out.stdout + out.stderr

    def image_present(self, image):
        return (self.image_dir(image) / "rootfs").is_dir()


class DryRuntime(Runtime):
    """Prints what the wrapped runtime would run; nothing is started or created."""

    def __init__(self, inner: Runtime, echo=print):
        super().__init__(inner.scratch, inner.port, echo)
        self.inner = inner
        self.kind = inner.kind
        self.dry = True

    def show(self, spec: Spec, verb: str):
        if isinstance(self.inner, DockerRuntime):
            argv = self.inner.args(spec, detached=verb == "start")
        elif isinstance(self.inner, ApptainerRuntime) and spec.image is not None:
            argv = self.inner.prefix(spec, "run" if spec.entrypoint else "exec") + list(spec.command)
        else:
            argv = [ProcessRuntime.translate(self.inner, spec, c) for c in spec.command]
        self.echo(f"    [{verb}] {shell(argv)}")

    def run(self, spec, log, timeout_s):
        self.show(spec, "run")
        return Measured(0.0, None, 0)

    def start(self, spec, log):
        self.show(spec, "start")
        return Server(spec, None, log)

    def alive(self, server):
        return True

    def stop(self, server):
        self.echo(f"    [stop] {server.spec.name}")
        return None

    def exec(self, server, command, timeout_s=3600):
        self.echo(f"    [exec in {server.spec.name}] {shell(command)}")
        return 0, ""

    def url(self, spec):
        return self.inner.url(spec)

    def new_store(self, name):
        self.echo(f"    [store] {name}")
        return name if isinstance(self.inner, DockerRuntime) else str(self.scratch / name)

    def store_bytes(self, source):
        return None

    def remove_store(self, source):
        self.echo(f"    [remove store] {source}")

    def image_present(self, image):
        return self.inner.image_present(image)


if __name__ == "__main__":
    # Prefix an existing launcher before preparation. exec preserves its inherited leases.
    unit, memory, interval, stop, *command = sys.argv[1:]
    if not command or not os.environ.get("FLEET_SLICE") or os.environ.get("SUITE_SCOPE_PARENT"):
        raise ValueError("a fresh Fleet scope and command are required")
    argv = scope_argv(unit, memory, interval_s=float(interval), stop_s=float(stop))
    os.environ.update(SUITE_SCOPE_PARENT=unit, SUITE_SCOPE_STOP_S=stop)
    os.execvpe(argv[0], [*argv, *command], os.environ)
