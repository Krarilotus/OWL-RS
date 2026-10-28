"""NRESE against each comparator of a run, judged as `compare` judges two runs: answers
first, then time over what both answered right, then load and memory by kind. The run
records carry these entries (`versus`), and `suite.py standings` sets them side by side.

    versus(directory)   the entries of one suite run (results.csv, answers.json beside it), or
                        of a batch: the runs in it pooled (a queue job's chunks, one
                        system or repetition each), each run's repetitions its own

An entry, per case (or workload tier) and comparator:

| Field | Meaning |
|---|---|
| `case`, `semantics`, `system`, `run`, `home_turf` | what ran, under what, against whom, in which run (a track is a run of its own); whose home turf the case is |
| `answers` | the comparator's answers against the references: `ok`, or what is wrong, missing or `unchecked` (no reference answered) and whose (NRESE's own answers are judged alike); only queries both answered right are timed |
| `timed` | queries both answered right and completely, of those both ran; for a rule engine, `reason` (its materialisation), timed only where its relation counts (`count:REL`) passed their checks |
| `time` | NRESE / comparator: the sum of the timed queries' medians, its 95 % bootstrap interval |
| `verdict` | `win` (NRESE faster beyond max(10 %, spread) and 2 ms, the interval below 1), `loss` (the other way round), `tie`, or `no-verdict` (nothing timed both answered right) |
| `load`, `memory` | NRESE / comparator: the median load time; the median serve peak, with its kind |

A ratio below 1 is NRESE ahead. Cache modes are compared like with like: NRESE with its
result cache off against a system's off, or against one without a cache.
"""
from __future__ import annotations

import json
import statistics
from collections import defaultdict
from pathlib import Path

from .answers import timed as answer_timed, timing_verdict
from .compare import bootstrap_ratio, thin
from .schema import read

THRESHOLD, MIN_MS = 0.10, 2.0


def number(text) -> float | None:
    try:
        return float(text)
    except (TypeError, ValueError):
        return None


def answer_verdicts(directory: Path) -> dict[tuple, str]:
    """(tier, query file stem, system) -> the answer verdict, from answers.json; `unchecked`
    where no reference answered (the answer may be right; nothing showed it). A batch's
    captures are classified together: the references may have answered in another run."""
    from .ledger import run_directories
    paths = [d / "answers.json" for d in run_directories(directory) if (d / "answers.json").exists()]
    if not paths:
        return {}
    from .answers import Recorder
    from .summary import adjudications
    data = [json.loads(path.read_text(encoding="utf-8")) for path in paths]
    if len(paths) > 1 and all("captures" in d for d in data):
        found = Recorder.load(paths, None, adjudications()).verdicts()
    else:
        # Captures without their classification: classify them (answers.py).
        found = [v for d, path in zip(data, paths)
                 for v in (d["verdicts"] if "verdicts" in d else Recorder.load([path], None, adjudications()).verdicts())]
    return {(v["tier"], Path(v["query"]).stem, v["system"]): timing_verdict(v) for v in found}


def side(rows: list[dict], cache: str) -> dict:
    """One system's rows of one tier: per run its ok query latencies in the cache mode
    `cache` (or without a cache); the statuses per query; load times; serve peaks and their
    kind."""
    out = {"runs": defaultdict(dict), "status": defaultdict(set), "checks": defaultdict(set), "load": [], "peaks": [],
           "kind": set(), "publish": "free"}
    for r in rows:
        out["publish"] = r.get("publish") or out["publish"]
        if (r["task"] == "query" and not r["item"].startswith("clients-") and r["item"] != "*"
                and (r.get("cache") or "-") in (cache, "-")):
            out["status"][r["item"]].add(r["status"])
            ms = number(r["ms"])
            if r["status"] == "ok" and ms is not None and str(r["repeat"]) not in ("0", ""):
                out["runs"][r["run"]].setdefault(r["item"], []).append(ms)
        elif r["task"] == "reason":
            # A rule engine's materialisation: one timed item per run, as the engine reports it.
            out["status"]["reason"].add(r["status"])
            ms = number(r["ms"])
            if r["status"] == "ok" and ms is not None:
                out["runs"][r["run"]].setdefault("reason", []).append(ms)
        elif r["task"] == "count" and r["item"]:
            # A derived relation's count against the known one: a check, never timed.
            out["checks"][r["item"]].add(r["status"])
        elif r["task"] == "load" and r["status"] == "ok" and number(r["ms"]) is not None:
            out["load"].append(number(r["ms"]))
        elif r["task"] == "serve" and number(r["peak_mib"]) is not None:
            out["peaks"].append(number(r["peak_mib"]))
            out["kind"].add(r.get("memory_kind") or ("sampled" if r.get("runtime") == "docker" else "lifetime"))
    return out


