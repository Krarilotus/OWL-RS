#!/usr/bin/env python3
"""vector-knn-filtered's queries (benches/cases/core.toml: the generator's five query vectors,
k = 10, one kind in ten) through a perf_lab binary in variants, each variant's recall@10
against its truth and its time: the filtered search (the case), the same with a wider beam
and exact, and the unfiltered search as the graph's own recall. For a host without Docker,
where fast.py doesn't run (Phuoc-Yu).

    knn_filtered.py PERF_LAB DATA.nt EXPECT.json OUT [--runs 20] [perf_lab args...]

DATA.nt and EXPECT.json are `perf_lab generate vectors items=200000 dim=64 kinds=10` output.
Prints a table and writes OUT/summary.json.
"""
import argparse
import json
import re
import subprocess
from pathlib import Path

PREFIX = "PREFIX e: <http://example.org/fast/>\nPREFIX nrv: <urn:nrese:vector:>\n"
# name -> (filtered, the search's options); the first exact variant of each kind is the truth
# for the unfiltered ones, the generator's answer for the filtered ones.
VARIANTS = {"filtered": (True, ""), "filtered-256": (True, " ; nrv:searchBudget 256"),
            "filtered-exact": (True, " ; nrv:exact true"), "unfiltered": (False, ""),
            "unfiltered-exact": (False, " ; nrv:exact true")}


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("perf_lab")
    p.add_argument("data")
    p.add_argument("expect", type=Path)
    p.add_argument("out", type=Path)
    p.add_argument("--runs", type=int, default=20)
    args, extra = p.parse_known_args()
    expect = json.loads(args.expect.read_text(encoding="utf-8"))
    expect = expect.get("expect", expect)
    found, p50 = {}, {}
    for name, (filtered, options) in VARIANTS.items():
        pattern = "?item e:kind e:k3 ; e:embedding ?v ." if filtered else "?item e:embedding ?v ."
        queries = args.out / "queries" / name
        results = args.out / "results" / name
        queries.mkdir(parents=True, exist_ok=True)
        results.mkdir(parents=True, exist_ok=True)
        for q in range(5):
            (queries / f"knn-{q}.rq").write_text(
                PREFIX + f"SELECT ?item WHERE {{ {pattern}\n  SERVICE nrv:search {{ ?v nrv:near "
                f"\"{expect[f'query_{q}']}\"^^nrv:vector ; nrv:k 10{options} ; nrv:rank ?r }} }}\nORDER BY ?r\n",
                encoding="utf-8")
        report = args.out / f"{name}.json"
        subprocess.run([args.perf_lab, "--load", args.data, "--queries", str(queries), "--results", str(results),
                        "--runs", str(args.runs), "--json", str(report), *extra], check=True,
                       stdout=open(args.out / f"{name}.log", "w"), stderr=subprocess.STDOUT)
        p50[name] = sum(q.get("p50_ms") or 0 for q in json.loads(report.read_text(encoding="utf-8"))["queries"])
        found[name] = [{m.group(1) for m in re.finditer(r"(item\d+)>", (results / f"knn-{q}.out").read_text())}
                       for q in range(5)]
    summary = {}
    for name, (filtered, _) in VARIANTS.items():
        truth = [set(expect[f"nearest_{q}"]) for q in range(5)] if filtered else found["unfiltered-exact"]
        recall = sum(len(found[name][q] & truth[q]) for q in range(5)) / 50
        summary[name] = {"recall": recall, "sum_p50_ms": round(p50[name], 3)}
        print(f"{name:<18} recall@10 {recall:.2f}   sum p50 {p50[name]:.3f} ms")
    (args.out / "summary.json").write_text(json.dumps(summary, indent=1), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
