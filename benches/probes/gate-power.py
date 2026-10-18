#!/usr/bin/env python3
"""The release gate's noise and power (benches/PROTOCOL.md §5): how much each series moves
between passes of the same binary, and how often the gate, confirm on rerun included, catches a
repeatable loss of a given size at that noise.

    gate-power.py noise PASS.json... [--min-ms 0.005] [--out noise.json]
        per series (case, series, binary revision): the passes' medians and the noise, the
        standard deviation of their logs; a binary's passes are its group (an A/A run's sides
        are one binary, so one group). The distribution: median, p75, p90, how many above 2 %
        and 5 %.
    gate-power.py power noise.json [--sims 400] [--passes 4 | --measure MEASURE.toml] [--resolution-ms 0.005]
                       [--out power.json]
        the gate's chain simulated on each noise: round 0 at the passes a side the case gets,
        then confirmations of 4 to 24 passes; a loss is caught when two rounds show it at 95 %
        (`confirmed`). Against losses of 1, 2, 3 and 8 %: the share caught at the median and the
        p75 noise, and the share of series caught at least 90 % of the time; `with resolution`:
        the same where a series' loss below the gate's resolution (compare.RESOLUTION, 5 µs)
        counts as never caught, as the gate judges it (the agent's simulation leaves it out).

The simulation is the gate agent's of 18 Oct 2026 (the confirm-on-rerun receipts in PROTOCOL.md
§5): one log-normal value a pass at the series' noise, the bootstrap over the passes (2,000
draws), `judge`'s verdicts and the passes its `needs` estimate asks for. The noises are
binned (powers interpolated between bins), so a whole catalogue takes minutes.
"""
from __future__ import annotations

import argparse
import json
import math
import re
import statistics as st
import sys
from pathlib import Path

import numpy as np

R = 2000
MAXP, MINP, ROUNDS = 24, 4, 3
LOSSES = (1.01, 1.02, 1.03, 1.08)


# --- noise -------------------------------------------------------------------------------------

def pass_series(report: dict) -> dict[tuple[str, str], list[float]]:
    """A pass report's values per (case, series): the samples, or one p50 per process."""
    out = {}
    for r in report.get("records", []):
        if r.get("status") != "ok":
            continue
        for series, values in (r.get("samples") or {}).items():
            if isinstance(values, list) and values:
                out[(r["case"], series)] = [v for v in values if isinstance(v, (int, float))]
    return out


def noise(paths: list[Path], min_ms: float) -> list[dict]:
    groups: dict[str, list[dict]] = {}
    for path in paths:
        report = json.loads(path.read_text(encoding="utf-8"))
        rev = (report.get("binaries") or {}).get("revision") or "?"
        groups.setdefault(rev[:8], []).append(pass_series(report))
    rows = []
    for rev, passes in groups.items():
        common = set.intersection(*(set(p) for p in passes)) if passes else set()
        for case, series in sorted(common):
            meds = [st.median(p[(case, series)]) for p in passes if p[(case, series)]]
            if len(meds) == len(passes) >= 2 and min(meds) > 0 and st.median(meds) >= min_ms:
                rows.append({"binary": rev, "case": case, "series": series, "passes": len(meds),
                             "median_ms": st.median(meds), "meds": meds,
                             "sd": st.stdev(math.log(v) for v in meds)})
    return rows


def summary(rows: list[dict]) -> dict:
    sds = sorted(r["sd"] for r in rows)
    if not sds:
        return {"series": 0}
    q = lambda f: sds[min(len(sds) - 1, int(f * len(sds)))]  # noqa: E731 (the agent's quantile)
    return {"series": len(sds), "median": st.median(sds), "p75": q(0.75), "p90": q(0.90),
            "above_2pct": sum(s > 0.02 for s in sds), "above_5pct": sum(s > 0.05 for s in sds)}


# --- the chain ---------------------------------------------------------------------------------

def one_round(rng, sigma: float, loss: float, n: int) -> tuple[str, float]:
    a = np.exp(rng.normal(0, sigma, n))
    b = np.exp(rng.normal(math.log(loss), sigma, n))
    r = np.sort(np.median(b[rng.integers(0, n, (R, n))], 1) / np.median(a[rng.integers(0, n, (R, n))], 1))
    lo, hi = r[int(.025 * R)], r[int(.975 * R) - 1]
    point = np.median(b) / np.median(a)
    noise_ = max(np.std(np.log(a), ddof=1), np.std(np.log(b), ddof=1))
    if lo > 1 and point > 1:
        v = "regression"
    elif hi < 1:
        v = "better"
    elif math.log(hi) <= 2 * noise_:
        v = "neutral"
    else:
        v = "suspect"
    half = (math.log(hi) - math.log(lo)) / 2
    lp = math.log(point)
    gap = lp if (v == "regression" or lp > 2 * noise_) else 2 * noise_ - lp
    need = n * (half / gap) ** 2 if gap > 0 else math.inf
    return v, need


