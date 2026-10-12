#!/usr/bin/env python3
"""Datalog programs on the rule engines' home turf: the same program and facts on NRESE
(Notation3 user rules), Soufflé, Nemo, VLog and GLog, checked before timed.

    datalog.py list                                     the cases, and which engine can express each
    datalog.py prepare [--cases tc-G10K,...]            unpack the pinned downloads; N-Triples for NRESE
    datalog.py translate CASE DIR                       the program in every engine's language, to read
    datalog.py run --cases ... --systems ... --threads 4 --runs 3 --results DIR [--qualify]

Settings (environment): DATALOG_DOWNLOADS (the pinned zips), DATALOG_DATA (unpacked data,
written by `prepare`), NRESE_BIN (the server binary; it runs in the image NRESE_IMAGE,
default ubuntu:24.04, which must match the binary's C library), DATALOG_SCRATCH, DOCKER_MEMORY
(every container's cap, default 28g; a step
starts only when that much memory is available and the host's owners don't need it:
../hostguard.py).

A run measures, per case, repetition and system, from fresh scratch (the repetitions
interleaved across the systems, each round in an order rotated by one):
- **load**: the wall time from the input files to the complete materialisation, as each
  engine is used: NRESE `nrese-server load` (parse, bulk load into its on-disk store,
  materialise), Soufflé the compiled program (its compilation is timed apart and named in the
  note), Nemo and VLog/GLog their command line; peak memory from `docker stats` (sampled);
- **reason**: the reasoning time the engine itself reports, and the number of derived facts;
- with `--qualify` (once, before timing): one `query` row per derived relation,
  `count:REL`, its number of facts (NRESE: a SPARQL count over the closure; Soufflé:
  `.printsize`; Nemo and VLog: the exported relation), checked against the published count.

Rows follow the suite's schema (benches/suite/suitekit/schema.py), workload
the case's `workload` and `tier` in cases.toml (default `datalog-graphs` and the case's name:
FlowLog's programs; RDFox's are tiers of `claros` and `lubm`), regime `-`; `suite.py report` and `compare` read them.
A derived count that differs from the expected one is `wrong`, never a time.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import sys
import time
import tomllib
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))
sys.path.insert(0, str(ROOT / "benches/suite"))

import hostguard  # noqa: E402
import program as P  # noqa: E402
from suitekit.manifest import Manifest  # noqa: E402
from suitekit.runtime import DockerRuntime, Mount, Spec  # noqa: E402
from suitekit.schema import Result, Writer  # noqa: E402

SYSTEMS = ("nrese", "souffle", "nemo", "vlog", "glog", "rdfox")
IMAGES = {"souffle": "nrese-bench/souffle:2.5", "nemo": "nrese-bench/nemo",
          "vlog": "nrese-bench/vlog:1.3.7", "glog": "nrese-bench/glog:69d4842",
          "rdfox": "oxfordsemantic/rdfox:7.6b"}
# Systems whose results may be published only with the vendor's consent (systems.toml).
RESTRICTED = {"rdfox"}
ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")


def setting(key: str, default: str = "") -> str:
    return os.environ.get(key) or default


def cases() -> dict[str, dict]:
    registry = tomllib.loads((HERE / "cases.toml").read_text(encoding="utf-8"))
    return {c["name"]: c | {"source": registry["source"][c["source"]] | {"key": c["source"]}}
            for c in registry["case"]}


def workload(case: dict) -> str:
    """The workload of workloads.toml a case is a tier of (default datalog-graphs)."""
    return case.get("workload", "datalog-graphs")


def tier(case: dict) -> str:
    """The case's tier in its workload (default its name)."""
    return case.get("tier", case["name"])


def downloads() -> Path:
    return Path(setting("DATALOG_DOWNLOADS", str(Path.home() / "nrese-reasoning-suite/downloads")))


def program_path(case: dict) -> Path:
    """The program file: in the repository (FlowLog's, Apache-2.0), or fetched by its pinned
    hash (GLog's package states no licence, so its files aren't committed)."""
    if "program_url" in case:
        return downloads() / "programs" / case["program"]
    return HERE / case["program"]


def load_program(case: dict) -> P.Program:
    text = program_path(case).read_text(encoding="utf-8")
    return P.parse_triples(text) if case.get("format") == "triples" else P.parse(text)


def publish(system: str) -> str:
    return "permission" if system in RESTRICTED else "free"


def rdfox_licence() -> str:
    """RDFOX_LICENSE, else RDFox.lic in the licences directory; "" if neither."""
    if setting("RDFOX_LICENSE"):
        return setting("RDFOX_LICENSE")
    path = Path(setting("NRESE_LICENSES", str(Path.home() / "nrese-bench" / "licenses"))) / "RDFox.lic"
    return str(path) if path.is_file() else ""


