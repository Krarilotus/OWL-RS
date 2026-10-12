#!/usr/bin/env python3
"""ERA-SHACL (github.com/oeg-upm/ERA-SHACL-Benchmark): SHACL validation of the European
railway knowledge graph, three subsets x three shape sets, each engine in-memory in a
container of its own (benches/breadth/README.md, "ERA-SHACL").

    run.py images                      build the engine images (NRESE from this worktree)
    run.py run [--subsets ES,FR] [--shapes tds,core,era] [--engines ...] [--runs 3]
               [--memory 28g] [--heap 24g] [--label NAME]
    run.py check RESULTS_DIR [--reference topbraid]

Each step is one container: load, validate, write the report. The times are the engine's
own (upstream's contract: its load and validation timers, in seconds); the peak memory is
the container's (`docker stats`, sampled). Steps are interleaved over the repetitions
(PROTOCOL.md §4). Rows go to RESULTS_DIR/results.csv in the suite's schema: `load` (ms,
peak, statements) and `validate` (item = the shape set; ms; rows = validation results).
`check` compares every report with the reference engine's (validation results as
(focus node, path, value, constraint component, severity) bags; blank-node shapes and
messages differ between engines by design and are left out) and marks differing steps
`wrong` (the rows before the check stay in results.raw.csv).
"""
from __future__ import annotations

import argparse
import collections
import csv
import hashlib
import json
import re
import shutil
import time
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
import kit  # noqa: E402

COMMIT = "fe109c940bf8f1b631f0b703bd7adfad07c570e5"
REPO = "oeg-upm/ERA-SHACL-Benchmark"
# Engine -> (upstream directory, how its image is built, version as upstream pins it).
ENGINES = {
    "nrese": (None, "nrese", "this worktree"),
    "jena": ("engines/jena", "java", "jena-shacl 5.4.0"),
    "topbraid": ("engines/topbraid", "java", "TopBraid SHACL 1.4.4"),
    "rdf4j": ("engines/rdf4j", "java", "rdf4j-shacl 5.1.3"),
    "corese": ("engines/corese", "java", "corese-core 4.6.3"),
    "pyshacl": ("engines/pyshacl", "upstream", "pyshacl 0.30.1, rdflib 7.1.3"),
    "maplib": ("engines/maplib", "upstream", "maplib 0.15.15"),
}
SYSTEM = {"nrese": "nrese", "jena": "jena", "rdf4j": "rdf4j"}  # systems.toml keys where one exists
DATA = kit.DATA / "era-shacl"


def tag(engine: str) -> str:
    return f"nrese-bench/era-{engine}:{COMMIT[:7]}"


def images(args) -> int:
    src = kit.checkout(REPO, COMMIT)
    for engine in args.engines.split(","):
        directory, how, _ = ENGINES[engine]
        if how == "nrese":
            made = kit.build_nrese([["-p", "nrese-store", "--example", "perf_lab"]])
            context = kit.SCRATCH / "era-nrese-image"
            shutil.rmtree(context, ignore_errors=True)
            context.mkdir(parents=True)
            shutil.copy(kit.BIN / "perf_lab", context / "perf_lab")
            shutil.copy(HERE / "nrese-era.sh", context / "nrese-era.sh")
            kit.image(tag(engine), HERE / "nrese.Dockerfile", context)
            print(f"perf_lab {made['perf_lab']}")
        elif how == "java":
            kit.image(tag(engine), HERE / "java.Dockerfile", src / directory)
        else:
            kit.image(tag(engine), src / directory / "Dockerfile", src / directory)
        print(f"{engine}: {tag(engine)}")
    return 0


def parse(log: str) -> dict:
    found = {}
    for key, pattern in (("load_s", r"Load time:\s*([0-9.]+)"), ("validate_s", r"Validation time:\s*([0-9.]+)"),
                         ("statements", r"Data graph size:\s*([0-9]+)")):
        m = re.search(pattern, log)
        if m:
            found[key] = float(m.group(1))
    return found


