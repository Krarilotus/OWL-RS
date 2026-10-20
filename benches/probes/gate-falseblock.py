#!/usr/bin/env python3
"""The release gate's A/A false-block rate, by simulation: the evidence behind the per-block copies
and the A-A' floor (PROTOCOL.md §5, "the unit"; 20 Oct 2026). Evidence for a review, not a gate step.

    gate-falseblock.py RESULTS [--blocks 4 6 12] [--gates 2000] [--modes old old-kept new copies] [--scale X]

RESULTS: lks-aa-20's results (`fleet results lks-aa-20`, its out/ directory). That job timed a build
against a byte-identical copy of it on office-pc (`<workload>-<n>-<a|b>.json`, a and b the two
files), in blocks of four runs (a b b a), the blocks alternating between two layouts (two file
pairs). Per workload and metric it gives sigma_w, the sd of one block's log contrast between two
fixed files, and sigma_p, the sd of the contrast's file-instance component, from the two pairs:
var(m1 - m2) = 2 sigma_p^2 + (s1^2 + s2^2) / 3.

--scale: sigma_p's factor. Default `calibrated`: the factor at which the model's rate of a
per-layout test (3 blocks, two-sided, at 0.05) equals the rate observed in lks-aa-20 itself (8 of
108; 0.502, which then also gives 1.8 % against 1.9 % observed at 0.01). 1: sigma_p as estimated
(one degree of freedom a metric: it overstates the spread).

Each simulated gate: 1,218 hypotheses (release_gate.py's family over the catalogue, both
baselines), each drawing a (sigma_w, sigma_p) of the measured metrics; round 1 on every case,
round 2 on the cases with a hypothesis at p <= 0.045, round 3 where the gate's own decide() says
it is pending; a false block is a loss decide() confirms on identical builds. Modes:

    old       each side's instance drawn once a round and shared by its blocks; a new session
              (round) draws it anew (lks-abba-11 and -13: the same files flipped)
    old-kept  the same, but rounds 2 and 3 keep round 1's instances (the files still cached)
    new       every block draws its own copies (fast.py block --fresh-copies), and each test's
              spread is at least the floor, estimated from an A-A' run of 8 such blocks
    copies    the copies without the floor

Seeds: random and numpy both 20261020, set again for each --blocks value; the modes draw in the
order given, so a rate reproduces with the same arguments, inputs and gate revision. The run
prints the gate revision whose decide() and loss_test() it calls, and a digest of its input
files (sha256 over the sorted names and contents' sha256). The p-values are computed vectorised
and checked against gatestats.loss_test on the first gate of each --blocks value.

ASSUMED, NOT YET VALIDATED: in `new` and `copies` each block's copies draw their offsets
independently, from the spread lks-aa-20 saw between two files. lks-aa-20 shows that two fixed
files differ; it does not show that copying and deleting a file for every block redraws the offset
each time (pages can be reused, placement need not change). The A-A' floor run on phuoc-yu is to
test exactly that: the spread of copy-to-copy contrasts across its blocks against lks-aa-20's
between-file spread. Until then the `new` rates hold under this model only. Zero events in 2,000
gates bounds the rate at 0.15 % (one-sided 95 %), not at 0.

Counted: complete simulated gates (every round a gate's decide() asks for), one per trial; a
false block is a gate whose decide() confirms at least one loss among identical builds.
Calibration data (lks-aa-20) and simulated observations are separate; the floor in `new` is drawn
from its own simulated A-A' run, never from the gate's observations.

The run of 20 Oct 2026 (gate revision 9efb2314; inputs 96 files, sha256 601c17e4...; full output
in Harness/notes/2026-10-20-gate-falseblock.md), false-block rate of complete A/A gates:

    blocks a round                       4                 6                 12
    old                                  0.002 (4/2000)    0.202             0.982
    old-kept                             0.013             0.776             1.000
    new                                  0/2000            0/2000            0/2000
    copies (no floor)                    0/2000            0/2000            0/2000
    --scale 1: old / old-kept / new      0.053/0.291/0     0.904/0.999/0
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import math
import random
import re
import statistics
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

import numpy as np
from scipy import optimize, stats

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "benches" / "suite"))
sys.path.insert(0, str(ROOT / "benches" / "fast"))
from suitekit import cases as cases_registry, gatestats as gs  # noqa: E402

spec = importlib.util.spec_from_file_location("release_gate", ROOT / "benches" / "fast" / "release_gate.py")
rg = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rg)

SEED = 20261020
FLOOR_BLOCKS = 8


def runs_of(results: Path) -> dict:
    """{workload: {(n, side): {metric: value}}} from lks-aa-20's run reports."""
    import json
    runs: dict = defaultdict(dict)
    for p in results.glob("*.json"):
        m = re.match(r"(\w+)-(\d+)-([ab])\.json$", p.name)
        if not m:
            continue
        r = json.loads(p.read_text())
        metrics = {"load_s": r.get("load_s"), "sum_p50_ms": r.get("sum_p50_ms"),
                   "reason_p50_ms": (r.get("reason") or {}).get("p50_ms")}
        for k, v in (r.get("commits") or {}).items():
            if k in ("whole_s", "p50_ms", "p99_ms", "read_p50_ms", "read_p99_ms"):
                metrics[f"commits.{k}"] = v
        for q in r.get("queries") or []:
            metrics[f"q.{q['name']}"] = q.get("p50_ms")
        runs[m.group(1)][(int(m.group(2)), m.group(3))] = {k: v for k, v in metrics.items()
                                                           if isinstance(v, (int, float)) and v > 0}
    return runs


