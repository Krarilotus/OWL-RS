#!/usr/bin/env python3
"""LDBC Semantic Publishing Benchmark 2.0 (Apache-2.0, github.com/ldbc/ldbc_spb_bm_2.0): GraphDB's
home turf, on NRESE and GraphDB with the same ruleset (the benchmark's own
RdfsRules-optimized-spb.pie), the same generated data and the benchmark's own driver.

    spb.py generate --size 50000000                  data, generated once and frozen (NRESE hosts it)
    spb.py run --systems nrese,graphdb --size 50000000 --agents 8,2 --runs 3 --results DIR

Unaudited results are "not an LDBC benchmark result" (LDBC fair-use policy).

**Generation** (once per size): the ontologies and reference datasets bulk-loaded into NRESE
with the SPB ruleset (`nrese-server load`: the reference files, up to 472 MB each, exceed the
server's upload budget), then the driver's generateCreativeWorks phase against its RDF4J
endpoint; the creative works are written as N-Quads and frozen (their SHA-256 in
generated/files.json).

**A run**, per system and repetition, from a fresh store:
1. **load**: the ontologies, the reference datasets and the creative works with the system's own
   bulk loader (NRESE `nrese-server load`, GraphDB `importrdf load` with the ruleset),
   inference included; wall time and peak memory;
2. **validate**: the driver's validateQueryResults phase against the reference answers
   (data/validation), on a store with the ontologies and reference data only, and
   checkConformance (the OWL 2 RL conformance queries) where the ruleset is OWL 2 RL; an
   invalid answer makes the run `wrong`;
3. **benchmark**: generateQuerySubstitutionParameters, warmUp, then runBenchmark: A
   aggregation agents and E editorial agents at once (`--agents A,E`; the concurrent mode),
   for --duration-s. The driver's own summary (operations per second, per query) is parsed
   into `query` rows (`aggregation:<query>`, `editorial:<op>`: the average ms, the count in
   the note) and the totals into `serve`-free `query` rows `rate:aggregation`,
   `rate:editorial` (rows = operations per second, rounded).

Settings: SPB_HOME (the built benchmark, dist/), SPB_DATA (generated data, default
~/nrese-reasoning-suite/data/spb), NRESE_BIN, NRESE_LICENSES (GraphDB), DOCKER_MEMORY, DOCKER_CPUS,
JAVA (default `java` in the image SPB_IMAGE, eclipse-temurin:11-jre).
"""
from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(ROOT / "benches/suite"))
sys.path.insert(0, str(HERE.parent))
sys.path.insert(0, str(HERE.parent / "maintenance"))

from maintenance import NreseBinary  # noqa: E402
from suitekit import ns, quiet  # noqa: E402
from suitekit.adapters import Context, Graphdb  # noqa: E402
from suitekit.manifest import Manifest  # noqa: E402
from suitekit.runtime import DockerRuntime, Mount, Spec  # noqa: E402
from suitekit.schema import Result, Writer  # noqa: E402

WORKLOAD = "ldbc-spb"
RULESET = "RdfsRules-optimized-spb.pie"


def setting(key: str, default: str = "") -> str:
    return os.environ.get(key) or default


def spb_home() -> Path:
    return Path(setting("SPB_HOME", str(Path.home() / "nrese-reasoning-suite/downloads/spb/ldbc_spb_bm_2.0/dist")))


def data_root(size: int) -> Path:
    return Path(setting("SPB_DATA", str(Path.home() / "nrese-reasoning-suite/data/spb"))) / f"{size // 1_000_000}m"


def tier(size: int) -> str:
    return f"{size // 1_000_000}m"


def properties(path: Path, endpoint: str, update: str, size: int, phases: set[str], agents=(8, 2),
               duration_s: int = 600, warmup_s: int = 60, generated: str = "/spb-data/generated"):
    """The driver's test.properties: the shipped file's keys, every phase off except `phases`."""
    every = ["loadOntologies", "loadReferenceDatasets", "generateCreativeWorks", "loadCreativeWorks",
             "generateQuerySubstitutionParameters", "validateQueryResults", "warmUp", "runBenchmark",
             "runBenchmarkOnlineReplicationAndBackup", "checkConformance", "cleanupDatabase"]
    values = {
        "aggregationAgents": agents[0], "editorialAgents": agents[1], "dataGeneratorWorkers": 4,
        "warmupPeriodSeconds": warmup_s, "benchmarkRunPeriodSeconds": duration_s, "datasetSize": size,
        "generatedTriplesPerFile": 500000, "queryTimeoutSeconds": 300, "querySubstitutionParameters": 100000,
        "benchmarkByQueryMixRuns": 0, "minUpdateRateThresholdOps": 0.0, "minUpdateRateThresholdReachTimePercent": 0,
        "maxUpdateRateThresholdOps": 0.0, "endpointURL": endpoint, "endpointUpdateURL": update,
        "ontologiesPath": "/spb/data/ontologies", "referenceDatasetsPath": "/spb/data/datasets",
        "creativeWorksPath": generated, "queriesPath": "/spb/data/sparql", "validationPath": "/spb/data/validation",
        "definitionsPath": "/spb/definitions.properties", "scriptsPath": "/spb/data/scripts",
        "enableCompressionOnGeneratedData": "false", "creativeWorksInfo": "dataset.info",
        "generateCreativeWorksFormat": "n-quads", "currentRateReportPeriodSeconds": 0,
        "saveDetailedQueryLogs": "true", "verbose": "false",
    }
    values |= {phase: "true" if phase in phases else "false" for phase in every}
    path.write_text("".join(f"{k}={v}\n" for k, v in values.items()), encoding="utf-8")