def steps(args) -> int:
    subsets, shapes, engines = (args.subsets.split(","), args.shapes.split(","), args.engines.split(","))
    out = Path(args.results or kit.RESULTS / args.label).resolve()
    (out / "reports").mkdir(parents=True, exist_ok=True)
    (out / "logs").mkdir(exist_ok=True)
    writer = kit.Writer(out / "results.csv", echo=print)
    runtime = kit.DockerRuntime(kit.SCRATCH, port=0)
    pairs = [(d, s, e) for d in subsets for s in shapes for e in engines]
    plan = kit.interleaved(pairs, args.runs)
    entry = kit.stamp(out, sys.argv, [tag(e) for e in engines], [DATA / f"{d}.ttl" for d in subsets],
                      {"runs": args.runs, "memory": args.memory, "heap": args.heap, "timeout_s": args.timeout_s,
                       "scenario": f"matched: every engine in one container capped at {args.memory} without swap, "
                                   f"all host CPUs; JVM heap {args.heap}"})
    (out / "manifest.json").write_text(json.dumps({"invocations": [entry]}, indent=1), encoding="utf-8")
    date = entry["started"][:16]
    try:
        for rep, (subset, shape, engine) in plan:
            name = f"{engine}-{subset}-{shape}-r{rep}"
            report = f"/reports/{name}.ttl"
            env = {"JAVA_OPTS": f"-Xms1g -Xmx{args.heap}"} if ENGINES[engine][1] == "java" else {}
            spec = kit.Spec(name=f"nrese-suite-era-{name}".lower(), image=tag(engine), entrypoint=True,
                            command=[f"/data/{subset}.ttl", f"/shapes/{shape}_shapes.ttl", report],
                            env=env, memory=args.memory,
                            mounts=[kit.Mount(str(DATA), "/data"), kit.Mount(str(DATA / "shapes"), "/shapes"),
                                    kit.Mount(str(out / "reports"), "/reports", readonly=False)])
            while True:
                kit.wait_until_free()
                started = time.time()
                measured = runtime.run(spec, out / "logs" / f"{name}.log", args.timeout_s)
                if not kit.retreated_since(started):
                    break
                print(f"{name}: the host's owners came back during the step; it runs again", flush=True)
            got = parse((out / "logs" / f"{name}.log").read_text(encoding="utf-8", errors="replace"))
            status = "timeout" if measured.timed_out else ("ok" if measured.ok and "validate_s" in got else "failed")
            base = dict(date=date, host=entry["machine"].get("host", ""), runtime="docker",
                        system=SYSTEM.get(engine, engine), version=ENGINES[engine][2], publish="free",
                        workload="era-shacl", tier=subset, regime="none", run=rep)
            note = f"container peak over load and validation (docker stats, sampled); exit {measured.rc}"
            writer.write(kit.Result(**base, task="load", status=status,
                                    ms=got["load_s"] * 1000 if "load_s" in got else "",
                                    peak_mib=measured.peak_mib or "", rows=int(got.get("statements", 0)) or "",
                                    note=note))
            writer.write(kit.Result(**base, task="validate", item=shape, status=status,
                                    ms=got["validate_s"] * 1000 if "validate_s" in got else "",
                                    peak_mib=measured.peak_mib or "", rows=results_in(out / "reports" / f"{name}.ttl"),
                                    note=f"wall {measured.ms / 1000:.1f} s for the container"))
    finally:
        runtime.cleanup()
    return 0


# --- the reports -----------------------------------------------------------------------------

SH = "http://www.w3.org/ns/shacl#"
RDF = "http://www.w3.org/1999/02/22-rdf-syntax-ns#"
RDF_TYPE = RDF + "type"
TERM = re.compile(r'(<[^>]*>|_:\S+|"(?:[^"\\]|\\.)*"(?:@[A-Za-z0-9-]+|\^\^<[^>]*>)?)')


def triples(path: Path):
    """N-Triples of a report: engines write Turtle, converted once with Jena's riot."""
    nt = path.with_suffix(".nt")
    if not nt.exists() or nt.stat().st_mtime < path.stat().st_mtime:
        kit.run(["docker", "run", "--rm", "-v", f"{path.parent}:/r", "--entrypoint", "riot",
                 "nrese-bench/jena:6.2.0", "--output=nt", f"/r/{path.name}"], stdout=open(nt, "w"))
    with open(nt, encoding="utf-8") as f:
        for line in f:
            terms = TERM.findall(line)
            if len(terms) == 3:
                yield terms


def results_in(path: Path) -> int | str:
    """How many sh:ValidationResult a report has ('' if there's none to read)."""
    if not path.exists():
        return ""
    try:
        return sum(1 for s, p, o in triples(path) if p == f"<{RDF_TYPE}>" and o == f"<{SH}ValidationResult>")
    except Exception:  # an unreadable report is the check's finding, not the run's
        return ""


