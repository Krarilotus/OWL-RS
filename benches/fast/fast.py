#!/usr/bin/env python3
"""The fast suite: many small performance cases, each 10-300 s, all of them in under an
hour, run like unit tests for performance (README.md here; the cases: benches/cases/).

    python benches/fast/fast.py list                     the cases, their semantics and comparators
    python benches/fast/fast.py build                    perf lab and DL tools, Linux, in a capped container
    python benches/fast/fast.py run [--cases A,B] [--areas X,Y] [--reps 2] [--label L]
    python benches/fast/fast.py compare BASE.json NEW.json
    python benches/fast/fast.py prepare [--cases A,B]   a release gate's data, once, by one generator (native)
    python benches/fast/fast.py compete [--cases A,B] [--systems S,T]   NRESE beside its comparators
    python benches/fast/fast.py table [--baseline R] [--write]   the cases as CATALOG.md's table
    python benches/fast/fast.py clean [--build] [--data]  this checkout's scratch; --build its containers and
                                                          build volume (a job's cleanup); --data the shared data volume

`run` generates each case's data once (deterministic; kept in the fast volume, hosts.toml),
runs each case in its own container under the case's memory cap, checks the answers
before a time counts, asserts the plan or engine path the case must take, and writes
benches/baselines/fast/<date>-<label>.json. Repetitions are interleaved: every case once,
then every case again. `compare` reports regressions and wins beyond a bootstrap 95 %
confidence interval of the ratio of medians.

The build runs scripts/cargo-guarded.sh inside the pinned Rust image, into this
checkout's own target volume (suitekit/ns.py: the job's namespace), with CARGO_BUILD_JOBS and
NRESE_MEMORY_CAP_GB from the environment (defaults 4 and 8) and the container capped
to match.
"""
from __future__ import annotations

import argparse
import contextlib
import datetime
import functools
import hashlib
import json
import math
import os
import random
import re
import socket
import statistics
import subprocess
import sys
import time
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "benches" / "suite"))
from suitekit import cases as cases_registry, hosts, manifest  # noqa: E402
from suitekit.runtime import end_scope, scope_argv  # noqa: E402
from suitekit.compare import (KEPT_POINTS, UNSTABLE_PEAK, bootstrap_ratio, client_throughput,  # noqa: E402,F401
                              compare_reports, counter_changes, load_report, peak_spread, pooled,
                              process_peak, series_ratios, spread, statistic, thin)
from suitekit.workloads import fast_data_name  # noqa: E402
from suitekit import ns  # noqa: E402

# The volumes (benches/hosts.toml [volumes]; NRESE_FAST_VOLUME and NRESE_BENCH_VOLUME name others).
DATA_VOLUME = hosts.volume("fast")
BENCH_VOLUME = hosts.volume("data")
# This checkout's own build volume, and its containers' and scratch's names: the job's
# namespace (suitekit/ns.py); the Cargo registry is a cache every job shares.
TARGET_VOLUME = ns.name("target", ROOT)
CARGO_VOLUME = ns.shared("cargo")
OWNER = "fast"
# Its scratch in the shared fast volume.
VOLUME_SCRATCH = f"/fast/scratch/{ns.name('scratch', ROOT)}"
# The host lock under which shared fast data is made (generated or concatenated): one job at a time.
FAST_LOCK = ns.lock_dir() / "fast-data.lock"
SCRATCH = ROOT / "tmp" / "fast"
BASELINES = Path(os.environ.get("NRESE_FAST_BASELINES") or ROOT / "benches" / "baselines" / "fast")
# The full reports, outside git (the same path on the main and the office PC).
REPORTS = Path(os.environ.get("NRESE_BENCH_REPORTS") or Path.home() / "nrese-bench" / "reports") / "fast"
CASE_TIMEOUT_S = 900
ENV = {**os.environ, "MSYS_NO_PATHCONV": "1"}


def rust_image() -> str:
    channel = re.search(r'channel = "(.*)"', (ROOT / "rust-toolchain.toml").read_text()).group(1)
    return os.environ.get("RUST_IMAGE", f"rust:{channel}-bookworm")


def pinned(tag: str) -> str:
    """The image ID this job runs for `tag`, pinned at its start (pin_images): a retag meanwhile
    can't change what a later case or side runs; a tag not pinned is an error, never run."""
    return ns.image_id(tag, ROOT)


def pin_images(tags) -> None:
    """At the start, before anything is timed: every image the run's containers use, pulled if
    missing and resolved to its ID for the whole job (ns.pin_images); one that can't be stops it."""
    try:
        ns.pin_images(tags, ROOT, run=docker)
    except ns.ImageError as error:
        sys.exit(str(error))


def host_path(path: Path) -> str:
    return path.as_posix()


def load_cases() -> list[dict]:
    """Every case of every group (benches/cases/, suitekit/cases.py)."""
    return cases_registry.load()


def select(cases: list[dict], args) -> list[dict]:
    if args.cases is not None and not args.cases.strip(","):
        # An empty selection (a gate's plan that couldn't be read) is an error, never every case.
        sys.exit("--cases names no case")
    if args.cases:
        wanted = args.cases.split(",")
        unknown = set(wanted) - {c["name"] for c in cases}
        if unknown:
            sys.exit(f"unknown cases: {', '.join(sorted(unknown))}")
        cases = [c for c in cases if c["name"] in wanted]
    if getattr(args, "areas", None):
        cases = [c for c in cases if c["area"] in args.areas.split(",")]
    return cases


# --- where cases run -------------------------------------------------------------------------

# docker: each case in a container of the pinned Rust image, its files in the volumes above.
# native: each case a host process in a capped user scope (systemd-run), on a host without
# Docker (benches/hosts.toml): the binaries a build host made (NRESE_FAST_BIN, laid out as a
# target directory), the datasets from the verified inputs (read-only), generated data in a
# directory of the host's (NRESE_FAST_DATA), peak memory from the scope's cgroup.
RUNTIME = os.environ.get("NRESE_FAST_RUNTIME") or ("native" if hosts.lacking(["docker"]) else "docker")
FAST_DATA = Path(os.environ.get("NRESE_FAST_DATA") or Path.home() / "nrese-bench" / "fast-data")
# A container path at the start of a word: the native runtime's directory for it.
CONTAINER_PATH = re.compile(r"(?<![\w./:-])(/target|/fast|/data|/src|/out|/inputs|/ore)(?=/|\s|'|\"|;|$)")
# The scope's peak, printed by the native runtime's wrapper after the command.
SCOPE_PEAK = re.compile(r"^fast-scope-peak (\d+)$", re.M)
# The scope's pressure stall totals (PSI, µs): time its tasks waited for a CPU, for I/O (the page
# cache missing), for memory (reclaim). What a process met that another didn't, per case.
SCOPE_PRESSURE = re.compile(r"^fast-scope-pressure((?: \w+=\d*)*)$", re.M)


def native_dirs() -> dict[str, Path]:
    """The native runtime's directory for each container path."""
    binaries = os.environ.get("NRESE_FAST_BIN")
    if not binaries:
        raise RuntimeError("missing input: the native runtime needs NRESE_FAST_BIN, a build host's target "
                           "directory (release/examples/perf_lab, release/nrese-server, ...)")
    data = hosts.input_path("general")
    return {"/target": Path(binaries), "/fast": FAST_DATA, "/data": data or FAST_DATA / "no-datasets",
            "/src": ROOT, "/out": SCRATCH, "/inputs": FAST_INPUTS or FAST_DATA / "no-inputs",
            "/ore": ORE_DIR or FAST_DATA / "no-ore"}


def native(text: str, dirs: dict[str, Path] | None = None) -> str:
    """`text` with its container paths as the native runtime's directories."""
    dirs = dirs or native_dirs()
    return CONTAINER_PATH.sub(lambda m: dirs[m.group(1)].as_posix(), text)


def files(script: str) -> subprocess.CompletedProcess:
    """A shell script over the fast data (/fast), the datasets (/data, read-only) and the
    verified inputs (/inputs, read-only): in an Alpine container, or natively."""
    if RUNTIME == "native":
        FAST_DATA.mkdir(parents=True, exist_ok=True)
        return subprocess.run(["sh", "-c", native(script)], capture_output=True, text=True, encoding="utf-8",
                              errors="replace")
    mounts = ["-v", f"{DATA_VOLUME}:/fast", "-v", f"{BENCH_VOLUME}:/data:ro"]
    if FAST_INPUTS is not None:
        mounts += ["-v", f"{host_path(FAST_INPUTS)}:/inputs:ro"]
    return docker(["run", "--rm", *mounts, "alpine", "sh", "-c", script])