def expressible(case: dict, system: str) -> str | None:
    """None, or why the system can't run the case's program as written."""
    program = load_program(case)
    try:
        if system == "nrese":
            P.to_n3(program)
        elif system == "rdfox":
            P.to_rdfox(program)
        elif system in ("vlog", "glog"):
            P.to_vlog(program, "/data")
    except P.NotExpressible as e:
        return str(e)
    return None


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def data_dir(case: dict) -> Path:
    return Path(setting("DATALOG_DATA", str(Path.home() / "nrese-reasoning-suite/data/datalog"))) / case["data"]


# --- prepare ------------------------------------------------------------------------------------

def fetch(url: str, path: Path, digest: str):
    """A download, kept and checked against its pinned SHA-256."""
    if not path.exists():
        print(f"fetching {url}")
        path.parent.mkdir(parents=True, exist_ok=True)
        partial = path.with_name(path.name + ".partial")
        urllib.request.urlretrieve(url, partial)
        partial.rename(path)
    got = sha256(path)
    if got != digest:
        raise SystemExit(f"{path}: SHA-256 {got}, the registry pins {digest}")


def docker_as_user(image: str, args: list[str], mounts: dict[str, str], entrypoint: str | None = None):
    """A preparation step in a container, as the invoking user, working in the first mount
    (UBA writes its log into its working directory)."""
    import subprocess
    command = ["docker", "run", "--rm", "-u", f"{os.getuid()}:{os.getgid()}", "-e", "HOME=/tmp",
               "-w", next(iter(mounts.values()))]
    for source, target in mounts.items():
        command += ["-v", f"{source}:{target}"]
    if entrypoint:
        command += ["--entrypoint", entrypoint]
    subprocess.run(command + [image, *args], check=True)


def prepare(case: dict):
    if "program_url" in case:
        fetch(case["program_url"], program_path(case), case["program_sha256"])
    target = data_dir(case)
    kind = case["source"]["key"]
    if kind == "flowlog":
        archive = downloads() / "flowlog" / f"{case['data']}.zip"
        fetch(case["source"]["url"].format(name=case["data"]), archive, case["sha256"])
        if not target.exists():
            print(f"unpacking {archive.name}")
            staging = target.with_name(target.name + ".partial")
            shutil.rmtree(staging, ignore_errors=True)
            with zipfile.ZipFile(archive) as z:
                z.extractall(staging)
            inner = staging / case["data"]
            (inner if inner.is_dir() else staging).rename(target)
            shutil.rmtree(staging, ignore_errors=True)
    elif kind == "oxford":
        prepare_oxford(case, target)
    elif kind == "lubm":
        prepare_lubm(case, target)
    else:
        raise SystemExit(f"no preparation for source {kind}")
    program = load_program(case)
    if program.over_triples:
        prepare_triples(target)
    facts = target / "facts.nt"
    if not program.over_triples and expressible(case, "nrese") is None and not facts.exists():
        print(f"writing {facts}")
        partial = facts.with_suffix(".nt.partial")
        with open(partial, "w", encoding="utf-8", newline="\n") as out:
            for rel, file in program.inputs.items():
                with open(target / file, encoding="utf-8") as f:
                    rows = (line.rstrip("\n").split(",") for line in f if line.strip())
                    out.writelines(P.ntriples(rel, rows, program.arity[rel]))
        partial.rename(facts)
    manifest = {f.name: {"bytes": f.stat().st_size, "sha256": sha256(f)} for f in sorted(target.iterdir()) if f.is_file()}
    (target.parent / f"{case['data']}.files.json").write_text(json.dumps(manifest, indent=1) + "\n", encoding="utf-8")


def prepare_oxford(case: dict, target: Path):
    """RDFox's ISWC 2015 test data (Oxford): Claros.zip holds facts/Claros.zip, which holds
    Claros.ttl; Jena's riot writes it as N-Triples."""
    archive = downloads() / "oxford" / case["archive"]
    fetch(case["source"]["url"].format(name=case["archive"]), archive, case["sha256"])
    if (target / "facts.nt").exists():
        return
    staging = target.with_name(target.name + ".partial")
    shutil.rmtree(staging, ignore_errors=True)
    staging.mkdir(parents=True)
    with zipfile.ZipFile(archive) as outer:
        inner_name = next(n for n in outer.namelist() if n.endswith("facts/" + case["archive"]))
        outer.extract(inner_name, staging)
    with zipfile.ZipFile(staging / inner_name) as inner:
        turtle = next(n for n in inner.namelist() if n.endswith(".ttl"))
        inner.extract(turtle, staging)
    print(f"converting {turtle} to N-Triples")
    with open(staging / "facts.nt.partial", "wb") as out:
        import subprocess
        subprocess.run(["docker", "run", "--rm", "-u", f"{os.getuid()}:{os.getgid()}", "-v", f"{staging}:/in:ro",
                        "--entrypoint", "/opt/apache-jena-6.2.0/bin/riot", "nrese-bench/jena:6.2.0",
                        "--quiet", "--output=nt", f"/in/{turtle}"], stdout=out, check=True)
    target.mkdir(parents=True, exist_ok=True)
    (staging / "facts.nt.partial").rename(target / "facts.nt")
    shutil.rmtree(staging, ignore_errors=True)