def canonical(path: Path) -> collections.Counter:
    """The report's results as a bag of keys: focus node, path (blank-node paths spelled
    out), value (blank nodes as one placeholder), constraint component, severity."""
    by: dict[str, dict[str, list[str]]] = collections.defaultdict(lambda: collections.defaultdict(list))
    kinds = set()
    for s, p, o in triples(path):
        by[s][p].append(o)
        if p == f"<{RDF_TYPE}>" and o == f"<{SH}ValidationResult>":
            kinds.add(s)

    def spell(node: str, depth: int = 0) -> str:
        if not node.startswith("_:") or depth > 8:
            return node if not node.startswith("_:") else "_:"
        props = by.get(node, {})
        if f"<{RDF}first>" in props:  # an RDF list (sequence path)
            items, cur = [], node
            while cur.startswith("_:") and len(items) < 64:
                items.append(spell(by[cur].get(f"<{RDF}first>", ["?"])[0], depth + 1))
                cur = by[cur].get(f"<{RDF}rest>", ["nil"])[0]
            return "(" + " ".join(items) + ")"
        return "[" + " ".join(f"{k} {spell(v[0], depth + 1)}" for k, v in sorted(props.items())) + "]"

    bag = collections.Counter()
    for r in kinds:
        props = by[r]
        key = "|".join(spell(props.get(f"<{SH}{k}>", [""])[0]) for k in
                       ("focusNode", "resultPath", "value", "sourceConstraintComponent", "resultSeverity"))
        bag[hashlib.sha1(key.encode()).digest()] += 1
    return bag


def check(args) -> int:
    out = Path(args.results).resolve()
    rows = list(csv.DictReader(open(out / "results.csv", encoding="utf-8")))
    reports = out / "reports"
    verdicts, reference_bags = {}, {}
    for row in rows:
        if row["task"] != "validate" or row["status"] != "ok":
            continue
        engine = next(e for e in ENGINES if SYSTEM.get(e, e) == row["system"])
        name = f"{engine}-{row['tier']}-{row['item']}-r{row['run']}"
        ref = f"{args.reference}-{row['tier']}-{row['item']}"
        if ref not in reference_bags:
            candidates = sorted(reports.glob(f"{ref}-r*.ttl"))
            reference_bags[ref] = canonical(candidates[0]) if candidates else None
        if engine == args.reference:
            verdicts[name] = ("reference", "")
            continue
        theirs, mine = reference_bags[ref], canonical(reports / f"{name}.ttl")
        if theirs is None:
            verdicts[name] = ("unchecked", f"no {args.reference} report")
        elif mine == theirs:
            verdicts[name] = ("same", f"{sum(mine.values())} results")
        else:
            missing, extra = sum((theirs - mine).values()), sum((mine - theirs).values())
            verdicts[name] = ("wrong", f"against {args.reference}: {missing} results missing, {extra} extra")
    shutil.copy(out / "results.csv", out / "results.raw.csv")
    with open(out / "results.csv", "w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, rows[0].keys())
        w.writeheader()
        for row in rows:
            engine = next(e for e in ENGINES if SYSTEM.get(e, e) == row["system"])
            v = verdicts.get(f"{engine}-{row['tier']}-{row['item']}-r{row['run']}")
            if row["task"] == "validate" and v and v[0] == "wrong":
                row["status"], row["note"] = "wrong", v[1]
            w.writerow(row)
    for name, (verdict, note) in sorted(verdicts.items()):
        print(f"{name:<32} {verdict:<10} {note}")
    return 0


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    i = sub.add_parser("images")
    i.add_argument("--engines", default=",".join(ENGINES))
    r = sub.add_parser("run")
    r.add_argument("--subsets", default="ES,FR,ERA")
    r.add_argument("--shapes", default="tds,core,era")
    r.add_argument("--engines", default="nrese,jena,topbraid,rdf4j,pyshacl,maplib")
    r.add_argument("--runs", type=int, default=3)
    r.add_argument("--memory", default="28g")
    r.add_argument("--heap", default="24g")
    r.add_argument("--timeout-s", type=float, default=4 * 3600)
    r.add_argument("--label", default="era-shacl")
    r.add_argument("--results")
    c = sub.add_parser("check")
    c.add_argument("results")
    c.add_argument("--reference", default="topbraid")
    args = p.parse_args(argv)
    return {"images": images, "run": steps, "check": check}[args.command](args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
