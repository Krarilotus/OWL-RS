#!/usr/bin/env python3
"""Maintenance on RDFox's own programs, in the setup of Motik et al., AIJ 2019 (Tables 3 and
6): a program of the Datalog kit (cases.toml) materialised on NRESE as user rules, then for
each of N samples of K explicit facts drawn uniformly (seeds 1..N), the K facts deleted and
added back, each a single SPARQL Update, timed from request to response.

    maintain.py run --cases claros-l-noeq --samples 30 --size 1000 --cpus 1 --runs 1 --results DIR

The paper ran RDFox single-threaded on Claros L and LE with equality off (the `*-noeq`
variants), 30 samples of 1,000 facts, and reports the mean with its 95% confidence interval;
its numbers are reported rows (../reported/rdfox.csv), never measured here (RDFox's licence
was refused). NRESE is the only system: VLog, GLog, Soufflé and Nemo don't maintain a
materialisation. Every change is undone, so the closure must have the base's size at the
end, or the run is `wrong`.

Rows follow the suite's schema: the case's workload and tier (cases.toml), task `update`,
items `delete-K-uniform` and `add-K-uniform` (`repeat` = the sample's seed), a `count` row
before and after. Settings as datalog.py's (NRESE_BIN, DATALOG_DATA, DOCKER_MEMORY).
"""
from __future__ import annotations

import argparse
import datetime
import json
import os
import platform
import random
import shutil
import sys
import urllib.parse
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))
sys.path.insert(0, str(HERE.parent / "maintenance"))
sys.path.insert(0, str(ROOT / "benches/suite"))

import datalog  # noqa: E402
import hostguard  # noqa: E402
import program as P  # noqa: E402
from maintenance import CountingWriter, NreseBinary, update  # noqa: E402
from suitekit.adapters import Context  # noqa: E402
from suitekit.manifest import Manifest  # noqa: E402
from suitekit.runtime import DockerRuntime, Mount  # noqa: E402
from suitekit.schema import Result  # noqa: E402


class RulesNrese(NreseBinary):
    """NRESE with a program of the kit as its whole rule set (mode `custom`)."""
    regimes = {"custom": "custom"}

    def __init__(self, rules_dir: Path):
        super().__init__()
        self.rules_dir = rules_dir

    def spec(self, ctx, name, store, command, env):
        spec = super().spec(ctx, name, store, command, env)
        spec.env["NRESE_REASONING_RULES"] = "/rules/program.n3"
        spec.mounts.append(Mount(str(self.rules_dir), "/rules"))
        return spec


def samples(case: dict, size: int, count: int) -> list[list[str]]:
    """The input facts of seeds 1..count, `size` each, drawn uniformly without replacement
    (Motik et al.'s uniform sampling), kept beside the data so every run deletes the same."""
    data = datalog.data_dir(case)
    cache = data / f"maintain-uniform-{size}x{count}.json"
    if cache.exists():
        return json.loads(cache.read_text(encoding="utf-8"))
    facts = data / "facts.nt"
    # Facts naming a blank node can't be deleted by DELETE DATA, and INSERT DATA would make
    # new blank nodes: the samples are drawn from the other facts.
    with open(facts, encoding="utf-8") as f:
        eligible = [i for i, line in enumerate(f)
                    if (terms := P.ntriples_terms(line)) and not (terms[0].startswith("_:") or terms[2].startswith("_:"))]
    wanted: dict[int, list[int]] = {}
    for seed in range(1, count + 1):
        for index in random.Random(seed).sample(eligible, size):
            wanted.setdefault(index, []).append(seed)
    chosen: dict[int, list[str]] = {seed: [] for seed in range(1, count + 1)}
    with open(facts, encoding="utf-8") as f:
        for i, line in enumerate(f):
            for seed in wanted.get(i, ()):
                chosen[seed].append(line if line.endswith("\n") else line + "\n")
    out = [chosen[seed] for seed in range(1, count + 1)]
    cache.write_text(json.dumps(out), encoding="utf-8")
    return out


def closure_size(url: str, timeout: float) -> int | None:
    body = urllib.parse.urlencode({"query": "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }"}).encode()
    request = urllib.request.Request(url, data=body, headers={"Accept": "application/sparql-results+json"})
    try:
        with urllib.request.urlopen(request, timeout=timeout) as r:
            return int(next(iter(json.loads(r.read())["results"]["bindings"][0].values()))["value"])
    except (OSError, ValueError, KeyError, IndexError, StopIteration):
        return None


