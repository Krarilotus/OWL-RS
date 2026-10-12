#!/usr/bin/env python3
"""NRESE's estimates and G-CARE's estimators on the same queries, by G-CARE's measure.

    python3 -I summarise.py --truth QUERIES-DIR --nrese ESTIMATES.tsv [--gcare DIR] [--json OUT]

QUERIES-DIR is SPARQL_DATA/queries/gcare-NAME (its truth.tsv); ESTIMATES.tsv is
estimates.py's output; DIR holds run-gcare.sh's NAME_METHOD_query_result.txt. Per estimator
and query topology: the q-errors of every trial (G-CARE's 30 per query for its sampling
estimators, one for the deterministic ones and NRESE), their 5th, 25th, 50th, 75th and 95th
percentiles (the paper's Figures 6 to 8), the share within 10x, the failures (a query an
estimator gave no estimate for, or an error), and the mean estimation time in ms.

A query an estimator failed on counts as a failure, never as a q-error: the percentiles are
over the queries it answered, so `n` and `failed` are read beside them.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path


def truth(directory: Path) -> dict[str, int]:
    out = {}
    for line in (directory / "truth.tsv").read_text(encoding="utf-8").splitlines():
        if line and not line.startswith("#"):
            name, value = line.split("\t")[:2]
            if value.strip().isdigit():
                out[name] = int(value)
    return out


def qerror(estimate: float, true: int) -> float:
    e, t = max(estimate, 1.0), max(float(true), 1.0)
    return max(e, t) / min(e, t)


def gcare_results(path: Path) -> dict[str, tuple[list[float], float]]:
    """query name -> (every trial's estimate, mean time in ms) from a query_result file."""
    out: dict[str, tuple[list[float], float]] = {}
    lines = [l for l in path.read_text(encoding="utf-8", errors="replace").splitlines() if l and not l.startswith("#")]
    for head, trials in zip(lines[0::2], lines[1::2]):
        parts = head.split()
        m = re.search(r"/([^/]+)/([^/]+)\.txt$", parts[0])
        if not m or len(parts) < 3:
            continue
        values = [float(v) for v in trials.replace(",", " ").split()[1:]]
        out[f"{m.group(1)}-{m.group(2)}"] = (values, float(parts[2]))
    return out


def stats(values: list[float]) -> dict:
    v = sorted(values)
    at = lambda q: v[min(len(v) - 1, round(q * (len(v) - 1)))]
    return {f"p{int(q * 100)}": round(at(q), 3) for q in (0.05, 0.25, 0.5, 0.75, 0.95)} | \
           {"within_10x": round(sum(1 for x in v if x <= 10) / len(v), 3)}


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--truth", type=Path, required=True)
    p.add_argument("--nrese", type=Path, required=True)
    p.add_argument("--gcare", type=Path)
    p.add_argument("--json", type=Path)
    args = p.parse_args(argv)
    true = truth(args.truth)
    dataset = args.truth.name.removeprefix("gcare-")
    estimators: dict[str, dict[str, tuple[list[float], float]]] = {"nrese": {}}
    for line in args.nrese.read_text(encoding="utf-8").splitlines()[1:]:
        name, _, _, estimate, _, _, ms = line.split("\t")
        if estimate != "-":
            estimators["nrese"][name] = ([float(estimate)], float(ms))
    if args.gcare:
        for path in sorted(args.gcare.glob(f"{dataset}_*_query_result.txt")):
            method = path.name[len(dataset) + 1:].split("_")[0]
            estimators[method] = gcare_results(path)
    report = {}
    for method, results in estimators.items():
        groups: dict[str, list[float]] = {}
        times = []
        for name, t in true.items():
            topology = name.split("-", 1)[0]
            if name not in results or not results[name][0]:
                groups.setdefault(f"{topology}:failed", []).append(1)
                continue
            trials, ms = results[name]
            times.append(ms)
            for estimate in trials:
                groups.setdefault(topology, []).append(qerror(estimate, t))
                groups.setdefault("all", []).append(qerror(estimate, t))
        entry = {}
        for group in sorted(g for g in groups if not g.endswith(":failed")):
            entry[group] = {"n": len(groups[group]), **stats(groups[group]),
                            "failed": len(groups.get(f"{group}:failed", []))}
        entry["all"]["failed"] = sum(len(v) for g, v in groups.items() if g.endswith(":failed"))
        entry["mean_ms"] = round(sum(times) / len(times), 3) if times else None
        report[method] = entry
    print(f"G-CARE {dataset}: q-error percentiles over all trials (p5 p25 p50 p75 p95), share within 10x, failed queries, mean ms")
    for method, entry in report.items():
        a = entry["all"]
        print(f"{method:<8} n={a['n']:<6} {a['p5']:>8} {a['p25']:>8} {a['p50']:>8} {a['p75']:>10} {a['p95']:>12}"
              f"  {a['within_10x']:>5}  failed={a['failed']:<4} {entry['mean_ms']} ms")
    if args.json:
        args.json.write_text(json.dumps({"dataset": dataset, "estimators": report}, indent=1), encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