def profiles_of(runs: dict) -> tuple[list[tuple[float, float]], list[float]]:
    """Per workload and metric (sigma_w, sigma_p); and every per-layout test's two-sided p."""
    profiles, tests = [], []
    for wl, by in sorted(runs.items()):
        for metric in sorted(set.intersection(*(set(v) for v in by.values()))):
            blocks: dict = defaultdict(lambda: {"a": [], "b": []})
            for (n, side), v in by.items():
                blocks[(n - 1) // 4][side].append(math.log(v[metric]))
            contrasts = {b: statistics.fmean(x["b"]) - statistics.fmean(x["a"]) for b, x in blocks.items()}
            halves = [[c for b, c in contrasts.items() if b % 2 == k] for k in (0, 1)]
            (m1, m2), (s1, s2) = ([statistics.fmean(h) for h in halves], [statistics.stdev(h) for h in halves])
            for h in halves:
                p = gs.loss_test(h)["p"]
                tests.append(2 * min(p, 1 - p))
            w2 = (s1 ** 2 + s2 ** 2) / 2
            if w2 > 0:
                profiles.append((math.sqrt(w2), math.sqrt(max(0.0, ((m1 - m2) ** 2 - (s1 ** 2 + s2 ** 2) / 3) / 2))))
    return profiles, tests


def model_rate(profiles, scale: float, alpha: float = 0.05, n: int = 3) -> float:
    """A per-layout test's rate (n blocks sharing one file pair, two-sided at alpha) in the model."""
    crit = stats.t.ppf(1 - alpha / 2, n - 1)
    return statistics.fmean(2 * stats.t.sf(crit / math.sqrt(1 + n * (scale * p / w) ** 2), n - 1) for w, p in profiles)


def simulate(profiles, mode: str, blocks: int, gates: int, family: list[str], cases: dict, rng, check: bool) -> int:
    hits = 0
    for g in range(gates):
        prof = {h: random.choice(profiles) for h in family}
        floors = {}
        if mode == "new":
            for h, (w, p) in prof.items():
                floors[h] = float(np.std(rng.normal(0, p, FLOOR_BLOCKS) + rng.normal(0, w, FLOOR_BLOCKS), ddof=1))
        kept: dict = {}

        def round_(hs):
            hs = list(hs)
            w = np.array([prof[h][0] for h in hs])
            s = np.array([prof[h][1] for h in hs])
            if mode in ("new", "copies"):
                c = rng.normal(0, 1, (len(hs), blocks)) * s[:, None] + rng.normal(0, 1, (len(hs), blocks)) * w[:, None]
            else:
                d = np.array([kept[h] if mode == "old-kept" and h in kept else rng.normal(0, prof[h][1]) for h in hs])
                for h, x in zip(hs, d):
                    kept.setdefault(h, x)
                c = d[:, None] + rng.normal(0, 1, (len(hs), blocks)) * w[:, None]
            fl = np.array([floors.get(h, 0.0) for h in hs])
            sd = np.maximum(c.std(1, ddof=1), fl)
            p = stats.t.sf(c.mean(1) / (sd / math.sqrt(blocks)), blocks - 1)
            if check and g == 0:
                for i in range(3):
                    q = gs.loss_test(list(c[i]), floor=fl[i])["p"]
                    assert abs(q - p[i]) < 1e-9, (q, p[i])
            return dict(zip(hs, p.tolist()))

        reports = [{"family": family, "pvalues": round_(family)}]
        due = {h.split("|")[1] for h, p in reports[0]["pvalues"].items() if p <= gs.ALPHA * gs.PAIR_WEIGHTS[0]}
        if not due:
            continue
        reports.append({"family": family, "pvalues": round_([h for c in due for h in cases[c]])})
        decided = rg.decide([reports])
        if not decided["losses"]:
            third = {h.split("|")[1] for h in decided["pending"]}
            if not third:
                continue
            reports.append({"family": family, "pvalues": round_([h for c in third for h in cases[c]])})
            decided = rg.decide([reports])
        hits += bool(decided["losses"])
    return hits


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("results", type=Path, help="lks-aa-20's out/ directory")
    ap.add_argument("--blocks", type=int, nargs="+", default=[4, 6, 12])
    ap.add_argument("--gates", type=int, default=2000)
    ap.add_argument("--modes", nargs="+", default=["old", "old-kept", "new", "copies"])
    ap.add_argument("--scale", default="calibrated")
    args = ap.parse_args()
    files = sorted(args.results.glob("*.json"))
    digest = hashlib.sha256("".join(f"{f.name} {hashlib.sha256(f.read_bytes()).hexdigest()}\n" for f in files)
                            .encode()).hexdigest()

    def git(*a):
        return subprocess.run(["git", *a], cwd=ROOT, capture_output=True, text=True).stdout.strip()
    dirty = git("status", "--porcelain", "--", "benches/fast/release_gate.py", "benches/suite/suitekit/gatestats.py")
    print(f"gate revision {git('rev-parse', 'HEAD')}{' (release_gate.py or gatestats.py modified!)' if dirty else ''}; "
          f"inputs {args.results.as_posix()}: {len(files)} files, sha256 {digest}")
    profiles, tests = profiles_of(runs_of(args.results))
    observed = sum(p <= 0.05 for p in tests) / len(tests)
    if args.scale == "calibrated":
        scale = round(optimize.brentq(lambda s: model_rate(profiles, s) - observed, 0.01, 1.0), 3)
    else:
        scale = float(args.scale)
    ratios = sorted(p / w for w, p in profiles)
    print(f"lks-aa-20: {len(profiles)} metrics; sigma_p / sigma_w median {statistics.median(ratios):.2f}, "
          f"max {max(ratios):.2f}; per-layout tests at 0.05: {sum(p <= 0.05 for p in tests)}/{len(tests)} observed, "
          f"{model_rate(profiles, scale):.3f} modelled at scale {scale} "
          f"(at 0.01: {sum(p <= 0.01 for p in tests)}/{len(tests)}, {model_rate(profiles, scale, 0.01):.3f})")
    profiles = [(w, p * scale) for w, p in profiles]
    family = rg.family_of(cases_registry.load(), [c["name"] for c in cases_registry.load()])
    cases: dict = {}
    for h in family:
        cases.setdefault(h.split("|")[1], []).append(h)
    print(f"family m = {len(family)} over {len(cases)} cases; seed {SEED}")
    for blocks in args.blocks:
        random.seed(SEED)
        rng = np.random.default_rng(SEED)
        for k, mode in enumerate(args.modes):
            hits = simulate(profiles, mode, blocks, args.gates, family, cases, rng, check=k == 0)
            print(f"{mode:9s} blocks {blocks:2d}: false-block rate {hits / args.gates:.3f} ({hits}/{args.gates} A/A gates)",
                  flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