def prepare_lubm(case: dict, target: Path):
    """LUBM(N) from the pinned UBA generator (benches/reasoning/lubm), instance data only:
    the L and LE programs hold the univ-bench TBox as rules."""
    if (target / "facts.nt").exists():
        return
    staging = target.with_name(target.name + ".partial")
    shutil.rmtree(staging, ignore_errors=True)
    staging.mkdir(parents=True)
    docker_as_user("nrese-bench/lubm-uba", ["-u", str(case["universities"]), "-f", "NTRIPLES",
                                            "--consolidate", "Maximal", "-t", "8", "-q", "-o", "/out/gen"],
                   {str(staging): "/out"})
    with open(staging / "facts.nt", "w", encoding="utf-8", newline="\n") as out:
        for part in sorted((staging / "gen").iterdir()):
            with open(part, encoding="utf-8") as f:
                out.writelines(line for line in f if not line.startswith("<> "))
    target.mkdir(parents=True, exist_ok=True)
    (staging / "facts.nt").rename(target / "facts.nt")
    shutil.rmtree(staging, ignore_errors=True)


def prepare_triples(target: Path):
    """The facts of a program over triples in each engine's input form: N-Triples (NRESE,
    Nemo), a tab-separated table of N-Triples terms (Soufflé; a tab inside a literal is
    escaped) and a Trident store (VLog, GLog: `vlog load`)."""
    table = target / "triples.tsv"
    if not table.exists():
        print(f"writing {table}")
        with open(target / "facts.nt", encoding="utf-8") as f, \
                open(table.with_suffix(".tsv.partial"), "w", encoding="utf-8", newline="\n") as out:
            for line in f:
                terms = P.ntriples_terms(line)
                if terms:
                    out.write("\t".join(t.replace("\t", "\\t") for t in terms) + "\n")
        table.with_suffix(".tsv.partial").rename(table)
    trident = target / "trident"
    if not trident.exists():
        print(f"building the Trident store {trident}")
        docker_as_user(IMAGES["vlog"], ["load", "-i", "/data/facts.nt", "-o", "/data/trident.partial", "--maxThreads", "8"],
                       {str(target): "/data"})
        (target / "trident.partial").rename(trident)


# --- one engine, one repetition ---------------------------------------------------------------------

