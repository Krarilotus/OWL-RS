#!/usr/bin/env python3
"""The fast suite: many small performance cases, each 10-300 s, all of them in under an
hour, run like unit tests for performance (README.md here; cases.toml).

    python benches/fast/fast.py list                     the cases, their semantics and comparators
    python benches/fast/fast.py build                    perf lab and DL tools, Linux, in a capped container
    python benches/fast/fast.py run [--cases A,B] [--areas X,Y] [--reps 2] [--label L]
    python benches/fast/fast.py compare BASE.json NEW.json
    python benches/fast/fast.py compete [--cases A,B] [--systems S,T]   NRESE beside its comparators
    python benches/fast/fast.py table [--baseline R] [--write]   the cases as CATALOG.md's table
    python benches/fast/fast.py clean [--data]           this worktree's scratch; --data the shared data volume too

`run` generates each case's data once (deterministic; kept in the fast volume, hosts.toml),
runs each case in its own container under the case's memory cap, checks the answers
before a time counts, asserts the plan or engine path the case must take, and writes
benches/baselines/fast/<date>-<label>.json. Repetitions are interleaved: every case once,
then every case again. `compare` reports regressions and wins beyond a bootstrap 95 %
confidence interval of the ratio of medians.

The build runs scripts/cargo-guarded.sh inside the pinned Rust image, into this
worktree's own target volume (nrese-target-<worktree>), with CARGO_BUILD_JOBS and
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
from suitekit import hosts, manifest  # noqa: E402
from suitekit.compare import (KEPT_POINTS, bootstrap_ratio, client_throughput, compare_reports,  # noqa: E402,F401
                              counter_changes, load_report, pooled, series_ratios, spread, statistic, thin)
from suitekit.workloads import fast_data_name  # noqa: E402

# The volumes (benches/hosts.toml [volumes]; NRESE_FAST_VOLUME and NRESE_BENCH_VOLUME name others).
DATA_VOLUME = hosts.volume("fast")
BENCH_VOLUME = hosts.volume("data")
TARGET_VOLUME = os.environ.get("NRESE_TARGET_VOLUME", f"nrese-target-{ROOT.name}")
CARGO_VOLUME = "nrese-cargo"
SCRATCH = ROOT / "tmp" / "fast"
BASELINES = ROOT / "benches" / "baselines" / "fast"  # the compact records (committed)
# The full reports, outside git (the same path on the main and the office PC).
REPORTS = Path(os.environ.get("NRESE_BENCH_REPORTS") or Path.home() / "nrese-bench" / "reports") / "fast"
CASE_TIMEOUT_S = 900
ENV = {**os.environ, "MSYS_NO_PATHCONV": "1"}


def rust_image() -> str:
    channel = re.search(r'channel = "(.*)"', (ROOT / "rust-toolchain.toml").read_text()).group(1)
    return os.environ.get("RUST_IMAGE", f"rust:{channel}-bookworm")


def host_path(path: Path) -> str:
    return path.as_posix()


def load_cases() -> list[dict]:
    cases = tomllib.loads((HERE / "cases.toml").read_text(encoding="utf-8"))["case"]
    names = [c["name"] for c in cases]
    assert len(names) == len(set(names)), "case names must be unique"
    return cases


def select(cases: list[dict], args) -> list[dict]:
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
    return subprocess.run(["docker", *args], capture_output=capture, text=True, timeout=timeout, env=ENV,
                          encoding="utf-8", errors="replace")


def capped(gb: float) -> float:
    """A container's memory within a machine's ceiling for the whole run (FAST_MAX_GB, as on
    a shared PC), whatever a case asks for."""
    limit = os.environ.get("FAST_MAX_GB")
    return min(gb, float(limit)) if limit else gb


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
    holder = subprocess.Popen([bash, str(ROOT / "scripts" / "quiet-slot.sh"), "bash", "-c", "echo held; read _", what],
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, env=ENV)
    if holder.stdout.readline().strip() != "held":
        holder.kill()
        raise RuntimeError(f"quiet slot not taken for {what}")
    try:
        yield
    finally:
        holder.stdin.close()
        holder.wait(timeout=60)


def container(name: str, cap_gb: float, command: list[str], env: dict | None = None,
              timeout: float = CASE_TIMEOUT_S) -> tuple[int, str, str, float]:
    """Runs `command` under a memory cap without swap, in the Rust image or natively; (exit
    code, stdout, stderr, wall seconds). Exit 137: killed at the cap."""
    cap_gb = capped(cap_gb)
    SCRATCH.mkdir(parents=True, exist_ok=True)
    if RUNTIME == "native":
        return process(name, cap_gb, command, env, timeout)
    argv =["run", "--rm", "--name", name, f"--memory={cap_gb}g", f"--memory-swap={cap_gb}g", *cpu_args(),
            "-v", f"{TARGET_VOLUME}:/target:ro", "-v", f"{DATA_VOLUME}:/fast",
            "-v", f"{BENCH_VOLUME}:/data:ro", "-v", f"{host_path(ROOT)}:/src:ro",
            "-v", f"{host_path(SCRATCH)}:/out"]
    for key, value in (env or {}).items():
        argv += ["-e", f"{key}={value}"]
    argv += [rust_image(), *command]
    started = time.monotonic()
    try:
        done = docker(argv, timeout=timeout)
    except subprocess.TimeoutExpired:
        docker(["rm", "-f", name])
        return 124, "", f"timed out after {timeout} s", time.monotonic() - started
    return done.returncode, done.stdout, done.stderr, time.monotonic() - started


def process(name: str, cap_gb: float, command: list[str], env: dict | None = None,
            timeout: float = CASE_TIMEOUT_S) -> tuple[int, str, str, float]:
    """`container` natively: the command, its container paths mapped (native_dirs), in a user
    scope capped at `cap_gb` without swap (as Docker's --memory and --memory-swap). A wrapper
    shell in the scope prints the scope's memory.peak after the command (`fast-scope-peak`
    on stderr); a kill at the cap may take the wrapper too, and then there is no peak."""
    dirs = native_dirs()
    FAST_DATA.mkdir(parents=True, exist_ok=True)
    unit = f"{name}-{os.getpid()}-{int(time.monotonic() * 1000)}"
    wrapper = ('g=/sys/fs/cgroup$(cut -d: -f3 /proc/self/cgroup); "$@"; rc=$?; '
               'echo "fast-scope-peak $(cat "$g/memory.peak" 2>/dev/null)" >&2; exit $rc')
    argv = ["systemd-run", "--user", "--scope", "-q", f"--unit={unit}", "-p", f"MemoryMax={int(cap_gb * 1024)}M",
            "-p", "MemorySwapMax=0", "--", "sh", "-c", wrapper, "sh", *(native(c, dirs) for c in command)]
    started = time.monotonic()
    try:
        done = subprocess.run(argv, capture_output=True, text=True, timeout=timeout, encoding="utf-8",
                              errors="replace", env={**ENV, **{k: str(v) for k, v in (env or {}).items()}})
    except subprocess.TimeoutExpired:
        subprocess.run(["systemctl", "--user", "kill", "--signal=KILL", f"{unit}.scope"], capture_output=True)
        return 124, "", f"timed out after {timeout} s", time.monotonic() - started
    return done.returncode, done.stdout, done.stderr, time.monotonic() - started


def scope_peak_mib(stderr: str) -> int | None:
    """The native runtime's scope peak, in MiB, if the wrapper printed it."""
    found = SCOPE_PEAK.findall(stderr or "")
    return int(found[-1]) // 1048576 if found else None