def driver_spec(name: str, work: Path, data: Path, memory: str | None) -> Spec:
    """The SPB driver in a JRE container on the host network (it reaches the endpoints the
    runtime published on localhost)."""
    return Spec(name, setting("SPB_IMAGE", "eclipse-temurin:11-jre"),
                ["sh", "-c", f"cd /work && java -Xmx8g -jar /spb/{jar().name} /work/test.properties"],
                {}, [Mount(str(spb_home()), "/spb"), Mount(str(data), "/spb-data", readonly=False),
                     Mount(str(work), "/work", readonly=False)], memory=memory)


def jar() -> Path:
    return next(spb_home().glob("semantic_publishing_benchmark-basic-standard*.jar"))


def run_driver(name: str, work: Path, data: Path, log: Path, timeout_s: int) -> int:
    """Runs the driver with host networking (the endpoints listen on localhost)."""
    spec = driver_spec(name, work, data, None)
    command = [ns.executable(), "run", "--rm", *ns.name_args(ns.name(name, ROOT), ROOT, "reasoning"), "--network", "host",
               "-u", f"{os.getuid()}:{os.getgid()}", "-e", "HOME=/tmp"]
    for m in spec.mounts:
        command += ["-v", f"{m.source}:{m.target}{':ro' if m.readonly else ''}"]
    command += [spec.image, *spec.command]
    with open(log, "w", encoding="utf-8") as out:
        try:
            return subprocess.run(command, stdout=out, stderr=subprocess.STDOUT, timeout=timeout_s).returncode
        except subprocess.TimeoutExpired:
            subprocess.run(["docker", "rm", "-f", name], capture_output=True)
            return 124


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def context(runtime, work: Path, logs: Path, data: Path, run_id: str, timeout_s: int) -> Context:
    settings = dict(os.environ)
    settings.setdefault("DOCKER_MEMORY", "28g")
    settings["NRESE_REASONING_RULES"] = f"/spb/data/{RULESET}"
    return Context(runtime, ROOT, work, logs, Mount(str(data), "/spb-data"),
                   [Mount(str(spb_home()), "/spb")], timeout_s, settings, run_id)


class SpbNrese(NreseBinary):
    """NRESE with the SPB ruleset: mode `custom` with the .pie file as its whole program."""
    regimes = {"spb": "custom"}

    def spec(self, ctx, name, store, command, env):
        spec = super().spec(ctx, name, store, command, env)
        spec.env["NRESE_REASONING_RULES"] = f"/spb/data/{RULESET}"
        spec.mounts.append(Mount(str(spb_home()), "/spb"))
        return spec

    def serve(self, ctx, store, regime):
        """The driver speaks the RDF4J protocol, as it does with GraphDB: queries to the
        repository, RDF payloads and updates to its statements."""
        endpoint = super().serve(ctx, store, regime)
        base = endpoint.query.rsplit("/dataset/", 1)[0]
        endpoint.query, endpoint.update = f"{base}/repositories/nrese", f"{base}/repositories/nrese/statements"
        return endpoint


class SpbGraphdb(Graphdb):
    """GraphDB with the benchmark's ruleset file, as the SPB documentation configures the
    repository: the .pie file as its ruleset, the context index on (the creative works are
    N-Quads), native sameAs on (the ruleset has owl:sameAs)."""
    regimes = {"spb": f"/spb/data/{RULESET}"}

    def repository_settings(self, ctx, regime):
        values = super().repository_settings(ctx, regime)
        values["disable-sameAs"] = "false"
        values["enable-context-index"] = "true"
        return values


def system_for(key: str):
    return {"nrese": SpbNrese, "graphdb": SpbGraphdb}[key]()


# --- generate ---------------------------------------------------------------------------------