class Run:
    """The steps of one case on one system: writes the program, runs it, reports rows."""

    def __init__(self, args, case: dict, system: str, rep: int, runtime: DockerRuntime, writer: Writer, results: Path):
        self.args, self.case, self.system, self.rep = args, case, system, rep
        self.runtime, self.writer = runtime, writer
        self.program = load_program(case)
        self.expected = case.get("expected", {})
        self.name = f"{case['name']}-{system}-t{args.threads}-r{rep}"
        self.work = Path(setting("DATALOG_SCRATCH", str(Path.home() / "nrese-reasoning-suite/scratch"))) / f"datalog-{os.getpid()}" / self.name
        shutil.rmtree(self.work, ignore_errors=True)
        (self.work / "out").mkdir(parents=True)
        self.logs = results / "logs" / self.name
        self.logs.mkdir(parents=True, exist_ok=True)
        self.data = Mount(str(data_dir(case)), "/data")
        self.memory = setting("DOCKER_MEMORY", "28g")

    def spec(self, image: str, command: list[str], env: dict | None = None, extra: list[Mount] | None = None,
             entrypoint: bool = True) -> Spec:
        # As the invoking user: what the engines write in /work stays removable.
        user = f"{os.getuid()}:{os.getgid()}" if hasattr(os, "getuid") else None
        return Spec(f"nrese-suite-datalog-{self.name}", image, command, {"HOME": "/tmp", **(env or {})},
                    [self.data, Mount(str(self.work), "/work", readonly=False), *(extra or [])],
                    entrypoint=entrypoint, user=user, memory=None if self.memory == "0" else self.memory)

    def row(self, task: str, **fields) -> Result:
        return Result(date=self.args.date, host=self.args.host, runtime="docker", system=self.system,
                      version=self.args.versions.get(self.system, "-"), publish=publish(self.system), workload=workload(self.case),
                      tier=tier(self.case), regime="-", run=self.rep, task=task, **fields)

    def write(self, result: Result):
        self.writer.write(result)

    def total_expected(self) -> int | None:
        """The derived total a right run reports: the published counts' sum where every
        derived relation has one and the engine counts derived facts as Soufflé does (FlowLog's
        programs); else the total this engine reported in its qualifying run, whose
        per-relation counts were checked (`--qualify`, kept in qualified.json)."""
        derived = self.program.derived
        if not self.program.over_triples and all(rel in self.expected for rel in derived):
            return sum(self.expected[rel] for rel in derived)
        if self.args.qualify:
            return None
        return self.qualified().get(f"{self.case['name']}/{self.system}")

    def qualified(self) -> dict:
        """The qualified totals: --qualified, else qualified.json in the results directory."""
        path = Path(self.args.qualified) if self.args.qualified else Path(self.args.results) / "qualified.json"
        return json.loads(path.read_text(encoding="utf-8")) if path.exists() else {}

    def finish(self, measured, reason_ms: float | None, derived: int | None, note: str):
        load_status = "timeout" if measured.timed_out else "ok" if measured.ok else "failed"
        self.write(self.row("load", status=load_status, ms=measured.ms, peak_mib=measured.peak_mib or "",
                            note=note + ("" if measured.ok else f"; exit {measured.rc}")))
        if not measured.ok:
            return
        expected = self.total_expected()
        self.derived = derived
        status = "ok" if derived is not None and (expected is None or derived == expected) else "wrong"
        problem = "" if status == "ok" else f"derived {derived}, expected {expected}"
        if status == "ok" and expected is None and not self.args.qualify:
            # Checked before timed: without a published or qualified total, no time counts.
            status, problem = "failed", "not qualified: no checked total to compare with (run --qualify first)"
        self.write(self.row("reason", status=status, ms=reason_ms if reason_ms is not None else "",
                            rows=derived if derived is not None else "", note=problem or "derived facts; time as the engine reports it"))

    def counts(self, counts: dict[str, int | None], note: str):
        """The qualifying counts, one row per derived relation; where every count is there
        and none contradicts a published one, the engine's total becomes the one its timed
        runs must reproduce (qualified.json). Counts that differ between engines without a
        published one are the suite's disputes (benches/suite/suitekit/summary.py)."""
        passed = True
        for rel in self.program.derived:
            n = counts.get(rel)
            expected = self.expected.get(rel)
            status = "failed" if n is None else "ok" if expected is None or n == expected else "wrong"
            passed &= status == "ok"
            self.write(self.row("query", item=f"count:{rel}", repeat=1, status=status,
                                rows=n if n is not None else "",
                                note=note + ("" if status != "wrong" else f"; expected {expected}")))
        if passed and getattr(self, "derived", None) is not None:
            known = self.qualified() | {f"{self.case['name']}/{self.system}": self.derived}
            (Path(self.args.results) / "qualified.json").write_text(json.dumps(known, indent=1) + "\n", encoding="utf-8")

    def go(self):
        getattr(self, f"run_{self.system}")()

    # --- NRESE ---
    def run_nrese(self):
        binary = Path(setting("NRESE_BIN"))
        (self.work / "program.n3").write_text(P.to_n3(self.program), encoding="utf-8")
        image = setting("NRESE_IMAGE", "ubuntu:24.04")
        bin_mount = Mount(str(binary.parent), "/nrese")
        env = {"NRESE_STORE_MODE": "on-disk", "NRESE_DATA_DIR": "/work/store", "NRESE_REASONING_MODE": "custom",
               "NRESE_REASONING_RULES": "/work/program.n3", "RUST_LOG": "info"}
        log = self.logs / "load.log"
        measured = self.runtime.run(self.spec(image, [f"/nrese/{binary.name}", "load", "/data/facts.nt"], env,
                                              [bin_mount], entrypoint=False), log, self.args.timeout_s)
        text = ANSI.sub("", log.read_text(encoding="utf-8", errors="replace"))
        inferred = re.findall(r"inferred stack rematerialised.*?inferred=(\d+).*?ms=(\d+)", text)
        derived, reason_ms = (int(inferred[-1][0]), float(inferred[-1][1])) if inferred else (None, None)
        size = sum(f.stat().st_size for f in (self.work / "store").rglob("*") if f.is_file()) if (self.work / "store").exists() else None
        self.finish(measured, reason_ms, derived, "nrese-server load: parse, bulk load into the on-disk store, materialise")
        if measured.ok and size:
            self.write(self.row("size", bytes=size, note="the on-disk store with its closure"))
        if self.args.qualify and measured.ok:
            self.write_counts_nrese(image, bin_mount, env)

    def write_counts_nrese(self, image, bin_mount, env):
        import urllib.parse
        port = self.args.port
        spec = self.spec(image, [f"/nrese/{Path(setting('NRESE_BIN')).name}"],
                         env | {"NRESE_BIND_ADDR": "0.0.0.0:8080", "NRESE_READ_REQUESTS_PER_WINDOW": "100000000",
                                "NRESE_QUERY_TIMEOUT_MS": str(self.args.timeout_s * 1000)},
                         [bin_mount], entrypoint=False)
        spec.name += "-serve"
        spec.port = 8080
        server = self.runtime.start(spec, self.logs / "serve.log")
        counts: dict[str, int | None] = {}
        try:
            base = f"http://127.0.0.1:{port}"
            import time
            for _ in range(600):
                try:
                    with urllib.request.urlopen(f"{base}/readyz", timeout=5) as r:
                        if r.status == 200:
                            break
                except OSError:
                    time.sleep(0.5)
            for rel in self.program.derived:
                query = P.count_query(self.program, rel)
                body = urllib.parse.urlencode({"query": query}).encode()
                request = urllib.request.Request(f"{base}/dataset/query", data=body,
                                                 headers={"Accept": "application/sparql-results+json"})
                try:
                    with urllib.request.urlopen(request, timeout=self.args.timeout_s) as r:
                        binding = json.loads(r.read())["results"]["bindings"][0]
                        counts[rel] = int(next(iter(binding.values()))["value"])
                except (OSError, ValueError, KeyError, IndexError):
                    counts[rel] = None
        finally:
            self.runtime.stop(server)
        self.counts(counts, "SPARQL COUNT over the closure")

    # --- Soufflé ---
    def run_souffle(self):
        (self.work / "program.dl").write_text(P.to_souffle(self.program), encoding="utf-8")
        image = IMAGES["souffle"]
        compile_log = self.logs / "compile.log"
        compiled = self.runtime.run(self.spec(image, ["-o", "/work/program", "-j", str(self.args.threads),
                                                      "/work/program.dl"]), compile_log, self.args.timeout_s)
        if not compiled.ok:
            self.write(self.row("load", status="failed", note=f"compilation failed (exit {compiled.rc})"))
            return
        log = self.logs / "run.log"
        measured = self.runtime.run(self.spec(image, ["/work/program", "-F", "/data", "-D", "/work/out",
                                                      "-j", str(self.args.threads)], entrypoint=False),
                                    log, self.args.timeout_s)
        text = log.read_text(encoding="utf-8", errors="replace")
        counts = {m.group(1): int(m.group(2)) for m in re.finditer(r"^(\w+)\t(\d+)\s*$", text, re.M)}
        derived = sum(counts.get(rel, 0) for rel in self.program.derived) if counts else None
        self.finish(measured, None, derived,
                    f"compiled (souffle -o, -j {self.args.threads}); compilation {compiled.ms / 1000:.1f} s apart, not in the time")
        if self.args.qualify and measured.ok:
            self.counts(counts, ".printsize")

    # --- Nemo ---
    def run_nemo(self):
        (self.work / "program.rls").write_text(P.to_nemo(self.program), encoding="utf-8")
        image = IMAGES["nemo"]
        log = self.logs / "run.log"
        measured = self.runtime.run(self.spec(image, ["-I", "/data", "-e", "none", "--report", "short",
                                                      "/work/program.rls"]), log, self.args.timeout_s)
        text = ANSI.sub("", log.read_text(encoding="utf-8", errors="replace"))
        derived = re.findall(r"Derived (\d+) facts", text)
        reasoning = re.findall(r"Reasoning:\s+(\d+)ms", text)
        self.finish(measured, float(reasoning[-1]) if reasoning else None, int(derived[-1]) if derived else None,
                    "nmo -e none (single-threaded by design)")
        if self.args.qualify and measured.ok:
            qlog = self.logs / "qualify.log"
            out = self.runtime.run(self.spec(image, ["-I", "/data", "-D", "/work/out", "-o", "/work/program.rls"]),
                                   qlog, self.args.timeout_s)
            counts = {rel: line_count(self.work / "out" / f"{rel}.csv") if out.ok else None for rel in self.program.derived}
            self.counts(counts, "the exported relation's lines")

    # --- RDFox (UNVERIFIED until a licensed run) ---
    def run_rdfox(self):
        """RDFox 7.6 in its sandbox: the facts imported, then the rules (one materialisation),
        `threads N` as every engine's CPUs; the time is the container's, start-up included
        (RDFox is native code, its start-up is small). The derived total is the store's
        triples less the input's, as RDFox's own SPARQL counts them; the qualifying run
        serves the endpoint and counts every relation over HTTP, as NRESE's does. Its rows
        carry publish = permission: they stay on the machine."""
        licence = rdfox_licence()
        if not licence:
            self.write(self.row("load", status="skipped",
                                note="needs RDFox.lic in the licences directory, or RDFOX_LICENSE=/path"))
            return
        (self.work / "program.dlog").write_text(P.to_rdfox(self.program), encoding="utf-8")
        script = ["dstore create bench", "active bench", f"threads {self.args.threads}",
                  "import /data/facts.nt", "import /work/program.dlog"]
        image = IMAGES["rdfox"]
        lic = Mount(licence, "/license/RDFox.lic")
        args = ["-license-file", "/license/RDFox.lic", "sandbox", "/work", "exec /work/run.rdfox"]
        inputs = line_count(data_dir(self.case) / "facts.nt") or 0
        if not self.args.qualify:
            (self.work / "run.rdfox").write_text("\n".join(
                script + ["SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }", "quit"]) + "\n", encoding="utf-8")
            log = self.logs / "run.log"
            measured = self.runtime.run(self.spec(image, args, extra=[lic]), log, self.args.timeout_s)
            text = log.read_text(encoding="utf-8", errors="replace")
            found = re.findall(r"^\s*(\d+)\s*$", text, re.M)
            derived = int(found[-1]) - inputs if found else None
            self.finish(measured, None, derived, "RDFox sandbox: import facts, import rules, threads as the CPUs; UNVERIFIED")
            return
        port = 12110
        (self.work / "run.rdfox").write_text("\n".join(
            script + [f"set endpoint.port {port}", "endpoint start"]) + "\n", encoding="utf-8")
        spec = self.spec(image, args, extra=[lic])
        spec.keep_stdin, spec.port = True, port
        started = time.monotonic()
        server = self.runtime.start(spec, self.logs / "serve.log")
        base = f"http://127.0.0.1:{self.args.port}/datastores/bench/sparql"
        counts: dict[str, int | None] = {}
        total = None
        try:
            def count(query: str) -> int | None:
                body = urllib.parse.urlencode({"query": query}).encode()
                request = urllib.request.Request(base, data=body, headers={"Accept": "application/sparql-results+json"})
                try:
                    with urllib.request.urlopen(request, timeout=self.args.timeout_s) as r:
                        binding = json.loads(r.read())["results"]["bindings"][0]
                        return int(next(iter(binding.values()))["value"])
                except (OSError, ValueError, KeyError, IndexError):
                    return None
            while time.monotonic() - started < self.args.timeout_s:
                total = count("SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }")
                if total is not None or not self.runtime.alive(server):
                    break
                time.sleep(1)
            elapsed = (time.monotonic() - started) * 1000
            for rel in self.program.derived:
                counts[rel] = count(P.count_query(self.program, rel))
        finally:
            peak = self.runtime.stop(server)
        from suitekit.runtime import Measured
        measured = Measured(elapsed, peak, 0 if total is not None else 1)
        self.finish(measured, None, total - inputs if total is not None else None,
                    "RDFox sandbox, qualifying run (time includes serving the endpoint); UNVERIFIED")
        self.counts(counts, "SPARQL COUNT over RDFox's store")

    # --- VLog and GLog ---
    def run_vlog(self):
        self.run_karma("vlog")

    def run_glog(self):
        self.run_karma("glog")

    def run_karma(self, which: str):
        rules, conf = P.to_vlog(self.program, "/data")
        (self.work / "program.dlog").write_text(rules, encoding="utf-8")
        (self.work / "edb.conf").write_text(conf, encoding="utf-8")
        image = IMAGES[which]
        # The default: one thread, as the authors' papers measure (VLog's multithreaded mode,
        # `--multithreaded 1 --nthreads N`, is experimental: a variant, not the default).
        command = ["mat", "--edb", "/work/edb.conf", "--rules", "/work/program.dlog"]
        if which == "glog":
            # GLog's materialisation guided by trigger graphs, as its VLDB 2021 package runs
            # it (tgchase --querycont 1 --edbcheck 1); the two optimisations added since
            # stay off unless GLOG_OPTIONS says otherwise.
            command = ["tgchase", "--edb", "/work/edb.conf", "--rules", "/work/program.dlog",
                       *setting("GLOG_OPTIONS", "--querycont 1 --edbcheck 1 --rewritecliques 0 --delProofs 0").split()]
        if self.args.vlog_threads:
            command += ["--multithreaded", "1", "--nthreads", str(self.args.threads)]
        # VLog reports no count of what it derived, so every run stores its materialisation
        # and the files are counted afterwards, untimed; the time VLog logs for storing them
        # comes off the wall time.
        command += ["--storemat_path", "/work/out", "--storemat_format", "csv"]
        log = self.logs / "run.log"
        measured = self.runtime.run(self.spec(image, command, extra=[]), log, self.args.timeout_s)
        text = log.read_text(encoding="utf-8", errors="replace")
        runtime = re.findall(r"Runtime materialization = ([0-9.]+) milliseconds", text)
        storing = re.findall(r"Time to index and store the materialization on disk = ([0-9.]+) seconds", text)
        if storing and measured.ok:
            measured.ms -= float(storing[-1]) * 1000
        counts = {}
        if measured.ok:
            files = [f for f in (self.work / "out").iterdir() if f.is_file()]
            for rel in self.program.derived:
                # RP1's files, not RP10's: the name goes on with a non-digit, or ends.
                own = [f for f in files if re.fullmatch(rf"{re.escape(rel)}(\D.*)?", f.name)]
                counts[rel] = sum(line_count(f) for f in own) if own else 0
            if self.args.qualify:
                self.counts(counts, "the stored materialisation's lines")
        total = sum(counts.values()) if counts else None
        mode = f"--multithreaded 1 --nthreads {self.args.threads}" if self.args.vlog_threads else "single-threaded (the default)"
        note = f"{' '.join(command[:1] + command[5:-4])}, {mode}"
        if storing:
            note += f"; storing the materialisation ({float(storing[-1]):.1f} s, for the count) taken off the time"
        self.finish(measured, float(runtime[-1]) if runtime else None, total, note)