def run_case(args, case: dict, rep: int, runtime: DockerRuntime, writer, results: Path):
    name = case["name"]
    run_id = f"{args.stamp}-maintain-{name}-r{rep}"
    work = Path(runtime.scratch) / run_id
    rules = work / "rules"
    rules.mkdir(parents=True, exist_ok=True)
    (rules / "program.n3").write_text(P.to_n3(datalog.load_program(case)), encoding="utf-8")
    logs = results / "logs" / run_id
    logs.mkdir(parents=True, exist_ok=True)
    settings = dict(os.environ)
    settings.setdefault("DOCKER_MEMORY", "28g")
    ctx = Context(runtime, ROOT, work, logs, Mount(str(datalog.data_dir(case)), "/data"), [], args.timeout_s,
                  settings, run_id)

    def row(task, **fields):
        return Result(date=args.date, host=args.host, runtime="docker", system="nrese",
                      version=os.environ.get("NRESE_VERSION", "-"), publish="free",
                      workload=datalog.workload(case), tier=datalog.tier(case), regime="-", run=rep, task=task, **fields)

    nrese = RulesNrese(rules)
    store = runtime.new_store(ctx.name("store"))
    try:
        step = nrese.load(ctx, store, ["/data/facts.nt"], "custom")
        m = step.measured
        writer.write(row("load", status="ok" if m.ok else "timeout" if m.timed_out else "failed", ms=m.ms,
                         peak_mib=m.peak_mib or "", note=f"the program as user rules, {args.cpus} CPU(s)"))
        if not m.ok:
            return
        endpoint = nrese.serve(ctx, store, "custom")
        try:
            base = closure_size(endpoint.query, args.timeout_s)
            writer.write(row("count", status="ok" if base is not None else "failed", rows=base if base is not None else "",
                             note="the closure after the load, input included"))
            failures = 0
            for seed, facts in enumerate(samples(case, args.size, args.samples), 1):
                block = "".join(facts)
                for item, verb in ((f"delete-{args.size}-uniform", "DELETE"), (f"add-{args.size}-uniform", "INSERT")):
                    ms, status, detail = update(endpoint.update, f"{verb} DATA {{\n{block}}}", args.timeout_s)
                    ok = 200 <= status < 300
                    failures += not ok
                    writer.write(row("update", item=item, repeat=seed, status="ok" if ok else "failed", ms=ms,
                                     rows=len(facts), note=("uniform sample of the facts without blank nodes"
                                                            if ok else f"HTTP {status}: {detail}")))
            after = closure_size(endpoint.query, args.timeout_s)
            same = after is not None and after == base
            writer.write(row("count", status="ok" if same and not failures else "wrong", rows=after if after is not None else "",
                             note="the closure after every sample was deleted and added back: "
                                  + ("the base's size" if same else f"{after} against the base's {base}")))
        finally:
            peak = nrese.stop(ctx)
            writer.write(row("serve", peak_mib=peak or "", note="the server's peak over the maintenance"))
    finally:
        runtime.remove_store(store)
        shutil.rmtree(work, ignore_errors=True)


def cmd_run(args) -> int:
    results = Path(args.results)
    results.mkdir(parents=True, exist_ok=True)
    args.date = datetime.datetime.now().isoformat(timespec="minutes")
    args.stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    args.host = platform.node()
    os.environ["DOCKER_CPUS"] = str(args.cpus)
    protocol = argparse.Namespace(runs=args.runs, query_runs=0, cache="-", order="fixed", seed=1,
                                  timeout_s=args.timeout_s, query_timeout_s=args.timeout_s)
    manifest = Manifest(results, ROOT, sys.argv[1:], protocol, "docker", dict(os.environ))
    writer = CountingWriter(results / "results.csv", echo=print)
    runtime = DockerRuntime(Path(os.environ.get("DATALOG_SCRATCH", str(Path.home() / "nrese-reasoning-suite/scratch"))),
                            args.port)
    try:
        for name in datalog.selected(args.cases):
            for rep in range(1, args.runs + 1):
                hostguard.wait(hostguard.gib(os.environ.get("DOCKER_MEMORY", "28g")))
                with hostguard.slot(f"maintain {name} r{rep}", timed=True):
                    run_case(args, datalog.cases()[name], rep, runtime, writer, results)
    finally:
        runtime.cleanup()
        manifest.finish(0)
    return 1 if writer.bad else 0


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    pr = sub.add_parser("run")
    pr.add_argument("--cases", required=True)
    pr.add_argument("--samples", type=int, default=30)
    pr.add_argument("--size", type=int, default=1000)
    pr.add_argument("--cpus", type=int, default=1, help="the container's CPUs (the paper: one thread)")
    pr.add_argument("--runs", type=int, default=1)
    pr.add_argument("--timeout-s", type=int, default=7200)
    pr.add_argument("--port", type=int, default=19354)
    pr.add_argument("--results", required=True)
    args = p.parse_args(argv)
    sys.exit({"run": cmd_run}[args.command](args))


if __name__ == "__main__":
    main()
