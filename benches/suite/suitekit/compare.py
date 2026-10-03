"""`suite.py compare`: a run against a baseline run, pair by pair: what got slower, what got
faster, what answers differently. The regression check before a milestone.

    suite.py compare BASE NEW [--systems nrese] [--threshold 0.10] [--min-ms 2]

BASE and NEW are results directories or results.csv files (several, comma-separated).

Per query: the median of the measured repetitions within each run, then the median over the
runs. Its noise is the spread of the per-run medians relative to their median (with one
run: the interquartile range of the repetitions, or "?" with fewer than four). A change
counts only if the ratio exceeds
1 + max(threshold, noise of both sides) and the absolute difference exceeds --min-ms; the
rest is reported as within noise. Load, store size, restart, count and peak memory are
compared the same way over the runs. Differing answer or statement counts are listed first:
they are correctness findings, not performance ones. Restricted systems are left out unless
--include-restricted.
"""
from __future__ import annotations

import argparse
import math
from collections import defaultdict
from pathlib import Path
from statistics import median, quantiles

from .schema import read


def files(spec: str) -> list[Path]:
    out = []
    for part in spec.split(","):
        path = Path(part)
        out.append(path / "results.csv" if path.is_dir() else path)
    return out


def number(text) -> float | None:
    try:
        return float(text)
    except (TypeError, ValueError):
        return None


def summarise(values_per_run: dict[str, list[float]]) -> tuple[float, float] | None:
    """(median over runs of the per-run medians, relative noise)."""
    per_run = [median(v) for v in values_per_run.values() if v]
    if not per_run:
        return None
    m = median(per_run)
    if m <= 0:
        return m, 0.0
    if len(per_run) >= 2:
        return m, (max(per_run) - min(per_run)) / m
    only = next(v for v in values_per_run.values() if v)
    if len(only) >= 4:
        q = quantiles(only, n=4)
        return m, (q[2] - q[0]) / m
    return m, None  # too few values to estimate the noise


def collect(paths: list[Path], systems: set[str] | None, restricted: bool):
    """group -> {"steps": metric -> run -> [values], "queries": item -> run -> [ms],
    "first": item -> run -> [ms], "answers": item -> {rows}, "counts": {rows}}"""
    groups: dict[tuple, dict] = defaultdict(lambda: {
        "steps": defaultdict(lambda: defaultdict(list)), "queries": defaultdict(lambda: defaultdict(list)),
        "first": defaultdict(lambda: defaultdict(list)), "answers": defaultdict(set), "counts": set(),
        "status": defaultdict(set)})
    for path in paths:
        for r in read(path):
            if systems and r["system"] not in systems:
                continue
            if r["publish"] == "permission" and not restricted:
                continue
            if r["status"] == "skipped":
                continue
            cache = r.get("cache") or "-"
            key = (r["workload"], r["tier"], r["system"], r["regime"])
            run = f"{path}:{r['run']}"
            task = r["task"]
            ms = number(r["ms"])
            if task == "query":
                g = groups[key + (cache,)]
                g["status"][r["item"]].add(r["status"])
                if r["status"] in ("ok", "wrong") and ms is not None:
                    (g["first"] if str(r["repeat"]) == "0" else g["queries"])[r["item"]][run].append(ms)
                    if r["rows"] != "":
                        g["answers"][r["item"]].add(r["rows"])
                continue
            if r["status"] != "ok":
                continue
            # Steps are measured once per run, before the queries: they belong to every cache line.
            for g in [groups[key + ("-",)]]:
                if ms is not None and task in ("load", "reason", "restart", "count"):
                    g["steps"][f"{task} ms"][run].append(ms)
                if task in ("load", "serve") and number(r["peak_mib"]) is not None:
                    g["steps"][f"{task} peak MiB"][run].append(number(r["peak_mib"]))
                if task == "size" and number(r["bytes"]) is not None:
                    g["steps"]["store MiB"][run].append(number(r["bytes"]) / 2**20)
                if task in ("load", "reason", "count") and r["rows"] != "":
                    g["counts"].add((task, r["rows"]))
    return groups


def verdict(base, new, threshold: float, min_abs: float) -> str:
    (b, nb), (n, nn) = base, new
    if b <= 0:
        return "="
    ratio = n / b
    bound = max(threshold, (nb or 0) + (nn or 0))
    if ratio > 1 + bound and n - b > min_abs:
        return "slower"
    if ratio < 1 / (1 + bound) and b - n > min_abs:
        return "faster"
    return "="