def line_count(path: Path) -> int | None:
    if not path.exists():
        return None
    with open(path, "rb") as f:
        return sum(1 for _ in f)


# --- the commands ---------------------------------------------------------------------------------

def cmd_list(args):
    for name, case in cases().items():
        if not program_path(case).exists():
            print(f"{name:<22} {case['program']:<32} (the program is fetched by `prepare`)")
            continue
        why = {s: expressible(case, s) for s in SYSTEMS}
        runs = ", ".join(s for s, w in why.items() if w is None)
        print(f"{name:<22} {case['program']:<32} runs on: {runs}")
        for s, w in why.items():
            if w:
                print(f"{'':<22}   {s}: not expressible ({w})")


def cmd_prepare(args):
    for name in selected(args.cases):
        hostguard.wait()
        with hostguard.slot(f"datalog prepare {name}", timed=False):
            prepare(cases()[name])


def cmd_translate(args):
    case = cases()[args.case]
    program = load_program(case)
    out = Path(args.dir)
    out.mkdir(parents=True, exist_ok=True)
    (out / "program.dl").write_text(P.to_souffle(program), encoding="utf-8")
    (out / "program.rls").write_text(P.to_nemo(program), encoding="utf-8")
    for name, make in (("program.n3", lambda: P.to_n3(program)),
                       ("program.dlog", lambda: P.to_vlog(program, "/data")[0])):
        try:
            (out / name).write_text(make(), encoding="utf-8")
        except P.NotExpressible as e:
            (out / (name + ".not-expressible")).write_text(str(e) + "\n", encoding="utf-8")
    print(f"written to {out}")


