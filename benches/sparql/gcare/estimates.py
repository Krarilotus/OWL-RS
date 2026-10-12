#!/usr/bin/env python3
"""The planner's cardinality estimates on G-CARE's queries, against their true cardinalities.

    python3 -I estimates.py --endpoint URL --queries DIR --out FILE.tsv [--repeat 3]

DIR is one of benches/competitors/queries/gcare-NAME (its .rq files and truth.tsv); the
endpoint is an NRESE server on gcare-NAME.nt. For each query the pattern is asked as
`SELECT *` with `explain=plan` (planned, never run), and the estimate of its basic graph
pattern (the `bgp` node; else the first node with one) is compared with the published true
cardinality: the q-error max(e, t) / min(e, t) with both at least 1 (Moerkotte et al., VLDB
2009; G-CARE's measure), and its direction (over, under, exact). The request's wall time is
the estimation time (planning plus HTTP), the median of --repeat requests.

FILE.tsv: query, topology (G-CARE's TOPOLOGY_SIZE group), truth, estimate, q-error,
direction, ms. A summary per topology and over all is printed: the 5th, 25th, 50th, 75th
and 95th percentiles of the q-error, as G-CARE's Figures 6 to 8 report them.
"""
from __future__ import annotations

import argparse
import json
import statistics
import sys
import time
import urllib.parse
import urllib.request
from pathlib import Path


def plan_estimate(endpoint: str, query: str) -> tuple[int | None, float]:
    data = urllib.parse.urlencode({"query": query, "explain": "plan"}).encode()
    started = time.perf_counter()
    with urllib.request.urlopen(urllib.request.Request(endpoint, data=data), timeout=120) as response:
        body = json.loads(response.read())
    ms = (time.perf_counter() - started) * 1000
    steps = body.get("steps") or []
    for step in steps:
        if step.get("operator") == "bgp" and step.get("estimated_rows") is not None:
            return int(step["estimated_rows"]), ms
    for step in steps:
        if step.get("estimated_rows") is not None:
            return int(step["estimated_rows"]), ms
    return None, ms


def percentiles(values: list[float]) -> str:
    v = sorted(values)
    at = lambda q: v[min(len(v) - 1, round(q * (len(v) - 1)))]
    return " ".join(f"p{int(q * 100)}={at(q):.3g}" for q in (0.05, 0.25, 0.5, 0.75, 0.95))


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--endpoint", required=True)
    p.add_argument("--queries", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--repeat", type=int, default=3)
    args = p.parse_args(argv)
    truth = {}
    for line in (args.queries / "truth.tsv").read_text(encoding="utf-8").splitlines():
        if line and not line.startswith("#"):
            name, value = line.split("\t")[:2]
            truth[name] = int(value) if value.strip().isdigit() else None
    rows, groups = [], {}
    for path in sorted(args.queries.glob("*.rq")):
        name = path.stem
        if truth.get(name) is None:
            continue
        text = path.read_text(encoding="utf-8").replace("SELECT (COUNT(*) AS ?count)", "SELECT *")
        estimates, times = [], []
        for _ in range(max(args.repeat, 1)):
            estimate, ms = plan_estimate(args.endpoint, text)
            estimates.append(estimate)
            times.append(ms)
        estimate = estimates[0]
        t = truth[name]
        topology = name.split("-", 1)[0] if "-" in name else "-"
        if estimate is None:
            rows.append([name, topology, str(t), "-", "-", "none", f"{statistics.median(times):.3f}"])
            continue
        e, tt = max(estimate, 1), max(t, 1)
        q = max(e, tt) / min(e, tt)
        direction = "exact" if e == tt else ("over" if e > tt else "under")
        rows.append([name, topology, str(t), str(estimate), f"{q:.6g}", direction, f"{statistics.median(times):.3f}"])
        groups.setdefault(topology, []).append(q)
        groups.setdefault("all", []).append(q)
    args.out.write_text("query\ttopology\ttruth\testimate\tqerror\tdirection\tms\n" +
                        "".join("\t".join(r) + "\n" for r in rows), encoding="utf-8")
    for group in sorted(groups, key=lambda g: (g == "all", g)):
        print(f"{group:<12} n={len(groups[group]):<5} {percentiles(groups[group])}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
