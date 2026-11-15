"""The run manifest: what a result directory was measured with, so a number can be traced
to its commit, machine, settings and images years later.

`manifest.json` sits next to `results.csv`. A results directory can hold several
`suite.py run` invocations (benches/suite/batch.sh runs one per argument group); each
appends one entry to `invocations`:

| Field | Meaning |
|---|---|
| invocation | its id, in attempts.jsonl before it is here (append_attempts); one run at a time holds manifest.lock |
| started, finished | ISO 8601 times (finished is null while it runs, or if it died) |
| argv | the `suite.py run` arguments |
| protocol | runs, query runs, cache modes, order, seed, timeouts: the protocol knobs |
| git | commit, branch, dirty (number of changed or untracked files), describe |
| machine | host, OS, CPU model, logical cores, RAM; Docker's own CPU and memory limits; the start-up of a no-op container (container_start_ms), which every step timed around `docker run` includes |
| settings | the environment variables the suite reads (JAVA_HEAP, DOCKER_MEMORY, ...) |
| images | image -> its content id, for every image the run used |
| datasets | /data path -> size in bytes, for every input the run read |
| sources | the downloads behind the datasets: URL and the server's Last-Modified (a "latest" dump changes) |
| cells | `workload/tier/system` -> its receipt once it ran it all (receipts.py); its attempt, before its work, is in attempts.jsonl beside it (append_attempts) |
| data_hash_s | the seconds the receipts' data hashes took |
| exit | the run's exit code |

Nothing in it is secret: licence paths are recorded as set/unset only.
"""
from __future__ import annotations

import contextlib
import datetime
import functools
import hashlib
import json
import os
import platform
import shutil
import subprocess
import uuid
from pathlib import Path

from .lock import lock

# The variables the suite and its adapters read; licence variables are recorded as set or not.
SETTINGS = ["JAVA_HEAP", "DOCKER_MEMORY", "DOCKER_CPUS", "SKIP_COUNT", "QUERY_MEMORY_MIB", "JENA_OWL_REASONER", "ONTOLOGY",
            "NRESE_BENCH_SCRATCH", "NRESE_BENCH_KEEP", "NRESE_OXIGRAPH_SRC", "CARGO_TARGET_DIR",
            "GRAPHDB_IMAGE", "GRAPHDB_EDITION", "GRAPHDB_ENTITY_ID_SIZE", "GRAPHDB_ENTITY_INDEX_SIZE",
            "GRAPHDB_CONTEXT_INDEX", "GRAPHDB_CHECK_INCONSISTENCIES", "GRAPHDB_PAGE_CACHE_SIZE",
            "GRAPHDB_QUERY_MEMORY_THRESHOLD", "GRAPHDB_MAX_DIRECT_MEMORY",
            "GRAPHDB_INFERENCE_CONCURRENCY", "GRAPHDB_INFERENCE_BUFFER",
            # the SPARQL contenders' settings: a comparison is as fair as these were
            "NRESE_BIN", "HARNESS", "NRESE_ANSWER_CAP_MB", "TOTAL_QUERY_MEMORY_MIB", "SPARQL_DATA",
            "BSBM_CLIENTS", "BSBM_REFERENCES", "JENA_IMAGE", "JENA_LOADER", "OXIGRAPH_IMAGE",
            "QLEVER_IMAGE", "QLEVER_INDEX_SETTINGS", "QLEVER_VOCABULARY_TYPE", "QLEVER_CACHE_MAX_SIZE",
            "QLEVER_RUNTIME", "QLEVER_MEMORY", "QLEVER_SORT_MEMORY", "QLEVER_SIMULTANEOUS_QUERIES",
            "VIRTUOSO_IMAGE", "VIRTUOSO_NUMBER_OF_BUFFERS", "VIRTUOSO_MAX_DIRTY_BUFFERS",
            "VIRTUOSO_THREADS_PER_QUERY", "VIRTUOSO_HTTP_THREADS", "VIRTUOSO_SERVER_THREADS",
            "MILLENNIUMDB_IMAGE", "MDB_THREADS", "MDB_IMPORT_BUFFER", "MDB_STRINGS_STATIC",
            "MDB_STRINGS_DYNAMIC", "MDB_VERSIONED_BUFFER", "MDB_UNVERSIONED_BUFFER",
            # a build's configuration (P2 design review F4): its Rust image, its CPU, flags inherited
            "RUST_IMAGE", "NRESE_TARGET_CPU", "RUSTFLAGS"]