def docker(args: list[str], timeout: float | None = None, capture: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run([ns.executable(), *args], capture_output=capture, text=True, timeout=timeout, env=ENV,
                          encoding="utf-8", errors="replace")


def capped(gb: float) -> float:
    """A container's memory within a machine's ceiling for the whole run (FAST_MAX_GB, as on
    a shared PC), whatever a case asks for."""
    limit = os.environ.get("FAST_MAX_GB")
    return min(gb, float(limit)) if limit else gb


def quiet_label() -> list[str]:
    """The quiet slot's token as a container label: the measurement's own containers."""
    token = ENV.get("NRESE_QUIET_TOKEN") or os.environ.get("NRESE_QUIET_TOKEN")
    labels = ["--label", f"nrese.quiet={token}"] if token else []
    # A queue job's containers: its host's guard stops them with the job (benches/suite/queue.py).
    if os.environ.get("QUEUE_JOB"):
        labels += ["--label", f"nrese.queue={os.environ['QUEUE_JOB']}"]
    return labels


def cpu_args() -> list[str]:
    """`--cpus N` when a run gives every container the same CPUs (DOCKER_CPUS, as the suite)."""
    cpus = os.environ.get("DOCKER_CPUS")
    return ["--cpus", cpus] if cpus else []


@contextlib.contextmanager
def quiet_slot(what: str):
    """Holds the machine's quiet slot (scripts/quiet-slot.sh: one timing at a time, no build of
    ours running, new builds waiting) while the block runs. A shell holds it until its stdin
    closes, so waiting for the slot doesn't count against a case's timeout and the slot is
    freed if this process dies. NRESE_QUIET_SLOT=0 skips it (counts need no slot)."""
    if os.environ.get("NRESE_QUIET_SLOT") == "0":
        yield
        return
    bash = os.environ.get("NRESE_BASH") or (r"C:\Program Files\Git\bin\bash.exe" if os.name == "nt" else "bash")
    holder = subprocess.Popen([bash, str(ROOT / "scripts" / "quiet-slot.sh"), "bash", "-c",
                               'echo "held $NRESE_QUIET_TOKEN"; read _', what],
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, env=ENV)
    said = holder.stdout.readline().split()
    if not said or said[0] != "held":
        holder.kill()
        raise RuntimeError(f"quiet slot not taken for {what}")
    # The slot's token labels the measurement's containers: the slot's watcher pauses others.
    ENV["NRESE_QUIET_TOKEN"] = said[1] if len(said) > 1 else ""
    try:
        yield
    finally:
        ENV.pop("NRESE_QUIET_TOKEN", None)
        holder.stdin.close()
        holder.wait(timeout=60)


def container(kind: str, cap_gb: float, command: list[str], env: dict | None = None,
              timeout: float | None = CASE_TIMEOUT_S) -> tuple[int, str, str, float]:
    """Runs `command` under a memory cap without swap, in the Rust image or natively; (exit
    code, stdout, stderr, wall seconds). Exit 137: killed at the cap. Its name: `kind` in the
    job's namespace."""
    name = ns.name(f"fast-{kind}", ROOT)
    cap_gb = capped(cap_gb)
    SCRATCH.mkdir(parents=True, exist_ok=True)
    if RUNTIME == "native":
        return process(name, cap_gb, command, env, timeout)
    argv = ["run", "--rm", *ns.name_args(name, ROOT, OWNER), f"--memory={cap_gb}g", f"--memory-swap={cap_gb}g", *cpu_args(),
            *quiet_label(),
            "-v", f"{TARGET_VOLUME}:/target:ro", "-v", f"{DATA_VOLUME}:/fast",
            "-v", f"{BENCH_VOLUME}:/data:ro", "-v", f"{host_path(ROOT)}:/src:ro",
            "-v", f"{host_path(SCRATCH)}:/out"]
    for key, value in (env or {}).items():
        argv += ["-e", f"{key}={value}"]
    argv += [pinned(rust_image()), *command]
    started = time.monotonic()
    try:
        done = docker(argv, timeout=timeout)
    except subprocess.TimeoutExpired:
        docker(["rm", "-f", name])
        return 124, "", f"timed out after {timeout} s", time.monotonic() - started
    return done.returncode, done.stdout, done.stderr, time.monotonic() - started


def process(name: str, cap_gb: float, command: list[str], env: dict | None = None,
            timeout: float | None = CASE_TIMEOUT_S) -> tuple[int, str, str, float]:
    """`container` natively: the command, its container paths mapped (native_dirs), in a user
    scope capped at `cap_gb` without swap (as Docker's --memory and --memory-swap). A wrapper
    shell in the scope prints the scope's memory.peak after the command (`fast-scope-peak`
    on stderr), also after a kill at the cap."""
    dirs = native_dirs()
    FAST_DATA.mkdir(parents=True, exist_ok=True)
    unit = f"{name}-{os.getpid()}-{int(time.monotonic() * 1000)}"
    # The process's initial stack sits below its environment: a pad of random size each process
    # moves it (Mytkowicz et al., ASPLOS 2009), on top of address randomisation.
    pad = {"NRESE_LAYOUT_PAD": "x" * random.SystemRandom().randrange(0, 4096)}
    wrapper = ('g=/sys/fs/cgroup$(cut -d: -f3 /proc/self/cgroup); "$@"; rc=$?; '
               'echo "fast-scope-peak $(cat "$g/memory.peak" 2>/dev/null)" >&2; p=; '
               'for r in cpu io memory; do p="$p $r=$(sed -n "s/^some .*total=//p" "$g/$r.pressure" 2>/dev/null)"; done; '
               'echo "fast-scope-pressure$p" >&2; exit $rc')
    # At the cap the kernel kills the command alone (OOMPolicy=continue): the wrapper then still prints the peak.
    # Pinned (pinned_cpus) the command and all it starts stay on one L3 and NUMA node.
    cpus = pinned_cpus()
    argv = [*scope_argv(unit, f"{int(cap_gb * 1024)}m"), *(["taskset", "-c", cpus] if cpus else []),
            "sh", "-c", wrapper, "sh", *(native(c, dirs) for c in command)]
    started = time.monotonic()
    cleanup_error = ""
    try:
        done = subprocess.run(argv, capture_output=True, text=True, timeout=timeout, encoding="utf-8",
                              errors="replace", env={**ENV, **pad, **{k: str(v) for k, v in (env or {}).items()}},
                              pass_fds=tuple(map(int, os.environ.get("SUITE_LEASE_FDS", "").split())))
    except subprocess.TimeoutExpired:
        done = subprocess.CompletedProcess(argv, 124, "", f"timed out after {timeout} s")
    finally:
        # Nothing outlives its case: what the scope still holds (a child the command left) goes.
        try:
            end_scope(unit)
        except Exception as error:
            cleanup_error = f"scope cleanup failed ({unit}): {error}"
            # Report a secondary cleanup failure without replacing an in-flight exception.
            print(cleanup_error, file=sys.stderr)
    # The peak first: a record's note is the last line of stderr, the command's own.
    peaks = SCOPE_PEAK.findall(done.stderr)
    pressures = SCOPE_PRESSURE.findall(done.stderr)
    err = SCOPE_PRESSURE.sub("", SCOPE_PEAK.sub("", done.stderr)).rstrip("\n")
    err = ((f"fast-scope-peak {peaks[-1]}\n" if peaks else "")
           + (f"fast-scope-pressure{pressures[-1]}\n" if pressures else "") + err)
    if cleanup_error:
        err += "\n" + cleanup_error
    return done.returncode or (1 if cleanup_error else 0), done.stdout, err, time.monotonic() - started


def scope_peak_mib(stderr: str) -> int | None:
    """The native runtime's scope peak, in MiB, if the wrapper printed it."""
    found = SCOPE_PEAK.findall(stderr or "")
    return int(found[-1]) // 1048576 if found else None


def scope_stalls_ms(stderr: str) -> dict | None:
    """The native runtime's pressure stalls of the scope, ms per resource, if printed."""
    found = SCOPE_PRESSURE.findall(stderr or "")
    if not found:
        return None
    pairs = (item.split("=") for item in found[-1].split())
    return {k: round(int(v) / 1000, 1) for k, v in pairs if v.isdigit()} or None


def binaries() -> dict:
    return manifest.binaries(native_dirs()["/target"], os.environ.get("NRESE_FAST_REVISION", ""))


# --- build -----------------------------------------------------------------------------------

BUILD = [
    ["-p", "nrese-store", "--example", "perf_lab", "--example", "ql_print_check"],
    ["-p", "nrese-dl", "--example", "tableau_consistency", "--example", "context_classify"],
    ["-p", "nrese-reasoner", "--example", "classify"],
    ["-p", "nrese-server"],
]


def build(args) -> int:
    if RUNTIME == "native":
        sys.exit("the native runtime builds nothing: a build host's binaries come in NRESE_FAST_BIN "
                 "(scripts/cargo-guarded.sh build --release --locked with the packages of fast.py BUILD)")
    pin_images([rust_image()])
    jobs =os.environ.get("CARGO_BUILD_JOBS", "4")
    cap = os.environ.get("NRESE_MEMORY_CAP_GB", "8")
    docker(ns.volume("create", TARGET_VOLUME, ROOT, OWNER))
    print(f"build volume {TARGET_VOLUME} (fast.py clean --build removes it)", flush=True)
    for packages in BUILD:
        command = ["run", "--rm", *ns.name_args(ns.name("fast-build", ROOT), ROOT, OWNER),
                   f"--memory={cap}g", f"--memory-swap={cap}g",
                   f"--cpus={jobs}", "-v", f"{host_path(ROOT)}:/src:ro", "-v", f"{TARGET_VOLUME}:/target",
                   "-v", f"{CARGO_VOLUME}:/usr/local/cargo/registry", "-e", "CARGO_TARGET_DIR=/target",
                   "-e", f"CARGO_BUILD_JOBS={jobs}", "-e", f"NRESE_MEMORY_CAP_GB={cap}", "-w", "/src", pinned(rust_image()),
                   "bash", "scripts/cargo-guarded.sh", "build", "--release", "--locked", "--quiet", *packages]
        print("build:", " ".join(packages), flush=True)
        done = docker(command, capture=False)
        if done.returncode != 0:
            return done.returncode
    return 0


# --- data ------------------------------------------------------------------------------------

def data_file(spec: str) -> str:
    """The container path of a data spec: `volume:FILE` from the dataset volume, `ore:ID`
    an ORE ontology converted to N-Triples, `concat:A+B` the specs' files concatenated, or a
    generated file in the fast suite's volume."""
    if spec.startswith("ore:"):
        return f"/fast/ore/{spec[len('ore:'):]}.nt"
    if spec.startswith("concat:"):
        return "/fast/concat-" + "-".join(fast_data_name(p).removesuffix(".nt") for p in spec[7:].split("+")) + ".nt"
    return ("/data/" if spec.startswith("volume:") else "/fast/") + fast_data_name(spec)


# Verified inputs on this host (benches/hosts.toml; the variables override): the ORE corpus,
# and a read-only copy of the data volume's converted ontologies and reference taxonomies.
ORE_DIR = hosts.input_path("ore")
FAST_INPUTS = hosts.input_path("fast")


def seed_from_inputs(target: str) -> bool:
    """Copies `/fast/<relative>` and its sidecars (`<relative>.*`: the generator's answers,
    commit files) from the verified inputs into the fast data when it lacks the file and the
    inputs have it; whether the fast data has it now."""
    if files(f"test -s {target}").returncode == 0:
        return True
    relative = target.removeprefix("/fast/")
    if FAST_INPUTS is None or not (FAST_INPUTS / relative).is_file():
        return False
    print(f"  copying {relative} from {FAST_INPUTS}", flush=True)
    parent = Path(target).parent.as_posix()
    files(f"mkdir -p {parent} && for f in /inputs/{relative} /inputs/{relative}.*; do "
          f"[ -e \"$f\" ] && cp -r \"$f\" {parent}/; done; true")
    return files(f"test -s {target}").returncode == 0


# A release gate's passes (release_gate.py): `run --prepared` reads data `prepare` made with
# one generator, the gate's, before any pass, and generates nothing itself (a pass on an
# engine's binaries would otherwise generate with that engine's perf lab: main's has no
# generator, an older one may generate other data). `--no-routes` and `--empty-queries` run a
# perf lab older than the harness (main's): no route report, `--queries` required.
OPTIONS = {"prepared": False, "routes": True, "empty_queries": False}
PREPARED = "PREPARED.json"

# The release gate's measurement of each case (`--measure benches/fast/measure.toml`, written by
# release_gate.py calibrate from the gate's own noise): the perf lab's runs and warm-ups a
# process, and the case's repetitions (fresh processes for the perf lab, files a process for
# the DL tools), the same for every side of a gate whatever the case's own arguments say.
# `batch`: executions a timed sample (perf_lab --batch), of the queries `batch_queries` names
# (all without), applied only with `run --batch`, when every side's perf lab has it (the
# gate's CAPABILITIES).
# `processes`: a DL tool's fresh processes a pass, pooled (its `repeat` runs inside one process;
# a process's memory layout, hence its time, differs from the next one's).
MEASURE: dict[str, dict] = {}
MEASURED_KEYS = ("runs", "warmup", "repeat", "batch", "batch_queries", "processes")


def load_measure(path: Path) -> tuple[dict[str, dict], str]:
    """The calibration's cases (only the keys fast.py applies) and the file's SHA-256."""
    data = path.read_bytes()
    cases = tomllib.loads(data.decode("utf-8")).get("case", {})
    return ({name: {k: (v if k == "batch_queries" else int(v)) for k, v in entry.items() if k in MEASURED_KEYS}
             for name, entry in cases.items()}, hashlib.sha256(data).hexdigest())


def with_flag(args: list[str], flag: str, value) -> list[str]:
    """`args` with `flag VALUE`: the existing value replaced (every occurrence), else appended."""
    out, found = list(args), False
    for i, a in enumerate(out[:-1]):
        if a == flag:
            out[i + 1], found = str(value), True
    return out if found else out + [flag, str(value)]


def measured(case: dict) -> dict:
    """The case as the calibration measures it (MEASURE): the case itself without an entry."""
    m = MEASURE.get(case["name"])
    if not m:
        return case
    case = dict(case)
    if case.get("tool", "perf_lab") == "perf_lab":
        args = list(case.get("args", []))
        for key in ("runs", "warmup") + (("batch", "batch_queries") if OPTIONS.get("batch") else ()):
            if key in m:
                value = ",".join(m[key]) if key == "batch_queries" else m[key]
                args = with_flag(args, "--" + key.replace("_", "-"), value)
        case["args"] = args
    for key in ("repeat", "processes"):
        if key in m:
            case[key] = m[key]
    return case


def warm_inputs(case: dict) -> int:
    """Reads a warm case's input files once before its timing, so that every pass meets them
    in the page cache whatever ran before (a large case evicting them, the first pass after
    prepare); the bytes read. Cold runs empty the cache themselves (drop_page_cache)."""
    if RUNTIME != "native" or "cold" in case.get("page_cache", ["warm"]):
        return 0
    total = 0
    for path in input_files(case):
        try:
            with path.open("rb") as stream:
                while chunk := stream.read(1 << 22):
                    total += len(chunk)
        except OSError:
            continue  # a missing file is the case's own failure to report
    return total


def pinned_cpus() -> str | None:
    """The CPUs timings run on (taskset notation): NRESE_FAST_CPUS, else the host
    configuration's `cpus` (benches/hosts.toml: one L3 and NUMA node), else unpinned."""
    if os.environ.get("NRESE_FAST_CPUS") is not None:
        return os.environ["NRESE_FAST_CPUS"] or None
    found = hosts.current()
    config = hosts.configuration(found[0]) if found else None
    return (config or {}).get("cpus")


def host_state() -> dict | None:
    """The host's state now (scripts/hosts/hygiene.py): frequency policy, THP, load, others
    busy; None off Linux."""
    if RUNTIME != "native" or not Path("/proc/self").exists():
        return None
    cpus = pinned_cpus()
    done = subprocess.run([sys.executable, str(ROOT / "scripts" / "hosts" / "hygiene.py"),
                           *(["--cpus", cpus] if cpus else []), "--sample-s", "1"],
                          capture_output=True, text=True, timeout=60)
    try:
        return json.loads(done.stdout)
    except ValueError:
        return {"problems": [f"hygiene.py failed: {done.stderr.strip()[-200:]}"]}


def ensure_data(spec: str) -> dict:
    """Generates `spec` once (or takes it from the verified inputs); its `expect` (the
    generator's own answers)."""
    if spec.startswith("volume:") or spec == "none":
        return {}
    if spec.startswith("ore:"):
        ensure_ore(spec[len("ore:"):])
        return {}
    if spec.startswith("concat:"):
        target = data_file(spec)
        parts = " ".join(data_file(part) for part in spec[len("concat:"):].split("+"))
        if OPTIONS["prepared"]:
            if files(f"test -s {target}").returncode != 0:
                raise RuntimeError(f"missing input: {spec} was not prepared (fast.py prepare)")
            return {}
        # Into the shared volume whole, one job at a time per host (FAST_LOCK): another job may
        # be reading or making the same file.
        part = f"{target}.{ns.name('part', ROOT)}"
        with ns.lock(FAST_LOCK):
            files(f"test -s {target} || {{ cat {parts} > {part} && mv -f {part} {target}; }}")
        return {}
    path = data_file(spec)
    seed_from_inputs(path)
    found = files(f"cat {path}.expect.json")
    if found.returncode == 0 and found.stdout.strip():
        return json.loads(found.stdout)["expect"]
    if OPTIONS["prepared"]:
        raise RuntimeError(f"missing input: {spec} was not prepared (fast.py prepare)")
    kind, *params = spec.split()
    # One job at a time per host makes a shared file (FAST_LOCK), checked again under the lock;
    # it is generated into this job's staging directory beside it (the same file system) and
    # published whole: its side files and data first, the answers (.expect.json, what says a
    # file is there) last. A reader never sees a half-written file under its name.
    with ns.lock(FAST_LOCK):
        found = files(f"cat {path}.expect.json")
        if found.returncode == 0 and found.stdout.strip():
            return json.loads(found.stdout)["expect"]
        print(f"  generating {path}", flush=True)
        folder, name = path.rsplit("/", 1)
        stage = f"{folder}/.{ns.name('gen', ROOT)}"
        script = (f"rm -rf {stage} && mkdir -p {stage} && /target/release/examples/perf_lab generate {kind} "
                  f"{stage}/{name} {' '.join(params)} > {stage}/{name}.expect.json"
                  f" && for f in {stage}/{name}.*; do case $f in *.expect.json) ;; *) mv -f $f {folder}/ ;; esac; done"
                  f" && mv -f {stage}/{name} {path} && mv -f {stage}/{name}.expect.json {path}.expect.json"
                  f" && rm -rf {stage} && cat {path}.expect.json")
        code, out, err, _ = container("gen", 8, ["bash", "-c", script])
    if code != 0:
        raise RuntimeError(f"generating {spec} failed: {err[-500:]}")
    return json.loads(out)["expect"]


@functools.cache
def dl_image() -> str:
    """The DL kit's reference image (benches/reasoning/dl), or its verified preserved alias here."""
    return hosts.image("nrese-bench/dl-reference")


def ensure_ore(name: str):
    """An ORE 2015 ontology (functional syntax, from the corpus cache; CC BY-NC-ND, never
    committed) converted once to N-Triples by the DL kit's runner, for the NRESE tools."""
    target = f"/fast/ore/{name}.nt"
    if seed_from_inputs(target):
        return
    if ORE_DIR is None or not (ORE_DIR / f"{name}.owl").exists():
        raise RuntimeError(f"missing input: {name}.owl, ORE 2015 corpus at {ORE_DIR or 'no path'} "
                           "(benches/hosts.toml inputs.ore, or NRESE_ORE_DIR)")
    if RUNTIME == "native":
        raise RuntimeError(f"missing input: ore/{name}.nt (the native runtime converts no ontology: the DL kit's "
                           "converter runs in Docker; the verified inputs, hosts.toml inputs.fast, hold the converted ones)")
    print(f"  converting {name} to N-Triples", flush=True)
    script = (f"mkdir -p /fast/ore && cp /ore/{name}.owl /fast/ore/{name}.owl && "
              f"printf '{name}\\tconvert\\tntriples\\t/fast/ore/{name}.owl\\n' > /fast/ore/{name}.manifest.tsv")
    docker(["run", "--rm", "-v", f"{DATA_VOLUME}:/fast", "-v", f"{host_path(ORE_DIR)}:/ore:ro", "alpine", "sh", "-c", script])
    docker(["run", "--rm", "--memory=10g", "-v", f"{DATA_VOLUME}:/fast", pinned(dl_image()), "batch",
            f"/fast/ore/{name}.manifest.tsv", f"/fast/ore/{name}.results.tsv", "/fast/ore", "600"], timeout=900)
    files(f"rm -f /fast/ore/{name}.owl /fast/ore/{name}.manifest.tsv /fast/ore/{name}.results.tsv")
    if files(f"test -s {target}").returncode != 0:
        raise RuntimeError(f"converting {name} failed")


def vector_queries(expect: dict, exact: bool) -> Path:
    """The vector cases' queries, written from the generator's query vectors."""
    directory = SCRATCH / "queries" / ("vectors-exact" if exact else "vectors")
    directory.mkdir(parents=True, exist_ok=True)
    option = " ; nrv:exact true" if exact else ""
    for q in range(5):
        (directory / f"knn-{q}.rq").write_text(
            "PREFIX e: <http://example.org/fast/>\nPREFIX nrv: <urn:nrese:vector:>\n"
            f"SELECT ?item WHERE {{ ?item e:kind e:k3 ; e:embedding ?v .\n"
            f"  SERVICE nrv:search {{ ?v nrv:near \"{expect[f'query_{q}']}\"^^nrv:vector ; nrv:k 10{option} ;"
            " nrv:rank ?r } }\nORDER BY ?r\n", encoding="utf-8")
    return directory


def parse_queries() -> Path:
    """Large and deeply nested queries for the parse-and-rewrite cases (deterministic)."""
    directory = SCRATCH / "queries" / "parse"
    directory.mkdir(parents=True, exist_ok=True)
    for old in directory.glob("*.rq"):
        old.unlink()
    for name, text in parse_query_definitions().items():
        (directory / f"{name}.rq").write_text("PREFIX e: <http://example.org/fast/>\n" + text + "\n", encoding="utf-8")
    return directory


def parse_query_definitions() -> dict:
    return {
        "values-5000": "SELECT ?u ?n WHERE { VALUES ?u { " + " ".join(f"e:u{i}" for i in range(5000))
                       + " } ?u e:name ?n }",
        "bgp-1000": "SELECT * WHERE { " + " ".join(f"?x{i} e:follows ?x{i + 1} ." for i in range(1000)) + " }",
        "union-500": "SELECT ?x WHERE { " + " UNION ".join(f"{{ ?x e:city e:c{i} }}" for i in range(500)) + " }",
        "optional-120-nested": "SELECT * WHERE { ?x0 e:follows ?x1 "
                               + "".join(f"OPTIONAL {{ ?x{i} e:follows ?x{i + 1} " for i in range(1, 120))
                               + "}" * 119 + " }",
        "subqueries-50-deep": "SELECT ?x WHERE { " + "{ SELECT ?x WHERE { " * 50 + "?x e:follows e:u1 "
                              + "} } " * 50 + "}",
        "filter-or-2000": "SELECT ?u WHERE { ?u e:age ?a FILTER(" + " || ".join(f"?a = {i}" for i in range(2000)) + ") }",
        "path-alternatives-300": "SELECT ?y WHERE { e:u1 (" + "|".join(f"e:p{i}" for i in range(300)) + ")+ ?y }",
        "expression-depth-120": "SELECT ?v WHERE { ?u e:age ?a BIND(" + "(" * 120 + "?a" + " + 1)" * 120 + " AS ?v) }",
    }


def cache_log(entries: int = 1000, seed: int = 7) -> Path:
    """A repeating query log over the social network, replayed in order (one file per
    entry, `--warmup 0 --runs 1`): as in a real endpoint's log, a few queries recur often
    (ranks drawn by a power law) and a long tail of parameterised lookups seldom does."""
    directory = SCRATCH / "queries" / "cache-log"
    directory.mkdir(parents=True, exist_ok=True)
    for old in directory.glob("*.rq"):
        old.unlink()
    prefix = "PREFIX e: <http://example.org/fast/>\nPREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>\n"
    rng = random.Random(seed)
    zipf = lambda n: min(n - 1, int(n ** rng.random()) - 1)  # noqa: E731
    templates = [
        lambda: f"SELECT ?p ?o WHERE {{ e:u{zipf(300000)} ?p ?o }}",
        lambda: f"SELECT ?f WHERE {{ ?f e:follows e:u{zipf(300000)} }}",
        lambda: f"SELECT ?u ?n WHERE {{ ?u e:city e:c{zipf(1000)} ; e:name ?n }} LIMIT 100",
        lambda: f"SELECT (COUNT(*) AS ?n) WHERE {{ ?p e:tag e:t{zipf(2000)} }}",
        lambda: f"SELECT ?p ?s WHERE {{ ?p e:author e:u{zipf(300000)} ; e:score ?s }}",
    ]
    heavy = [
        "SELECT ?t (COUNT(*) AS ?n) WHERE { ?p e:tag ?t } GROUP BY ?t ORDER BY DESC(?n) LIMIT 20",
        "SELECT ?c (COUNT(*) AS ?n) WHERE { ?p e:likes ?u . ?u e:city ?c } GROUP BY ?c ORDER BY DESC(?n) LIMIT 10",
        "SELECT (COUNT(*) AS ?n) WHERE { ?a e:follows ?b . ?b e:follows ?c . ?c e:follows ?a }",
        "SELECT ?t (AVG(?s) AS ?avg) WHERE { ?p e:tag ?t ; e:score ?s } GROUP BY ?t HAVING (COUNT(*) > 100)",
    ]
    for i in range(entries):
        roll = rng.random()
        text = heavy[zipf(len(heavy))] if roll < 0.05 else templates[zipf(len(templates))]()
        (directory / f"{i:05d}.rq").write_text(prefix + text + "\n", encoding="utf-8")
    return directory


def queries_path(case: dict, expect: dict) -> str | None:
    name = case.get("queries", "")
    if not name:
        return None
    if name.startswith("@vectors"):
        return f"/out/queries/{name[1:]}"
    if name == "@parse":
        parse_queries()
        return "/out/queries/parse"
    if name == "@cache-log":
        cache_log()
        return "/out/queries/cache-log"
    if name.startswith("@"):
        # Another kit's query set: benches/competitors/queries/NAME, or, natively, a generated
        # set kept with the data (NRESE_FAST_QUERY_SETS/NAME: WatDiv's, never committed; their
        # SHA-256 in benches/sparql/queries.sha256, checked by release-gate.sh prepare).
        kept = os.environ.get("NRESE_FAST_QUERY_SETS")
        if kept and RUNTIME == "native" and (Path(kept) / name[1:]).is_dir():
            return (Path(kept) / name[1:]).as_posix()
        return f"/src/benches/competitors/queries/{name[1:]}"
    if name in ("lubm", "owl2bench"):
        return f"/src/benches/reasoning/queries/{name}"
    return f"/src/benches/fast/queries/{name}"


# --- one case --------------------------------------------------------------------------------

def substitute(values: list[str], data: str, scratch: str) -> list[str]:
    return [v.replace("{data}", data).replace("{scratch}", scratch) for v in values]


def parse_tableau(stdout: str) -> list[dict]:
    """tableau_consistency's lines: file, answer, `key=value` metrics, reason."""
    rows = []
    for line in stdout.splitlines():
        parts = line.split("\t")
        if len(parts) < 3:
            continue
        row = {"file": parts[0], "answer": parts[1]}
        for pair in parts[2].split():
            key, _, value = pair.partition("=")
            try:
                row[key] = float(value)
            except ValueError:
                row[key] = value
        rows.append(row)
    return rows


def profile_line(stderr: str, stdout: str, prefix: str = "profile ") -> dict:
    for line in reversed((stderr + "\n" + stdout).splitlines()):
        if line.startswith(prefix):
            out = {}
            for pair in line[len(prefix):].split():
                key, _, value = pair.partition("=")
                try:
                    out[key] = float(value)
                except ValueError:
                    out[key] = value
            return out
    return {}


def run_case(case: dict, label: str, rep: int) -> dict:
    """Runs a case once; the record: status, metric, samples, memory, checks, routes."""
    case = measured(case)
    expect = ensure_data(case["data"])
    alt_expect = ensure_data(case["data_alt"]) if case.get("data_alt") else {}
    for extra in case.get("extra", []):
        ensure_data(extra)
    data = data_file(case["data"])
    if case.get("queries", "").startswith("@vectors"):
        vector_queries(expect, exact=case["queries"] == "@vectors-exact")
    # Times count only from the machine's quiet slot, held for this case's repetitions alone,
    # so other agents' builds wait for one case, not for the whole run.
    waited = time.monotonic()
    with quiet_slot(f"fast {case['name']}"):
        waited = time.monotonic() - waited
        warmed = warm_inputs(case)
        record = measure(case, label, rep, expect, alt_expect, data)
        # Not in a gate's pass (--prepared): the rerun past the limit times nothing the gate
        # judges, and took a quarter of a pass (dl-transitive-400: 226 of 1,018 s, 19 Oct 2026).
        if rep == 1 and case.get("frontier") and record["status"] in LIMITS and not OPTIONS["prepared"]:
            record["frontier"] = frontier(case, label, rep, expect, alt_expect, data)
    record["slot_wait_s"] = round(waited, 1)
    # How the case was measured: the calibration's settings, if it has an entry, and the bytes
    # of input read into the page cache first.
    record["measurement"] = {**MEASURE.get(case["name"], {}), "warmed_mib": warmed >> 20}
    return record


# A run that ended at one of these stopped at a limit of the case (its memory cap, its time),
# not with a wrong answer.
LIMITS = ("memory-limit", "timeout", "gave-up")


def frontier(case: dict, label: str, rep: int, expect: dict, alt_expect: dict, data: str) -> dict:
    """A case stopped at its limit, run once more past it (`frontier`: a larger `cap_gb`, a
    longer `timeout_s`), so that its record says what it needs rather than only that it
    didn't fit: that run's status, metric, peak and checks. Not a timing of the case."""
    relaxed = {**case, **case["frontier"]}
    print(f"      at its limit: once more with {', '.join(f'{k} = {v}' for k, v in case['frontier'].items())}",
          flush=True)
    record = measure(relaxed, label, rep, expect, alt_expect, data)
    return {"settings": case["frontier"], "status": record["status"], "metric": record.get("metric"),
            "peak_mib": record.get("peak_mib"), "phase_peaks": record.get("phase_peaks"),
            "checks": record.get("checks"), "notes": record.get("notes", [])[:2]}


HARNESS_FAILURES = ("missing input", "Unable to find image", "No such image", "pull access denied",
                    "No such container", "quiet slot not taken")


def verdict(case: dict, record: dict) -> dict:
    """What a run says, whatever was expected: `ok`; `wrong` (a check or a route failed);
    `incomplete` (stopped at a limit, or without its input); `unsupported`; or `failed`.
    `whose`: the product's, or the harness's (an input or image missing). A known issue the
    case declares (`known`) is named when the run repeats it."""
    status = record["status"]
    kind = {"ok": "ok", "wrong": "wrong", "off-route": "wrong", "unsupported": "unsupported"}.get(
        status, "incomplete" if status in LIMITS else "failed")
    notes = " ".join(record.get("notes", []))
    if kind == "ok":
        whose = "-"
    elif any(marker in notes for marker in HARNESS_FAILURES):
        kind, whose = "incomplete", "harness"
    else:
        # A failure without a known cause (an exit status) is nobody's yet: its notes say more.
        whose = "unknown" if kind == "failed" else "product"
    out = {"verdict": kind, "whose": whose}
    if kind != "ok" and status == record.get("expected_outcome") and case.get("known"):
        out["issue"] = case["known"]
    return out


def measure(case: dict, label: str, rep: int, expect: dict, alt_expect: dict, data: str) -> dict:
    """run_case's measurement, its data ready: the record."""
    name = case["name"]
    tool = case.get("tool", "perf_lab")
    out_json = f"/out/{case['name']}.json"
    record = {"case": case["name"], "rep": rep, "status": "ok", "notes": []}
    scratch = f"{VOLUME_SCRATCH}/{case['name']}"
    repeat = int(case.get("repeat", 1))
    # Each measured invocation gets the case's wall limit; 0 means unlimited in both runtimes.
    timeout_s = case.get("timeout_s", CASE_TIMEOUT_S) or None
    result: dict = {}
    observations = []
    if tool == "perf_lab":
        base = ["/target/release/examples/perf_lab", "--label", label,
                *(["--routes"] if OPTIONS["routes"] else []), "--json", out_json]
        if not OPTIONS["routes"]:
            record["notes"].append("routes unchecked: this perf lab has no --routes")
        loads = [] if "setup" in case or case["data"] == "none" else \
            [x for f in [case["data"], *case.get("extra", [])] for x in ("--load", data_file(f))]
        if case.get("tool_loads") is False:
            loads = []
        # The canonicalisation case reads its files itself.
        if any(a == "--canonicalize" for a in case.get("args", [])):
            loads = []
        queries = queries_path(case, expect)
        if not queries and OPTIONS["empty_queries"]:
            # An older perf lab requires --queries: an empty directory, no query run.
            (SCRATCH / "queries" / "empty").mkdir(parents=True, exist_ok=True)
            queries = "/out/queries/empty"
        if timed_metric(case) != case["metric"]:
            record["notes"].append(f"timed by its load: its metric ({case['metric']}) is the suite driver's")
        command = [*base, *loads, *(["--queries", queries] if queries else []),
                   *substitute(case.get("args", []), data, scratch)]
        script = ""
        if "setup" in case:
            setup = ["/target/release/examples/perf_lab", *substitute(case["setup"], data, scratch)]
            script = f"rm -rf {scratch} && mkdir -p {scratch} && {' '.join(setup)} >/dev/null 2>&1 && "
        if case.get("queries", "").startswith("@vectors"):
            command += ["--results", f"/out/results/{case['name']}"]
        samples_runs = []
        code = 0
        err = ""
        wall = 0.0
        sweep = case.get("sweep", [[]])
        modes, why = page_cache_modes(case)
        if why:
            record["notes"].append(why)
        record["page_cache"] = modes
        # Warm and cold alternate, so that drift over the case touches both alike.
        runs = invocation_plan(case, modes)
        for index, identity in enumerate(runs):
            entry, mode = sweep[identity["sweep"]], identity["cache"]
            if mode == "cold" and index > 0 and not drop_page_cache(record):
                break
            # The setup (a store on disk) runs once; each repeat is a fresh process opening it.
            shell = (script if index == 0 else "") + " ".join(f"'{c}'" for c in [*command, *entry])
            code, _, err, w = container(name, case["cap_gb"], ["bash", "-c", shell], case.get("env"), timeout=timeout_s)
            wall += w
            path = SCRATCH / f"{case['name']}.json"
            if not path.exists():
                record["status"] = "failed"
                record["notes"].append("invocation did not produce its JSON report")
                break
            result = json.loads(path.read_text(encoding="utf-8"))
            path.unlink()
            result["_sweep"] = " ".join(entry)
            result["_page_cache"] = mode
            if case.get("queries", "").startswith("@vectors") and code == 0:
                result["recall"] = vector_recall(case, expect)
            observation = invocation_observation(case, result, err, identity)
            observation["exit"] = code
            observation["checks"] = [check(p, w, result, expect, alt_expect) for p, w in case.get("check", {}).items()]
            observation["routes"] = [route(p, w, result) for p, w in case.get("route", {}).items()]
            observations.append(observation)
            samples_runs.append(result)
            if code != 0:
                break
        if "setup" in case:
            container(name, 1, ["rm", "-rf", scratch])
        if samples_runs:
            result = samples_runs[-1]
            if len(samples_runs) > 1:
                result = {**result, "_repeats": samples_runs}
        record["wall_s"] = round(wall, 2)
        if code != 0:
            record["status"] = classify_failure(code, err, result)
            record["notes"].append(err.strip().splitlines()[-1][:300] if err.strip() else f"exit {code}")
    elif tool == "tableau":
        files = [data] * repeat + ([data_file(case["data_alt"])] * repeat if case.get("data_alt") else [])
        timeout = str(case.get("timeout_s", 300))
        # The engine's budgets are the case's limits, as any system's in its container: the
        # time limit and the container's memory as the run's envelope, nothing tighter.
        envelope = str(int(case["cap_gb"] * 1024))
        command = ["/target/release/examples/tableau_consistency", "--timeout", timeout,
                   "--memory-mib", envelope, *files]
        # Fresh processes (`processes`), their rows pooled; a failing one ends the case.
        rows, wall = [], 0.0
        for _ in range(int(case.get("processes", 1))):
            code, out, err, w = container(name, case["cap_gb"], command, timeout=timeout_s)
            rows += parse_tableau(out)
            wall += w
            if code != 0:
                break
        # The tool names each file as it was given: natively, by its host path.
        named = native(data) if RUNTIME == "native" else data
        main = [r for r in rows if r["file"] == named]
        alt = [r for r in rows if r["file"] != named]
        result = dict(main[0]) if main else {}
        result["samples_ms"] = [r.get("whole_ms", 0.0) for r in rows]
        # The pipeline's phases, one series each (reading, normalising, then the tableau's).
        result["phase_samples"] = {phase: [r[f"{phase}_ms"] for r in rows if isinstance(r.get(f"{phase}_ms"), float)]
                                   for phase in DL_PHASES}
        result["whole_ms"] = statistics.median(result["samples_ms"]) if rows else None
        if alt:
            result["alt"] = {"answer": alt[0]["answer"]}
        record["wall_s"] = round(wall, 2)
        if code != 0 or not rows:
            record["status"] = classify_failure(code, err, result)
            record["notes"].append(err.strip()[-300:])
        elif result.get("answer") in ("gave-up", "timeout", "unsupported"):
            record["status"] = "gave-up"
    elif tool in ("classify-el", "classify-horn"):
        binary = "classify" if tool == "classify-el" else "context_classify"
        out_tsv = f"/out/{case['name']}.tsv"
        command = ["bash", "-c", f"/target/release/examples/{binary} --out {out_tsv} --repeat {repeat} {data}"
                                 f" && sha256sum {out_tsv} | cut -c1-16 && wc -l < {out_tsv}"]
        # Fresh processes (`processes`): each one's profile, pooled below; the last one's answer.
        profiles, wall = [], 0.0
        for _ in range(int(case.get("processes", 1))):
            code, out, err, w = container(name, case["cap_gb"], command, timeout=timeout_s)
            profiles.append(profile_line(err, out))
            wall += w
            if code != 0:
                break
        result = profiles[-1]
        lines = out.split()
        if code == 0 and len(lines) >= 2:
            result["hash"], result["subsumptions"] = lines[-2], int(lines[-1])
        # The context core's profile has `total`; the EL classifier's, its phases.
        phases = ("read", "normalise", "prepare", "saturate", "assemble", "write")
        total = result.get("total")
        if total is None and all(isinstance(result.get(k), float) for k in phases):
            total = sum(result[k] for k in phases)
        totals = [pr.get("total") if pr.get("total") is not None else
                  (sum(pr[k] for k in phases) if all(isinstance(pr.get(k), float) for k in phases) else None)
                  for pr in profiles]
        result["samples_ms"] = [x for x in totals if x is not None]
        result["whole_ms"] = statistics.median(result["samples_ms"]) if result["samples_ms"] else total
        result["phase_samples"] = {k: [pr[k] for pr in profiles if isinstance(pr.get(k), float)] for k in
                                   ("read", "owl", "normalise", "compile", "prepare", "saturate", "assemble", "write")
                                   if isinstance(result.get(k), float)}
        record["wall_s"] = round(wall, 2)
        if code != 0:
            record["status"] = classify_failure(code, err, result)
            record["notes"].append(err.strip()[-300:])
        elif case.get("reference"):
            if seed_from_inputs(case["reference"]):
                result["taxonomy_equal"] = same_taxonomy(case)
            else:  # no oracle is a missing input, not a wrong answer
                record["status"] = "failed"
                record["notes"].append(f"missing input: reference taxonomy {case['reference']} "
                                       "(fast.py compete writes it; benches/hosts.toml inputs.fast)")
    if tool != "perf_lab":
        observations.append({**invocation_observation(case, result, err, invocation_plan(case)[0]), "exit": code})
    record.update(invocation_observation(case, result, err, {}))
    record.pop("identity")
    record["observations"] = observations
    if case.get("metric") == "cold_ms":
        record["cache_peaks"] = {}
        for observation in observations:
            record["cache_peaks"].setdefault(observation["identity"]["cache"], []).append(observation["peak_mib"])
    record["checks"] = [check(path, want, result, expect, alt_expect) for path, want in case.get("check", {}).items()]
    record["routes"] = [route(path, want, result) for path, want in case.get("route", {}).items()]
    wanted = case.get("outcome", "ok")
    if record["status"] == "ok":
        # A check the tool can't report (ok None: --no-routes) is unchecked, not failed.
        if any(c["ok"] is False for c in record["checks"] + [c for o in observations for c in o.get("checks", [])]):
            record["status"] = "wrong"
        elif any(r["ok"] is False for r in record["routes"] + [r for o in observations for r in o.get("routes", [])]):
            record["status"] = "off-route"
    record["expected_outcome"] = wanted
    record["verdict"] = verdict(case, record)
    record["result"] = slim(result)
    return record


def invocation_plan(case: dict, modes: list[str] | None = None) -> list[dict]:
    """Process identities, distinct from repeats performed inside a single process."""
    if case.get("tool", "perf_lab") != "perf_lab":
        return [{"sweep": 0, "repeat": 0, "cache": "warm"}]
    return [{"sweep": s, "repeat": r, "cache": m}
            for s in range(len(case.get("sweep", [[]]))) for r in range(int(case.get("repeat", 1)))
            for m in (modes if modes is not None else case.get("page_cache", ["warm"]))]


def invocation_observation(case: dict, result: dict, stderr: str, identity: dict) -> dict:
    """Small measurement projection; raw answers stay with the existing answer recorder."""
    phases = {k: v.get("peak_mib") for k, v in (result.get("phases") or {}).items()}
    peak = process_peak(result.get("peak_mib"), phases)
    levels = client_throughput(result)
    return {"identity": identity, "samples": samples(case, result), "estimator": estimator(result),
            **({"batched": batched} if (batched := {q["name"]: q["batch"] for r in result.get("_repeats", [result])
                                                    for q in r.get("queries", []) if q.get("batch")}) else {}),
            "metric": metric_value(case, result), "counters": counters(case, result),
            **({"throughput": levels} if levels else {}),
            "rows": {q["name"]: q.get("rows") for q in result.get("queries", []) if "name" in q},
            "phase_peaks": phases, "peak_mib": peak, "memory_kind": "lifetime" if peak else "unknown",
            "cgroup_peak_mib": result.get("cgroup_peak_mib") or scope_peak_mib(stderr),
            **({"stall_ms": stalls} if (stalls := scope_stalls_ms(stderr)) else {})}


def estimator(result: dict) -> str:
    """How a process's query series were measured: every timed run (`samples`), or, from a
    perf lab that reports only each query's p50, min and max (main's, f9b515e8), one value per
    process (`process-p50`): its p50, the median the newer perf lab also reports
    (`times[len / 2]` in both). A comparison with such a side compares process p50s on
    both sides (release_gate.py), never its p50 against the other's raw samples."""
    queries = [q for q in result.get("queries", []) if "error" not in q]
    return "process-p50" if any("samples_ms" not in q and "p50_ms" in q for q in queries) else "samples"


def timed_metric(case: dict) -> str:
    """The metric a case is timed by here: its own; for a case of the suite driver's (its
    metric a description, BIBM's `ms per mix ...`) without queries here, its load."""
    metric = case["metric"]
    if " " in metric and not case.get("queries") and case.get("tool", "perf_lab") == "perf_lab":
        return "load_s"
    return metric


DL_PHASES = ("parse", "read", "normalise", "compile", "saturate", "blocking", "expand", "search", "datatypes")


# Deterministic work counters, recorded for every case and compared exactly: a change in
# them is a change in the work done, whatever the machine's noise.
COUNTERS = ("reason.new_facts", "reason.inferred", "reason.rounds",
            "commits.inferred_inserted", "commits.inferred_deleted", "commits.max_rounds",
            "held.index_mib", "held.dictionary_terms", "held.inferred",
            "clauses", "nodes_created", "branch_points", "backjumps", "clashes", "merges", "facts", "clauses_fired",
            "concepts", "contexts", "subsumers", "links", "conclusions", "duplicates", "dl_clauses", "functions",
            "shacl.results", "kernel.rows", "held.index_bytes",
            "reason.bindings", "reason.probes", "reason.passes", "cache.hits", "subset_checks", "clauses_generated", "clauses_kept",
            "redundant_forward", "redundant_backward", "contexts_created", "hyper", "pred")


def counters(case: dict, result: dict) -> dict:
    out = {}
    for path in (*COUNTERS, *case.get("counters", [])):
        value = lookup(path, result)
        if isinstance(value, (int, float)) and not isinstance(value, bool):
            out[path] = value
    for q in result.get("queries", []):
        for field in ("steps", "rewrites"):
            if field in q:
                out[f"q.{q['name']}.{field}"] = q[field]
    for phase, values in (result.get("phases") or {}).items():
        for field in ("allocations", "allocated_mib"):
            if field in values:
                out[f"phases.{phase}.{field}"] = values[field]
    return out


def same_taxonomy(case: dict) -> str | None:
    """Whether NRESE's closure (the classifier's `--out`) is the reference's taxonomy, in the
    DL kit's canonical form (benches/reasoning/dl/canonical.py `compare`): `"true"`,
    `"false"`, or `"signature-differs"` when the reference lacks classes NRESE classifies (it
    read a different ontology). The reference is a `.tax` file a reference reasoner wrote
    into the fast volume (`fast.py compete` keeps ELK's); None if it isn't there."""
    sys.path.insert(0, str(ROOT / "benches" / "reasoning" / "dl"))
    import canonical
    found = files(f"cat {case['reference']}")
    closure = SCRATCH / f"{case['name']}.tsv"
    if found.returncode != 0 or not closure.exists():
        return None
    (SCRATCH / f"{case['name']}.reference.tax").write_text(found.stdout, encoding="utf-8")
    pairs = [tuple(line.rstrip("\n").split("\t")[:2]) for line in open(closure, encoding="utf-8") if "\t" in line]
    return canonical.compare(found.stdout, pairs)


def classify_failure(code: int, err: str, result: dict) -> str:
    text = (err or "") + json.dumps(result.get("error", ""))
    if code == 137 or "memory limit" in text.lower() or "ProcessMemoryLimit" in text:
        return "memory-limit"
    if code == 124:
        return "timeout"
    if "unsupported:" in text:
        return "unsupported"
    return "failed"


def slim(result: dict) -> dict:
    """The result without per-sample arrays (they are kept in `samples`)."""
    out = {}
    for key, value in result.items():
        if key in ("_repeats", "phase_samples"):
            continue
        if key == "queries":
            out["queries"] = {q["name"]: {k: v for k, v in q.items() if k not in ("samples_ms", "name")}
                              for q in value}
        elif isinstance(value, dict):
            out[key] = {k: v for k, v in value.items() if k != "samples_ms"}
        elif key != "samples_ms":
            out[key] = value
    return out


def lookup(path: str, result: dict):
    """A value by its path: q.NAME.FIELD into the query list, else dotted keys."""
    if path.startswith("q."):
        _, name, field = path.split(".", 2)
        for q in result.get("queries", []):
            if q["name"] == name:
                # A field can go deeper: q.NAME.status.complete.
                value = q
                for part in field.split("."):
                    if not isinstance(value, dict) or part not in value:
                        return None
                    value = value[part]
                return value
        return None
    value = result
    for part in path.split("."):
        if not isinstance(value, dict) or part not in value:
            return None
        value = value[part]
    return value


def resolve(want, expect: dict):
    if isinstance(want, str) and want.startswith("expect."):
        return expect.get(want[len("expect."):])
    return want


def same(a, b) -> bool:
    try:
        return float(a) == float(b)
    except (TypeError, ValueError):
        return str(a).lower() == str(b).lower()


UNCHECKED = "not reported by this perf lab (no --routes): unchecked"


def check(path: str, want, result: dict, expect: dict, alt_expect: dict) -> dict:
    expected = resolve(want, alt_expect if path.startswith("alt.") else expect)
    got = lookup(path, result)
    shown = (want if isinstance(want, str) and (want[:2] in (">=", "<=") or want.startswith(("has ", "lacks ")))
             else expected)
    if got is None and not OPTIONS["routes"]:
        # A perf lab without --routes reports no query's value, status or operators: such a
        # check is unchecked (ok None), not a wrong answer; its rows still are checked.
        return {"path": path, "expected": shown, "got": None, "ok": None, "note": UNCHECKED}
    if isinstance(want, str) and (want[:2] in (">=", "<=") or want.startswith(("has ", "lacks "))):
        ok = compare_op(got, want)
    else:
        ok = got is not None and expected is not None and same(got, expected)
    return {"path": path, "expected": shown, "got": got, "ok": ok}


def compare_op(got, want: str) -> bool:
    op, _, value = want.partition(" ")
    if got is None:
        return False
    if op in ("has", "lacks"):
        # A list holds the value; a text contains it.
        present = value in got if isinstance(got, (list, str)) else value == got
        return present if op == "has" else not present
    try:
        g, v = float(got), float(value)
    except (TypeError, ValueError):
        return op == "==" and str(got).lower() == value.lower()
    return {"==": g == v, "<=": g <= v, ">=": g >= v}[op]


def route(path: str, want: str, result: dict) -> dict:
    got = lookup(path, result)
    if got is None and not OPTIONS["routes"]:
        return {"path": path, "expected": want, "got": None, "ok": None, "note": UNCHECKED}
    return {"path": path, "expected": want, "got": got, "ok": compare_op(got, want)}


def cold_series(result: dict, detailed: bool = False) -> dict[str, list[float]]:
    """cold_ms of each fresh process, by the page cache it met: `_` when the case ran in one
    mode, else `warm` and `cold`."""
    runs = result.get("_repeats", [result])
    modes = {r.get("_page_cache", "warm") for r in runs}
    out: dict[str, list[float]] = {}
    for r in runs:
        if detailed:
            mode = r.get("_page_cache", "warm")
            out.setdefault(f"{mode}/open", []).append(r["open_s"] * 1000 if "open_s" in r else None)
            for q in r.get("queries", []):
                for label, field in (("first", "first_ms"), ("repeat", "p50_ms")):
                    out.setdefault(f"{mode}/{q['name']}/{label}", []).append(q.get(field))
            continue
        value = cold_ms(r)
        if value is not None:
            out.setdefault("_" if len(modes) == 1 else r.get("_page_cache", "warm"), []).append(value)
    return out


def page_cache_modes(case: dict) -> tuple[list[str], str]:
    """The page-cache modes a case runs in here (`page_cache`, default warm only), and why a
    declared one is left out: emptying the cache needs the host's command (hosts.toml
    config `cold_cache`)."""
    wanted = case.get("page_cache", ["warm"])
    if "cold" in wanted and not cold_cache_command():
        why = "page cache: warm only (this host's configuration names no cold_cache command, hosts.toml)"
        return [m for m in wanted if m != "cold"] or ["warm"], why
    return wanted, ""


@functools.cache
def cold_cache_command() -> str | None:
    found = hosts.current()
    config = hosts.configuration(found[0]) if found else None
    return (config or {}).get("cold_cache")


def drop_page_cache(record: dict) -> bool:
    """Empties the page cache (the host's cold_cache command); a failure ends the case's
    runs as a harness failure."""
    done = subprocess.run(["sh", "-c", cold_cache_command()], capture_output=True, text=True)
    if done.returncode != 0:
        record["status"] = "failed"
        record["notes"].append(f"missing input: the cold-cache command failed: {(done.stderr or '').strip()[:200]}")
    return done.returncode == 0


def cold_ms(result: dict) -> float | None:
    """Opening the store and the first execution of each query, in ms."""
    if "open_s" not in result:
        return None
    firsts = [q.get("first_ms", q.get("p50_ms", 0.0)) for q in result.get("queries", []) if "error" not in q]
    return result["open_s"] * 1000.0 + sum(firsts)


def metric_value(case: dict, result: dict):
    case = {**case, "metric": timed_metric(case)}
    if case["metric"] == "cold_ms":
        # With the page cache warm and emptied: the sum of the two modes' medians.
        total = None
        for values in cold_series(result).values():
            if values:
                total = (total or 0.0) + statistics.median(values)
        return total
    value = lookup(case["metric"], result)
    if case["metric"] in ("load_s", "open_s") and value is not None:
        value = value * 1000.0
    if "_repeats" in result and case["metric"] in ("load_s", "open_s", "clients.p99_ms", "canonicalize.p50_ms"):
        values = [lookup(case["metric"], r) for r in result["_repeats"]]
        values = [v * (1000.0 if case["metric"] in ("load_s", "open_s") else 1.0) for v in values if v is not None]
        value = statistics.median(values) if values else value
    return value


def samples(case: dict, result: dict) -> dict:
    """Per series, the measured samples (ms) the metric is a median or sum of medians of."""
    case = {**case, "metric": timed_metric(case)}
    metric = case["metric"]
    if result.get("client_sweep"):
        return {f"{level['clients']} clients p99": [level["p99_ms"]] for level in result["client_sweep"]}
    if case.get("sweep") and "_repeats" in result:
        scale = 1000.0 if metric in ("load_s", "open_s") else 1.0
        series: dict[str, list[float]] = {}
        for r in result["_repeats"]:
            value = lookup(metric, r)
            if value is not None:
                series.setdefault(r.get("_sweep") or "-", []).append(value * scale)
        return series
    if result.get("phase_samples") and any(result["phase_samples"].values()):
        return {k: v for k, v in result["phase_samples"].items() if v}
    if metric == "cold_ms":
        return cold_series(result, detailed=True)
    if metric == "sum_p50_ms":
        # A perf lab without samples (main's): its p50, one value per process (estimator()).
        # Repeated processes (`repeat`) pool their samples: the pass's values are all of them.
        series: dict[str, list[float]] = {}
        for r in result.get("_repeats", [result]):
            for q in r.get("queries", []):
                if "error" not in q and ("samples_ms" in q or "p50_ms" in q):
                    series.setdefault(q["name"], []).extend(q["samples_ms"] if "samples_ms" in q else [q["p50_ms"]])
        return series
    section = metric.split(".")[0]
    # Repeated processes (`repeat`) pool their samples, as the queries' above.
    runs = result.get("_repeats", [result])
    if isinstance(result.get(section), dict) and "samples_ms" in result[section]:
        return {"_": [s for r in runs for s in (r.get(section) or {}).get("samples_ms", [])]}
    if "samples_ms" in result:
        return {"_": [s for r in runs for s in r.get("samples_ms", []) if s is not None]}
    if "_repeats" in result:
        values = [lookup(metric, r) for r in result["_repeats"]]
        scale = 1000.0 if metric in ("load_s", "open_s") else 1.0
        return {"_": [v * scale for v in values if v is not None]}
    value = metric_value(case, result)
    return {"_": [value]} if value is not None else {}


def case_manifest(catalog: list[dict], root: Path = ROOT) -> dict:
    """Existing definitions and intended series, without preparing data or writing queries."""
    series, queries, clients, required_counters, cache_modes = {}, {}, {}, {}, {}
    for case in catalog:
        name, query = case["name"], case.get("queries", "")
        required_counters[name] = case.get("counters", [])
        if "--clients" in case.get("args", []):
            clients[name] = case["args"][case["args"].index("--clients") + 1].split(",")
        directory = (queries_path(case, {}) if case.get("metric") in ("sum_p50_ms", "cold_ms")
                     and not query.startswith(("@vectors", "@parse", "@cache-log")) else None)
        paths = sorted((root / directory.removeprefix("/src/")).glob("*.rq")) if directory else []
        # A query set kept with the data (NRESE_FAST_QUERY_SETS: WatDiv's) is outside the tree:
        # named by its set and file, the same on every host.
        queries.update({(p.relative_to(root).as_posix() if p.is_relative_to(root) else f"{query}/{p.name}"):
                        hashlib.sha256(p.read_bytes()).hexdigest() for p in paths})
        if case.get("sweep"):
            series[name] = [" ".join(s) or "-" for s in case["sweep"]]
        elif case.get("metric") == "sum_p50_ms":
            if query.startswith("@vectors"):
                series[name] = [f"knn-{q}" for q in range(5)]
            elif query == "@parse":
                series[name] = list(parse_query_definitions())
            else:
                series[name] = [p.stem for p in paths]
        elif case.get("metric") == "cold_ms":
            modes = case.get("page_cache", ["warm"])
            cache_modes[name] = modes
            series[name] = [f"{m}/{part}" for m in modes
                            for part in ["open", *(f"{p.stem}/{phase}" for p in paths for phase in ("first", "repeat"))]]
        elif "--clients" in case.get("args", []):
            levels = case["args"][case["args"].index("--clients") + 1].split(",")
            series[name] = [f"{n} clients p99" for n in levels] if len(levels) > 1 else ["_"]
        elif case.get("tool", "perf_lab") == "perf_lab":
            series[name] = ["_"]
    fingerprint = hashlib.sha256(json.dumps([catalog, queries, series], sort_keys=True).encode()).hexdigest()
    return {"fingerprint": fingerprint, "series": series, "clients": clients,
            "counters": required_counters, "cache_modes": cache_modes,
            "invocations": {c["name"]: invocation_plan(c) for c in catalog},
            "invocation_series": {c["name"]: [
                (["_"] if c.get("sweep") else [s for s in series.get(c["name"], [])
                 if c["name"] not in cache_modes or s.startswith(i["cache"] + "/")])
                for i in invocation_plan(c)] for c in catalog}}


def vector_recall(case: dict, expect: dict) -> float:
    found = 0
    for q in range(5):
        path = SCRATCH / "results" / case["name"] / f"knn-{q}.out"
        if not path.exists():
            return 0.0
        got = {m.group(1) for m in re.finditer(r"(item\d+)>", path.read_text(encoding="utf-8"))}
        found += len(got & set(expect[f"nearest_{q}"]))
    return found / 50.0


# --- run -------------------------------------------------------------------------------------

def input_files(case: dict) -> list[Path]:
    """The host files a case reads (native runtime): its data, alternative and extra data with
    their sidecars (the generator's answers, commit files), and its reference taxonomy."""
    out = []
    specs = [case["data"], *([case["data_alt"]] if case.get("data_alt") else []), *case.get("extra", [])]
    for spec in specs:
        if spec == "none":
            continue
        path = Path(native(data_file(spec)))
        out.append(path)
        if not spec.startswith("volume:") and path.parent.is_dir():
            out += sorted(p for p in path.parent.glob(path.name + ".*") if p.is_file())
    if case.get("reference"):
        out.append(Path(native(case["reference"])))
    return out


def file_digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


# One copy of a host's prepared data for every gate (NRESE_FAST_SHARED; release-gate.sh sets it):
# each file under FAST_DATA lives once, read-only, as SHARED/blobs/<sha256>, hardlinked into each
# gate's FAST_DATA, and SHARED/index/<generator>.json says which files a data file and its
# sidecars were, so that the next gate links them in instead of generating them again (the data
# is deterministic in the generator, perf_lab/generate.rs, and its parameters). Office kept a
# 6.1 GB copy per gate name (19 Oct 2026).
SHARED = Path(os.environ["NRESE_FAST_SHARED"]) if os.environ.get("NRESE_FAST_SHARED") else None


def generator_key(root: Path = ROOT) -> str:
    return file_digest(root / "crates" / "nrese-store" / "examples" / "perf_lab" / "generate.rs")[:16]


def spec_files(path: Path) -> list[Path]:
    """A data file and its sidecars (`FILE.*`: the generator's answers, commit files)."""
    return [path, *sorted(p for p in path.parent.glob(path.name + ".*") if p.is_file())] if path.parent.is_dir() else [path]


def adopt(path: Path, index: dict, shared: Path, data: Path) -> bool:
    """Links `path` and its sidecars in from the shared blobs, if the index has them all."""
    group = index.get(path.relative_to(data).as_posix())
    if not group or path.exists():
        return False
    links = [(data / rel, shared / "blobs" / sha) for rel, sha in group.items()]
    if not all(blob.is_file() for _, blob in links):
        return False
    for target, blob in links:
        target.parent.mkdir(parents=True, exist_ok=True)
        if not target.exists():
            os.link(blob, target)
    return True


def share(paths: list[Path], index: dict, shared: Path, data: Path) -> None:
    """Each file of `paths` under `data` into the shared blobs (read-only), hardlinked back; the
    index learns the group under its first file. A file on another filesystem stays unshared."""
    group = {}
    for path in paths:
        if not path.is_file() or path.is_symlink() or not path.resolve().is_relative_to(data.resolve()):
            continue
        sha = file_digest(path)
        blob = shared / "blobs" / sha
        blob.parent.mkdir(parents=True, exist_ok=True)
        try:
            if not blob.exists():
                os.link(path, blob)
                os.chmod(blob, 0o444)
            elif not os.path.samefile(blob, path):
                linked = path.with_name(path.name + ".sharing")
                os.link(blob, linked)
                os.replace(linked, path)
        except OSError:
            continue
        group[path.relative_to(data).as_posix()] = sha
    if group:
        index[next(iter(group))] = group


def prepare(args) -> int:
    """`prepare`: the selected cases' data, generated once by this perf lab (NRESE_FAST_BIN:
    the harness revision's, for every side of a gate), and FAST_DATA/PREPARED.json: every file
    the cases read, generated or a dataset, with its size, modification time and SHA-256.
    `run --prepared` then generates nothing and checks the files before and after its pass."""
    if RUNTIME != "native":
        sys.exit("prepare is the native runtime's (a release gate's host)")
    cases = select(load_cases(), args)
    entries, failed = {}, {}
    index_path = SHARED / "index" / f"{generator_key()}.json" if SHARED else None
    index = json.loads(index_path.read_text(encoding="utf-8")) if index_path and index_path.is_file() else {}
    adopted = 0
    for case in cases:
        try:
            specs = [case["data"], *([case["data_alt"]] if case.get("data_alt") else []), *case.get("extra", [])]
            local = [Path(native(data_file(s))) for s in specs if s != "none" and not s.startswith("volume:")]
            if SHARED:
                adopted += sum(adopt(path, index, SHARED, FAST_DATA) for path in local)
            for spec in specs:
                ensure_data(spec)
            if case.get("reference") and not seed_from_inputs(case["reference"]):
                raise RuntimeError(f"missing input: reference taxonomy {case['reference']}")
            if SHARED:
                for path in local + ([Path(native(case["reference"]))] if case.get("reference") else []):
                    share(spec_files(path), index, SHARED, FAST_DATA)
            for path in input_files(case):
                if not path.is_file():
                    raise RuntimeError(f"missing input: {path}")
                if path.as_posix() not in entries:
                    stat = path.stat()
                    entries[path.as_posix()] = {"bytes": stat.st_size, "mtime_ns": stat.st_mtime_ns,
                                                "sha256": file_digest(path)}
            print(f"prepared {case['name']}", flush=True)
        except Exception as error:  # a case without its data is reported, the others prepared
            failed[case["name"]] = str(error)[:300]
            print(f"NOT prepared {case['name']}: {failed[case['name']]}", flush=True)
    if index_path:
        index_path.parent.mkdir(parents=True, exist_ok=True)
        index_path.with_suffix(".partial").write_text(json.dumps(index, indent=0), encoding="utf-8")
        os.replace(index_path.with_suffix(".partial"), index_path)
        print(f"shared data ({SHARED}): {adopted} data files linked in, not generated")
    manifest_path = FAST_DATA / PREPARED
    manifest_path.write_text(json.dumps({"git": manifest.git(ROOT), "binaries": binaries(),
                                         "cases": [c["name"] for c in cases if c["name"] not in failed],
                                         "failed": failed, "files": entries}, indent=1), encoding="utf-8")
    print(f"{len(cases) - len(failed)} of {len(cases)} cases prepared, {len(entries)} files: {manifest_path}")
    return 1 if failed else 0


def prepared_inputs(cases: list[dict]) -> dict:
    """PREPARED.json, its SHA-256, and what stands in the way of a pass over `cases`: a case
    not prepared, a file changed since (size or modification time)."""
    path = FAST_DATA / PREPARED
    try:
        data = path.read_bytes()
        recorded = json.loads(data)
    except (OSError, ValueError) as error:
        return {"errors": [f"no prepared data ({path}: {error}); run fast.py prepare first"], "files": {}}
    errors = [f"{c['name']} was not prepared" for c in cases if c["name"] not in recorded["cases"]]
    errors += [f"{file} changed since it was prepared" for file in changed_inputs(recorded["files"])]
    return {"errors": errors, "files": recorded["files"], "sha256": hashlib.sha256(data).hexdigest()}


def changed_inputs(files: dict) -> list[str]:
    changed = []
    for file, was in files.items():
        try:
            stat = Path(file).stat()
        except OSError:
            changed.append(file)
            continue
        if (stat.st_size, stat.st_mtime_ns) != (was["bytes"], was["mtime_ns"]):
            changed.append(file)
    return changed


def not_as_expected(records: list[dict], off_route_ok: bool = False) -> list[dict]:
    """The runs that make `run` fail: each whose status isn't its case's expected outcome,
    except, for an A/B's base (`off_route_ok`), a case off its expected route: measured
    without the operator the branch brings. Failed, wrong and missing runs always count."""
    tolerated = {"off-route"} if off_route_ok else set()
    return [r for r in records
            if r["status"] != r.get("expected_outcome", "ok") and r["status"] not in tolerated]


def run(args) -> int:
    if RUNTIME == "native":
        try:
            native_dirs()
        except RuntimeError as error:
            sys.exit(str(error))
    elif lacking := hosts.lacking(["docker"]):
        sys.exit(f"this host lacks {', '.join(lacking)} (benches/hosts.toml): NRESE_FAST_RUNTIME=native runs "
                 "the cases as host processes")
    catalog = load_cases()
    cases = select(catalog, args)
    if RUNTIME != "native":
        # Every image the cases' containers use, pinned before the first case (pin_images).
        ore = any(spec.startswith("ore:") for c in cases for spec in [c["data"], *c.get("extra", [])])
        pin_images([rust_image(), *([dl_image()] if ore else [])])
    OPTIONS.update(prepared=getattr(args, "prepared", False), routes=not getattr(args, "no_routes", False),
                   empty_queries=getattr(args, "empty_queries", False))
    # A timed gate's pass (scripts/release-gate.sh sets NRESE_TIMED_GATE) reads prepared data
    # only: it never makes data while it times (fast.py prepare does, before any pass).
    if os.environ.get("NRESE_TIMED_GATE") == "1" and not OPTIONS["prepared"]:
        sys.exit("a timed gate's pass reads prepared data only: fast.py prepare first, then run --prepared")
    MEASURE.clear()
    OPTIONS["measure"] = None
    OPTIONS["batch"] = getattr(args, "batch", False)
    OPTIONS["timing"] = getattr(args, "timing", None)
    if getattr(args, "measure", None):
        found, OPTIONS["measure"] = load_measure(Path(args.measure))
        MEASURE.update(found)
    OPTIONS["cpus"] = pinned_cpus() if RUNTIME == "native" else None
    prepared = None
    if OPTIONS["prepared"]:
        prepared = prepared_inputs(cases)
        if prepared["errors"]:
            print("\n".join(f"prepared data: {e}" for e in prepared["errors"]), file=sys.stderr)
            return 2
    label = args.label or (manifest.git(ROOT).get("commit") or "nrese")[:8]
    started = datetime.datetime.now()
    # The full report outside git; the repository keeps its compact record (README, "What is kept").
    REPORTS.mkdir(parents=True, exist_ok=True)
    BASELINES.mkdir(parents=True, exist_ok=True)
    out = (Path(args.out) if args.out else REPORTS / f"{started:%Y-%m-%d}-{label}.json").resolve()
    report = {"label": label, "started": started.isoformat(timespec="seconds"), "started_ns": time.time_ns(),
              "git": manifest.git(ROOT), "machine": manifest.machine(RUNTIME, ROOT), "runtime": RUNTIME,
              "reps": args.reps, "planned_cases": [c["name"] for c in cases],
              "all_cases": [c["name"] for c in catalog], "case_manifest": case_manifest(catalog),
              "options": dict(OPTIONS), "records": []}
    if prepared:
        report["prepared"] = {"manifest_sha256": prepared["sha256"], "files": len(prepared["files"])}
    # The host as the pass met it, and as it left it: a governor, boost or another user's work
    # that differs between passes is in the passes' own reports (release_gate.py reports it).
    report["host_state"] = host_state()
    if RUNTIME == "native":
        # What the binaries are: they come from a build host, not from this checkout.
        report["binaries"] = binaries()
    else:
        report["image"] = rust_image()
    order = list(cases)
    for rep in range(1, args.reps + 1):
        # Interleaved, each round in an order rotated by one.
        order = order[1:] + order[:1] if rep > 1 else order
        for case in order:
            t0 = time.monotonic()
            try:
                record = run_case(case, label, rep)
            except Exception as error:  # a case that breaks mustn't stop the suite
                record = {"case": case["name"], "rep": rep, "status": "failed", "notes": [str(error)[:300]],
                          "checks": [], "routes": [], "samples": {}, "metric": None,
                          "expected_outcome": case.get("outcome", "ok")}
            record["verdict"] = verdict(case, record)
            # The case's own time, without the wait for the quiet slot.
            record["case_s"] = round(time.monotonic() - t0 - record.get("slot_wait_s", 0.0), 1)
            report["records"].append(record)
            mark = "" if record["status"] == record.get("expected_outcome", "ok") else "  <<<"
            metric = record.get("metric")
            print(f"rep {rep} {case['name']:<24} {record['status']:<13} "
                  f"{(f'{metric:,.{1 if metric >= 10 else 4}f} ms' if isinstance(metric, (int, float)) else '-'):>14} "
                  f"peak {record.get('peak_mib') or '-':>6} MiB  {record['case_s']:>6.1f} s{mark}", flush=True)
            for c in record["checks"] + record["routes"]:
                if c["ok"] is False:
                    print(f"      {c['path']}: expected {c['expected']}, got {c['got']}")
            for note in record.get("notes", []):
                print(f"      {note}")
            v = record["verdict"]
            if v["verdict"] != "ok":
                print(f"      verdict: {v['verdict']} ({v['whose']})" + (f"; known: {v['issue']}" if v.get("issue") else ""))
            if record.get("frontier"):
                f = record["frontier"]
                print(f"      past its limit ({', '.join(f'{k} = {x}' for k, x in f['settings'].items())}): {f['status']}, "
                      f"metric {f['metric']}, peak {f['peak_mib']} MiB")
            out.write_text(json.dumps(report, indent=1), encoding="utf-8")
    report["git_finished"] = manifest.git(ROOT)
    report["host_state_finished"] = host_state()
    if RUNTIME == "native":
        report["binaries_finished"] = binaries()
    if prepared:
        # The prepared data must be what it was at the start of the pass, file by file.
        report["prepared"]["changed"] = changed_inputs(prepared["files"])
    if RUNTIME != "native":
        report["images"] = ns.pinned(ROOT)  # what the cases ran: the job's image IDs
    report["finished_ns"] = time.time_ns()
    report["finished"] = datetime.datetime.now().isoformat(timespec="seconds")
    report["duration_s"] = round((datetime.datetime.now() - started).total_seconds(), 1)
    out.write_text(json.dumps(report, indent=1), encoding="utf-8")
    kept = write_compact(out, BASELINES / out.name)
    bad = not_as_expected(report["records"], getattr(args, "off_route_ok", False))
    # A run of nothing measured nothing (an A/B's side whose binary refused every case).
    if not report["records"]:
        print("no case ran")
        return 1
    # Every case's verdict, the known issues included: an expected limit is still no answer.
    by_verdict: dict[str, set] = {}
    for r in report["records"]:
        v = r.get("verdict", {})
        by_verdict.setdefault(f"{v.get('verdict', '?')} ({v.get('whose', '?')})", set()).add(r["case"])
    for key in sorted(by_verdict, key=lambda k: (k.startswith("ok"), k)):
        names = sorted(by_verdict[key])
        print(f"{key}: {len(names)}" + ("" if key.startswith("ok") else ": " + ", ".join(names)))
    print(f"\n{len(report['records'])} runs of {len(cases)} cases in {report['duration_s'] / 60:.1f} min; "
          f"{len(bad)} not as expected; report: {out}; kept: {kept}")
    return 1 if bad else 0


# --- what is kept ----------------------------------------------------------------------------



def write_compact(full: Path, target: Path) -> Path:
    """The compact record of a full report, for the repository: the run's commit, machine
    and configuration; per case and repetition its status, checks, routes, counters, metric,
    peaks and time, and each series as KEPT_POINTS order statistics (for compare's bootstrap);
    and where the full report is, with its hash."""
    data = full.read_bytes()
    report = json.loads(data)
    home = Path.home()
    where = f"~/{full.relative_to(home).as_posix()}" if full.is_relative_to(home) else full.as_posix()
    keep = {k: v for k, v in report.items() if k != "records"}
    keep["full_report"] = {"path": where, "host": socket.gethostname(), "bytes": len(data),
                           "sha256": hashlib.sha256(data).hexdigest()}
    keep["records"] = [compact_record(r) for r in report["records"]]
    target.write_text(json.dumps(keep, indent=1), encoding="utf-8")
    return target


def compact_record(r: dict) -> dict:
    """A record without its raw result, long check values shortened, its series thinned."""
    def short(c: dict) -> dict:
        got = c.get("got")
        if len(json.dumps(got)) > 160:
            got = f"<{type(got).__name__} of {len(got) if hasattr(got, '__len__') else '?'}>"
        return {**c, "got": got}
    out = {k: v for k, v in r.items() if k not in ("result", "samples", "checks", "routes")}
    out["checks"] = [short(c) for c in r.get("checks", [])]
    out["routes"] = [short(c) for c in r.get("routes", [])]
    out["samples"] = {k: thin(sorted(v for v in vs if v is not None), KEPT_POINTS)
                      for k, vs in (r.get("samples") or {}).items()}
    return out


def compare(args) -> int:
    """`compare BASE NEW`: the one compare (benches/suite/suitekit/compare.py) on two reports."""
    min_ms = getattr(args, "min_ms", 2.0)
    if not math.isfinite(min_ms) or min_ms < 0:
        raise ValueError("--min-ms must be finite and nonnegative")
    return compare_reports(load_report(Path(args.base)), load_report(Path(args.new)), min_ms,
                           getattr(args, "allow_host_change", False))


# --- list, clean -----------------------------------------------------------------------------

def list_cases(args) -> int:
    for c in select(load_cases(), args):
        systems = ", ".join(c.get("systems", [])) or "NRESE only"
        turf = f"  [home turf: {c['home_turf']}]" if c.get("home_turf") else ""
        print(f"{c['name']:<24} {c['area']:<12} {c['semantics']:<10} {systems}{turf}")
    return 0


TURF = {"qlever": "QLever", "oxigraph": "Oxigraph", "virtuoso": "Virtuoso", "jena": "Jena", "rdf4j": "RDF4J",
        "nemo": "Nemo", "glog-vlog": "GLog/VLog (no runner: Nemo stands in)", "elk": "ELK", "konclude": "Konclude",
        "hermit": "HermiT", "openllet": "Openllet"}
MARKERS = ("<!-- fast-cases:start (fast.py table --write) -->", "<!-- fast-cases:end -->")


def table(args) -> int:
    """The cases as CATALOG.md's table: semantics, comparators and home turf per case, and
    each case's time and peak in a baseline (`--baseline`)."""
    cases = load_cases()
    times: dict[str, tuple] = {}
    if args.baseline:
        report = json.loads(Path(args.baseline).read_text(encoding="utf-8"))
        for r in report["records"]:
            times.setdefault(r["case"], (r.get("case_s"), r.get("peak_mib"), r.get("status")))
    lines = ["| Case | Area | NRESE runs | Comparators (system: semantics) | Home turf of | Measures |"
             + (" Case s, peak MiB |" if times else ""),
             "|---|---|---|---|---|---|" + ("---|" if times else "")]
    for c in cases:
        hangs = c.get("hangs") or {}
        systems = ", ".join(f"{s.split(':')[0]}: {s.split(':')[1]}" + (" (hangs)" if s in hangs else "")
                            for s in c.get("systems", [])) or "none: NRESE's own column"
        turf = TURF.get(c.get("home_turf", ""), c.get("home_turf", ""))
        row = f"| `{c['name']}` | {c['area']} | {c['semantics']} | {systems} | {turf} | {c['what']} |"
        if times:
            s_, peak, status = times.get(c["name"], (None, None, None))
            note = "" if status in (None, "ok", c.get("outcome")) else f" ({status})"
            row += f" {s_ if s_ is not None else '-'}, {peak or '-'}{note} |"
        lines.append(row)
    text = "\n".join(lines)
    if not args.write:
        print(text)
        return 0
    path = ROOT / "benches" / "CATALOG.md"
    catalog = path.read_text(encoding="utf-8")
    start, end = catalog.index(MARKERS[0]) + len(MARKERS[0]), catalog.index(MARKERS[1])
    path.write_text(catalog[:start] + "\n" + text + "\n" + catalog[end:], encoding="utf-8")
    print(f"wrote {len(cases)} cases into {path.relative_to(ROOT)}")
    return 0


def clean(args) -> int:
    """This checkout's scratch and its containers; with `--build` its build volume too (a job's
    cleanup: what carries its namespace and checkout, nothing else, ns.clean); the fast data
    volume only with `--data`, by hand, and never while a container uses it: it is shared by
    every job on the host and holds copies of the verified inputs besides the generated data."""
    import shutil
    if RUNTIME == "native":
        shutil.rmtree(SCRATCH, ignore_errors=True)
        files(f"rm -rf {VOLUME_SCRATCH}")
        print(f"removed {SCRATCH} and {VOLUME_SCRATCH}")
        if args.data:
            shutil.rmtree(FAST_DATA, ignore_errors=True)
            print(f"removed the generated data {FAST_DATA}")
        return 0
    files(f"rm -rf {VOLUME_SCRATCH}")
    removed = [] if not args.build else ns.clean(ROOT, OWNER, run=docker)
    shutil.rmtree(SCRATCH, ignore_errors=True)
    print(f"removed {SCRATCH} and {VOLUME_SCRATCH}" + (f"; this checkout's {', '.join(removed)}" if removed else ""))
    if not args.data:
        print(f"kept the volume {DATA_VOLUME} (shared; `clean --data` removes it)")
        return 0
    users = docker(["ps", "-a", "--filter", f"volume={DATA_VOLUME}", "--format", "{{.Names}}"]).stdout.split()
    if users:
        print(f"kept the volume {DATA_VOLUME}: in use by {', '.join(users)}")
        return 1
    done = docker(ns.volume("rm", DATA_VOLUME))
    print(f"removed the volume {DATA_VOLUME}" if done.returncode == 0 else f"volume {DATA_VOLUME}: {done.stderr.strip()}")
    return done.returncode


def main() -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    for name in ("list", "run", "compete", "prepare"):
        s = sub.add_parser(name)
        s.add_argument("--cases")
        s.add_argument("--areas")
        if name == "run":
            s.add_argument("--reps", type=int, default=2)
            s.add_argument("--label")
            s.add_argument("--out")
            s.add_argument("--off-route-ok", action="store_true",
                           help="an A/B's base: a case off its expected route (an operator the base "
                                "hasn't) is a measured run, not a failure; failed, wrong or missing "
                                "runs still are")
            s.add_argument("--prepared", action="store_true",
                           help="a gate's pass: the data `prepare` made, nothing generated, checked before and after")
            s.add_argument("--no-routes", action="store_true",
                           help="a perf lab without --routes (main's): route checks and the values only it "
                                "reports are unchecked (ok null), never failed")
            s.add_argument("--empty-queries", action="store_true",
                           help="a perf lab that requires --queries (main's): an empty directory where a case has none")
            s.add_argument("--measure", metavar="TOML",
                           help="the release gate's measurement per case (benches/fast/measure.toml: runs, "
                                "warm-ups, repetitions), for every side alike")
            s.add_argument("--timing", choices=("watchdog", "spawn"),
                           help="how this perf lab's timed window is kept (its build's CAPABILITIES): "
                                "the gate refuses sides timed differently")
            s.add_argument("--batch", action="store_true",
                           help="with --measure: its batched timing (perf_lab --batch), when every side's "
                                "perf lab has it")
        if name == "compete":
            s.add_argument("--systems")
            s.add_argument("--runs", type=int, default=1)
            s.add_argument("--licensed", action="store_true",
                           help="also the licensed systems (results stay in benches/fast/results)")
            s.add_argument("--merge", action="store_true",
                           help="cases on the same data in one suite run per system (fewer loads, one long "
                                "quiet slot: for a night batch); default one run and slot per case")
            s.add_argument("--counts-only", action="store_true",
                           help="the printer's verdicts for the rewritten comparators only: rows against "
                                "NRESE's, no timings, no slot")
    t = sub.add_parser("table")
    t.add_argument("--baseline", help="a run's report, for each case's time and peak")
    t.add_argument("--write", action="store_true", help="into benches/CATALOG.md between its markers")
    sub.add_parser("build")
    k = sub.add_parser("clean")
    k.add_argument("--data", action="store_true",
                   help="also the shared fast data volume, by hand (refused while a container uses it)")
    k.add_argument("--build", action="store_true",
                   help="also this checkout's containers and build volume (a job's cleanup: never another's)")
    c = sub.add_parser("compare")
    c.add_argument("base")
    c.add_argument("new")
    c.add_argument("--min-ms", type=float, default=2.0,
                   help="absolute latency floor in ms (default 2); explicitly lower for microcase comparisons")
    c.add_argument("--allow-host-change", action="store_true",
                   help="compare runs of different hosts or host configurations anyway, flagged")
    args = p.parse_args()
    if args.command == "compete":
        from compete import compete
        return compete(args)
    return {"list": list_cases, "build": build, "run": run, "compare": compare, "clean": clean,
            "table": table, "prepare": prepare}[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
