"""The run manifest: what a result directory was measured with, so a number can be traced
to its commit, machine, settings and images years later.

`manifest.json` sits next to `results.csv`. A results directory can hold several
`suite.py run` invocations (benches/suite/batch.sh runs one per argument group); each
appends one entry to `invocations`:

| Field | Meaning |
|---|---|
| started, finished | ISO 8601 times (finished is null while it runs, or if it died) |
| argv | the `suite.py run` arguments |
| protocol | runs, query runs, cache modes, order, seed, timeouts: the protocol knobs |
| git | commit, branch, dirty (number of changed tracked files), describe |
| machine | host, OS, CPU model, logical cores, RAM; Docker's own CPU and memory limits; the start-up of a no-op container (container_start_ms), which every step timed around `docker run` includes |
| settings | the environment variables the suite reads (JAVA_HEAP, DOCKER_MEMORY, ...) |
| images | image -> its content id, for every image the run used |
| datasets | /data path -> size in bytes, for every input the run read |
| sources | the downloads behind the datasets: URL and the server's Last-Modified (a "latest" dump changes) |
| exit | the run's exit code |

Nothing in it is secret: licence paths are recorded as set/unset only.
"""
from __future__ import annotations

import datetime
import functools
import json
import os
import platform
import shutil
import subprocess
from pathlib import Path

# The variables the suite and its adapters read; licence variables are recorded as set or not.
SETTINGS = ["JAVA_HEAP", "DOCKER_MEMORY", "QUERY_MEMORY_MIB", "JENA_OWL_REASONER", "ONTOLOGY",
            "NRESE_BENCH_SCRATCH", "NRESE_BENCH_KEEP", "NRESE_OXIGRAPH_SRC", "CARGO_TARGET_DIR"]
SECRET_SETTINGS = ["NRESE_LICENSES", "GRAPHDB_LICENSE", "RDFOX_LICENSE", "RGGS_REPO"]


def _out(command: list[str], cwd: Path | None = None, timeout: float = 30) -> str | None:
    if shutil.which(command[0]) is None:
        return None
    try:
        done = subprocess.run(command, cwd=cwd, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.SubprocessError):
        return None
    return done.stdout.strip() if done.returncode == 0 else None


def cpu_model() -> str:
    if platform.system() == "Windows":
        try:
            import winreg
            key = winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, r"HARDWARE\DESCRIPTION\System\CentralProcessor\0")
            return winreg.QueryValueEx(key, "ProcessorNameString")[0].strip()
        except OSError:
            pass
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                return line.split(":", 1)[1].strip()
    except OSError:
        pass
    return _out(["sysctl", "-n", "machdep.cpu.brand_string"]) or platform.processor() or "unknown"


def memory_bytes() -> int | None:
    if platform.system() == "Windows":
        import ctypes

        class Status(ctypes.Structure):
            _fields_ = [("length", ctypes.c_ulong), ("load", ctypes.c_ulong),
                        ("total", ctypes.c_ulonglong), ("avail", ctypes.c_ulonglong),
                        ("page_total", ctypes.c_ulonglong), ("page_avail", ctypes.c_ulonglong),
                        ("virtual_total", ctypes.c_ulonglong), ("virtual_avail", ctypes.c_ulonglong),
                        ("extended", ctypes.c_ulonglong)]
        status = Status()
        status.length = ctypes.sizeof(Status)
        if ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(status)):
            return int(status.total)
        return None
    try:
        for line in Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemTotal:"):
                return int(line.split()[1]) * 1024
    except OSError:
        pass
    return None


@functools.cache
def docker_memory_bytes() -> int | None:
    """The memory Docker's containers share: the host's on Linux, the VM's on Docker
    Desktop."""
    out = _out(["docker", "info", "--format", "{{.MemTotal}}"])
    return int(out) if out and out.isdigit() else None


def container_start_ms() -> float | None:
    """The median wall time of three no-op containers: what every load and kit step timed
    around `docker run` carries on top of the system's own work."""
    import statistics
    import time
    times = []
    for _ in range(3):
        started = time.monotonic()
        if _out(["docker", "run", "--rm", "alpine:latest", "true"], timeout=60) is None:
            return None
        times.append((time.monotonic() - started) * 1000)
    return round(statistics.median(times), 1)


def machine(runtime: str) -> dict:
    out = {
        "host": platform.node(),
        "os": platform.platform(),
        "cpu": cpu_model(),
        "logical_cores": os.cpu_count(),
        "memory_gib": round((memory_bytes() or 0) / 2**30, 1),
        "python": platform.python_version(),
    }
    if runtime == "docker":
        out["container_start_ms"] = container_start_ms()
        info = _out(["docker", "info", "--format", "{{.ServerVersion}}|{{.NCPU}}|{{.MemTotal}}|{{.OperatingSystem}}"])
        if info:
            version, cpus, memory, system = (info.split("|") + ["", "", "", ""])[:4]
            out["docker"] = {"version": version, "cpus": int(cpus or 0),
                             "memory_gib": round(int(memory or 0) / 2**30, 1), "os": system}
    if runtime == "apptainer":
        out["apptainer"] = _out(["apptainer", "--version"])
    return out


def git(root: Path) -> dict:
    status = _out(["git", "status", "--porcelain", "--untracked-files=no"], root)
    return {
        "commit": _out(["git", "rev-parse", "HEAD"], root),
        "branch": _out(["git", "rev-parse", "--abbrev-ref", "HEAD"], root),
        "describe": _out(["git", "describe", "--always", "--dirty"], root),
        "dirty": len(status.splitlines()) if status else 0,
    }


def image_ids(images: list[str]) -> dict[str, str | None]:
    ids = {}
    for image in images:
        ids[image] = _out(["docker", "image", "inspect", "--format", "{{.Id}}", image])
    return ids


def now() -> str:
    return datetime.datetime.now().isoformat(timespec="seconds")


class Manifest:
    """One invocation's entry in <results>/manifest.json, written at the start and updated
    as the run learns its images and datasets, and at the end."""

    def __init__(self, results: Path, root: Path, argv: list[str], args, runtime: str, settings: dict):
        self.path = results / "manifest.json"
        self.entry = {
            "started": now(),
            "finished": None,
            "argv": argv,
            "protocol": {
                "runs": args.runs, "query_runs": args.query_runs, "cache": args.cache,
                "order": args.order, "seed": args.seed, "timeout_s": args.timeout_s,
                "query_timeout_s": args.query_timeout_s, "runtime": runtime,
            },
            "git": git(root),
            "machine": machine(runtime),
            "settings": {k: settings[k] for k in SETTINGS if settings.get(k)}
                        | {k: "set" for k in SECRET_SETTINGS if settings.get(k)},
            "images": {},
            "datasets": {},
            "sources": [],
            "exit": None,
        }
        data = self._read()
        data.setdefault("invocations", []).append(self.entry)
        self.index = len(data["invocations"]) - 1
        self._write(data)

    def _read(self) -> dict:
        try:
            return json.loads(self.path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return {"schema": 1, "invocations": []}

    def _write(self, data: dict):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
        os.replace(tmp, self.path)

    def update(self, **fields):
        for key, value in fields.items():
            if isinstance(value, dict) and isinstance(self.entry.get(key), dict):
                self.entry[key].update(value)
            else:
                self.entry[key] = value
        data = self._read()
        if self.index < len(data.get("invocations", [])):
            data["invocations"][self.index] = self.entry
        else:
            data.setdefault("invocations", []).append(self.entry)
        self._write(data)

    def finish(self, code: int | None):
        self.update(finished=now(), exit=code)