def passes_for(need: float) -> int:
    n = MAXP if not math.isfinite(need) else min(MAXP, max(MINP, math.ceil(need)))
    return n + (n % 2)


def chain(rng, sigma: float, loss: float, n0: int = 4) -> str:
    """'block-confirmed' (two rounds show the loss), 'block-undecided' (still undecided, or shown
    once with no round left to repeat it), or 'pass' (decided without a loss, or not confirmed)."""
    detections, n = 0, n0
    for _ in range(ROUNDS):
        v, need = one_round(rng, sigma, loss, n)
        if v == "regression":
            detections += 1
            if detections >= 2:
                return "block-confirmed"
        elif v in ("neutral", "better"):
            return "pass"
        n = passes_for(need)
    return "block-undecided"


def caught(sigma: float, loss: float, n0: int, sims: int, seed: int) -> dict:
    rng = np.random.default_rng(seed)
    out = {"block-confirmed": 0, "block-undecided": 0, "pass": 0}
    for _ in range(sims):
        out[chain(rng, sigma, loss, n0)] += 1
    return {k: v / sims for k, v in out.items()}


GRID = np.exp(np.linspace(math.log(0.001), math.log(0.4), 28))


def curves(n0s: set[int], sims: int) -> dict[int, dict[float, np.ndarray]]:
    """For each round-0 pass count and loss: the confirmed share on the noise grid."""
    out = {}
    for n0 in sorted(n0s):
        out[n0] = {loss: np.array([caught(s, loss, n0, sims, seed=11 + i)["block-confirmed"]
                                   for i, s in enumerate(GRID)]) for loss in LOSSES}
    return out


def at(curve: np.ndarray, sigma: float) -> float:
    return float(np.interp(math.log(max(sigma, GRID[0])), np.log(GRID), curve))


def power(rows: list[dict], sims: int, plan: dict | None, passes: int, resolution_ms: float = 0.005) -> dict:
    """Each series' chance to be caught, at the round-0 passes its case gets."""
    n_of = lambda r: (plan or {}).get(r["case"], passes)  # noqa: E731
    table = curves({n_of(r) for r in rows} | {passes}, sims)
    s = summary(rows)
    out = {"noise": s, "passes": passes, "planned": bool(plan), "losses": {}}
    for loss in LOSSES:
        each = [at(table[n_of(r)][loss], r["sd"]) for r in rows]
        resolved = [p if r["median_ms"] * (loss - 1) >= resolution_ms else 0.0 for p, r in zip(each, rows)]
        out["losses"][f"{loss - 1:.0%}"] = {
            f"at median sd {s['median']:.4f} ({passes} passes)": at(table[passes][loss], s["median"]),
            f"at p75 sd {s['p75']:.4f} ({passes} passes)": at(table[passes][loss], s["p75"]),
            "series caught >= 90 %": sum(p >= 0.9 for p in each) / len(each),
            "mean over series": sum(each) / len(each),
            "with resolution: series caught >= 90 %": sum(p >= 0.9 for p in resolved) / len(resolved),
            "with resolution: series below it": sum(r["median_ms"] * (loss - 1) < resolution_ms for r in rows)}
    # Identical binaries: a gate run's false blocks. Without a loss the chain doesn't depend on
    # the noise's size (every verdict compares logs), so one simulation per pass count serves.
    false = {n0: caught(0.01, 1.0, n0, 20 * sims, seed=7) for n0 in {n_of(r) for r in rows}}
    out["identical, per 400 series"] = {k: round(400 * sum(false[n_of(r)][k] for r in rows) / len(rows), 2)
                                        for k in ("block-confirmed", "block-undecided")}
    return out


def main() -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    s = sub.add_parser("noise")
    s.add_argument("reports", type=Path, nargs="+")
    s.add_argument("--min-ms", type=float, default=0.005)
    s.add_argument("--out", type=Path)
    s = sub.add_parser("power")
    s.add_argument("noise", type=Path)
    s.add_argument("--sims", type=int, default=400)
    s.add_argument("--passes", type=int, default=4)
    s.add_argument("--measure", type=Path, help="passes a side per case (benches/fast/measure.toml)")
    s.add_argument("--resolution-ms", type=float, default=0.005)
    s.add_argument("--out", type=Path)
    args = p.parse_args()
    if args.command == "noise":
        rows = noise(args.reports, args.min_ms)
        print(json.dumps(summary(rows), indent=1))
        if args.out:
            args.out.write_text(json.dumps(rows, indent=0), encoding="utf-8")
        return 0
    rows = json.loads(args.noise.read_text(encoding="utf-8"))
    plan = None
    if args.measure:
        import tomllib
        plan = {c: int(e.get("passes", args.passes)) for c, e in
                tomllib.loads(args.measure.read_text(encoding="utf-8")).get("case", {}).items()}
    out = power(rows, args.sims, plan, args.passes, args.resolution_ms)
    print(json.dumps(out, indent=1))
    if args.out:
        args.out.write_text(json.dumps(out, indent=1), encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())