def binaries() -> dict:
    """The native runtime's binaries: their directory, the source revision a build host
    states (NRESE_FAST_REVISION), and each one's SHA-256."""
    target = native_dirs()["/target"]
    out = {"dir": target.as_posix(), "revision": os.environ.get("NRESE_FAST_REVISION", ""), "sha256": {}}
    for relative in ("release/examples/perf_lab", "release/examples/tableau_consistency",
                     "release/examples/context_classify", "release/examples/classify", "release/nrese-server"):
        path = target / relative
        if path.is_file():
            out["sha256"][relative] = hashlib.sha256(path.read_bytes()).hexdigest()
    return out


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
    jobs =os.environ.get("CARGO_BUILD_JOBS", "4")
    cap = os.environ.get("NRESE_MEMORY_CAP_GB", "8")
    for volume in (TARGET_VOLUME, CARGO_VOLUME):
        docker(["volume", "create", volume])
    for packages in BUILD:
        command = ["run", "--rm", "--name", f"fast-build-{ROOT.name}", f"--memory={cap}g", f"--memory-swap={cap}g",
                   f"--cpus={jobs}", "-v", f"{host_path(ROOT)}:/src:ro", "-v", f"{TARGET_VOLUME}:/target",
                   "-v", f"{CARGO_VOLUME}:/usr/local/cargo/registry", "-e", "CARGO_TARGET_DIR=/target",
                   "-e", f"CARGO_BUILD_JOBS={jobs}", "-e", f"NRESE_MEMORY_CAP_GB={cap}", "-w", "/src", rust_image(),
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
        files(f"test -s {target} || cat {parts} > {target}")
        return {}
    path = data_file(spec)
    seed_from_inputs(path)
    found = files(f"cat {path}.expect.json")
    if found.returncode == 0 and found.stdout.strip():
        return json.loads(found.stdout)["expect"]
    kind, *params = spec.split()
    print(f"  generating {path}", flush=True)
    code, out, err, _ = container(f"fast-gen-{ROOT.name}", 8, [
        "bash", "-c", f"/target/release/examples/perf_lab generate {kind} {path} {' '.join(params)} > {path}.expect.json"
                      f" && cat {path}.expect.json"])
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
    docker(["run", "--rm", "--memory=10g", "-v", f"{DATA_VOLUME}:/fast", dl_image(), "batch",
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
    prefix = "PREFIX e: <http://example.org/fast/>\n"
    queries = {
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
    for name, text in queries.items():
        (directory / f"{name}.rq").write_text(prefix + text + "\n", encoding="utf-8")
    return directory


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
        # Another kit's query set: benches/competitors/queries/NAME.
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
        record = measure(case, label, rep, expect, alt_expect, data)
        if rep == 1 and case.get("frontier") and record["status"] in LIMITS:
            record["frontier"] = frontier(case, label, rep, expect, alt_expect, data)
    record["slot_wait_s"] = round(waited, 1)
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
    name = f"fast-{case['name']}-{ROOT.name}"
    tool = case.get("tool", "perf_lab")
    out_json = f"/out/{case['name']}.json"
    record = {"case": case["name"], "rep": rep, "status": "ok", "notes": []}
    scratch = f"/fast/scratch/{case['name']}"
    repeat = int(case.get("repeat", 1))
    result: dict = {}
    if tool == "perf_lab":
        base = ["/target/release/examples/perf_lab", "--label", label, "--routes", "--json", out_json]
        loads = [] if "setup" in case or case["data"] == "none" else \
            [x for f in [case["data"], *case.get("extra", [])] for x in ("--load", data_file(f))]
        if case.get("tool_loads") is False:
            loads = []
        # The canonicalisation case reads its files itself.
        if any(a == "--canonicalize" for a in case.get("args", [])):
            loads = []
        queries = queries_path(case, expect)
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
        runs = [(entry, i) for entry in sweep for i in range(repeat)]
        for index, (entry, _) in enumerate(runs):
            # The setup (a store on disk) runs once; each repeat is a fresh process opening it.
            shell = (script if index == 0 else "") + " ".join(f"'{c}'" for c in [*command, *entry])
            code, _, err, w = container(name, case["cap_gb"], ["bash", "-c", shell], case.get("env"))
            wall += w
            path = SCRATCH / f"{case['name']}.json"
            if not path.exists():
                break
            result = json.loads(path.read_text(encoding="utf-8"))
            path.unlink()
            result["_sweep"] = " ".join(entry)
            samples_runs.append(result)
            if code != 0:
                break
        if "setup" in case:
            container(name, 1, ["rm", "-rf", scratch])
        if samples_runs:
            result = samples_runs[-1]
            if len(samples_runs) > 1:
                result["_repeats"] = samples_runs
        record["wall_s"] = round(wall, 2)
        if code != 0:
            record["status"] = classify_failure(code, err, result)
            record["notes"].append(err.strip().splitlines()[-1][:300] if err.strip() else f"exit {code}")
    elif tool == "tableau":
        files = [data] * repeat + ([data_file(case["data_alt"])] * repeat if case.get("data_alt") else [])
        timeout = str(case.get("timeout_s", 300))
        command = ["/target/release/examples/tableau_consistency", "--timeout", timeout, *files]
        code, out, err, wall = container(name, case["cap_gb"], command)
        rows = parse_tableau(out)
        main = [r for r in rows if r["file"] == data]
        alt = [r for r in rows if r["file"] != data]
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
        code, out, err, wall = container(name, case["cap_gb"], command)
        result = profile_line(err, out)
        lines = out.split()
        if code == 0 and len(lines) >= 2:
            result["hash"], result["subsumptions"] = lines[-2], int(lines[-1])
        # The context core's profile has `total`; the EL classifier's, its phases.
        phases = ("read", "normalise", "prepare", "saturate", "assemble", "write")
        total = result.get("total")
        if total is None and all(isinstance(result.get(k), float) for k in phases):
            total = sum(result[k] for k in phases)
        result["whole_ms"] = total
        result["samples_ms"] = [total] if total is not None else []
        result["phase_samples"] = {k: [v] for k, v in result.items()
                                   if k in ("read", "owl", "normalise", "compile", "prepare", "saturate", "assemble", "write")
                                   and isinstance(v, float)}
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
    # Natively, the case's scope is its container: its peak is the cgroup's.
    if not result.get("cgroup_peak_mib") and (peak := scope_peak_mib(err)):
        result["cgroup_peak_mib"] = peak
    record["metric"] = metric_value(case, result)
    record["samples"] = samples(case, result)
    record["counters"] = counters(case, result)
    if levels := client_throughput(result):
        record["throughput"] = levels
    record["phase_peaks"] = {k: v.get("peak_mib") for k, v in (result.get("phases") or {}).items()}
    record["peak_mib"] = result.get("cgroup_peak_mib") or result.get("peak_mib")
    record["memory_kind"] = ("cgroup" if result.get("cgroup_peak_mib")
                             else "lifetime" if result.get("peak_mib") else "unknown")
    if case.get("queries", "").startswith("@vectors") and record["status"] == "ok":
        result["recall"] = vector_recall(case, expect)
    record["checks"] = [check(path, want, result, expect, alt_expect) for path, want in case.get("check", {}).items()]
    record["routes"] = [route(path, want, result) for path, want in case.get("route", {}).items()]
    wanted = case.get("outcome", "ok")
    if record["status"] == "ok":
        if any(not c["ok"] for c in record["checks"]):
            record["status"] = "wrong"
        elif any(not r["ok"] for r in record["routes"]):
            record["status"] = "off-route"
    record["expected_outcome"] = wanted
    record["verdict"] = verdict(case, record)
    record["result"] = slim(result)
    return record


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


def check(path: str, want, result: dict, expect: dict, alt_expect: dict) -> dict:
    expected = resolve(want, alt_expect if path.startswith("alt.") else expect)
    got = lookup(path, result)
    if isinstance(want, str) and (want[:2] in (">=", "<=") or want.startswith(("has ", "lacks "))):
        ok = compare_op(got, want)
    else:
        ok = got is not None and expected is not None and same(got, expected)
    return {"path": path, "expected": want if isinstance(want, str) and (want[:2] in (">=", "<=") or want.startswith(("has ", "lacks "))) else expected,
            "got": got, "ok": ok}


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
    return {"path": path, "expected": want, "got": got, "ok": compare_op(got, want)}


def cold_ms(result: dict) -> float | None:
    """Opening the store and the first execution of each query, in ms."""
    if "open_s" not in result:
        return None
    firsts = [q.get("first_ms", q.get("p50_ms", 0.0)) for q in result.get("queries", []) if "error" not in q]
    return result["open_s"] * 1000.0 + sum(firsts)


def metric_value(case: dict, result: dict):
    if case["metric"] == "cold_ms":
        values = [cold_ms(r) for r in result.get("_repeats", [result])]
        values = [v for v in values if v is not None]
        return statistics.median(values) if values else None
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
        return {"_": [v for v in (cold_ms(r) for r in result.get("_repeats", [result])) if v is not None]}
    if metric == "sum_p50_ms":
        return {q["name"]: q["samples_ms"] for q in result.get("queries", []) if "samples_ms" in q}
    section = metric.split(".")[0]
    if isinstance(result.get(section), dict) and "samples_ms" in result[section]:
        return {"_": result[section]["samples_ms"]}
    if "samples_ms" in result:
        return {"_": [s for s in result["samples_ms"] if s is not None]}
    if "_repeats" in result:
        values = [lookup(metric, r) for r in result["_repeats"]]
        scale = 1000.0 if metric in ("load_s", "open_s") else 1.0
        return {"_": [v * scale for v in values if v is not None]}
    value = metric_value(case, result)
    return {"_": [value]} if value is not None else {}


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

def run(args) -> int:
    if RUNTIME == "native":
        try:
            native_dirs()
        except RuntimeError as error:
            sys.exit(str(error))
    elif lacking := hosts.lacking(["docker"]):
        sys.exit(f"this host lacks {', '.join(lacking)} (benches/hosts.toml): NRESE_FAST_RUNTIME=native runs "
                 "the cases as host processes")
    cases = select(load_cases(), args)
    label = args.label or (manifest.git(ROOT).get("commit") or "nrese")[:8]
    started = datetime.datetime.now()
    # The full report outside git; the repository keeps its compact record (README, "What is kept").
    REPORTS.mkdir(parents=True, exist_ok=True)
    BASELINES.mkdir(parents=True, exist_ok=True)
    out = (Path(args.out) if args.out else REPORTS / f"{started:%Y-%m-%d}-{label}.json").resolve()
    report = {"label": label, "started": started.isoformat(timespec="seconds"),
              "git": manifest.git(ROOT), "machine": manifest.machine(RUNTIME), "runtime": RUNTIME,
              "reps": args.reps, "records": []}
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
                if not c["ok"]:
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
    report["finished"] = datetime.datetime.now().isoformat(timespec="seconds")
    report["duration_s"] = round((datetime.datetime.now() - started).total_seconds(), 1)
    out.write_text(json.dumps(report, indent=1), encoding="utf-8")
    kept = write_compact(out, BASELINES / out.name)
    bad = [r for r in report["records"] if r["status"] != r.get("expected_outcome", "ok")]
    # Every case's verdict, the known issues included: an expected limit is still no answer.
    by_verdict: dict[str, set] = {}
    for r in report["records"]:
        v = r.get("verdict", {})
        by_verdict.setdefault(f"{v.get('verdict', '?')} ({v.get('whose', '?')})", set()).add(r["case"])
    for key in sorted(by_verdict, key=lambda k: (k.startswith("ok"), k)):
        names = sorted(by_verdict[key])
        print(f"{key}: {len(names)}" + ("" if key.startswith("ok") else ": " + ", ".join(names)))
    print(f"\n{len(report['records'])} runs of {len(cases)} cases in {report['duration_s'] / 60:.1f} min; "
          f"{len(bad)} not as expected; report: {out}; kept: {kept.relative_to(ROOT)}")
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
    return compare_reports(load_report(Path(args.base)), load_report(Path(args.new)), min_ms)


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
    """This worktree's scratch and its stopped containers; the fast data volume only with
    `--data`, and never while a container uses it: the volume is shared by every worktree and
    campaign on the host, and holds copies of the verified inputs besides the generated data."""
    import shutil
    if RUNTIME == "native":
        shutil.rmtree(SCRATCH, ignore_errors=True)
        print(f"removed {SCRATCH}")
        if args.data:
            shutil.rmtree(FAST_DATA, ignore_errors=True)
            print(f"removed the generated data {FAST_DATA}")
        return 0
    stale = docker(["ps", "-a", "--filter", "status=exited", "--filter", f"name=-{ROOT.name}$",
                    "--format", "{{.Names}}"]).stdout.split()
    for name in stale:
        docker(["rm", name])
    shutil.rmtree(SCRATCH, ignore_errors=True)
    print(f"removed {SCRATCH}" + (f" and {len(stale)} stopped containers of this worktree" if stale else ""))
    if not args.data:
        print(f"kept the volume {DATA_VOLUME} (shared; `clean --data` removes it)")
        return 0
    users = docker(["ps", "-a", "--filter", f"volume={DATA_VOLUME}", "--format", "{{.Names}}"]).stdout.split()
    if users:
        print(f"kept the volume {DATA_VOLUME}: in use by {', '.join(users)}")
        return 1
    done = docker(["volume", "rm", DATA_VOLUME])
    print(f"removed the volume {DATA_VOLUME}" if done.returncode == 0 else f"volume {DATA_VOLUME}: {done.stderr.strip()}")
    return done.returncode


def main() -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    for name in ("list", "run", "compete"):
        s = sub.add_parser(name)
        s.add_argument("--cases")
        s.add_argument("--areas")
        if name == "run":
            s.add_argument("--reps", type=int, default=2)
            s.add_argument("--label")
            s.add_argument("--out")
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
                   help="also the shared fast data volume (refused while a container uses it)")
    c = sub.add_parser("compare")
    c.add_argument("base")
    c.add_argument("new")
    c.add_argument("--min-ms", type=float, default=2.0,
                   help="absolute latency floor in ms (default 2); explicitly lower for microcase comparisons")
    args = p.parse_args()
    if args.command == "compete":
        from compete import compete
        return compete(args)
    return {"list": list_cases, "build": build, "run": run, "compare": compare, "clean": clean,
            "table": table}[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