def selected(spec: str | None) -> list[str]:
    known = cases()
    names = spec.split(",") if spec else list(known)
    unknown = [n for n in names if n not in known]
    if unknown:
        raise SystemExit(f"unknown cases: {', '.join(unknown)}")
    return names


def qualified_for(args, case: dict, system: str) -> bool:
    """Whether a timed run of `system` on `case` has a total to check against: the case's
    published counts for every derived relation (FlowLog's programs), or the system's
    qualified total."""
    program = load_program(case)
    expected = case.get("expected", {})
    if not program.over_triples and all(rel in expected for rel in program.derived):
        return True
    known = json.loads(Path(args.qualified).read_text(encoding="utf-8"))
    return f"{case['name']}/{system}" in known


def version(system: str) -> str:
    if system == "nrese":
        return setting("NRESE_VERSION", "-")
    return IMAGES[system].rsplit(":", 1)[-1] if ":" in IMAGES[system] else "-"


def cmd_run(args):
    import datetime
    import platform
    if "nrese" in args.systems.split(",") and not setting("NRESE_BIN"):
        raise SystemExit("set NRESE_BIN to the nrese-server binary")
    if args.qualified and not Path(args.qualified).is_file():
        raise SystemExit(f"no qualified totals at {args.qualified}: the qualifying run hasn't passed; nothing timed")
    results = Path(args.results)
    results.mkdir(parents=True, exist_ok=True)
    args.date = datetime.datetime.now().isoformat(timespec="minutes")
    args.host = platform.node()
    systems = args.systems.split(",")
    args.versions = {s: version(s) for s in systems}
    os.environ["DOCKER_CPUS"] = str(args.threads)
    # The manifest's protocol fields, in the suite's terms.
    protocol = argparse.Namespace(runs=args.runs, query_runs=0, cache="-", order="rotated", seed=0,
                                  timeout_s=args.timeout_s, query_timeout_s=args.timeout_s)
    manifest = Manifest(results, ROOT, sys.argv[1:], protocol, "docker", dict(os.environ))
    manifest.update(images={s: IMAGES.get(s, setting("NRESE_IMAGE", "ubuntu:24.04")) for s in systems},
                    settings={"DOCKER_CPUS": str(args.threads), "DOCKER_MEMORY": setting("DOCKER_MEMORY", "28g"),
                              "NRESE_BIN": setting("NRESE_BIN")})
    writer = CountingWriter(results / "results.csv", echo=print)
    runtime = DockerRuntime(Path(setting("DATALOG_SCRATCH", str(Path.home() / "nrese-reasoning-suite/scratch"))),
                            args.port)
    code = 0
    try:
        for name in selected(args.cases):
            case = cases()[name]
            runnable = []
            for system in systems:
                why = expressible(case, system)
                if why:
                    writer.write(Result(date=args.date, host=args.host, runtime="docker", system=system,
                                        version=args.versions[system], publish=publish(system), workload=workload(case), tier=tier(case),
                                        regime="-", run=0, task="load", status="skipped",
                                        note=f"not expressible: {why}"))
                elif args.qualified and not qualified_for(args, case, system):
                    writer.write(Result(date=args.date, host=args.host, runtime="docker", system=system,
                                        version=args.versions[system], publish=publish(system), workload=workload(case),
                                        tier=tier(case), regime="-", run=0, task="load", status="skipped",
                                        note="not qualified: its qualifying run gave no checked total"))
                else:
                    runnable.append(system)
            for rep in range(1, args.runs + 1):
                k = (rep - 1) % max(1, len(runnable))
                for system in runnable[k:] + runnable[:k]:
                    hostguard.wait(hostguard.gib(setting("DOCKER_MEMORY", "28g")))
                    print(f"{name} / {system} / {args.threads} threads / run {rep}")
                    run = Run(args, case, system, rep, runtime, writer, results)
                    try:
                        with hostguard.slot(f"datalog {name} {system} t{args.threads} r{rep}", timed=not args.qualify):
                            run.go()
                    finally:
                        if not args.keep:
                            shutil.rmtree(run.work, ignore_errors=True)
    except KeyboardInterrupt:
        code = 130
        raise
    finally:
        runtime.cleanup()
        manifest.finish(code)
    print(f"results: {results / 'results.csv'}")
    if writer.bad:
        # A queue job stops here: nothing after a wrong or failed step should be timed.
        print(f"{writer.bad} rows wrong, failed or timed out")
        return 1
    return 0


