"""Per workload tier and system: load ms, serve peak, query sum (median per query of the
measured repeats, cache off and on), from a suite results CSV. Prints locally only.

Usage: python benches/suite/summarise.py <results.csv>"""
import csv
import statistics
import sys
from collections import defaultdict

path = sys.argv[1]
rows = list(csv.DictReader(open(path, encoding="utf-8")))
load = {}
peak = {}
queries = defaultdict(list)  # (wl, tier, system, cache, item) -> [ms]
status = defaultdict(set)
for r in rows:
    key = (r["workload"], r["tier"], r["system"])
    if r["task"] == "load":
        load[key] = (r["status"], r["ms"], r["peak_mib"])
    elif r["task"] == "serve":
        peak[key] = r["peak_mib"]
    elif r["task"] == "query":
        if r["status"] != "ok":
            status[key + (r["cache"],)].add(r["item"] + ":" + r["status"])
            continue
        if r["repeat"] in ("0", "") or not r["ms"]:
            continue
        queries[key + (r["cache"], r["item"])].append(float(r["ms"]))

sums = defaultdict(dict)
for (wl, tier, system, cache, item), values in queries.items():
    sums[(wl, tier, cache)].setdefault(system, {})[item] = statistics.median(values)

for (wl, tier, cache), per_system in sorted(sums.items()):
    items = sorted({i for s in per_system.values() for i in s})
    print(f"\n== {wl} {tier} cache={cache}")
    header = "query".ljust(28) + "".join(s[:12].rjust(13) for s in sorted(per_system))
    print(header)
    for item in items:
        line = item[:27].ljust(28)
        for s in sorted(per_system):
            v = per_system[s].get(item)
            line += (f"{v:13.2f}" if v is not None else "            -")
        print(line)
    line = "SUM".ljust(28)
    for s in sorted(per_system):
        line += f"{sum(per_system[s].values()):13.1f}"
    print(line)
    for s in sorted(per_system):
        failed = status.get((wl, tier, s, cache))
        if failed:
            print(f"   {s} not ok: {sorted(failed)[:6]}")
print("\n== loads (status, ms, peak MiB) and serve peaks")
for key in sorted(load):
    print(key, load[key], "serve", peak.get(key))