def ratio(mine: list[float], theirs: list[float]) -> float | None:
    return round(statistics.median(mine) / statistics.median(theirs), 3) if mine and theirs and statistics.median(theirs) else None


def judge(nrese: dict, other: dict, timed: list[str]) -> tuple[str, dict]:
    """The time verdict over `timed`: compare's rule with the comparator as the base."""
    reps = [[{q: thin(sorted(run[q])) for q in timed if run.get(q)} for run in s["runs"].values()] for s in (other, nrese)]
    reps = [[rep for rep in side_reps if set(rep) == set(timed)] for side_reps in reps]
    if not timed or not all(reps):
        return "no-verdict", {}
    point, low, high = bootstrap_ratio(reps[0], reps[1])
    sums = [[sum(statistics.median(rep[q]) for q in timed) for rep in side_reps] for side_reps in reps]
    spread = max((max(s) - min(s)) / statistics.median(s) if len(s) > 1 and statistics.median(s) else 0.0 for s in sums)
    threshold = max(THRESHOLD, spread)
    theirs, ours = statistics.median(sums[0]), statistics.median(sums[1])
    verdict = "tie"
    if high < 1.0 and point <= 1 / (1 + threshold) and theirs - ours >= MIN_MS:
        verdict = "win"
    elif low > 1.0 and point >= 1 + threshold and ours - theirs >= MIN_MS:
        verdict = "loss"
    return verdict, {"ratio": round(point, 3), "ci": [round(low, 3), round(high, 3)],
                     "threshold": round(threshold, 3), "nrese_ms": round(ours, 3), "their_ms": round(theirs, 3)}


def versus(directory: Path, home_turfs: dict[str, str] | None = None) -> list[dict]:
    """The entries of one suite run: each comparator against NRESE on each tier they share."""
    from .ledger import run_directories
    runs = run_directories(directory) if directory.is_dir() else []
    # Runs of a batch number their repetitions alike: each keeps its own.
    rows = [r | {"run": f"{run.name}:{r['run']}"} if len(runs) > 1 else r for run in runs for r in read(run / "results.csv")]
    verdicts = answer_verdicts(directory)
    by: dict[tuple, list[dict]] = defaultdict(list)
    for r in rows:
        if r["status"] != "skipped":
            by[(r["workload"], r["tier"], r["system"])].append(r)
    out = []
    for (workload, tier, system), mine in sorted(by.items()):
        ours = by.get((workload, tier, "nrese"))
        if system == "nrese" or not ours:
            continue
        # Like with like: result caches off (a system without one has none to turn off).
        nrese, other = side(ours, "off"), side(mine, "off")
        case, _, semantics = tier.partition("~")
        name = case if workload == "fast" else f"{workload} {tier}"
        both = sorted(set(nrese["status"]) & set(other["status"]))
        problems = []
        for who, s in (("nrese", nrese), (system, other)):
            for q in both:
                verdict = verdicts.get((tier, Path(q).stem, who))
                if not answer_timed(verdict):
                    problems.append(f"{q} {verdict} ({who})")
                elif s["status"][q] != {"ok"}:
                    problems.append(f"{q} {'/'.join(sorted(s['status'][q] - {'ok'}))} ({who})")
            # Answers before time: a rule engine whose counts failed their check isn't timed.
            failed = sorted(c for c, statuses in s["checks"].items() if statuses != {"ok"})
            for c in failed:
                problems.append(f"{c} {'/'.join(sorted(s['checks'][c] - {'ok'}))} ({who})")
            if failed and "reason" in both:
                problems.append(f"reason unchecked ({who})")
        bad = {p.split()[0] for p in problems}
        timed = [q for q in both if q not in bad]
        verdict, time = judge(nrese, other, timed)
        entry = {"case": name, "semantics": semantics or "-", "system": system, "run": directory.name,
                 "home_turf": (home_turfs or {}).get(case, ""), "publish": other["publish"],
                 "answers": "ok" if not problems else "; ".join(problems[:6]) + (" ..." if len(problems) > 6 else ""),
                 "checked": "answers.json" if verdicts else "counts only",
                 "timed": f"{len(timed)}/{len(both)}", "verdict": verdict, **({"time": time} if time else {})}
        load = ratio(nrese["load"], other["load"])
        if load is not None:
            entry["load"] = load
        peak = ratio(nrese["peaks"], other["peaks"])
        if peak is not None:
            kinds = nrese["kind"] | other["kind"]
            entry["memory"] = {"ratio": peak, "kind": "/".join(sorted(kinds)) or "unknown"}
        out.append(entry)
    return out