class CountingWriter(Writer):
    """The suite's writer, counting the rows that went wrong (a skipped pair isn't one)."""

    def __init__(self, path: Path, echo=None):
        super().__init__(path, echo)
        self.bad = 0

    def write(self, result: Result):
        super().write(result)
        if result.status in ("wrong", "failed", "timeout"):
            self.bad += 1


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    sub.add_parser("list")
    pp = sub.add_parser("prepare")
    pp.add_argument("--cases")
    pt = sub.add_parser("translate")
    pt.add_argument("case")
    pt.add_argument("dir")
    pr = sub.add_parser("run")
    pr.add_argument("--cases")
    pr.add_argument("--systems", default=",".join(SYSTEMS))
    pr.add_argument("--threads", type=int, default=4, help="every container's CPUs (DOCKER_CPUS) and the engines' thread count")
    pr.add_argument("--runs", type=int, default=3)
    pr.add_argument("--timeout-s", type=int, default=1800)
    pr.add_argument("--results", required=True)
    pr.add_argument("--port", type=int, default=19351)
    pr.add_argument("--qualify", action="store_true", help="also count every derived relation (untimed check)")
    pr.add_argument("--qualified", help="the qualifying run's qualified.json, when it wrote another results directory")
    pr.add_argument("--keep", action="store_true", help="keep each run's scratch")
    pr.add_argument("--vlog-threads", action="store_true", help="VLog and GLog in their multithreaded mode")
    args = p.parse_args(argv)
    sys.exit({"list": cmd_list, "prepare": cmd_prepare, "translate": cmd_translate, "run": cmd_run}[args.command](args) or 0)


if __name__ == "__main__":
    main()
