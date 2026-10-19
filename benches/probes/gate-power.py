#!/usr/bin/env python3
"""The release gate's noise and power (benches/PROTOCOL.md §5): how much each series moves
between passes of the same binary, the gate's power against a loss at that noise, and its false
blocks on identical binaries.

    gate-power.py noise PASS.json... [--min-ms 0.005] [--out noise.json]
        per series (case, series, binary revision): the passes' medians and the noise, the
        standard deviation of their logs; a binary's passes are its group (an A/A run's sides
        are one binary, so one group). The distribution: median, p75, p90, how many above 2 %
        and 5 %.
    gate-power.py power noise.json --m M [--blocks 2 | --measure MEASURE.toml] [--out power.json]
        the gate's power per series (gatestats.power_at: the one-sided t at alpha / M, Holm's
        first threshold, by the noncentral t), a block contrast's sd taken as the series' run-to-run
        sd (each side's two runs of a block against the other's: sigma^2 / 2 + sigma^2 / 2). Against
        losses of 1, 2, 3 and 8 %: the share of series whose two rounds both catch it (power per
        round at least sqrt 0.9) at the blocks they get, the blocks a round needs at the median,
        p75 and p90 noise, and per case the blocks its noisiest series needs. A loss below a
        series' counter step (1 us) counts as never caught.
    gate-power.py aa ROUND_DIR... [--splits 300] [--seed 1] [--out aa.json]
        the gate's false blocks on identical binaries, recomputed from A/A passes (one binary on
        every side; no new run): each round directory's passes split at random into pseudo-blocks
        (a baseline pass against a candidate pass each), compared as the gate compares them
        (release_gate.evidence, compare_case) and decided as the gate decides (release_gate.decide:
        round 2 on the cases round 1 left pending, the rounds combined, Holm over the family). Per
        round the share of hypotheses at p <= 0.05 and <= 0.045 (the pending threshold), and over
        the joint splits the share of gates with a false loss: the family-wise error the design
        holds at 0.05.
    gate-power.py designs SCREEN_DIR CONFIRM_DIR [--host NAME=DIR]... [--blocks1 3] [--splits 300]
        the current rounds against a prespecified screen-then-confirm (stage 1: every case, few
        blocks, one-sided at alpha1 0.1 or 0.2 uncorrected; stage 2: fresh blocks of the flagged
        cases, Holm or BH at 0.05 over the flagged), with the primary family (a case's headline
        time and its memory) gating or everything gating: per design the share of primary
        hypotheses at 90 % power against 1, 2 and 5 % losses, the false blocks on the A/A splits
        (SCREEN_DIR's splits the screen, CONFIRM_DIR's the confirmation) and the hours a gate
        takes on each host.
    gate-power.py layout PASS.json... [--out layout.json]
        passes of one revision built in several layouts (release-gate.sh GATE_LAYOUTS, each pass's
        `binaries.layout`): per series, a one-way analysis of variance of the log pass medians by
        layout; the layout's own component (sd), the rest's, and per case family (the case's area)
        the median and the share of series whose layout component exceeds 0.5 % and 1 %. Passes
        of several revisions are grouped by revision (two revisions' difference is no layout's).
"""
from __future__ import annotations

import argparse
import json
import math
import statistics as st
import sys
from pathlib import Path



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


def layout_components(paths: list[Path], min_ms: float) -> list[dict]:
    """Per series of each revision, the variance of its log pass medians split into the layouts'
    and the rest's (one revision's layouts only: another revision's code is no layout)."""
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "suite"))
    from suitekit import cases as registry
    areas = {c["name"]: c.get("area", "?") for c in registry.load()}
    by: dict[tuple, dict[int, list[float]]] = {}
    for path in paths:
        report = json.loads(path.read_text(encoding="utf-8"))
        binaries = report.get("binaries") or {}
        layout, revision = binaries.get("layout", 1), (binaries.get("revision") or "?")[:8]
        for (case, series), values in pass_series(report).items():
            by.setdefault((revision, case, series), {}).setdefault(layout, []).append(st.median(values))
    rows = []
    for (revision, case, series), groups in sorted(by.items()):
        sizes = {len(v) for v in groups.values()}
        if len(groups) < 2 or len(sizes) != 1 or min(sizes) < 2:
            continue
        n = sizes.pop()
        logs = {k: [math.log(v) for v in vs] for k, vs in groups.items() if min(vs) > 0}
        if len(logs) != len(groups) or st.median([v for vs in groups.values() for v in vs]) < min_ms:
            continue
        within = st.mean(st.variance(vs) for vs in logs.values())
        between = n * st.variance([st.mean(vs) for vs in logs.values()])
        layout_var = max(0.0, (between - within) / n)
        rows.append({"binary": revision, "case": case, "series": series, "area": areas.get(case, "?"),
                     "layouts": len(groups),
                     "per_layout": n, "median_ms": st.median([v for vs in groups.values() for v in vs]),
                     "layout_sd": math.sqrt(layout_var), "rest_sd": math.sqrt(within),
                     "f": between / within if within > 0 else math.inf})
    return rows


