"""`suite.py compare`: a run against a baseline run, pair by pair: what was lost or broke,
what answers differently, what got slower or faster. The regression check before a
milestone.

    suite.py compare BASE NEW [--systems nrese] [--threshold 0.10] [--min-ms 2]

BASE and NEW are results directories or results.csv files (several, comma-separated).
Completeness, timing eligibility and the estimator are `summary.py`'s, the same as the
report's and the ledger's.

Printed first, because they decide whether a timing may be compared at all:
- pairs and queries the baseline has and the new run doesn't, and loads or steps that
  failed in the new run (lost work is never silently dropped from the comparison);
- changed statement or answer counts, wrong answers, queries complete before and
  incomplete now, and queries whose answers differ between systems (disputed).

Then the speed verdict, over the queries that are complete, correct and undisputed on both
sides only: a change counts if the ratio exceeds 1 + max(threshold, noise of both sides)
and the absolute difference exceeds --min-ms; the rest is within noise. Load, store size,
restart, count and peak memory are compared the same way over the runs. Restricted systems
are left out unless --include-restricted.
"""
from __future__ import annotations

import argparse
import math
from pathlib import Path

from .schema import read
from .summary import Item, Pair, estimate, summarise


def files(spec: str) -> list[Path]:
    out = []
    for part in spec.split(","):
        path = Path(part)
        out.append(path / "results.csv" if path.is_dir() else path)
    return out


def collect(paths: list[Path], systems: set[str] | None, restricted: bool) -> dict[tuple, Pair]:
    rows = []
    # Distinct files can both live under records/; aliases of one file are not new runs.
    for path in dict.fromkeys(p.resolve() for p in paths):
        for r in read(path):
            if systems and r["system"] not in systems:
                continue
            if r["publish"] == "permission" and not restricted:
                continue
            rows.append(r | {"run": f"{path.as_posix()}/{r['run']}"})
    return summarise(rows)


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


def eligible(item: Item, name: str, pair: Pair) -> bool:
    return item.complete and name not in pair.disputed


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
    print(f"# {args.new} against {args.base}\n")
    print(f"threshold {args.threshold:.0%} or the measured noise, whichever is larger; at least {args.min_ms} ms; "
          "only complete, correct, undisputed queries enter the speed verdict\n")
    lost_pairs = sorted(k for k, pair in base.items() if pair.live and (k not in new or not new[k].live))
    if lost_pairs:
        print("**Lost: pairs the baseline ran and the new run didn't:** "
              + "; ".join(" ".join(k) for k in lost_pairs) + "\n")
    added = sorted(k for k, pair in new.items() if pair.live and (k not in base or not base[k].live))
    if added:
        print("New pairs without a baseline: " + "; ".join(" ".join(k) for k in added) + "\n")
    totals = {"slower": 0, "faster": 0, "=": 0, "not comparable": 0}
    findings = []
    for key in sorted(set(base) & set(new)):
        b, n = base[key], new[key]
        if not (b.live and n.live):
            continue
        title = f"{key[0]} {key[1]}, {key[2]} ({n.regime})"
        lines = []
        # What decides whether the timings may be compared at all.
        failed = len(n.loads) - len(n.ok_runs())
        if failed:
            lines.append(f"- **{failed} of {len(n.loads)} loads not ok in the new run**")
        failed_steps = sorted({f"{t} {s}" for (t, s), c in n.other.items() if s != "ok"})
        if failed_steps:
            lines.append(f"- **steps not ok in the new run:** {', '.join(failed_steps)}")
        if b.counts and n.counts and b.counts != n.counts:
            lines.append(f"- **statement counts differ:** {sorted(b.counts)} -> {sorted(n.counts)}")
        # Steps: always shown, marked where beyond noise.
        steps = []
        for metric in sorted(set(b.steps) & set(n.steps)):
            sb, sn = estimate(b.steps[metric]), estimate(n.steps[metric])
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
        for cache in sorted(set(b.items) | set(n.items)):
            bi, ni = b.items.get(cache, {}), n.items.get(cache, {})
            label = "" if cache == "-" else f" (cache {cache})"
            missing = sorted(set(bi) - set(ni))
            if missing:
                lines.append(f"- **lost queries{label}:** {', '.join(missing)}")
            notes, changed, ratios, sums = [], [], [], [0.0, 0.0]
            for name in sorted(set(bi) & set(ni)):
                ib, inn = bi[name], ni[name]
                if ib.answers and inn.answers and ib.answers != inn.answers:
                    notes.append(f"**{name}: answers differ** {'/'.join(sorted(ib.answers))} -> "
                                 f"{'/'.join(sorted(inn.answers))}")
                if inn.wrong:
                    notes.append(f"**{name}: wrong answer** ({inn.problems()})")
                elif ib.complete and not inn.complete:
                    notes.append(f"**{name}: was complete, now {inn.problems()}**")
                if name in n.disputed:
                    notes.append(f"{name}: answers differ between systems in the new run (disputed)")
                if not (eligible(ib, name, b) and eligible(inn, name, n)):
                    totals["not comparable"] += 1
                    continue
                sb, sn = ib.estimate(), inn.estimate()
                if not (sb and sn):
                    totals["not comparable"] += 1
                    continue
                sums[0] += sb[0]
                sums[1] += sn[0]
                if sb[0] > 0 and sn[0] > 0:
                    ratios.append(sn[0] / sb[0])
                v = verdict(sb, sn, args.threshold, args.min_ms)
                totals[v] += 1
                if v != "=":
                    changed.append((sn[0] / sb[0], name, sb, sn, v))
            if ratios:
                geo = math.exp(sum(math.log(r) for r in ratios) / len(ratios))
                lines.append(f"- queries{label}: sum of medians {fmt(sums[0])} -> {fmt(sums[1])} ms "
                             f"({sums[1] / sums[0] if sums[0] else float('nan'):.2f}x), geometric mean of ratios "
                             f"{geo:.2f}x over {len(ratios)} comparable queries of {len(set(bi) & set(ni))}")
            lines.extend(f"  - {note}" for note in notes)
            for ratio, name, sb, sn, v in sorted(changed, reverse=True):
                noise = "/".join("?" if x is None else f"{x:.0%}" for x in (sb[1], sn[1]))
                lines.append(f"  - {name}{label}: {fmt(sb[0])} -> {fmt(sn[0])} ms ({ratio:.2f}x, {v}; noise {noise})")
                if v == "slower":
                    findings.append(f"{title}: {name}{label} {ratio:.2f}x")
        if lines:
            print(f"## {title}\n")
            print("\n".join(lines) + "\n")
    print(f"**Queries:** {totals['slower']} slower, {totals['faster']} faster, {totals['=']} within noise, "
          f"{totals['not comparable']} not comparable (incomplete, wrong, disputed or without timings).")
    if lost_pairs:
        print(f"**Lost pairs:** {len(lost_pairs)}")
    if findings:
        print("\n**Worse beyond noise:**\n" + "\n".join(f"- {f}" for f in findings))
    return 0
