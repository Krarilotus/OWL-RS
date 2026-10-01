"""`suite.py report`: result files as Markdown tables, one per workload tier and regime.

Medians over the repetitions. A system with a result cache gets one line per cache mode
(off, on). "First" is the sum of the queries' first executions on a fresh server (their
medians over the repetitions); "Repeated" the sum of the medians of the later runs. Systems whose results need the vendor's permission are left
out unless --include-restricted (their numbers stay on this machine: benches/competitors/
README.md).
"""
from __future__ import annotations

import argparse
from collections import defaultdict
from pathlib import Path
from statistics import median

from .schema import read


def number(text: str) -> float | None:
    try:
        return float(text)
    except (TypeError, ValueError):
        return None


def med(values: list[float]) -> str:
    values = [v for v in values if v is not None]
    if not values:
        return "-"
    m = median(values)
    return f"{m:,.0f}" if m >= 100 else f"{m:,.1f}"


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(prog="suite.py report", description=__doc__)
    p.add_argument("files", nargs="+", type=Path, help="results.csv files")
    p.add_argument("--include-restricted", action="store_true",
                   help="include systems whose results need the vendor's permission")
    args = p.parse_args(argv)
    rows = [r for f in args.files for r in read(f)]
    restricted = {r["system"] for r in rows if r["publish"] == "permission"}
    if not args.include_restricted:
        rows = [r for r in rows if r["publish"] != "permission"]
    groups: dict[tuple, list[dict]] = defaultdict(list)
    for r in rows:
        groups[(r["workload"], r["tier"], r["regime"])].append(r)
    for (workload, tier, regime), group in sorted(groups.items()):
        print(f"\n### {workload} {tier} ({regime})\n")
        print("| System | Load ms | Load peak MiB | Asserted | Inferred | Store MiB | Restart ms | Count ms | "
              "Statements | Queries ok | First, sum ms | Repeated, sum of medians ms | Wrong | Serve peak MiB |")
        print("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
        by_system: dict[str, list[dict]] = defaultdict(list)
        for r in group:
            by_system[r["system"]].append(r)
        # A system with a result cache: one line per mode; the load, store and count
        # figures (measured once per run, before the queries) appear on each.
        lines: dict[str, list[dict]] = {}
        for system, rs in by_system.items():
            modes = sorted({r.get("cache") or "-" for r in rs if r["task"] == "query"} - {"-"})
            if not modes:
                lines[system] = rs
            for mode in modes:
                lines[f"{system} (cache {mode})"] = [
                    r for r in rs if r["task"] != "query" and r["task"] != "serve"
                    or (r.get("cache") or "-") in (mode, "-")]
        for system, rs in sorted(lines.items()):
            def of(task, key="ms"):
                return [number(r[key]) for r in rs if r["task"] == task and r["status"] == "ok"]
            if all(r["status"] == "skipped" for r in rs):
                print(f"| {system} | skipped: {rs[0]['note']} |" + " |" * 12)
                continue
            queries = [r for r in rs if r["task"] == "query"]
            per_query: dict[str, list[float]] = defaultdict(list)
            first: dict[str, list[float]] = defaultdict(list)
            for r in queries:
                if r["status"] in ("ok", "wrong") and number(r["ms"]) is not None:
                    (first if r["repeat"] == "0" else per_query)[r["item"]].append(number(r["ms"]))
            items = {r["item"] for r in queries}
            ok_items = {r["item"] for r in queries if r["status"] == "ok"}
            wrong = sorted({r["item"] for r in queries if r["status"] == "wrong"})
            total = sum(median(v) for v in per_query.values()) if per_query else None
            first_total = sum(median(v) for v in first.values()) if first else None
            store = [b / 1048576 for b in of("size", "bytes") if b is not None]
            print(f"| {system} | {med(of('load'))} | {med(of('load', 'peak_mib'))} | {med(of('load', 'rows'))} | "
                  f"{med(of('reason', 'rows'))} | {med(store)} | {med(of('restart'))} | {med(of('count'))} | "
                  f"{med(of('count', 'rows'))} | {len(ok_items)}/{len(items)} | "
                  f"{'-' if first_total is None else f'{first_total:,.1f}'} | {'-' if total is None else f'{total:,.1f}'} | {', '.join(wrong) or '-'} | "
                  f"{med(of('serve', 'peak_mib'))} |")
    if restricted and not args.include_restricted:
        print(f"\n(left out, publication needs the vendor's permission: {', '.join(sorted(restricted))})")
    return 0