def generate_now(args):
    out = data_root(args.size)
    if (out / "generated" / "files.json").exists():
        print(f"{out}: already generated")
        return
    (out / "generated").mkdir(parents=True, exist_ok=True)
    runtime = DockerRuntime(Path(setting("SPB_SCRATCH", str(Path.home() / "nrese-reasoning-suite/scratch"))), args.port)
    run_id = f"spb-generate-{tier(args.size)}"
    work = Path(runtime.scratch) / run_id
    work.mkdir(parents=True, exist_ok=True)
    logs = out / "logs"
    logs.mkdir(exist_ok=True)
    ctx = context(runtime, work, logs, out, run_id, 24 * 3600)
    nrese = SpbNrese()
    store = runtime.new_store(ctx.name("store"))
    try:
        # The ontologies and reference datasets by the bulk loader (files up to 472 MB, past
        # the server's upload budget), then only the generator phase over HTTP.
        loaded = nrese.load(ctx, store, inputs(out), "spb")
        if not loaded.measured.ok:
            raise SystemExit(f"loading the reference data failed; see {logs / 'load.log'}")
        endpoint = nrese.serve(ctx, store, "spb")
        properties(work / "test.properties", endpoint.query, endpoint.update, args.size,
                   {"generateCreativeWorks"})
        shutil.copy(spb_home() / "definitions.properties", work / "definitions.properties")
        started = time.monotonic()
        rc = run_driver(f"suite-{run_id}-driver", work, out, logs / "generate.log", 24 * 3600)
        print(f"generation exit {rc} after {time.monotonic() - started:.0f} s")
        if rc != 0:
            raise SystemExit(f"generation failed; see {logs / 'generate.log'}")
    finally:
        nrese.stop(ctx)
        runtime.remove_store(store)
        runtime.cleanup()
    files = {p.name: {"bytes": p.stat().st_size, "sha256": sha256(p)}
             for p in sorted((out / "generated").iterdir()) if p.is_file()}
    (out / "generated" / "files.json").write_text(json.dumps(files, indent=1) + "\n", encoding="utf-8")
    print(f"{len(files)} files, {sum(f['bytes'] for f in files.values()) / 1e9:.1f} GB")


def generate(args):
    """Generation is heavy (an hour at 50 M): it waits for the memory, untimed (no slot needed)."""
    quiet.wait_memory(quiet.gib(setting("DOCKER_MEMORY", "28g")))
    with quiet.step(f"spb generate {tier(args.size)}", timed=False):
        generate_now(args)


# --- run ----------------------------------------------------------------------------------------

def parse_summary(text: str) -> dict:
    """The driver's last results block: per-operation counts and averages, and the rates."""
    out = {"operations": {}, "rates": {}}
    for m in re.finditer(r"^\s*(\w[\w\-.]*)\s*:\s*(\d+)\s+ops?.*?avg\s*[:=]?\s*([0-9.]+)\s*ms", text, re.M | re.I):
        out["operations"][m.group(1)] = (int(m.group(2)), float(m.group(3)))
    for kind in ("editorial", "aggregation"):
        found = re.findall(rf"([0-9.]+)\s+average\s+{kind}\s+operations per second", text, re.I)
        if found:
            out["rates"][kind] = float(found[-1])
    return out


def cmd_run(args):
    results = Path(args.results)
    results.mkdir(parents=True, exist_ok=True)
    data = data_root(args.size)
    if not (data / "generated" / "files.json").exists():
        raise SystemExit(f"no generated data for {tier(args.size)}: run `spb.py generate --size {args.size}`")
    date = datetime.datetime.now().isoformat(timespec="minutes")
    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    systems = args.systems.split(",")
    agents = tuple(int(a) for a in args.agents.split(","))
    protocol = argparse.Namespace(runs=args.runs, query_runs=0, cache="-", order="rotated", seed=0,
                                  timeout_s=args.timeout_s, query_timeout_s=300)
    manifest = Manifest(results, ROOT, sys.argv[1:], protocol, "docker", dict(os.environ))
    writer = Writer(results / "results.csv", echo=print)
    runtime = DockerRuntime(Path(setting("SPB_SCRATCH", str(Path.home() / "nrese-reasoning-suite/scratch"))), args.port)
    registry = __import__("tomllib").loads((ROOT / "benches/suite/systems.toml").read_text(encoding="utf-8"))
    code = 0
    try:
        for rep in range(1, args.runs + 1):
            k = (rep - 1) % len(systems)
            for key in systems[k:] + systems[:k]:
                quiet.wait_memory(quiet.gib(setting("DOCKER_MEMORY", "28g")))
                with quiet.step(f"spb {key} {tier(args.size)} r{rep}", timed=True):
                    one(args, key, rep, runtime, writer, results, data, date, stamp, agents,
                        registry.get(key, {}).get("publish", "free"))
    except KeyboardInterrupt:
        code = 130
        raise
    finally:
        runtime.cleanup()
        manifest.finish(code)


