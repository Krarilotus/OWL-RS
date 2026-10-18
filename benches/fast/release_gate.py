#!/usr/bin/env python3
"""The release gate (benches/PROTOCOL.md §5, "The release gate"): does the candidate regress,
beyond noise, on any workload, in time or memory, against each required baseline? And are
the §0 guards green and the W3C suites' counts what they must be?

    release_gate.py plan --max-gb GB [--cases A,B]
        the cases a host giving GB can run at their declared caps; the others, with why
    release_gate.py report --passes DIR --candidate NAME --side NAME=REV ... --require NAME,...
                           --harness-revision REV [--min-passes 4] [--diagnostic]
                           [--not-run CASE=WHY] [--waive CASE=WHY] [--out MD] [--json JSON]
        the timed part: DIR holds the passes, pass-N-SIDE.json, each a full `fast.py run
        --reps 1 --prepared` report of one side's binaries over the same harness and data
    release_gate.py w3c LOGDIR --expected benches/fast/w3c-expected.toml [--json JSON]
    release_gate.py guards --names NAMES --log LOG [--json JSON]
    release_gate.py verdict --timed JSON [--timed JSON] --w3c JSON --guards JSON [--out MD]

Each subcommand exits 0 only when its part passes; `verdict` only when every part does.
"""
from __future__ import annotations

import argparse
import json
import math
import re
import statistics
import sys
import tomllib
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "benches" / "suite"))
from suitekit import cases as cases_registry  # noqa: E402
from suitekit.compare import FLOORS, MARGIN, RESOLUTION, counter_changes, judge  # noqa: E402
from suitekit.hosts import run_identity  # noqa: E402

CAMPAIGN_PASSES = 4  # per side: A-B-C-C-B-A twice
PASS_FILE = re.compile(r"pass-(\d+)-([A-Za-z0-9_.-]+)\.json$")


# --- plan --------------------------------------------------------------------------------------

def plan(catalog: list[dict], max_gb: float, selection: list[str] | None = None) -> tuple[list[str], dict]:
    """The cases a host giving `max_gb` runs at their declared caps, and the others with why.
    A cap is never lowered to fit (PROTOCOL.md §3)."""
    run, not_run = [], {}
    for case in catalog:
        if selection is not None and case["name"] not in selection:
            continue
        if case["cap_gb"] > max_gb:
            not_run[case["name"]] = f"its cap is {case['cap_gb']:g} GB, the gate's host gives {max_gb:g} GB"
        else:
            run.append(case["name"])
    return run, not_run


# --- passes and their schedule -----------------------------------------------------------------

def load_passes(directory: Path) -> list[dict]:
    passes = []
    for path in sorted(directory.glob("pass-*.json")):
        match = PASS_FILE.search(path.name)
        if match:
            passes.append({"n": int(match.group(1)), "side": match.group(2), "path": path.name,
                           "report": json.loads(path.read_text(encoding="utf-8"))})
    return passes


def schedule_errors(passes: list[dict], sides: list[str], min_passes: int) -> list[str]:
    """Defect 1 (audit, 18 Oct 2026): a gate passed with one baseline and passes whose times
    overlapped. Every side needs at least `min_passes` passes, all sides as many; the passes
    ran one after another (each ended before the next began, no two began at once); their
    order is a palindrome (A-B-B-A, A-B-C-C-B-A ...), so a drift linear in time falls on every
    side alike, and no side ran more than two passes in a row."""
    errors = []
    counts = Counter(p["side"] for p in passes)
    for side in sides:
        if counts[side] < min_passes:
            errors.append(f"{side}: {counts[side]} passes, at least {min_passes} required")
    if extra := set(counts) - set(sides):
        errors.append(f"passes of undeclared sides: {', '.join(sorted(extra))}")
    if len({counts[s] for s in sides}) > 1:
        errors.append("the sides have different numbers of passes: " + ", ".join(f"{s} {counts[s]}" for s in sides))
    timed = []
    for p in passes:
        r = p["report"]
        start, end = r.get("started_ns"), r.get("finished_ns")
        if not isinstance(start, int) or not isinstance(end, int) or not r.get("finished"):
            errors.append(f"{p['path']}: no start or end time (unfinished pass)")
        elif end <= start:
            errors.append(f"{p['path']}: ends before it starts")
        else:
            timed.append((start, end, p))
    timed.sort(key=lambda t: t[0])
    for (s1, e1, a), (s2, e2, b) in zip(timed, timed[1:]):
        if s2 == s1:
            errors.append(f"{a['path']} and {b['path']} started at the same time")
        elif s2 < e1:
            errors.append(f"{a['path']} and {b['path']} overlap: the passes didn't run one at a time")
    if [p["n"] for _, _, p in timed] != sorted(p["n"] for _, _, p in timed):
        errors.append("the passes' numbers don't follow the order they ran in")
    order = [p["side"] for _, _, p in timed]
    if order != order[::-1]:
        errors.append(f"not interleaved A-B-B-A: the passes ran {'-'.join(order)}, not a palindrome")
    run = 1
    for x, y in zip(order, order[1:]):
        run = run + 1 if x == y else 1
        if run > 2:
            errors.append(f"{x} ran more than two passes in a row ({'-'.join(order)})")
            break
    return errors


