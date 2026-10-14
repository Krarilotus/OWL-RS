#!/usr/bin/env python3
"""A query's swing at two or more revisions (the owner, 14 Oct 2026: q02-born-in-berlin on dbpedia-core runs
3.3 or 7.5 ms on the candidate, 2.0-2.1 on main; an unstable plan or schedule is a defect even where
the minimum holds). Per revision: N processes of its perf_lab, each loading the data, timing the
query RUNS times (after a warm-up) and then EXPLAINing it once (an execution of its own, with every
operator's time). Out: raw run distributions only when every process supplies all samples;
otherwise the distribution of independent process p50s, with min/max spans separately. EXPLAIN shapes
(operators, nesting and patterns, without counts and times) with the EXPLAIN's own time: whether
the fast and the slow executions run different plans.

    python3 benches/probes/query-swing.py --bin NAME=PATH --bin NAME=PATH --load FILE --queries DIR --query NAME
                     [--processes 30] [--runs 5] [--cap-gb 20] --out DIR
"""
import argparse
import json
import os
import re
import shutil
import statistics as st
import subprocess
import tempfile
from collections import Counter, defaultdict
from pathlib import Path

PLAN = re.compile(r"^    (\s*)(\S+) (.*?)  \[est (\S+) rows (\S+) ([\d.]+) ms\]$")


def scoped(unit, cap_gb, command):
    if not shutil.which("systemd-run"):
        return command
    placed = [f"--slice={os.environ['FLEET_SLICE']}"] if os.environ.get("FLEET_SLICE") else []
    return ["systemd-run", "--user", "--scope", "-q", *placed, f"--unit={unit}", "-p", f"MemoryMax={cap_gb}G",
            "-p", "MemorySwapMax=0", *command]


def plan_of(stdout):
    """The EXPLAIN printed after the query's row: its shape and the top operator's time."""
    shape, top = [], None
    for line in stdout.splitlines():
        m = PLAN.match(line)
        if not m:
            continue
        indent, op, detail, _est, _rows, ms = m.groups()
        pattern = detail.split(" | ")[0]
        shape.append(f"{len(indent) // 2}:{op} {pattern}")
        if top is None:
            top = float(ms)
    return (tuple(shape) or ("(no plan printed)",)), top


def distribution(queries, runs):
    """One observation type for the whole revision: complete raw runs, else process medians."""
    if not queries:
        return "no completed processes"
    raw = runs > 0 and all(isinstance(q.get("samples_ms"), list) and len(q["samples_ms"]) == runs for q in queries)
    flat = sorted(t for q in queries for t in (q["samples_ms"] if raw else [q["p50_ms"]]))
    percentiles = st.quantiles(flat, n=20) if len(flat) > 1 else [flat[0]] * 19
    kind = "timed raw samples" if raw else "independent process p50s"
    low, high = min(q["min_ms"] for q in queries), max(q["max_ms"] for q in queries)
    wide = sum(q["max_ms"] > 1.5 * q["min_ms"] for q in queries)
    return (f"{kind} n={len(flat)} ({len(queries)} processes): min {flat[0]:.3f}, p5 {percentiles[0]:.3f}, "
            f"p50 {st.median(flat):.3f}, p95 {percentiles[-1]:.3f}, max {flat[-1]:.3f} ms; "
            f"reported min/max span {low:.3f}-{high:.3f} ms; "
            f"processes whose reported min/max span > 1.5x: {wide} of {len(queries)}")


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--bin", action="append", required=True)
    p.add_argument("--load", required=True)
    p.add_argument("--queries", required=True)
    p.add_argument("--query", required=True)
    p.add_argument("--processes", type=int, default=30)
    p.add_argument("--runs", type=int, default=5)
    p.add_argument("--cap-gb", type=int, default=20)
    p.add_argument("--out", type=Path, required=True)
    a = p.parse_args()
    a.out.mkdir(parents=True, exist_ok=True)
    attempt = Path(tempfile.mkdtemp(prefix="swing-", dir=a.out))
    bins = dict(b.split("=", 1) for b in a.bin)
    data = defaultdict(lambda: {"runs": [], "plans": []})
    # Interleaved: process k of every revision before process k+1, so drift falls on both alike.
    for k in range(1, a.processes + 1):
        for name, exe in bins.items():
            report = attempt / f"{name}-{k}.json"
            unit = f"swing-{os.getpid()}-{name}-{k}"
            done = subprocess.run(scoped(unit, a.cap_gb, [exe, "--load", a.load, "--queries", a.queries, "--only", a.query,
                                                         "--runs", str(a.runs), "--warmup", "1", "--explain",
                                                         "--json", str(report)]), capture_output=True, text=True)
            if shutil.which("systemctl"):
                subprocess.run(["systemctl", "--user", "kill", "--signal=KILL", f"{unit}.scope"], capture_output=True)
            log = attempt / f"{name}-{k}.log"
            log.write_text(done.stdout + done.stderr, encoding="utf-8")
            if done.returncode != 0 or not report.is_file():
                raise SystemExit(f"{name} process {k}: exit {done.returncode}, fresh report {report.is_file()}; log {log}")
            q = [x for x in json.loads(report.read_text())["queries"] if x["name"] == a.query][0]
            shape, top = plan_of(done.stdout)
            data[name]["runs"].append(q)
            data[name]["plans"].append((shape, top, q["p50_ms"], q["min_ms"], q["max_ms"]))
    lines = [f"{a.query}: {a.processes} processes per revision, {a.runs} timed runs each (after one warm-up), "
             "then one EXPLAIN execution", ""]
    for name in bins:
        lines.append(f"{name}: {distribution(data[name]['runs'], a.runs)}")
        shapes = Counter(s for s, *_ in data[name]["plans"])
        lines.append(f"  EXPLAIN plan shapes: {len(shapes)}")
        for i, (shape, n) in enumerate(shapes.most_common(), start=1):
            tops = [t for s, t, *_ in data[name]["plans"] if s == shape and t is not None]
            p50s = [p for s, _, p, *_ in data[name]["plans"] if s == shape]
            explained = f"EXPLAIN top {min(tops):.2f}-{max(tops):.2f} ms; " if tops else ""
            lines.append(f"  shape {i}: {n} processes; {explained}their timed p50 {min(p50s):.2f}-{max(p50s):.2f} ms")
            lines += [f"      {step}" for step in shape]
    text = "\n".join(lines)
    print(text)
    (a.out / "swing.txt").write_text(text + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