# Of these, where a run keeps its files and data, which changes no measurement (P2 design review F8):
# a cell's receipt leaves them out of its settings (its data is in its own hashes).
OPERATIONAL_SETTINGS = {"NRESE_BENCH_SCRATCH", "NRESE_BENCH_KEEP", "CARGO_TARGET_DIR", "SPARQL_DATA"}
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


# The calibration container's image: timed like any measured container, so pinned like one.
CALIBRATION_IMAGE = "alpine:latest"


def container_start_ms(root: Path) -> float | None:
    """The median wall time of three no-op containers: what every load and kit step timed
    around `docker run` carries on top of the system's own work. Their image is pinned at the
    job's start and run by its ID (ns.pin_images); one that can't be resolved stops the job."""
    import statistics
    import time
    from . import ns
    ns.pin_images([CALIBRATION_IMAGE], root)
    image = ns.image_id(CALIBRATION_IMAGE, root)
    times = []
    for _ in range(3):
        started = time.monotonic()
        if ns.docker([*ns.run_args(root, "suite"), "--rm", image, "true"], timeout=60).returncode != 0:
            return None
        times.append((time.monotonic() - started) * 1000)
    return round(statistics.median(times), 1)


def machine(runtime: str, root: Path | None = None) -> dict:
    out = {
        "host": platform.node(),
        "os": platform.platform(),
        "cpu": cpu_model(),
        "logical_cores": os.cpu_count(),
        "memory_gib": round((memory_bytes() or 0) / 2**30, 1),
        "python": platform.python_version(),
    }
    # The fleet's name of the host a fleet job's step runs on (FLEET_HOST, set by the fleet): what a
    # round's manifest and a report's context name, so that passes from another host are told apart
    # (review r1 R1-F1: `host` is the machine's own name, which the fleet's may not be).
    if os.environ.get("FLEET_HOST"):
        out["fleet_host"] = os.environ["FLEET_HOST"]
    # What changes timings on the host, as the run found it (hosts.toml `config` records the
    # intended configuration; compare checks both).
    for key, path in (("governor", "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
                      ("perf_event_paranoid", "/proc/sys/kernel/perf_event_paranoid")):
        try:
            out[key] = Path(path).read_text(encoding="utf-8").strip()
        except OSError:
            pass
    if runtime == "docker" and root is not None:
        out["container_start_ms"] = container_start_ms(root)
        info = _out(["docker", "info", "--format", "{{.ServerVersion}}|{{.NCPU}}|{{.MemTotal}}|{{.OperatingSystem}}"])
        if info:
            version, cpus, memory, system = (info.split("|") + ["", "", "", ""])[:4]
            out["docker"] = {"version": version, "cpus": int(cpus or 0),
                             "memory_gib": round(int(memory or 0) / 2**30, 1), "os": system}
    if runtime == "apptainer":
        out["apptainer"] = _out(["apptainer", "--version"])
    return out


def git(root: Path) -> dict:
    status = _out(["git", "status", "--porcelain", "--untracked-files=normal"], root)
    return {
        "commit": _out(["git", "rev-parse", "HEAD"], root),
        "branch": _out(["git", "rev-parse", "--abbrev-ref", "HEAD"], root),
        "describe": _out(["git", "describe", "--always", "--dirty"], root),
        "dirty": len(status.splitlines()) if status is not None else None,
    }


def binaries(target: Path, revision: str = "") -> dict:
    """Hash native executables; release evidence additionally verifies the existing build records."""
    names = [f"release/examples/{n}" for n in ("perf_lab", "perf_lab.real", "ql_print_check",
             "tableau_consistency", "context_classify", "classify")] + ["release/nrese-server"]
    out = {"dir": target.as_posix(), "revision": revision, "sha256": {}, "verified": False}
    try:
        if (target / "REVISION").is_file():
            out["revision"] = (target / "REVISION").read_text().strip()
        # The build's layout (release-gate.sh GATE_LAYOUTS): 1 the plain link, else its seed.
        out["layout"] = int((target / "LAYOUT").read_text().strip()) if (target / "LAYOUT").is_file() else 1
        sums = target / "SHA256SUMS"
        expected = dict((line.split(maxsplit=1)[1].lstrip(" *"), line.split(maxsplit=1)[0])
                        for line in sums.read_text().splitlines()) if sums.is_file() else {}
        for name in sorted(set(names) | set(expected)):
            path = target / name
            if not path.resolve().is_relative_to(target.resolve()):
                raise ValueError("build hash path outside binary directory")
            if path.is_file():
                with path.open("rb") as stream:
                    out["sha256"][name] = hashlib.file_digest(stream, "sha256").hexdigest()
        out["verified"] = bool(expected and out["revision"] and (target / "REVISION").is_file()
                               and (not revision or out["revision"] == revision)
                               and expected == out["sha256"])
    except (OSError, ValueError, IndexError) as error:
        out["error"] = str(error)
    return out


def one_binary(hashes: dict[str, dict | None], identical: set[frozenset[str]] | frozenset = frozenset()) -> list[str]:
    """Why sides can't be compared by what they ran (D16-593): a side without its binaries' hashes
    (HASHES: each side's binaries() `sha256`), and two sides whose builds are one, every binary the
    same, unless IDENTICAL declares that pair so (an A/A run). A build compared with itself says
    "no difference" whatever the code: no finding, a failure. One binary alike on two sides isn't
    this: a tool whose code two trees share builds the same."""
    errors = [f"{side}: no hash of its binaries: not compared" for side, found in hashes.items() if not found]
    built = [(side, found) for side, found in hashes.items() if found]
    for k, (a, mine) in enumerate(built):
        for b, theirs in built[k + 1:]:
            if mine == theirs and frozenset((a, b)) not in identical:
                errors.append(f"{a} and {b} ran one build ({len(mine)} binaries, every hash the same): not compared")
    return errors


def image_ids(images: list[str]) -> dict[str, str | None]:
    ids = {}
    for image in images:
        ids[image] = _out(["docker", "image", "inspect", "--format", "{{.Id}}", image])
    return ids


def now() -> str:
    return datetime.datetime.now().isoformat(timespec="seconds")


class ManifestError(RuntimeError):
    """A results directory's manifest that can't be read or parsed: its run is refused."""


def sync_directory(directory: Path):
    """A new name or rename in `directory` made durable: on POSIX the directory's own fsync; on
    Windows FlushFileBuffers on the directory, opened for writing with FILE_FLAG_BACKUP_SEMANTICS
    (as nrese-engine's durability/place). Raises if it can't (some network shares can't)."""
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CreateFileW.restype, kernel32.FlushFileBuffers.argtypes = wintypes.HANDLE, [wintypes.HANDLE]
        kernel32.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, wintypes.LPVOID,
                                         wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
        kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
        # GENERIC_WRITE, shared read/write/delete, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS
        handle = kernel32.CreateFileW(str(directory), 0x40000000, 7, None, 3, 0x02000000, None)
        if handle in (None, wintypes.HANDLE(-1).value):
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            if not kernel32.FlushFileBuffers(handle):
                raise ctypes.WinError(ctypes.get_last_error())
        finally:
            kernel32.CloseHandle(handle)
        return
    fd = os.open(directory, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


ATTEMPTS = "attempts.jsonl"


def append_attempts(results: Path, attempts: dict[str, dict]):
    """Cells' attempts appended to <results>/attempts.jsonl, on disk before this returns: the
    file's own fsync (on Windows _commit, i.e. FlushFileBuffers) after the append, then its name
    (sync_directory). No rename: a file appended to only never comes back older after a crash,
    on Windows too (review cell-receipts r3). Each record starts its own line, so a torn one (a
    crash mid-append; its work never started) never takes the next with it. Raises if any of it
    fails: the cells' work must not start. Only the run that owns `results` appends (Manifest's
    lock): Windows' append is a seek, then a write, so two appenders could overwrite each other."""
    _append(results, [{"cell": cell, **attempt} for cell, attempt in attempts.items()])


def _append(results: Path, records: list[dict]):
    with open(results / ATTEMPTS, "a", encoding="utf-8", newline="\n") as f:
        f.write("".join("\n" + json.dumps(record) for record in records))
        f.flush()
        os.fsync(f.fileno())
    sync_directory(results)


def read_journal(results: Path) -> list[dict]:
    """<results>/attempts.jsonl's records in the order written: each invocation's start
    (`invocation`) and each cell's attempt (`cell`, `attempt`); a torn record is skipped."""
    path, out = results / ATTEMPTS, []
    for line in path.read_text(encoding="utf-8").splitlines() if path.exists() else []:
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if isinstance(record, dict) and (record.get("invocation")
                                         or isinstance(record.get("cell"), str) and record.get("attempt")):
            out.append(record)
    return out


class Manifest:
    """One invocation's entry in <results>/manifest.json, written at the start and updated
    as the run learns its images and datasets, and at the end. One run at a time owns a results
    directory, from here to finish() or close() (or its process's end): a second is refused."""

    def __init__(self, results: Path, root: Path, argv: list[str], args, runtime: str, settings: dict):
        self.path, self._owner = results / "manifest.json", contextlib.ExitStack()
        try:
            self._owner.enter_context(lock(results / "manifest.lock", wait=False))
        except OSError as error:
            raise ManifestError(f"{results}: another run is writing into it ({error}); let it finish, "
                                "or give this one its own --results") from error
        try:
            self._start(root, argv, args, runtime, settings)
        except BaseException:
            self.close()
            raise

    def _start(self, root: Path, argv: list[str], args, runtime: str, settings: dict):
        self.entry = {
            "invocation": uuid.uuid4().hex,
            "started": now(),
            "finished": None,
            "argv": argv,
            "protocol": {
                "runs": args.runs, "query_runs": args.query_runs, "cache": args.cache,
                "order": args.order, "seed": args.seed, "timeout_s": args.timeout_s,
                "query_timeout_s": args.query_timeout_s, "runtime": runtime,
            },
            "git": git(root),
            "machine": machine(runtime, root),
            "settings": {k: settings[k] for k in SETTINGS if settings.get(k)}
                        | {k: "set" for k in SECRET_SETTINGS if settings.get(k)},
            "images": {},
            "datasets": {},
            "sources": [],
            "exit": None,
        }
        data = self._read()
        # The results directory's own name on disk before any of its attempts: a batch member that
        # ran can't vanish while an older one's receipt stays (review cell-receipts r4 R2b).
        sync_directory(self.path.parent.parent)
        # The invocation in the journal before the manifest names it: a journal lacking an
        # invocation its manifest names has lost records (ledger; review cell-receipts r4 R2b).
        _append(self.path.parent, [{"invocation": self.entry["invocation"], "started": self.entry["started"]}])
        data.setdefault("invocations", []).append(self.entry)
        self.index = len(data["invocations"]) - 1
        self._write(data)

    def _read(self) -> dict:
        """The manifest as it is: none yet is an empty one; one that can't be read or parsed is
        refused (ManifestError), never taken for empty: the next write would drop every earlier
        invocation's evidence (review cell-receipts r2)."""
        if not self.path.exists():
            return {"schema": 1, "invocations": []}
        try:
            data = json.loads(self.path.read_text(encoding="utf-8"))
        except (OSError, ValueError) as error:
            raise ManifestError(f"{self.path}: can't be read as a manifest ({error}); it is left as it is") from error
        if not isinstance(data, dict) or not isinstance(data.get("invocations", []), list):
            raise ManifestError(f"{self.path}: not a manifest (no invocations list); it is left as it is")
        return data

    def _write(self, data: dict):
        """The manifest, durably: its bytes on disk before it replaces the old one, and the
        replacement itself on disk, before this returns (a cell's attempt must survive a host crash
        before its work starts: review cell-receipts r2). Raises if any of that fails."""
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(".json.tmp")
        with open(tmp, "w", encoding="utf-8", newline="\n") as f:
            f.write(json.dumps(data, indent=2) + "\n")
            f.flush()
            os.fsync(f.fileno())  # on Windows _commit, i.e. FlushFileBuffers
        os.replace(tmp, self.path)
        sync_directory(self.path.parent)

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
        try:
            self.update(finished=now(), exit=code)
        finally:
            self.close()

    def close(self):
        """The results directory given up, for the next run."""
        self._owner.close()
