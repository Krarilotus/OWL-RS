#!/usr/bin/env python3
"""What the perf lab's timing loop adds to microsecond queries (benches/PROTOCOL.md §5, "No
thread started in a timed window"): perf labs of two or more revisions, each with the canceller thread started
per run ("spawn") and with the watchdog ("watchdog"), run interleaved in fresh processes on the
same data and queries. Per query: each variant's process p50s, their noise (sd of the logs), and
between the revisions the gap of medians with each loop; the bias the loop adds to a revision
(spawn over watchdog) and to the gap.

    timing-loop.py --bin NAME=PATH ... --pair REV:SPAWN_NAME:WATCHDOG_NAME ... --load FILE --queries DIR
                   [--processes 8] [--runs 300] [--cap-gb 20] [--micro-ms 0.05] --out DIR

A process is `perf_lab --load FILE --queries DIR --runs R --warmup 3 --json`, in its own scope
when systemd-run is there (FLEET_SLICE). The binaries run in A-B-...-B-A order, process by process.
"""
import argparse
import json
import math
import os
import shutil
import statistics as st
import subprocess
from pathlib import Path


def scoped(unit, cap_gb, command):
    if not shutil.which("systemd-run"):
        return command
    placed = [f"--slice={os.environ['FLEET_SLICE']}"] if os.environ.get("FLEET_SLICE") else []
    return ["systemd-run", "--user", "--scope", "-q", *placed, f"--unit={unit}", "-p", f"MemoryMax={cap_gb}G",
            "-p", "MemorySwapMax=0", *command]


def run(path, args, out, n):
    report = out / f"{n}.json"
    command = [path, *sum((["--load", f] for f in args.load), []), "--queries", args.queries,
               "--runs", str(args.runs), "--warmup", "3", "--json", str(report)]
    done = subprocess.run(scoped(f"timing-loop-{os.getpid()}-{n}", args.cap_gb, command), capture_output=True,
                          text=True)
    if done.returncode != 0 or not report.is_file():
        raise SystemExit(f"{path} failed: {done.stderr[-500:]}")
    return {q["name"]: q["p50_ms"] for q in json.loads(report.read_text())["queries"] if "error" not in q}


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--bin", action="append", required=True, metavar="NAME=PATH")
    p.add_argument("--pair", action="append", required=True, metavar="REV:SPAWN:WATCHDOG")
    p.add_argument("--load", action="append", required=True)
    p.add_argument("--queries", required=True)
    p.add_argument("--processes", type=int, default=8)
    p.add_argument("--runs", type=int, default=300)
    p.add_argument("--cap-gb", type=float, default=20)
    p.add_argument("--micro-ms", type=float, default=0.05)
    p.add_argument("--out", type=Path, required=True)
    args = p.parse_args()
    bins = dict(b.split("=", 1) for b in args.bin)
    args.out.mkdir(parents=True, exist_ok=True)
    p50s = {name: {} for name in bins}
    n = 0
    for k in range(args.processes):
        for name in (list(bins) if k % 2 == 0 else list(bins)[::-1]):
            n += 1
            for q, v in run(bins[name], args, args.out, n).items():
                p50s[name].setdefault(q, []).append(v)
    pairs = [tuple(x.split(":")) for x in args.pair]
    queries = sorted(set.intersection(*(set(v) for v in p50s.values())))
    rows = []
    for q in queries:
        row = {"query": q}
        for name in bins:
            vals = p50s[name][q]
            row[name] = {"median_ms": st.median(vals), "noise": st.stdev(math.log(v) for v in vals) if min(vals) > 0
                         else None, "p50s": vals}
        for rev, spawn, wd in pairs:
            row[f"{rev} loop bias us"] = (row[spawn]["median_ms"] - row[wd]["median_ms"]) * 1000
        if len(pairs) == 2:
            (r1, s1, w1), (r2, s2, w2) = pairs
            row["gap us spawn"] = (row[s2]["median_ms"] - row[s1]["median_ms"]) * 1000
            row["gap us watchdog"] = (row[w2]["median_ms"] - row[w1]["median_ms"]) * 1000
        rows.append(row)
    micro = [r for r in rows if max(r[n]["median_ms"] for n in bins) < args.micro_ms]
    summary = {"queries": len(rows), "micro": len(micro)}
    for name in bins:
        noises = [r[name]["noise"] for r in micro if r[name]["noise"] is not None]
        summary[f"{name} micro noise median"] = st.median(noises) if noises else None
    for rev, spawn, wd in pairs:
        summary[f"{rev} loop bias us, micro median"] = st.median(r[f"{rev} loop bias us"] for r in micro) if micro else None
    if len(pairs) == 2 and micro:
        summary["gap us spawn, micro median"] = st.median(r["gap us spawn"] for r in micro)
        summary["gap us watchdog, micro median"] = st.median(r["gap us watchdog"] for r in micro)
    (args.out / "timing-loop.json").write_text(json.dumps({"summary": summary, "rows": rows}, indent=1))
    print(json.dumps(summary, indent=1))
    for r in micro:
        print(r["query"], " ".join(f"{name} {r[name]['median_ms'] * 1000:.2f}us±{(r[name]['noise'] or 0):.3f}"
                                   for name in bins),
              " ".join(f"{k} {v:.2f}" for k, v in r.items() if k.endswith("us") or k.startswith("gap")))


if __name__ == "__main__":
    main()
