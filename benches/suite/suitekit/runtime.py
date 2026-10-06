"""Where the systems run: Docker containers, Apptainer images, or plain processes.

An adapter describes a step as a `Spec` (image, command, mounts, environment, the port a
server listens on) in container terms; a runtime carries it out and measures it: wall
time, peak memory (Docker: the stream of `docker stats`, about one reading a second, so a step
shorter than that may have none; Apptainer and processes: the kernel's peak resident
memory of the process and the descendants it waited for (wait4), and the process tree
sampled from /proc), exit code. The process runtime maps
container paths in the command and environment to the host directories mounted there.

Everything a runtime creates is named with PREFIX and removed by `cleanup()`, also when
a run fails; nothing else is touched.
"""
from __future__ import annotations

import os
import re
import shlex
import shutil
import signal
import subprocess
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path

PREFIX = "nrese-suite-"
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
    return subprocess.run(command, capture_output=True, text=True, timeout=timeout)


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
        self.thread.start()

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
        self.thread.start()

    def read(self):
        for line in self.process.stdout:
            for part in ANSI.sub("\n", line).splitlines():
                value = mib(part) if part.strip() else None
                if value:
                    self.peak = value if self.peak is None else max(self.peak, value)

    def stop(self) -> int | None:
        self.process.terminate()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
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
        self.scratch = scratch
        self.port = port
        self.echo = echo
        self.dry = False
        self.created_stores: list[str] = []

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
        path = self.scratch / name
        if path.exists():
            shutil.rmtree(path)
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
        shutil.rmtree(source, ignore_errors=True)
        if source in self.created_stores:
            self.created_stores.remove(source)

    def image_present(self, image: str) -> bool:
        return True

    def cleanup(self):
        for source in list(self.created_stores):
            self.remove_store(source)


class DockerRuntime(Runtime):
    kind = "docker"

    def __init__(self, scratch: Path, port: int, echo=print):
        super().__init__(scratch, port, echo)
        self.containers: set[str] = set()

    def args(self, spec: Spec, detached: bool) -> list[str]:
        args = ["docker", "run", "--name", spec.name, "--init"]
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
        return args + [spec.image] + command

    def remove(self, name: str):
        run_quiet(["docker", "rm", "-f", "-v", name])
        self.containers.discard(name)

    def sample(self, name: str) -> DockerStats:
        return DockerStats(name)

    def run(self, spec, log, timeout_s):
        self.remove(spec.name)
        self.containers.add(spec.name)
        started = time.monotonic()
        subprocess.run(self.args(spec, detached=True), check=True, capture_output=True)
        sampler = self.sample(spec.name)
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
        with open(log, "w", encoding="utf-8", errors="replace") as f:
            logs = run_quiet(["docker", "logs", spec.name])
            f.write(logs.stdout + logs.stderr)
        self.remove(spec.name)
        return Measured(ms, peak, rc, timed_out)

    def start(self, spec, log):
        self.remove(spec.name)
        self.containers.add(spec.name)
        subprocess.run(self.args(spec, detached=True), check=True, capture_output=True)
        return Server(spec, spec.name, log, sampler=self.sample(spec.name))

    def alive(self, server):
        out = run_quiet(["docker", "inspect", "-f", "{{.State.Running}}", server.spec.name])
        return out.stdout.strip() == "true"

    def logs(self, server) -> str:
        out = run_quiet(["docker", "logs", server.spec.name])
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
        if server.log:
            with open(server.log, "w", encoding="utf-8", errors="replace") as f:
                f.write(self.logs(server))
        self.remove(server.spec.name)
        return peak

    def exec(self, server, command, timeout_s=3600):
        out = subprocess.run(["docker", "exec", server.spec.name] + command, capture_output=True,
                             text=True, timeout=timeout_s)
        return out.returncode, out.stdout + out.stderr

    def new_store(self, name):
        run_quiet(["docker", "volume", "rm", "-f", name])
        subprocess.run(["docker", "volume", "create", name], check=True, capture_output=True)
        self.created_stores.append(name)
        return name

    def store_bytes(self, source):
        out = run_quiet(["docker", "run", "--rm", "-v", f"{source}:/v:ro", "alpine", "du", "-sb", "/v"], 600)
        try:
            return int(out.stdout.split()[0])
        except (IndexError, ValueError):
            return None

    def remove_store(self, source):
        run_quiet(["docker", "volume", "rm", "-f", source])
        if source in self.created_stores:
            self.created_stores.remove(source)

    def image_present(self, image):
        return run_quiet(["docker", "image", "inspect", image]).returncode == 0

    def cleanup(self):
        for name in list(self.containers):
            self.remove(name)
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
        self.processes: dict[str, subprocess.Popen] = {}

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
        out = open(log, "w", encoding="utf-8", errors="replace")
        kwargs = {}
        if os.name == "posix":
            kwargs["start_new_session"] = True
        return subprocess.Popen(argv, stdout=out, stderr=subprocess.STDOUT, env=self.environment(spec),
                                cwd=self.cwd(spec), stdin=subprocess.PIPE if spec.keep_stdin else subprocess.DEVNULL,
                                **kwargs)

    def kill(self, process: subprocess.Popen, sig=signal.SIGTERM):
        if process.poll() is not None:
            return
        try:
            if os.name == "posix":
                os.killpg(process.pid, sig)
            else:
                process.kill()
        except ProcessLookupError:
            pass

    def run(self, spec, log, timeout_s):
        started = time.monotonic()
        process = self.popen(self.argv(spec), spec, log)
        sampler = Sampler(lambda: tree_rss_mib(process.pid))
        timed_out = False
        rc, rss = reap(process, timeout_s)
        if rc is None:
            self.kill(process)
            if reap(process, 30)[0] is None:
                self.kill(process, signal.SIGKILL if os.name == "posix" else signal.SIGTERM)
                reap(process, 30)
            timed_out, rc = True, 124
        ms = (time.monotonic() - started) * 1000
        return Measured(ms, larger(sampler.stop(), rss), rc, timed_out)

    def start(self, spec, log):
        process = self.popen(self.argv(spec), spec, log)
        self.processes[spec.name] = process
        return Server(spec, process, log, sampler=Sampler(lambda: tree_rss_mib(process.pid)))

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
        self.processes.pop(server.spec.name, None)
        return larger(peak, rss)

    def exec(self, server, command, timeout_s=3600):
        spec = server.spec
        out = subprocess.run([self.translate(spec, c) for c in command], capture_output=True, text=True,
                             env=self.environment(spec), timeout=timeout_s)
        return out.returncode, out.stdout + out.stderr

    def url(self, spec):
        return f"http://127.0.0.1:{spec.port}"

    def cleanup(self):
        for process in list(self.processes.values()):
            self.kill(process, signal.SIGKILL if os.name == "posix" else signal.SIGTERM)
        self.processes.clear()
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
