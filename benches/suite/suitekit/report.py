"""`suite.py report`: result files as Markdown tables, one per workload tier and regime.

What is complete, which timings count and how they are estimated is `summary.py`'s, the
same for every reader. A system with a result cache gets one line per cache mode (off,
on); the load, store and count figures (measured once per run, before the queries) appear
on each. "First" sums the queries' first executions on a fresh server, "Repeated" their
later executions; both over eligible (`ok`) timings only, each query as the median over
runs of its run medians, and only over the queries with a timing on that line ("n of m"
when some have none). Wrong answers, incomplete items (a failure or timeout in any
repetition) and items whose answer counts differ between systems are listed apart. Systems
whose results need the vendor's permission are left out unless --include-restricted (their
numbers stay on this machine: benches/competitors/README.md).
"""
from __future__ import annotations

import argparse
from collections import defaultdict
from pathlib import Path
from statistics import median

from .schema import read
from .summary import Pair, summarise


def med(per_run: dict[str, list[float]]) -> str:
    values = [v for vs in per_run.values() for v in vs]
    if not values:
        return "-"
    m = median(values)
    return f"{m:,.0f}" if m >= 100 else f"{m:,.1f}"


def total(items, first: bool) -> str:
    estimates = [(i.estimate_first() if first else i.estimate()) for i in items]
    have = [e[0] for e in estimates if e is not None]
    if not have:
        return "-"
    text = f"{sum(have):,.1f}"
    return text if len(have) == len(items) else f"{text} ({len(have)} of {len(items)})"


def line(name: str, pair: Pair, cache: str | None) -> str:
    steps = pair.steps
    store = {run: [v for v in vs] for run, vs in steps.get("store MiB", {}).items()}
    items = pair.items.get(cache, {}) if cache is not None else {}
    complete = sum(1 for i in items.values() if i.complete)
    wrong = sorted(n for n, i in items.items() if i.wrong)
    incomplete = sorted(n for n, i in items.items() if not i.complete and not i.wrong)
    disputed = sorted(n for n in items if n in pair.disputed)
    counts = {task: rows for task, rows in pair.counts}
    return (f"| {name} | {med(steps.get('load ms', {}))} | {med(steps.get('load peak MiB', {}))} | "
            f"{counts.get('load', '-')} | {counts.get('reason', '-')} | {med(store)} | "
            f"{med(steps.get('restart ms', {}))} | {med(steps.get('count ms', {}))} | {counts.get('count', '-')} | "
            f"{complete}/{len(items)} | {total(list(items.values()), True)} | {total(list(items.values()), False)} | "
            f"{', '.join(wrong) or '-'} | {', '.join(incomplete) or '-'} | {', '.join(disputed) or '-'} | "
            f"{med(steps.get('serve peak MiB', {}))} |")


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(prog="suite.py report", description=__doc__)
    p.add_argument("files", nargs="+", type=Path, help="results.csv files")
    p.add_argument("--include-restricted", action="store_true",
                   help="include systems whose results need the vendor's permission")
    args = p.parse_args(argv)
    rows = [r for f in args.files for r in read(f)]
    pairs = summarise(rows)
    restricted = {pair.system for pair in pairs.values() if pair.publish == "permission"}
    tables: dict[tuple, list[Pair]] = defaultdict(list)
    for pair in pairs.values():
        if pair.publish == "permission" and not args.include_restricted:
            continue
        tables[(pair.workload, pair.tier, pair.regime)].append(pair)
    for (workload, tier, regime), group in sorted(tables.items()):
        print(f"\n### {workload} {tier} ({regime})\n")
        print("| System | Load ms | Load peak MiB | Asserted | Inferred | Store MiB | Restart ms | Count ms | "
              "Statements | Queries complete | First, sum ms | Repeated, sum ms | Wrong | Incomplete | "
              "Disputed | Serve peak MiB |")
        print("|" + "---|" * 16)
        for pair in sorted(group, key=lambda p: p.system):
            if not pair.live:
                print(f"| {pair.system} | skipped: {pair.skipped_note[:120]} |" + " |" * 14)
                continue
            modes = sorted(m for m in pair.items if m != "-")
            if not modes:
                print(line(pair.system, pair, "-" if "-" in pair.items else None))
            for mode in modes:
                print(line(f"{pair.system} (cache {mode})", pair, mode))
    if restricted and not args.include_restricted:
        print(f"\n(left out, publication needs the vendor's permission: {', '.join(sorted(restricted))})")
    return 0