def provenance_errors(passes: list[dict], sides: dict[str, str], harness: str, planned: list[str]) -> list[str]:
    """What each pass must say about itself: complete, one repetition of exactly the planned
    cases, the clean harness revision at start and end, its side's verified binaries (the same
    in every pass of the side), the same prepared data unchanged, one host and configuration."""
    errors, binaries, prepared, identities, options = [], {}, set(), set(), {}
    for p in passes:
        r, name = p["report"], p["path"]
        if r.get("reps") != 1:
            errors.append(f"{name}: {r.get('reps')} repetitions; a pass is one")
        if sorted(r.get("planned_cases") or []) != sorted(planned):
            errors.append(f"{name}: its cases are not the gate's plan")
        seen = Counter(x.get("case") for x in r.get("records", []))
        if seen != Counter(planned):
            errors.append(f"{name}: records {dict(seen - Counter(planned)) or ''} beyond, "
                          f"{sorted(set(planned) - set(seen))} missing")
        for key in ("git", "git_finished"):
            git = r.get(key) or {}
            if git.get("commit") != harness or git.get("dirty") != 0:
                errors.append(f"{name}: {key}: harness {git.get('commit')} dirty {git.get('dirty')}, "
                              f"expected {harness} clean")
        rev = sides.get(p["side"])
        for key in ("binaries", "binaries_finished"):
            b = r.get(key) or {}
            if not b.get("verified") or b.get("revision") != rev:
                errors.append(f"{name}: {key}: revision {b.get('revision')} verified {b.get('verified')}, "
                              f"expected {rev} verified")
        if (r.get("binaries") or {}).get("sha256") != (r.get("binaries_finished") or {}).get("sha256"):
            errors.append(f"{name}: binaries changed during the pass")
        binaries.setdefault(p["side"], set()).add(json.dumps((r.get("binaries") or {}).get("sha256"), sort_keys=True))
        info = r.get("prepared") or {}
        if not info:
            errors.append(f"{name}: not a prepared pass (fast.py run --prepared)")
        elif info.get("changed"):
            errors.append(f"{name}: prepared data changed during the pass: {info['changed'][:3]}")
        prepared.add(info.get("manifest_sha256"))
        options.setdefault(p["side"], set()).add(json.dumps(r.get("options"), sort_keys=True))
        identities.add(json.dumps({**run_identity(r.get("machine") or {}, r.get("started", "")),
                                   "runtime": r.get("runtime")}, sort_keys=True))
    errors += [f"{side}: different binaries in its passes" for side, shas in binaries.items() if len(shas) > 1]
    errors += [f"{side}: different options in its passes" for side, opts in options.items() if len(opts) > 1]
    if len(prepared) > 1:
        errors.append("the passes read different prepared data")
    if len(identities) > 1:
        errors.append("not one host and configuration: " + " | ".join(sorted(identities)))
    return errors


# --- one case on one side ----------------------------------------------------------------------