def layout_summary(rows: list[dict]) -> dict:
    fam: dict[str, list[float]] = {}
    for r in rows:
        fam.setdefault(r["area"], []).append(r["layout_sd"])
    every = [r["layout_sd"] for r in rows]
    return {"series": len(rows),
            "all": {"median": st.median(every) if every else None,
                    "above_0.5pct": sum(s > 0.005 for s in every), "above_1pct": sum(s > 0.01 for s in every),
                    "rest_median": st.median(r["rest_sd"] for r in rows) if rows else None},
            "families": {a: {"series": len(v), "median": st.median(v), "above_0.5pct": sum(s > 0.005 for s in v),
                             "above_1pct": sum(s > 0.01 for s in v), "max": max(v)} for a, v in sorted(fam.items())}}


def summary(rows: list[dict]) -> dict:
    sds = sorted(r["sd"] for r in rows)
    if not sds:
        return {"series": 0}
    q = lambda f: sds[min(len(sds) - 1, int(f * len(sds)))]  # noqa: E731 (the agent's quantile)
    return {"series": len(sds), "median": st.median(sds), "p75": q(0.75), "p90": q(0.90),
            "above_2pct": sum(s > 0.02 for s in sds), "above_5pct": sum(s > 0.05 for s in sds)}


# --- power --------------------------------------------------------------------------------------

LOSSES = (0.01, 0.02, 0.03, 0.08)
STEP_MS = 0.001  # the perf lab's time step
TARGET = 0.9 ** 0.5  # per round: both rounds catch a loss at least 90 % of the time


def gatestats_module():
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "suite"))
    from suitekit import gatestats
    return gatestats


def power(rows: list[dict], m: int, blocks: dict | None, default_blocks: int) -> dict:
    """Each series' chance, per round and over both rounds, at the blocks its case gets."""
    gs = gatestats_module()
    n_of = lambda r: (blocks or {}).get(r["case"], default_blocks)  # noqa: E731
    s = summary(rows)
    out = {"noise": s, "m": m, "blocks": default_blocks, "planned": bool(blocks), "losses": {}}
    cache: dict = {}

    def pw(n, sigma, loss):
        key = (n, round(sigma, 5), loss)
        if key not in cache:
            cache[key] = gs.power_at(n, sigma, m, loss)
        return cache[key]

    for loss in LOSSES:
        each = [pw(n_of(r), r["sd"], loss) if r["median_ms"] * loss >= STEP_MS else 0.0 for r in rows]
        need = {q: gs.blocks_needed(s[q], m, loss) for q in ("median", "p75", "p90")}
        out["losses"][f"{loss:.0%}"] = {
            "series caught (both rounds >= 0.9)": round(sum(p >= TARGET for p in each) / len(each), 3),
            "mean power over series (both rounds)": round(sum(p * p for p in each) / len(each), 3),
            "below the counter step": sum(r["median_ms"] * loss < STEP_MS for r in rows),
            "blocks a round needs at the median, p75, p90 sd": need}
    worst: dict[str, float] = {}
    for r in rows:
        worst[r["case"]] = max(worst.get(r["case"], 0.0), r["sd"])
    out["cases"] = {c: {"sd": round(sd, 4), "blocks for 2 %": gs.blocks_needed(sd, m, 0.02) if sd > 0 else 2}
                    for c, sd in sorted(worst.items(), key=lambda kv: -kv[1])}
    needs = [v["blocks for 2 %"] or 10 ** 6 for v in out["cases"].values()]
    out["cases with 2 % power at"] = {f"{n} blocks": sum(x <= n for x in needs) for n in (2, 5, 9, 15, 30)}
    out["reference: blocks a round needs for 2 % at sd 0.5/1/2/3/5 %"] = {
        f"{sd:.1%}": gs.blocks_needed(sd, m, 0.02) for sd in (0.005, 0.01, 0.02, 0.03, 0.05)}
    return out