def inputs(data: Path) -> list[str]:
    """Every file the benchmark's database holds, as the systems see them."""
    files = [f"/spb/data/ontologies/{p.relative_to(spb_home() / 'data/ontologies').as_posix()}"
             for p in sorted((spb_home() / "data/ontologies").rglob("*.ttl"))]
    files += [f"/spb/data/datasets/{p.name}" for p in sorted((spb_home() / "data/datasets").glob("*.ttl"))]
    files += [f"/spb-data/generated/{p.name}" for p in sorted((data / "generated").glob("*.nq"))]
    return files


def one(args, key, rep, runtime, writer, results, data, date, stamp, agents, publish):
    run_id = f"{stamp}-spb-{key}-{tier(args.size)}-r{rep}"
    work = Path(runtime.scratch) / run_id
    work.mkdir(parents=True, exist_ok=True)
    logs = results / "logs" / run_id
    logs.mkdir(parents=True, exist_ok=True)
    ctx = context(runtime, work, logs, data, run_id, args.timeout_s)
    system = system_for(key)
    regime = "spb"

    def row(task, **fields):
        return Result(date=date, host=platform.node(), runtime="docker", system=key, version="-", publish=publish,
                      workload=WORKLOAD, tier=tier(args.size), regime="custom", run=rep, task=task, **fields)

    why = system.supports(ctx, inputs(data), regime)
    if why:
        writer.write(row("load", status="skipped", note=why))
        return
    store = runtime.new_store(ctx.name("store"))
    try:
        step = system.load(ctx, store, inputs(data), regime)
        m = step.measured
        writer.write(row("load", status="ok" if m.ok else "timeout" if m.timed_out else "failed", ms=m.ms,
                         peak_mib=m.peak_mib or "", note="ontologies, reference datasets and creative works, inference included"))
        if not m.ok:
            return
        endpoint = system.serve(ctx, store, regime)
        try:
            update = endpoint.update or endpoint.query
            shutil.copy(spb_home() / "definitions.properties", work / "definitions.properties")
            properties(work / "test.properties", endpoint.query, update, args.size,
                       {"generateQuerySubstitutionParameters", "warmUp", "runBenchmark"}, agents,
                       args.duration_s, args.warmup_s, generated="/work/generated")
            (work / "generated").mkdir(exist_ok=True)
            # The generator's account of what it made (the editorial agents read it).
            for info in (data / "generated").glob("*.info"):
                shutil.copy(info, work / "generated" / info.name)
            started = time.monotonic()
            rc = run_driver(f"suite-{run_id}-driver", work, data, logs / "benchmark.log",
                            args.duration_s + args.warmup_s + 3600)
            text = (logs / "benchmark.log").read_text(encoding="utf-8", errors="replace")
            summary = parse_summary(text)
            status = "ok" if rc == 0 else "failed"
            for name, (count, avg) in summary["operations"].items():
                writer.write(row("query", item=name, repeat=1, status=status, ms=avg, rows=count,
                                 note=f"{agents[0]} aggregation and {agents[1]} editorial agents; average ms, count in rows"))
            for kind, rate in summary["rates"].items():
                writer.write(row("query", item=f"rate:{kind}", repeat=1, status=status, rows=round(rate),
                                 note=f"{rate} {kind} operations per second over {args.duration_s} s"))
            if not summary["operations"]:
                writer.write(row("query", item="benchmark", repeat=1, status="failed",
                                 note=f"no summary parsed (exit {rc}, {time.monotonic() - started:.0f} s); see benchmark.log"))
        finally:
            peak = system.stop(ctx)
            writer.write(row("serve", peak_mib=peak or "", note="the server's peak over the benchmark"))
    finally:
        runtime.remove_store(store)
        shutil.rmtree(work, ignore_errors=True)


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    pg = sub.add_parser("generate")
    pg.add_argument("--size", type=int, default=50_000_000)
    pg.add_argument("--port", type=int, default=19353)
    pr = sub.add_parser("run")
    pr.add_argument("--systems", default="nrese,graphdb")
    pr.add_argument("--size", type=int, default=50_000_000)
    pr.add_argument("--agents", default="8,2", help="aggregation,editorial agents (the concurrent mode)")
    pr.add_argument("--runs", type=int, default=3)
    pr.add_argument("--duration-s", type=int, default=600)
    pr.add_argument("--warmup-s", type=int, default=60)
    pr.add_argument("--timeout-s", type=int, default=7200)
    pr.add_argument("--port", type=int, default=19353)
    pr.add_argument("--results", required=True)
    args = p.parse_args(argv)
    {"generate": generate, "run": cmd_run}[args.command](args)


if __name__ == "__main__":
    main()