def p50(values: list[float]) -> float:
    """The perf lab's own p50: the sorted samples' element at len / 2 (main and v2 alike)."""
    return sorted(values)[len(values) // 2]


def evidence(passes: list[dict], case: str) -> dict:
    """One side's evidence of a case, pass by pass."""
    out = {"status": [], "expected": [], "failed_checks": [], "unchecked": 0, "notes": [], "samples": [],
           "process": [], "estimator": set(), "lifetime": [], "phases": [], "cgroup": [], "qps": [],
           "client_peaks": [], "cache_peaks": [], "rows": [], "counters": {}}
    for p in passes:
        record = next((r for r in p["report"]["records"] if r.get("case") == case), None)
        if record is None:
            out["status"].append("missing")
            out["expected"].append("ok")
            continue
        observations = record.get("observations") or []
        out["status"].append(record.get("status"))
        out["expected"].append(record.get("expected_outcome", "ok"))
        checks = record.get("checks", []) + record.get("routes", []) + \
            [c for o in observations for c in o.get("checks", []) + o.get("routes", [])]
        out["failed_checks"] += [f"{c.get('path')}: expected {c.get('expected')}, got {c.get('got')}"
                                 for c in checks if c.get("ok") is False]
        out["unchecked"] += sum(c.get("ok") is None for c in checks)
        out["notes"] += [n for n in record.get("notes", []) if n not in out["notes"]]
        out["estimator"] |= {o.get("estimator", "samples") for o in observations} or {"samples"}
        out["samples"].append(record.get("samples") or {})
        process: dict[str, list[float]] = {}
        for o in observations:
            for series, values in (o.get("samples") or {}).items():
                if values:
                    process.setdefault(series, []).append(p50(values))
        out["process"].append(process)
        peaks = [o.get("peak_mib") for o in observations if o.get("peak_mib")] or \
            ([record["peak_mib"]] if record.get("peak_mib") else [])
        out["lifetime"].append(max(peaks) if peaks else None)
        phases: dict[str, float] = {}
        for o in observations or [record]:
            for phase, peak in (o.get("phase_peaks") or {}).items():
                if peak:
                    phases[phase] = max(phases.get(phase, 0), peak)
        out["phases"].append(phases)
        cgroups = [o.get("cgroup_peak_mib") for o in observations if o.get("cgroup_peak_mib")]
        out["cgroup"].append(max(cgroups) if cgroups else record.get("cgroup_peak_mib"))
        widths: dict[str, list[float]] = {}
        clients: dict[str, list[float]] = {}
        for o in observations or [record]:
            for level in o.get("throughput") or []:
                if level.get("qps") is not None:
                    widths.setdefault(str(level.get("clients")), []).append(level["qps"])
                if level.get("peak_mib") is not None:
                    clients.setdefault(str(level.get("clients")), []).append(level["peak_mib"])
        out["qps"].append(widths)
        out["client_peaks"].append(clients)
        out["cache_peaks"].append({mode: [v for v in values if v] for mode, values in
                                   (record.get("cache_peaks") or {}).items()})
        rows = {}
        for o in observations:
            rows.update({q: n for q, n in (o.get("rows") or {}).items() if n is not None})
        out["rows"].append(rows)
        for path, value in (record.get("counters") or {}).items():
            out["counters"].setdefault(path, []).append(value)
    return out


def state(e: dict) -> str:
    """ok (every pass ok), known (every pass the declared outcome, not ok), or failed."""
    if e["status"] and all(s == "ok" for s in e["status"]) and not e["failed_checks"]:
        return "ok"
    if e["status"] and all(s == x != "ok" for s, x in zip(e["status"], e["expected"])):
        return "known"
    return "failed"


CAPABILITY = re.compile(r"unknown argument (\S+)|unknown ruleset (\S+)|No such file or dir|"
                        r"--queries DIR is required|examples/\S+: not found|not found")


def why_not(e: dict, side: str) -> str:
    """A baseline's case that isn't ok, in words: what it lacks, or how it ended."""
    statuses = ", ".join(f"{s} {n}" for s, n in Counter(e["status"]).items())
    for note in e["notes"]:
        if match := CAPABILITY.search(note):
            return f"{side} can't run it ({match.group(0)}; {statuses})"
    if e["failed_checks"]:
        return f"{side}'s answer is wrong ({e['failed_checks'][0]}; {statuses})"
    return f"{side}: {statuses}" + (f" ({e['notes'][-1][:120]})" if e["notes"] else "")


def per_pass(values: list, key: str | None = None) -> list[list[float]]:
    """A dimension's values as judge() takes them: per pass, a list."""
    out = []
    for v in values:
        v = v.get(key) if key is not None else v
        out.append(v if isinstance(v, list) else ([] if v is None else [v]))
    return out


def compare_case(cand: dict, base: dict, side: str, checked_queries: set[str]) -> dict:
    """The candidate's case against one baseline's, dimension by dimension."""
    row = {"comparable": True, "dimensions": {}, "notes": [], "blocks": [], "reported": {}}
    if state(base) == "known" and state(cand) == "known":
        return {**row, "comparable": False, "reason": "a known issue on both sides (no timing verdict)"}
    if state(cand) == "known":
        if state(base) == "ok":
            # The candidate stops at its declared limit where the baseline finished: that is a
            # loss, whatever the declaration says.
            row["blocks"].append(f"stops at its declared outcome ({', '.join(sorted(set(cand['status'])))}) "
                                 f"where {side} completed")
        return {**row, "comparable": False, "reason": "the candidate's known issue"}
    if state(base) != "ok":
        return {**row, "comparable": False, "reason": why_not(base, side)}
    # Answers: each query's rows on both sides (every side reports them, main's perf lab too).
    differ = sorted(q for q in set(cand["rows"][0] if cand["rows"] else {}) &
                    set(base["rows"][0] if base["rows"] else {})
                    if {r.get(q) for r in cand["rows"]} != {r.get(q) for r in base["rows"]})
    excluded = set()
    for q in differ:
        text = (f"{q}: rows {sorted({r.get(q) for r in cand['rows']})} here, "
                f"{sorted({r.get(q) for r in base['rows']})} on {side}")
        excluded.add(q)
        if q in checked_queries:
            row["notes"].append(f"{text}: the candidate's match its pinned answer; {side}'s time not compared")
        else:
            row["blocks"].append(f"{text}: answers differ and no check pins them")
    adapted = "process-p50" in cand["estimator"] | base["estimator"]
    if adapted:
        row["notes"].append(f"times as one p50 per process on both sides ({side}'s perf lab reports no samples)")
    values = {"cand": cand["process" if adapted else "samples"], "base": base["process" if adapted else "samples"]}
    series = sorted({s for rep in values["cand"] + values["base"] for s in rep})
    for s in series:
        if s in excluded:
            continue
        on_cand = all(s in rep for rep in values["cand"])
        on_base = all(s in rep for rep in values["base"])
        if not on_cand and on_base:
            row["blocks"].append(f"time/{s}: lost (on {side}, not on every pass of the candidate)")
            continue
        if not on_base:
            row["notes"].append(f"time/{s}: not on every pass of {side}, not compared")
            continue
        row["dimensions"][f"time/{s}"] = judge(per_pass(values["base"], s), per_pass(values["cand"], s), unit="ms")
    if not series:
        row["blocks"].append("no timing evidence")
    # Memory (one policy, PROTOCOL.md §5): the process's own peak (VmHWM, every side's perf lab)
    # and each phase's are judged; the scope's cgroup peak, which also counts the page cache
    # the case's reads were charged, is reported beside them, never judged.
    row["dimensions"]["memory/lifetime"] = judge(per_pass(base["lifetime"]), per_pass(cand["lifetime"]), unit="MiB")
    if any(cand["phases"]) and not any(base["phases"]):
        row["notes"].append(f"phase peaks: {side}'s perf lab reports none, not compared")
    for name, key, unit, higher in (("memory/phase", "phases", "MiB", False), ("qps", "qps", "qps", True),
                                    ("memory/clients", "client_peaks", "MiB", False),
                                    ("memory/cache", "cache_peaks", "MiB", False)):
        for k in sorted({k for rep in cand[key] + base[key] for k in rep}):
            on_cand, on_base = (all(k in rep for rep in e[key]) for e in (cand, base))
            if on_base and not on_cand:
                row["blocks"].append(f"{name}/{k}: lost (on {side}, not on every pass of the candidate)")
            elif on_base:
                row["dimensions"][f"{name}/{k}"] = judge(per_pass(base[key], k), per_pass(cand[key], k),
                                                         unit=unit, higher_is_better=higher)
            elif any(base[key]):
                row["notes"].append(f"{name}/{k}: not on every pass of {side}, not compared")
    cgroups = [[v for v in e["cgroup"] if v] for e in (base, cand)]
    row["reported"]["cgroup_peak_mib"] = [statistics.median(v) if v else None for v in cgroups]
    common = {k: v for k, v in base["counters"].items() if k in cand["counters"]}
    grew, changes = counter_changes(common, {k: cand["counters"][k] for k in common})
    if changes:
        row["reported"]["work_counters"] = changes
    row["blocks"] += [f"{d}: {r['verdict']} ({describe(r)})" for d, r in row["dimensions"].items() if r["blocks"]]
    return row


def describe(r: dict) -> str:
    if r.get("ratio") is None:
        return f"{r['passes'][0]} and {r['passes'][1]} passes with values"
    ci = f" [{r['ci'][0]:.3f}, {r['ci'][1]:.3f}]" if r.get("ci") else ""
    return (f"{fmt(r['base'])} -> {fmt(r['new'])} {r['unit']}, ratio {r['ratio']:.3f}{ci}, "
            f"spread {r['spread']:.1%}, margin {r['margin']:.1%}")


def fmt(x) -> str:
    return "-" if x is None else f"{x:.4g}"


# --- the timed part ----------------------------------------------------------------------------

def gate(passes: list[dict], sides: dict[str, str], candidate: str, required: list[str], harness: str,
         min_passes: int = CAMPAIGN_PASSES, diagnostic: bool = False, not_run: dict | None = None,
         waived: dict | None = None, catalog: list[dict] | None = None) -> dict:
    not_run, waived = dict(not_run or {}), dict(waived or {})
    if candidate not in sides:
        raise ValueError(f"the candidate {candidate} is no declared side")
    baselines = [s for s in sides if s != candidate]
    summary = {"candidate": {"name": candidate, "revision": sides[candidate]},
               "baselines": {s: sides[s] for s in baselines}, "harness": harness,
               "policy": {"margin": MARGIN, "floors": FLOORS, "resolution": RESOLUTION},
               "passes": [{"n": p["n"], "side": p["side"], "file": p["path"],
                           "started": p["report"].get("started"), "finished": p["report"].get("finished")}
                          for p in sorted(passes, key=lambda p: p["n"])],
               "diagnostic": diagnostic or min_passes < CAMPAIGN_PASSES, "required": required,
               "not_run": not_run, "waived": waived, "errors": [], "candidate_failures": {},
               "known_issues": {}, "fixed": {}, "comparisons": {}, "blocking": []}
    if missing := [s for s in required if s not in baselines]:
        summary["errors"].append(f"required baselines missing: {', '.join(missing)}")
    if len(baselines) < 1:
        summary["errors"].append("no baseline")
    summary["errors"] += schedule_errors(passes, list(sides), min_passes)
    planned = sorted({c for p in passes for c in p["report"].get("planned_cases", [])})
    summary["errors"] += provenance_errors(passes, sides, harness, planned)
    summary["host"] = sorted({(p["report"].get("machine") or {}).get("host") for p in passes}, key=str)
    by_side = {s: sorted([p for p in passes if p["side"] == s], key=lambda p: p["n"]) for s in sides}
    checks = {c["name"]: {path.split(".")[1] for path in c.get("check", {}) if path.startswith("q.")}
              for c in (catalog or [])}
    for case in planned:
        cand = evidence(by_side[candidate], case)
        if state(cand) == "failed":
            summary["candidate_failures"][case] = (f"statuses {', '.join(map(str, cand['status']))}; "
                                                   + "; ".join(cand["failed_checks"][:3] + cand["notes"][-1:]))
        elif state(cand) == "known":
            summary["known_issues"][case] = ", ".join(sorted(set(cand["status"])))
        elif any(x != "ok" for x in cand["expected"]):
            summary["fixed"][case] = ", ".join(sorted(set(cand["expected"]) - {"ok"}))
        if cand["unchecked"] and state(cand) != "failed":
            summary["candidate_failures"][case] = f"{cand['unchecked']} checks unchecked on the candidate"
        for side in baselines:
            if state(cand) == "failed":
                continue
            row = compare_case(cand, evidence(by_side[side], case), side, checks.get(case, set()))
            summary["comparisons"].setdefault(side, {})[case] = row
    for case, why in summary["candidate_failures"].items():
        summary["blocking"].append(f"{case}: the candidate fails ({why})")
    for case, why in not_run.items():
        summary["blocking"].append(f"{case}: not run ({why})")
    for side, rows in summary["comparisons"].items():
        for case, row in rows.items():
            summary["blocking"] += [f"{case} against {side}: {b}" for b in row["blocks"]]
    for case, why in waived.items():
        kept = [b for b in summary["blocking"] if not b.startswith(f"{case}:") and not b.startswith(f"{case} against")]
        if len(kept) < len(summary["blocking"]):
            summary.setdefault("waived_blocks", {})[case] = why
        summary["blocking"] = kept
    summary["counts"] = {side: dict(Counter(r["verdict"] for row in rows.values()
                                            for r in row["dimensions"].values()))
                         for side, rows in summary["comparisons"].items()}
    summary["passed"] = not summary["diagnostic"] and not summary["errors"] and not summary["blocking"]
    return summary


def render(summary: dict) -> str:
    c = summary["candidate"]
    verdict = "PASSED" if summary["passed"] else ("DIAGNOSTIC ONLY, not a release verdict" if summary["diagnostic"]
                                                  else "NOT PASSED")
    lines = [f"# Release gate: {c['name']} {c['revision']}", "",
             f"Against {', '.join(f'{s} {r}' for s, r in summary['baselines'].items())}; harness {summary['harness']}; "
             f"host {', '.join(map(str, summary['host']))}; {len(summary['passes'])} passes "
             f"({' '.join(p['side'] for p in summary['passes'])}).", "",
             f"**{verdict}.** Policy: a dimension blocks beyond max({MARGIN:.0%}, its measured spread) with the "
             "bootstrap interval above 1 (regression) or reaching 1 (suspect); below 0.5 ms or 64 MiB only "
             "with the whole interval beyond twice the margin; missing evidence blocks (benches/PROTOCOL.md §5).", ""]
    if summary["errors"]:
        lines += ["## The passes don't qualify", ""] + [f"- {e}" for e in summary["errors"]] + [""]
    lines += ["## Blocking", ""] + ([f"- {b}" for b in summary["blocking"]] or ["- nothing"]) + [""]
    if summary.get("waived_blocks"):
        lines += ["## Waived (the owner's decision)", ""] + \
                 [f"- {k}: {v}" for k, v in summary["waived_blocks"].items()] + [""]
    if summary["known_issues"]:
        lines += ["Known issues as declared (no timing verdict): " +
                  ", ".join(f"{k} ({v})" for k, v in summary["known_issues"].items()), ""]
    if summary["fixed"]:
        lines += ["Declared issues the candidate now passes (update their declaration): " +
                  ", ".join(f"{k} ({v})" for k, v in summary["fixed"].items()), ""]
    for side, rows in summary["comparisons"].items():
        counts = summary["counts"].get(side, {})
        lines += [f"## Against {side} {summary['baselines'][side]}", "",
                  f"{len(rows)} cases; dimensions: " + ", ".join(f"{n} {v}" for v, n in sorted(counts.items())), "",
                  "| case / dimension | verdict | base | new | ratio | 95 % interval | spread | margin |",
                  "|---|---|---|---|---|---|---|---|"]
        for case, row in rows.items():
            for d, r in row["dimensions"].items():
                if r["verdict"] != "neutral" or r.get("below_floor"):
                    ci = f"{r['ci'][0]:.3f}-{r['ci'][1]:.3f}" if r.get("ci") else "-"
                    ratio = "-" if r.get("ratio") is None else f"{r['ratio']:.3f}"
                    floor = " (below floor)" if r.get("below_floor") else ""
                    lines.append(f"| {case} / {d} | {r['verdict']}{floor} | {fmt(r['base'])} | "
                                 f"{fmt(r['new'])} {r['unit']} | {ratio} | {ci} | {fmt(r.get('spread'))} | "
                                 f"{fmt(r.get('margin'))} |")
        lines.append("")
        not_compared = {k: v["reason"] for k, v in rows.items() if not v["comparable"]}
        if not_compared:
            lines += [f"Not compared against {side} ({len(not_compared)}):", ""] + \
                     [f"- {k}: {v}" for k, v in not_compared.items()] + [""]
        notes = [(k, n) for k, v in rows.items() for n in v["notes"]]
        if notes:
            lines += ["Notes:", ""] + [f"- {k}: {n}" for k, n in notes] + [""]
    return "\n".join(lines) + "\n"


# --- W3C and guards ----------------------------------------------------------------------------

def w3c_counts(logs: Path) -> dict[str, dict]:
    """Each suite's counts from the W3C kits' logs (`-- --nocapture`) and the OWL 2 DL TSV."""
    out = {}
    for log in sorted(logs.glob("*.log")):
        text = log.read_text(encoding="utf-8", errors="replace")
        title = None
        for line in text.splitlines():
            if m := re.match(r"W3C (.+) on engine v2: passed / failed / skipped", line):
                title = m.group(1)
            elif (m := re.match(r"\s+total: (\d+) / (\d+) / (\d+)", line)) and title:
                out[title] = {"passed": int(m[1]), "failed": int(m[2]), "skipped": int(m[3])}
                title = None
            elif m := re.match(r"(W3C SHACL[^:]*): (\d+) of (\d+) pass", line):
                out[m[1].removeprefix("W3C ")] = {"passed": int(m[2]), "failed": int(m[3]) - int(m[2]), "skipped": 0}
            elif m := re.match(r"W3C (OWL 2 RL)[^:]*: (\d+) passed, (\d+) failed \((\d+) expected\), (\d+) skipped", line):
                out[m[1]] = {"passed": int(m[2]), "failed": int(m[3]) - int(m[4]), "skipped": int(m[5])}
            elif m := re.match(r"GeoSPARQL compliance benchmark: (\d+) of (\d+) queries match the reference, "
                               r"(\d+) don't \((\d+) disputed\)", line):
                out["GeoSPARQL"] = {"passed": int(m[1]), "failed": int(m[3]) - int(m[4]), "disputed": int(m[4])}
            elif m := re.match(r"test result: (ok|FAILED)\. (\d+) passed; (\d+) failed", line):
                out.setdefault("test results", {"passed": 0, "failed": 0})
                out["test results"]["passed"] += int(m[2])
                out["test results"]["failed"] += int(m[3])
    for tsv in sorted(logs.glob("*.tsv")):
        verdicts = Counter(line.split("\t")[3] for line in tsv.read_text(encoding="utf-8").splitlines()
                           if line.count("\t") >= 3)
        out["OWL 2 DL"] = {"passed": verdicts.pop("pass", 0), "disputed": verdicts.pop("disputed", 0),
                           "failed": sum(verdicts.values()), "other": dict(verdicts)}
    return out


def w3c_check(counts: dict, expected: dict) -> list[str]:
    """Every expected suite present (its data was there: a suite without it fails, never
    skips), no failure, at least its passes, at most its known skips and disputes."""
    errors = []
    for suite, want in expected.items():
        got = counts.get(suite)
        if got is None:
            errors.append(f"{suite}: no counts (its kit didn't run, or its data was missing)")
            continue
        if got.get("failed", 0) > want.get("failed", 0):
            errors.append(f"{suite}: {got['failed']} failed")
        if got.get("passed", 0) < want["passed"]:
            errors.append(f"{suite}: {got.get('passed', 0)} passed, at least {want['passed']} required")
        for key in ("skipped", "disputed"):
            if got.get(key, 0) > want.get(key, 0):
                errors.append(f"{suite}: {got[key]} {key}, at most {want.get(key, 0)} known")
    if counts.get("test results", {}).get("failed"):
        errors.append(f"{counts['test results']['failed']} W3C test functions failed")
    return errors


GUARD = re.compile(r"`([a-z][a-z0-9]*(?:_[a-z0-9]+){2,})`")


def guard_names(performance_md: str) -> list[str]:
    """The guards §0 of docs/design/performance.md names: test functions in its Guard column."""
    section = performance_md.split("## 0.", 1)[1].split("\n## 1.", 1)[0]
    names = []
    for line in section.splitlines():
        cells = line.split("|")
        if len(cells) > 4 and not set(cells[3].strip()) <= {"-"}:
            names += [n for n in GUARD.findall(cells[3]) if n not in names]
    return names


def guards_check(names: list[str], log: str) -> tuple[list[str], dict]:
    """Each guard ran (as a whole test name or its last segment) and passed; ignored is not run."""
    results: dict[str, list[str]] = {}
    for m in re.finditer(r"^test (\S+) \.\.\. (ok|FAILED|ignored)", log, re.M):
        results.setdefault(m.group(1).rsplit("::", 1)[-1], []).append(m.group(2))
    errors = []
    for name in names:
        got = results.get(name)
        if not got:
            errors.append(f"{name}: no test of that name ran (named in performance.md §0)")
        elif any(s != "ok" for s in got):
            errors.append(f"{name}: {', '.join(got)}")
    return errors, {n: results.get(n, []) for n in names}


# --- the fleet jobs ----------------------------------------------------------------------------

BASE = "0f465633d33374bc56ab9ec33157cfa10ebf2d40"  # v1.5, the last recorded run's base
MAIN = "f9b515e81f013ecd8c3811f3e9c65e91450ee44c"  # main


def schedule(passes_per_side: int) -> list[str]:
    """A-B-C-C-B-A blocks: base, main, candidate, then mirrored; each block two passes a side."""
    if passes_per_side < 2 or passes_per_side % 2:
        raise ValueError("passes per side: an even number, at least 2")
    return ["base", "main", "cand", "cand", "main", "base"] * (passes_per_side // 2)


def toml_job(job: dict) -> str:
    def value(v):
        if isinstance(v, bool):
            return "true" if v else "false"
        if isinstance(v, (int, float)):
            return repr(v)
        if isinstance(v, list):
            return "[" + ", ".join(value(x) for x in v) + "]"
        if isinstance(v, dict):
            return "{ " + ", ".join(f"{k} = {value(x)}" for k, x in v.items()) + " }"
        return json.dumps(v)
    lines = [f"{k} = {value(v)}" for k, v in job.items() if k not in ("requires", "inputs")]
    lines += ["", "[requires]"] + [f"{k} = {value(v)}" for k, v in job["requires"].items()]
    for item in job["inputs"]:
        lines += ["", "[[inputs]]"] + [f"{k} = {value(v)}" for k, v in item.items()]
    return "\n".join(lines) + "\n"


def jobs(candidate: str, harness: str, checkout: str, prefix: str, passes_per_side: int, cases: str | None,
         base: str = BASE, main: str = MAIN, timed_host: str = "phuoc-yu", checks_host: str = "office-pc",
         max_gb: float = 24, checks: bool = True) -> dict[str, dict]:
    """The gate's fleet jobs: binaries and data, then the timed passes and their report, alone
    on the timed host; the W3C suites and the guards on the checks host. No priority is set:
    each job gets its class's (merge-gate)."""
    env = {"GATE_NAME": f"gate-v2-{candidate[:8]}", "GATE_MAX_GB": str(max_gb), "NRESE_QUIET_SLOT": "0",
           "GATE_MIN_PASSES": str(passes_per_side), **({"GATE_CASES": cases} if cases else {})}
    source = [{"name": "checkout", "source": {"git": checkout, "rev": harness}}]
    gate = ["bash", "scripts/release-gate.sh"]
    order = schedule(passes_per_side)
    revs = {"base": base, "main": main, "cand": candidate}
    small = bool(cases)
    out = {f"{prefix}-build": {
        "id": f"{prefix}-build", "group": "release", "kind": "owlrs.build", "priority_class": "merge-gate",
        "est_min": 20 if small else 100, "env": env,
        "steps": [[*gate, "build", rev] for rev in dict.fromkeys([base, main, candidate, harness])]
        + [[*gate, "prepare", harness]],
        "requires": {"hosts": [timed_host], "cores": 16, "ram_gb": 12}, "inputs": source}}
    per_pass = {"base": 1, "main": 1, "cand": 1} if small else {"base": 14, "main": 4, "cand": 14}
    out[f"{prefix}-timed"] = {
        "id": f"{prefix}-timed", "group": "release", "kind": "owlrs.timed", "class": "exclusive",
        "priority_class": "merge-gate", "after": [f"{prefix}-build"], "env": env,
        "est_min": sum(per_pass[s] for s in order) + 2, "stall_after_min": 40,
        "steps": [[*gate, "pass", str(n), side, revs[side], harness] for n, side in enumerate(order, start=1)]
        + [[*gate, "report", harness, f"cand={candidate}", f"base={base}", f"main={main}"], [*gate, "clean"]],
        "requires": {"hosts": [timed_host], "cores": 16, "ram_gb": max_gb}, "inputs": source}
    if checks:
        out[f"{prefix}-checks"] = {
            "id": f"{prefix}-checks", "group": "release", "kind": "owlrs.any", "priority_class": "merge-gate",
            "est_min": 100, "env": {**env, "CARGO_BUILD_JOBS": "16"},
            "steps": [[*gate, "w3c"], [*gate, "guards"], [*gate, "clean"]],
            "requires": {"hosts": [checks_host], "cores": 16, "ram_gb": 12},
            "inputs": [{"name": "checkout", "source": {"git": checkout, "rev": candidate}}]}
    return out


# --- the command line --------------------------------------------------------------------------

def pairs(values: list[str], what: str) -> dict[str, str]:
    out = {}
    for value in values:
        key, sep, rest = value.partition("=")
        if not key or not sep:
            raise SystemExit(f"{what}: NAME=VALUE expected, not {value}")
        out[key] = rest
    return out


def main() -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    s = sub.add_parser("plan")
    s.add_argument("--max-gb", type=float, required=True)
    s.add_argument("--cases")
    s = sub.add_parser("report")
    s.add_argument("--passes", type=Path, required=True)
    s.add_argument("--candidate", required=True)
    s.add_argument("--side", action="append", required=True, metavar="NAME=REV")
    s.add_argument("--require", required=True, help="the baselines the gate needs, comma-separated")
    s.add_argument("--harness-revision", required=True)
    s.add_argument("--min-passes", type=int, default=CAMPAIGN_PASSES)
    s.add_argument("--diagnostic", action="store_true")
    s.add_argument("--not-run", action="append", default=[], metavar="CASE=WHY")
    s.add_argument("--waive", action="append", default=[], metavar="CASE=WHY", help="the owner's decision, recorded")
    s.add_argument("--out", type=Path)
    s.add_argument("--json", type=Path)
    s = sub.add_parser("w3c")
    s.add_argument("logs", type=Path)
    s.add_argument("--expected", type=Path, required=True)
    s.add_argument("--json", type=Path)
    s = sub.add_parser("guards")
    s.add_argument("--names", type=Path, help="one guard per line; default: performance.md §0's")
    s.add_argument("--list-names", action="store_true")
    s.add_argument("--log", type=Path)
    s.add_argument("--json", type=Path)
    s = sub.add_parser("jobs", help="the fleet jobs of a whole gate, as TOML files")
    s.add_argument("candidate", help="the head to gate (a revision of --checkout)")
    s.add_argument("--harness", help="the revision whose fast suite, generator and gate run (default: the candidate)")
    s.add_argument("--checkout", default=ROOT.as_posix(), help="the repository fleet takes the checkout from")
    s.add_argument("--id", help="the jobs' id prefix (default rg-CAND8)")
    s.add_argument("--passes", type=int, default=CAMPAIGN_PASSES, help="passes per side (fewer than 4: diagnostic)")
    s.add_argument("--cases", help="a selection: a dry run, diagnostic only")
    s.add_argument("--base", default=BASE)
    s.add_argument("--main", default=MAIN)
    s.add_argument("--max-gb", type=float, default=24)
    s.add_argument("--no-checks", action="store_true", help="without the W3C and guards job")
    s.add_argument("--out", type=Path, default=Path("."))
    s = sub.add_parser("verdict")
    s.add_argument("--timed", type=Path, action="append", required=True)
    s.add_argument("--w3c", type=Path, required=True)
    s.add_argument("--guards", type=Path, required=True)
    s.add_argument("--out", type=Path)
    args = p.parse_args()

    if args.command == "plan":
        catalog = cases_registry.load()
        run, not_run = plan(catalog, args.max_gb, args.cases.split(",") if args.cases else None)
        print(json.dumps({"run": run, "not_run": not_run}, indent=1))
        return 0
    if args.command == "report":
        sides = pairs(args.side, "--side")
        passes = load_passes(args.passes)
        summary = gate(passes, sides, args.candidate, [s for s in args.require.split(",") if s], args.harness_revision,
                       args.min_passes, args.diagnostic, pairs(args.not_run, "--not-run"), pairs(args.waive, "--waive"),
                       cases_registry.load())
        text = render(summary)
        print(text)
        if args.out:
            args.out.write_text(text, encoding="utf-8")
        if args.json:
            args.json.write_text(json.dumps(summary, indent=1, default=str), encoding="utf-8")
        return 0 if summary["passed"] else 1
    if args.command == "w3c":
        expected = tomllib.loads(args.expected.read_text(encoding="utf-8"))["suite"]
        counts = w3c_counts(args.logs)
        errors = w3c_check(counts, expected)
        out = {"counts": counts, "expected": expected, "errors": errors, "passed": not errors}
        print(json.dumps(out, indent=1))
        if args.json:
            args.json.write_text(json.dumps(out, indent=1), encoding="utf-8")
        return 0 if not errors else 1
    if args.command == "guards":
        names = (args.names.read_text(encoding="utf-8").split() if args.names else
                 guard_names((ROOT / "docs/design/performance.md").read_text(encoding="utf-8")))
        if args.list_names:
            print("\n".join(names))
            return 0
        errors, results = guards_check(names, args.log.read_text(encoding="utf-8", errors="replace"))
        out = {"guards": len(names), "results": results, "errors": errors, "passed": not errors}
        print("\n".join(errors) or f"all {len(names)} guards ran and passed")
        if args.json:
            args.json.write_text(json.dumps(out, indent=1), encoding="utf-8")
        return 0 if not errors else 1
    if args.command == "jobs":
        import subprocess
        rev = lambda r: subprocess.run(["git", "-C", args.checkout, "rev-parse", r], capture_output=True,  # noqa: E731
                                       text=True, check=True).stdout.strip()
        candidate = rev(args.candidate)
        found = jobs(candidate, rev(args.harness or candidate), args.checkout, args.id or f"rg-{candidate[:8]}",
                     args.passes, args.cases, rev(args.base), rev(args.main), max_gb=args.max_gb,
                     checks=not args.no_checks)
        args.out.mkdir(parents=True, exist_ok=True)
        for name, job in found.items():
            (args.out / f"{name}.toml").write_text(toml_job(job), encoding="utf-8")
            print(args.out / f"{name}.toml")
        return 0
    parts = [json.loads(path.read_text(encoding="utf-8")) for path in [*args.timed, args.w3c, args.guards]]
    timed, w3c, guards = parts[:-2], parts[-2], parts[-1]
    lines = ["# Release verdict", ""]
    for t in timed:
        lines.append(f"- timed on {', '.join(map(str, t['host']))}: {'passed' if t['passed'] else 'NOT passed'} "
                     f"({len(t['blocking'])} blocking, {len(t['errors'])} pass errors"
                     f"{', diagnostic' if t['diagnostic'] else ''})")
    lines.append(f"- W3C: {'passed' if w3c['passed'] else 'NOT passed'} ({'; '.join(w3c['errors']) or 'every suite as required'})")
    lines.append(f"- §0 guards: {'passed' if guards['passed'] else 'NOT passed'} "
                 f"({'; '.join(guards['errors'][:5]) or str(guards['guards']) + ' guards ran and passed'})")
    passed = all(t["passed"] for t in timed) and w3c["passed"] and guards["passed"]
    lines += ["", f"**{'PASSED' if passed else 'NOT PASSED'}**"]
    text = "\n".join(lines) + "\n"
    print(text)
    if args.out:
        args.out.write_text(text, encoding="utf-8")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