# --- A/A: the gate's false blocks ----------------------------------------------------------------

PRIMARY = ("time/headline", "memory/peak")  # one time and one memory metric a case


def aa_splits(rounds: list[list[Path]], splits: int, seed: int) -> tuple[list[list[dict]], list[list[dict]]]:
    """Each round directory's A/A passes split at random into pseudo-blocks (a baseline pass
    against a candidate pass each), compared as the gate compares them (release_gate.evidence,
    compare_case): per split the p-values of every series' hypothesis, and of the primary ones,
    the case's headline metric and its process peak (standing in for anon + kernel memory, which
    these passes predate). Returns (series p-values, primary p-values) per round and split."""
    import random
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "fast"))
    import release_gate as rg
    catalog = rg.cases_registry.load()
    by_name = {c["name"]: c for c in catalog}
    declared = {c["name"] for c in catalog if c.get("outcome", "ok") != "ok"}
    checks = {c["name"]: {path.split(".")[1] for path in c.get("check", {}) if path.startswith("q.")}
              for c in catalog}
    loaded = []
    for paths in rounds:
        reports = [json.loads(p.read_text(encoding="utf-8")) for p in sorted(paths)]
        revisions = {(r.get("binaries") or {}).get("revision") for r in reports}
        if len(revisions) != 1:
            raise SystemExit(f"not one binary: {sorted(map(str, revisions))} (A/A only)")
        loaded.append(reports)
    rng = random.Random(seed)

    def metric(side: list[dict], case: str) -> list[list[float]]:
        out = []
        for p in side:
            r = next((x for x in p["report"]["records"] if x["case"] == case), None)
            out.append([r["metric"]] if r and r.get("status") == "ok" and isinstance(r.get("metric"), (int, float))
                       and r["metric"] > 0 else [])
        return out

    def one_split(reports: list[dict]) -> tuple[dict, dict]:
        order = rng.sample(range(len(reports)), len(reports))
        sides = {"base": [], "cand": []}
        for i in range(len(reports) // 2):
            for side, k in (("base", order[2 * i]), ("cand", order[2 * i + 1])):
                sides[side].append({"n": k, "side": side, "path": f"pass-{k}",
                                    "report": {**reports[k], "block": i + 1, "layout": 1}})
        planned = sorted(set.intersection(*(set(r.get("planned_cases") or []) for r in reports)) - declared)
        ps: dict[str, float] = {}
        primary: dict[str, float] = {}
        blocks = list(range(1, len(reports) // 2 + 1))
        for case in planned:
            cand, base = rg.evidence(sides["cand"], case), rg.evidence(sides["base"], case)
            row = rg.compare_case(cand, base, "base", checks.get(case, set()))
            for dim, r in row.get("dimensions", {}).items():
                ps[rg.hypothesis("base", case, dim)] = r.get("p", 1.0)
            higher = "qps" in str(by_name.get(case, {}).get("metric", ""))
            for dim, b_, c_, unit in (("time/headline", metric(sides["base"], case), metric(sides["cand"], case), "ms"),
                                      ("memory/peak", [[v] for v in base["lifetime"]],
                                       [[v] for v in cand["lifetime"]], "MiB")):
                r = rg.dimension(b_, blocks, c_, blocks, {}, unit, higher if unit == "ms" else False)
                if r["status"] != "incomplete":
                    primary[rg.hypothesis("base", case, dim)] = r["p"]
        return ps, primary

    per_round = [[one_split(reports) for _ in range(splits)] for reports in loaded]
    return [[s[0] for s in r] for r in per_round], [[s[1] for s in r] for r in per_round]


def aa(rounds: list[list[Path]], splits: int, seed: int) -> dict:
    """The current design's false losses on identical binaries (release_gate.decide per joint split:
    round 2 on the cases round 1 left pending, round 3 what it leaves pending)."""
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "fast"))
    import release_gate as rg
    per_round, _ = aa_splits(rounds, splits, seed)
    family = sorted(set.intersection(*(set(s) for s in per_round[0])))
    out = {"rounds": [len(r) for r in rounds], "m": len(family), "splits": splits, "per round": []}
    for k, samples in enumerate(per_round):
        ps = [s.get(h, 1.0) for s in samples for h in family]
        out["per round"].append({"round": k + 1, "share p <= 0.05": round(sum(p <= 0.05 for p in ps) / len(ps), 4),
                                 "share p <= 0.045 (pending)": round(sum(p <= 0.045 for p in ps) / len(ps), 4)})
    false_loss = third = 0
    for i in range(splits):
        first = {"family": family, "pvalues": {h: per_round[0][i].get(h, 1.0) for h in family}}
        reports = [first]
        if len(per_round) > 1:
            cases = {h.split("|")[1] for h in rg.decide([[first]])["pending"]}
            reports.append({"pvalues": {h: per_round[1][i].get(h, 1.0) for h in family if h.split("|")[1] in cases}})
        d = rg.decide([reports])
        false_loss += bool(d["losses"])
        third += len(reports) == 2 and bool(d["pending"])
    out["family-wise false losses (gates with any)"] = round(false_loss / splits, 4)
    out["gates with a hypothesis due round 3"] = round(third / splits, 4)
    return out


# --- designs: the current rounds against a prespecified screen-then-confirm ------------------------

def bh(pvalues: dict, alpha: float = 0.05) -> set:
    """Benjamini-Hochberg's step-up set at false-discovery rate alpha."""
    ordered = sorted(pvalues.items(), key=lambda kv: kv[1])
    k = max((i + 1 for i, (_, p) in enumerate(ordered) if p <= alpha * (i + 1) / len(ordered)), default=0)
    return {h for h, _ in ordered[:k]}


def case_noise(paths: list[Path]) -> dict[str, dict]:
    """Per case, from one binary's passes: the sd of the log headline metric and of the log process
    peak between passes, the time a run takes, and its series' count."""
    reports = [json.loads(p.read_text(encoding="utf-8")) for p in sorted(paths)]
    out: dict[str, dict] = {}
    for rep in reports:
        for r in rep.get("records", []):
            e = out.setdefault(r["case"], {"metric": [], "peak": [], "seconds": [], "series": 0, "ok": True})
            e["ok"] &= r.get("status") == "ok"
            e["seconds"].append(r.get("case_s") or 0.0)
            if isinstance(r.get("metric"), (int, float)) and r["metric"] > 0:
                e["metric"].append(r["metric"])
            if r.get("peak_mib"):
                e["peak"].append(r["peak_mib"])
            e["series"] = max(e["series"], len(r.get("samples") or {}))

    def sd(values):
        return st.stdev(math.log(v) for v in values) if len(values) >= 2 else None
    return {c: {"time_sd": sd(e["metric"]), "memory_sd": sd(e["peak"]), "seconds": st.mean(e["seconds"]),
                "series": e["series"], "ok": e["ok"]} for c, e in out.items()}


def designs(rounds: list[list[Path]], splits: int, seed: int, hosts: dict[str, list[Path]],
            big: dict[str, float], blocks1: int = 3, cap: int = 30, baselines: int = 2) -> dict:
    """Each design's power (analytic, the primary hypotheses at the case's noise), its false blocks
    (the A/A splits: round 1 the screen, round 2 the confirmation) and its host-hours.

    current: rounds 1 and 2 (and 3), a loss when both show it and Holm over the whole family
    (p = max(p1, p2) / 0.9 against alpha / m at Holm's first step). screen-then-confirm: stage 1
    `blocks1` blocks of every case, one-sided at alpha1 with no correction; stage 2 fresh blocks of
    the flagged cases only, Holm (or BH) at 0.05 over the flagged hypotheses of the gating family.
    A case's stage-2 blocks: the fewest (at most `cap`) that give 90 % against a 2 % loss over both
    stages, else a 5 % one, else none (unpowered); a case is rerun when a gating hypothesis of it is flagged (its other series ride along,
    reported)."""
    gs = gatestats_module()
    from scipy import stats
    noise = case_noise(rounds[-1])
    timed = {c: e for c, e in noise.items() if e["ok"] and e["time_sd"] is not None}
    m_primary = baselines * 2 * len(timed)
    m_all = m_primary + baselines * sum(max(1, e["series"]) for e in timed.values())

    def power1(n, sigma, loss, alpha):
        if sigma is None or n < 2:
            return 0.0
        if sigma <= 0:
            return 1.0
        return float(stats.nct.sf(stats.t.ppf(1 - alpha, n - 1), n - 1, math.log1p(loss) * math.sqrt(n) / sigma))

    def design(kind, alpha1=None, gate_all=False):
        m = m_all if gate_all else m_primary
        k_flag = max(1, round((alpha1 or 0) * m))  # hypotheses flagged under the null
        rows, stage2 = [], {}
        for c, e in timed.items():
            # A case reruns when a gating hypothesis of it is flagged; with the primary family
            # gating, a flag on another series opens an investigation, off the gate's clock.
            h_case = baselines * (2 + max(1, e["series"])) if gate_all else baselines * 2
            for sigma in (e["time_sd"], e["memory_sd"]):
                if kind == "current":
                    a = 0.05 * 0.9 / m
                    p1 = lambda loss, s=sigma: power1(2, s, loss, a)  # noqa: E731
                    p2 = lambda n, loss, s=sigma: power1(n, s, loss, a)  # noqa: E731
                else:
                    p1 = lambda loss, s=sigma: power1(blocks1, s, loss, alpha1)  # noqa: E731
                    p2 = lambda n, loss, s=sigma: power1(n, s, loss, 0.05 / k_flag)  # noqa: E731
                # Blocks only where they buy power: the fewest for 90 % at 2 %, else at 5 %, else
                # none (the case unpowered: its noise is NOISY.md's to fix, not blocks' to buy).
                n = next((n for loss in (0.02, 0.05) for n in range(2, cap + 1)
                          if p1(loss) * p2(n, loss) >= 0.9), 0)
                stage2[c] = max(stage2.get(c, 0), n)
                rows.append({loss: p1(loss) * p2(stage2[c], loss) for loss in (0.01, 0.02, 0.05)})
            screen = 0.045 if kind == "current" else alpha1
            stage2[c] = (stage2[c], 1 - (1 - screen) ** h_case)
        out = {"power >= 90 %": {f"{loss:.0%}": round(sum(r[loss] >= 0.9 for r in rows) / len(rows), 3)
                                 for loss in (0.01, 0.02, 0.05)}, "hours": {}}
        first = 2 if kind == "current" else blocks1
        for host, paths in hosts.items():
            seconds = {c: e["seconds"] for c, e in case_noise(paths).items()}
            total = sum(6 * (first * seconds.get(c, 0) + p * n * seconds.get(c, 0)) for c, (n, p) in stage2.items())
            declared = sum(6 * s for c, s in seconds.items() if c not in timed)  # status-only: block 1
            out["hours"][host] = round((total + declared) / 3600, 1)
        out["unpowered cases"] = sum(n == 0 for n, _ in stage2.values())
        n_big = st.median(n for n, _ in stage2.values())
        p_big = st.median(p for _, p in stage2.values())
        out["hours"]["khaleesi-wsl (est.)"] = round(sum(6 * s * (first + p_big * n_big) for s in big.values()) / 3600, 1)
        return out

    per_round, primary = aa_splits(rounds, splits, seed)
    series = sorted(set.intersection(*(set(s) for s in per_round[0])))
    prim = sorted(set.intersection(*(set(s) for s in primary[0])))

    def false_blocks(alpha1, correct, gate_all):
        hits = 0
        for i in range(splits):
            p1 = {**{h: per_round[0][i].get(h, 1.0) for h in series}, **{h: primary[0][i].get(h, 1.0) for h in prim}}
            p2 = {**{h: per_round[1][i].get(h, 1.0) for h in series}, **{h: primary[1][i].get(h, 1.0) for h in prim}}
            gating = set(series) | set(prim) if gate_all else set(prim)
            flagged = {h: p2[h] for h in gating if p1[h] <= alpha1}
            hits += bool(flagged and correct(flagged))
        return round(hits / splits, 4)

    out = {"m": {"primary": m_primary, "all": m_all}, "blocks1": blocks1, "cap": cap, "designs": {}}
    out["designs"]["current (rounds, Holm over all)"] = {**design("current", gate_all=True),
                                                         "false blocks (A/A)": "aa: family-wise false losses"}
    for alpha1 in (0.1, 0.2):
        for gate_all, label in ((False, "primary gates"), (True, "everything gates")):
            d = design("two-stage", alpha1, gate_all)
            d["false blocks (A/A)"] = {"Holm": false_blocks(alpha1, gs.holm, gate_all),
                                       "BH": false_blocks(alpha1, bh, gate_all)}
            out["designs"][f"screen alpha1 {alpha1}, {label}"] = d
    return out


def main() -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    s = sub.add_parser("noise")
    s.add_argument("reports", type=Path, nargs="+")
    s.add_argument("--min-ms", type=float, default=0.005)
    s.add_argument("--out", type=Path)
    s = sub.add_parser("layout")
    s.add_argument("reports", type=Path, nargs="+")
    s.add_argument("--min-ms", type=float, default=0.005)
    s.add_argument("--out", type=Path)
    s = sub.add_parser("power")
    s.add_argument("noise", type=Path)
    s.add_argument("--m", type=int, required=True, help="the declared family's size (release_gate.py plan)")
    s.add_argument("--blocks", type=int, default=2, help="blocks a round, a case without --measure's")
    s.add_argument("--measure", type=Path, help="blocks per case (benches/fast/measure.toml)")
    s.add_argument("--out", type=Path)
    s = sub.add_parser("designs")
    s.add_argument("rounds", type=Path, nargs=2, help="A/A passes: the screen's, then the confirmation's")
    s.add_argument("--host", action="append", default=[], metavar="NAME=DIR", help="a host's passes, for its hours")
    s.add_argument("--big-seconds", type=float, default=60.0, help="a run of each big case on khaleesi-wsl (est.)")
    s.add_argument("--blocks1", type=int, default=3)
    s.add_argument("--splits", type=int, default=300)
    s.add_argument("--seed", type=int, default=1)
    s.add_argument("--out", type=Path)
    s = sub.add_parser("aa")
    s.add_argument("rounds", type=Path, nargs="+", help="one directory of A/A passes per round, in order")
    s.add_argument("--splits", type=int, default=300)
    s.add_argument("--seed", type=int, default=1)
    s.add_argument("--out", type=Path)
    args = p.parse_args()
    if args.command == "noise":
        rows = noise(args.reports, args.min_ms)
        print(json.dumps(summary(rows), indent=1))
        if args.out:
            args.out.write_text(json.dumps(rows, indent=0), encoding="utf-8")
        return 0
    if args.command == "layout":
        rows = layout_components(args.reports, args.min_ms)
        print(json.dumps(layout_summary(rows), indent=1))
        if args.out:
            args.out.write_text(json.dumps(rows, indent=0), encoding="utf-8")
        return 0
    if args.command == "designs":
        hosts = {k: sorted(Path(v).glob("pass-*.json")) for k, v in (h.split("=", 1) for h in args.host)}
        big = {c: args.big_seconds for c in ("sparqloscope-dblp", "bsbm-explore-100m", "bsbm-bi-100m")}
        out = designs([sorted(d.glob("pass-*.json")) for d in args.rounds], args.splits, args.seed, hosts, big,
                      args.blocks1)
        print(json.dumps(out, indent=1))
        if args.out:
            args.out.write_text(json.dumps(out, indent=1), encoding="utf-8")
        return 0
    if args.command == "aa":
        out = aa([sorted(d.glob("pass-*.json")) for d in args.rounds], args.splits, args.seed)
        print(json.dumps(out, indent=1))
        if args.out:
            args.out.write_text(json.dumps(out, indent=1), encoding="utf-8")
        return 0
    rows = json.loads(args.noise.read_text(encoding="utf-8"))
    blocks = None
    if args.measure:
        import tomllib
        blocks = {c: int(e.get("blocks", args.blocks)) for c, e in
                  tomllib.loads(args.measure.read_text(encoding="utf-8")).get("case", {}).items()}
    out = power(rows, args.m, blocks, args.blocks)
    print(json.dumps(out, indent=1))
    if args.out:
        args.out.write_text(json.dumps(out, indent=1), encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())