def fmt(x: float) -> str:
    return f"{x:,.0f}" if abs(x) >= 100 else f"{x:,.1f}"


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(prog="suite.py compare", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("base", help="baseline: results directories or CSV files, comma-separated")
    p.add_argument("new", help="the run to check, same form")
    p.add_argument("--systems", help="comma-separated systems (default: all in both)")
    p.add_argument("--threshold", type=float, default=0.10, help="relative change that counts (default 0.10)")
    p.add_argument("--min-ms", type=float, default=2.0, help="absolute change that counts, ms (default 2)")
    p.add_argument("--include-restricted", action="store_true")
    args = p.parse_args(argv)
    systems = set(args.systems.split(",")) if args.systems else None
    base = collect(files(args.base), systems, args.include_restricted)
    new = collect(files(args.new), systems, args.include_restricted)
    common = sorted(set(base) & set(new))
    print(f"# {args.new} against {args.base}\n")
    print(f"threshold {args.threshold:.0%} or the measured noise, whichever is larger; at least {args.min_ms} ms\n")
    only = sorted(set(new) - set(base))
    if only:
        print("New pairs without a baseline: " + "; ".join(" ".join(k[:3]) + (f" cache {k[4]}" if k[4] != "-" else "")
                                                         for k in only) + "\n")
    totals = {"slower": 0, "faster": 0, "=": 0}
    findings = []
    for key in common:
        b, n = base[key], new[key]
        workload, tier, system, regime, cache = key
        title = f"{workload} {tier}, {system} ({regime}{', cache ' + cache if cache != '-' else ''})"
        lines = []
        # Correctness first.
        if cache == "-" and b["counts"] and n["counts"] and b["counts"] != n["counts"]:
            lines.append(f"- **statement counts differ:** {sorted(b['counts'])} -> {sorted(n['counts'])}")
        for item in sorted(set(b["answers"]) & set(n["answers"])):
            if b["answers"][item] != n["answers"][item]:
                lines.append(f"- **{item}: answers differ:** {'/'.join(sorted(b['answers'][item]))} -> "
                             f"{'/'.join(sorted(n['answers'][item]))}")
        for item in sorted(set(b["status"]) & set(n["status"])):
            if "ok" in b["status"][item] and "ok" not in n["status"][item]:
                lines.append(f"- **{item}: was ok, now {'/'.join(sorted(n['status'][item]))}**")
        # Steps: always shown, marked where beyond noise.
        steps = []
        for metric in sorted(set(b["steps"]) & set(n["steps"])):
            sb, sn = summarise(b["steps"][metric]), summarise(n["steps"][metric])
            if sb and sn and sb[0] > 0:
                v = verdict(sb, sn, args.threshold, args.min_ms if metric.endswith("ms") else 0)
                if metric.endswith("MiB"):
                    v = {"slower": "larger", "faster": "smaller"}.get(v, v)
                mark = f", **{v}**" if v != "=" else ""
                steps.append(f"{metric} {fmt(sb[0])} -> {fmt(sn[0])} ({sn[0] / sb[0]:.2f}x{mark})")
                if v in ("slower", "larger"):
                    findings.append(f"{title}: {metric} {sn[0] / sb[0]:.2f}x ({v})")
        if steps:
            lines.append("- steps: " + "; ".join(steps))
        # Queries.
        ratios, sums = [], [0.0, 0.0]
        changed = []
        for item in sorted(set(b["queries"]) & set(n["queries"])):
            sb, sn = summarise(b["queries"][item]), summarise(n["queries"][item])
            if not (sb and sn):
                continue
            sums[0] += sb[0]
            sums[1] += sn[0]
            if sb[0] > 0 and sn[0] > 0:
                ratios.append(sn[0] / sb[0])
            v = verdict(sb, sn, args.threshold, args.min_ms)
            totals[v] += 1
            if v != "=":
                changed.append((sn[0] / sb[0], item, sb, sn, v))
        if ratios:
            geo = math.exp(sum(math.log(r) for r in ratios) / len(ratios))
            lines.insert(0, f"- queries: sum of medians {fmt(sums[0])} -> {fmt(sums[1])} ms "
                            f"({sums[1] / sums[0] if sums[0] else float('nan'):.2f}x), geometric mean of ratios "
                            f"{geo:.2f}x over {len(ratios)} queries")
        for ratio, item, sb, sn, v in sorted(changed, reverse=True):
            noise = "/".join("?" if x is None else f"{x:.0%}" for x in (sb[1], sn[1]))
            lines.append(f"  - {item}: {fmt(sb[0])} -> {fmt(sn[0])} ms ({ratio:.2f}x, {v}; noise {noise})")
            if v == "slower":
                findings.append(f"{title}: {item} {ratio:.2f}x")
        if lines:
            print(f"## {title}\n")
            print("\n".join(lines) + "\n")
    print(f"**Queries:** {totals['slower']} slower, {totals['faster']} faster, {totals['=']} within noise.")
    if findings:
        print("\n**Slower beyond noise:**\n" + "\n".join(f"- {f}" for f in findings))
    return 0
