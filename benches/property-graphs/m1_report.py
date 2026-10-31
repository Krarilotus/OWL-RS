"""M1's tables from a pg-m1.sh result directory: per store (scale, variant, runs) and phase, the work
counters, the warm median and spread, the cold time, the time per probe, and `perf stat`'s counts per
probe above the `none` baseline. Not an LDBC benchmark result.

    python3 -I m1_report.py RESULTS_DIR [--markdown]
"""
from __future__ import annotations

import csv
import json
import re
import statistics
import sys
from pathlib import Path

EVENTS = ("cycles", "instructions", "cache-misses", "L1-dcache-load-misses", "dTLB-load-misses")


def perf(path: Path) -> dict[str, float]:
    """`perf stat -x,` counts by event; an unsupported or uncounted event is absent."""
    out = {}
    for row in csv.reader(line for line in path.read_text().splitlines() if line and not line.startswith("#")):
        if len(row) > 2 and row[0] not in ("<not supported>", "<not counted>"):
            try:
                out[row[2]] = float(row[0])
            except ValueError:
                pass
    return out


def rows(directory: Path) -> list[dict]:
    out = []
    for warm in sorted(directory.glob("m1-*-warm.json")):
        tag = re.match(r"m1-(.*)-warm\.json", warm.name).group(1)
        m = json.loads(warm.read_text())
        cold_file = directory / f"m1-{tag}-cold.json"
        cold = {p["phase"]: p["ns"][0] for p in json.loads(cold_file.read_text())["phases"]} if cold_file.exists() else {}
        none_file = directory / f"perf-{tag}-none.csv"
        base = perf(none_file) if none_file.exists() else {}
        reps = None
        env = (directory / "env.txt").read_text() if (directory / "env.txt").exists() else ""
        found = re.search(r"perf_reps=(\d+)", env)
        reps = int(found.group(1)) if found else 3
        for p in m["phases"]:
            ns = p["ns"]
            w = p["work"]
            unit = w["probes"] or w["hash_probes"] or w["decoded"] or 1
            median = statistics.median(ns)
            row = {
                "store": tag, "phase": p["phase"], "answer": p["answer"], "work": w, "unit": unit,
                "median_ms": median / 1e6, "min_ms": min(ns) / 1e6, "max_ms": max(ns) / 1e6,
                "spread": (max(ns) - min(ns)) / median if median else 0.0,
                "cold_ms": cold.get(p["phase"], float("nan")) / 1e6,
                "ns_per_unit": median / unit,
            }
            perf_file = directory / f"perf-{tag}-{p['phase']}.csv"
            if perf_file.exists() and base:
                counts = perf(perf_file)
                row["perf"] = {e: (counts[e] - base[e]) / reps / unit for e in EVENTS if e in counts and e in base}
            out.append(row)
    return out


def markdown(table: list[dict]) -> str:
    lines = ["| Store | Phase | Unit | Root searches | Warm median ms (spread) | Cold ms | ns/unit | cycles/unit | "
             "cache-misses/unit | L1d misses/unit |", "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|"]
    for r in table:
        p = r.get("perf", {})
        lines.append(
            f"| {r['store']} | {r['phase']} | {r['unit']:,} | {r['work']['root_searches']:,} | "
            f"{r['median_ms']:.2f} ({r['spread']:.0%}) | {r['cold_ms']:.2f} | {r['ns_per_unit']:.0f} | "
            f"{p.get('cycles', float('nan')):.0f} | {p.get('cache-misses', float('nan')):.2f} | "
            f"{p.get('L1-dcache-load-misses', float('nan')):.1f} |")
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    table = rows(Path(argv[1]))
    if "--markdown" in argv:
        print(markdown(table))
    else:
        print(json.dumps(table, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
