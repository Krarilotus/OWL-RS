#!/usr/bin/env python3
"""The release gate (benches/PROTOCOL.md §5, "The release gate"): does the candidate regress,
beyond noise, on any workload, in time or memory, against each required baseline? And are
the §0 guards green and the W3C suites' counts what they must be?

    release_gate.py plan --max-gb GB [--cases A,B]
        the cases a host giving GB can run at their declared caps; the others, with why
    release_gate.py report --passes DIR --candidate NAME --side NAME=REV ... --require NAME,...
                           --harness-revision REV [--min-passes 4] [--diagnostic]
                           [--not-run CASE=WHY] [--tradeoffs TOML] [--out MD] [--json JSON]
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
from suitekit.compare import CONFIDENCE, PRECISION, RESOLUTION, counter_changes, judge  # noqa: E402
from suitekit.hosts import run_identity  # noqa: E402

CAMPAIGN_PASSES = 4  # per side: A-B-C-C-B-A twice
PASS_FILE = re.compile(r"pass-(\d+)-([A-Za-z0-9_.-]+)\.json$")


# --- plan --------------------------------------------------------------------------------------

def plan(catalog: list[dict], max_gb: float, selection: list[str] | None = None, min_gb: float = 0,
         big_gb: float | None = None, big_host: str = "khaleesi") -> tuple[list[str], dict, dict]:
    """The cases a host giving `max_gb` runs at their declared caps (those above `min_gb`: the big
    host's share), the cases a big host giving `big_gb` runs instead (`elsewhere`), and the
    others with why. A cap is never lowered to fit (PROTOCOL.md §3)."""
    run, not_run, elsewhere = [], {}, {}
    for case in catalog:
        if selection is not None and case["name"] not in selection:
            continue
        cap = case["cap_gb"]
        if cap <= min_gb:
            continue
        if cap <= max_gb:
            run.append(case["name"])
        elif big_gb is not None and cap <= big_gb:
            elsewhere[case["name"]] = f"its cap is {cap:g} GB: timed on {big_host} (giving {big_gb:g} GB)"
        else:
            not_run[case["name"]] = f"its cap is {cap:g} GB, the gate's host gives {max_gb:g} GB"
    return run, not_run, elsewhere


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
    # A case in only some passes (a later block's, its noise needing more passes): the passes
    # that ran it keep the same rules, each side as many, at least `min_passes`, a palindrome.
    by_case: dict[str, list[str]] = {}
    for _, _, p in timed:
        for case in p["report"].get("planned_cases") or []:
            by_case.setdefault(case, []).append(p["side"])
    for case, sub in sorted(by_case.items()):
        if len(sub) == len(order):
            continue
        counts = Counter(sub)
        if any(counts[s] < min_passes for s in sides) or len({counts[s] for s in sides}) > 1:
            errors.append(f"{case}: " + ", ".join(f"{s} {counts[s]}" for s in sides)
                          + f" passes; each side as many, at least {min_passes}")
        elif sub != sub[::-1]:
            errors.append(f"{case}: its passes ran {'-'.join(sub)}, not a palindrome")
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
        own = r.get("planned_cases") or []
        if not own or not set(own) <= set(planned):
            errors.append(f"{name}: its cases are not of the gate's plan")
        seen = Counter(x.get("case") for x in r.get("records", []))
        if seen != Counter(own):
            errors.append(f"{name}: records {dict(seen - Counter(own)) or ''} beyond, "
                          f"{sorted(set(own) - set(seen))} missing")
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
    # The measurement (runs, warm-ups, repetitions: --measure's file) is the same for every side.
    measures = {(p["report"].get("options") or {}).get("measure") for p in passes}
    if len(measures) > 1:
        errors.append("the passes measured with different calibrations (fast.py --measure)")
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
    out = {"status": [], "expected": [], "failed_checks": [], "failed_routes": [], "unchecked": 0, "notes": [],
           "samples": [],
           "process": [], "estimator": set(), "lifetime": [], "phases": [], "cgroup": [], "qps": [],
           "client_peaks": [], "cache_peaks": [], "rows": [], "counters": {}}
    for p in passes:
        if case not in (p["report"].get("planned_cases") or [case]):
            continue  # a later block's pass, without this case (its noise needs no more)
        record = next((r for r in p["report"]["records"] if r.get("case") == case), None)
        if record is None:
            out["status"].append("missing")
            out["expected"].append("ok")
            continue
        observations = record.get("observations") or []
        out["status"].append(record.get("status"))
        out["expected"].append(record.get("expected_outcome", "ok"))
        checks = record.get("checks", []) + [c for o in observations for c in o.get("checks", [])]
        routes = record.get("routes", []) + [c for o in observations for c in o.get("routes", [])]
        for key, items in (("failed_checks", checks), ("failed_routes", routes)):
            out[key] += [f"{c.get('path')}: expected {c.get('expected')}, got {c.get('got')}"
                         for c in items if c.get("ok") is False and
                         f"{c.get('path')}: expected {c.get('expected')}, got {c.get('got')}" not in out[key]]
        out["unchecked"] += sum(c.get("ok") is None for c in checks + routes)
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


def state(e: dict, off_route_ok: bool = False) -> str:
    """ok (every pass ok), known (every pass the declared outcome, not ok), or failed. A
    baseline off the route the harness asserts (an operator it hasn't) with every answer
    right is a valid time: ok with `off_route_ok`."""
    fine = {"ok", "off-route"} if off_route_ok else {"ok"}
    if (e["status"] and all(s in fine for s in e["status"]) and not e["failed_checks"]
            and (off_route_ok or not e["failed_routes"])):
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
        if match := CAPABILITY.search(note.replace("\\n", "\n")):
            return f"{side} can't run it ({match.group(0).strip()}; {statuses})"
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
    base_state = state(base, off_route_ok=True)
    if state(base) == "known" and state(cand) == "known":
        return {**row, "comparable": False, "reason": "a known issue on both sides (no timing verdict)"}
    if state(cand) == "known":
        if base_state == "ok":
            # The candidate stops at its declared limit where the baseline finished: that is a
            # loss, whatever the declaration says.
            row["blocks"].append(f"stops at its declared outcome ({', '.join(sorted(set(cand['status'])))}) "
                                 f"where {side} completed")
        return {**row, "comparable": False, "reason": "the candidate's known issue"}
    if base_state != "ok":
        return {**row, "comparable": False, "reason": why_not(base, side)}
    if base["failed_routes"]:
        row["notes"].append(f"{side} is off the route the harness asserts ({base['failed_routes'][0]}); "
                            "its answers are right, its time compared")
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
    if any(base["lifetime"]) or any(cand["lifetime"]):
        row["dimensions"]["memory/lifetime"] = judge(per_pass(base["lifetime"]), per_pass(cand["lifetime"]), unit="MiB")
    else:
        row["notes"].append("memory/lifetime: the tool reports no process peak on either side (the cgroup's is reported)")
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
    return row


def describe(r: dict) -> str:
    if r.get("ratio") is None:
        return f"{r['passes'][0]} and {r['passes'][1]} passes with values"
    ci = f" [{r['ci'][0]:.3f}, {r['ci'][1]:.3f}]" if r.get("ci") else ""
    more = f"; needs {r['needs_passes']} passes a side" if r.get("needs_passes") else (
        "; more passes won't settle it" if r["verdict"] == "suspect" else "")
    return (f"{fmt(r['base'])} -> {fmt(r['new'])} {r['unit']}, ratio {r['ratio']:.3f}{ci}, "
            f"noise {fmt(r.get('noise'))}{more}")


def worse_end(r: dict) -> float | None:
    """The worse end of a dimension's interval, as a ratio: above 1 is worse."""
    if not r.get("ci"):
        return None
    return 1 / r["ci"][0] if r.get("higher_is_better") else r["ci"][1]


# --- the owner-approved trade-offs (benches/release-tradeoffs.toml) ----------------------------

TRADEOFF_KEYS = {"case", "dimension", "max_ratio", "approved", "by", "reason", "pays_with"}


def load_tradeoffs(path: Path | None) -> tuple[list[dict], list[str]]:
    """The approved trade-offs and what's wrong with the file. Each names a case, a dimension,
    optionally the baseline (`against`), the largest ratio its loss may reach (the worse end of
    its interval), the owner's approval (`approved` date, `by`, `reason`), and the win that pays
    for it (`pays_with`: case and dimension, which must show as better in the same report)."""
    if path is None or not path.is_file():
        return [], []
    entries = tomllib.loads(path.read_text(encoding="utf-8")).get("tradeoff", [])
    errors = []
    for i, e in enumerate(entries):
        missing = TRADEOFF_KEYS - set(e)
        if missing:
            errors.append(f"trade-off {i + 1}: missing {', '.join(sorted(missing))}")
        elif not (isinstance(e["max_ratio"], (int, float)) and e["max_ratio"] > 1):
            errors.append(f"trade-off {i + 1}: max_ratio must be a number above 1")
        elif not re.fullmatch(r"\d{4}-\d{2}-\d{2}", str(e["approved"])) or not str(e["reason"]).strip():
            errors.append(f"trade-off {i + 1}: an approval date (YYYY-MM-DD) and a reason are required")
        elif not (isinstance(e["pays_with"], dict) and {"case", "dimension"} <= set(e["pays_with"])):
            errors.append(f"trade-off {i + 1}: pays_with needs the case and dimension of the win")
    return ([] if errors else entries), errors


def accepted(tradeoffs: list[dict], side: str, case: str, dimension: str, r: dict, comparisons: dict) -> dict | None:
    """The approved trade-off covering this loss, if one does: its case, dimension and baseline,
    the loss's interval within its max_ratio, and its paying win there."""
    for t in tradeoffs:
        if (t["case"], t["dimension"]) != (case, dimension) or t.get("against", side) != side:
            continue
        end = worse_end(r)
        if end is None or end > t["max_ratio"]:
            continue
        win = comparisons.get(side, {}).get(t["pays_with"]["case"], {}).get("dimensions", {}).get(
            t["pays_with"]["dimension"])
        if win and win["verdict"] == "better":
            return t
    return None


def fmt(x) -> str:
    return "-" if x is None else f"{x:.4g}"


# --- the timed part ----------------------------------------------------------------------------

def gate(passes: list[dict], sides: dict[str, str], candidate: str, required: list[str], harness: str,
         min_passes: int = CAMPAIGN_PASSES, diagnostic: bool = False, not_run: dict | None = None,
         tradeoffs: list[dict] | None = None, catalog: list[dict] | None = None, elsewhere: dict | None = None,
         context: dict | None = None) -> dict:
    not_run, tradeoffs, elsewhere = dict(not_run or {}), list(tradeoffs or []), dict(elsewhere or {})
    if candidate not in sides:
        raise ValueError(f"the candidate {candidate} is no declared side")
    baselines = [s for s in sides if s != candidate]
    summary = {"candidate": {"name": candidate, "revision": sides[candidate]},
               "baselines": {s: sides[s] for s in baselines}, "harness": harness,
               "policy": {"confidence": CONFIDENCE, "precision": PRECISION, "resolution": RESOLUTION},
               "passes": [{"n": p["n"], "side": p["side"], "file": p["path"],
                           "started": p["report"].get("started"), "finished": p["report"].get("finished")}
                          for p in sorted(passes, key=lambda p: p["n"])],
               "diagnostic": diagnostic or min_passes < CAMPAIGN_PASSES, "required": required,
               "context": dict(context or {}), "flagged": [], "other_blocking": [],
               "not_run": not_run, "elsewhere": elsewhere, "tradeoffs": tradeoffs, "accepted": [], "errors": [],
               "candidate_failures": {},
               "known_issues": {}, "fixed": {}, "comparisons": {}, "blocking": []}
    if missing := [s for s in required if s not in baselines]:
        summary["errors"].append(f"required baselines missing: {', '.join(missing)}")
    if len(baselines) < 1:
        summary["errors"].append("no baseline")
    summary["errors"] += schedule_errors(passes, list(sides), min_passes)
    planned = sorted({c for p in passes for c in p["report"].get("planned_cases", [])})
    summary["planned"] = planned
    summary["errors"] += provenance_errors(passes, sides, harness, planned)
    host_errors, summary["host_notes"] = host_states(passes)
    summary["errors"] += host_errors
    summary["host"] = sorted({(p["report"].get("machine") or {}).get("host") for p in passes}, key=str)
    by_side = {s: sorted([p for p in passes if p["side"] == s], key=lambda p: p["n"]) for s in sides}
    checks = {c["name"]: {path.split(".")[1] for path in c.get("check", {}) if path.startswith("q.")}
              for c in (catalog or [])}
    for case in planned:
        cand = evidence(by_side[candidate], case)
        if state(cand) == "failed":
            why = cand["failed_checks"][:3] + cand["failed_routes"][:2] + cand["notes"][-1:]
            summary["candidate_failures"][case] = f"statuses {', '.join(map(str, cand['status']))}; " + "; ".join(why)
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
    # Each case's time a pass, per side (the confirmation job's estimate).
    summary["case_seconds"] = {}
    for p in passes:
        for r in p["report"].get("records", []):
            summary["case_seconds"].setdefault(r["case"], {}).setdefault(p["side"], []).append(r.get("case_s") or 0)
    summary["case_seconds"] = {c: {s: statistics.mean(v) for s, v in sides_.items()}
                               for c, sides_ in summary["case_seconds"].items()}
    for case, why in summary["candidate_failures"].items():
        summary["blocking"].append(f"{case}: the candidate fails ({why})")
    for case, why in not_run.items():
        summary["blocking"].append(f"{case}: not run ({why})")
    needs = []
    summary["other_blocking"] = list(summary["blocking"])
    for side, rows in summary["comparisons"].items():
        for case, row in rows.items():
            summary["blocking"] += [f"{case} against {side}: {b}" for b in row["blocks"]]
            summary["other_blocking"] += [f"{case} against {side}: {b}" for b in row["blocks"]]
            for d, r in row["dimensions"].items():
                if not r["blocks"]:
                    continue
                # Nothing but an owner-approved trade-off accepts a loss.
                t = accepted(tradeoffs, side, case, d, r, summary["comparisons"]) if r["verdict"] != "incomplete" else None
                if t:
                    r["accepted_tradeoff"] = {k: t[k] for k in ("max_ratio", "approved", "by", "reason", "pays_with")}
                    summary["accepted"].append(f"{case} against {side}: {d}: {describe(r)}; accepted trade-off "
                                               f"(at most {t['max_ratio']}, approved {t['approved']} by {t['by']}: "
                                               f"{t['reason']}; paid by {t['pays_with']['case']} "
                                               f"{t['pays_with']['dimension']})")
                    continue
                summary["blocking"].append(f"{case} against {side}: {d}: {r['verdict']} ({describe(r)})")
                if r["verdict"] in ("regression", "suspect"):
                    summary["flagged"].append({"side": side, "case": case, "dimension": d, **{
                        k: r.get(k) for k in ("verdict", "ratio", "ci", "noise", "needs_passes", "passes",
                                              "higher_is_better", "unit", "base", "new")}})
                else:
                    summary["other_blocking"].append(f"{case} against {side}: {d}: {r['verdict']} ({describe(r)})")
                if r["verdict"] == "suspect":
                    needs.append(r.get("needs_passes") or math.inf)
    summary["needs_passes"] = (None if not needs else "more than the gate can run" if math.inf in needs
                               else max(needs))
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
             f"**{verdict}.** Policy: any loss blocks whose {CONFIDENCE:.0%} interval lies entirely on the worse "
             f"side (at least {RESOLUTION['ms'] * 1000:g} µs or {RESOLUTION['MiB']:g} MiB); a case whose interval "
             f"reaches beyond {PRECISION:g} x its between-pass noise is suspect (needs more passes); only an "
             "owner-approved trade-off accepts a loss (benches/PROTOCOL.md §5).", ""]
    if summary.get("needs_passes"):
        lines += [f"Undecided cases need **{summary['needs_passes']}** passes a side (the largest estimate).", ""]
    if summary["errors"]:
        lines += ["## The passes don't qualify", ""] + [f"- {e}" for e in summary["errors"]] + [""]
    if summary.get("inputs") or summary.get("prepared_manifest"):
        prepared = ", ".join(summary.get("prepared_manifest", []))
        lines += ["## Inputs", "", f"Prepared data: PREPARED.json sha256 {prepared}, the same in every pass, "
                  "unchanged during each.", ""]
        lines += [f"- {line}" for line in summary.get("inputs", [])] + [""]
    lines += ["## Blocking", ""] + ([f"- {b}" for b in summary["blocking"]] or ["- nothing"]) + [""]
    if summary.get("host_notes"):
        lines += ["## The host as the passes met it (scripts/hosts/hygiene.py)", ""] +                  [f"- {n}" for n in summary["host_notes"]] + [""]
    if summary.get("elsewhere"):
        lines += ["Timed in another part of the gate (the verdict requires it): " +
                  "; ".join(f"{k}: {v}" for k, v in summary["elsewhere"].items()), ""]
    if summary.get("accepted"):
        lines += ["## Accepted trade-offs (owner-approved, benches/release-tradeoffs.toml)", ""] + \
                 [f"- {a}" for a in summary["accepted"]] + [""]
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
                  "| case / dimension | verdict | base | new | ratio | interval | noise (sd) | needs passes |",
                  "|---|---|---|---|---|---|---|---|"]
        for case, row in rows.items():
            for d, r in row["dimensions"].items():
                if r["verdict"] != "neutral":
                    ci = f"{r['ci'][0]:.3f}-{r['ci'][1]:.3f}" if r.get("ci") else "-"
                    ratio = "-" if r.get("ratio") is None else f"{r['ratio']:.3f}"
                    verdict = "accepted trade-off" if r.get("accepted_tradeoff") else r["verdict"]
                    needs = r.get("needs_passes") or ("unbounded" if r["verdict"] == "suspect" else "-")
                    lines.append(f"| {case} / {d} | {verdict} | {fmt(r['base'])} | "
                                 f"{fmt(r['new'])} {r['unit']} | {ratio} | {ci} | {fmt(r.get('noise'))} | {needs} |")
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
        # open: undecided (gave up, unsupported); every other verdict but a pass or a
        # recorded dispute is a failure (a wrong answer).
        out["OWL 2 DL"] = {"passed": verdicts.pop("pass", 0), "disputed": verdicts.pop("disputed", 0),
                           "open": verdicts.pop("open", 0), "failed": sum(verdicts.values()), "other": dict(verdicts)}
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
        for key in ("skipped", "disputed", "open"):
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


# --- the host each pass met --------------------------------------------------------------------

POLICY_KEYS = ("driver", "governor", "epp", "boost", "amd_pstate")


def host_states(passes: list[dict]) -> tuple[list[str], list[str]]:
    """What the passes' host states (fast.py `host_state`, scripts/hosts/hygiene.py) say: errors
    (the CPU frequency policy changed between passes: the passes aren't one configuration) and
    notes (each problem a pass met at its start or end, with the passes that met it)."""
    policies, problems = {}, {}
    for p in sorted(passes, key=lambda p: p["n"]):
        for key in ("host_state", "host_state_finished"):
            state = p["report"].get(key)
            if not state:
                continue
            if state.get("frequency"):
                policies.setdefault(json.dumps({k: state["frequency"].get(k) for k in POLICY_KEYS},
                                               sort_keys=True), []).append(p["n"])
            for problem in state.get("problems", []):
                # Load figures vary by the second; the problem's kind is what's reported.
                problems.setdefault(re.sub(r" [\d.]+(?=,|$)", "", problem), set()).add(p["n"])
    errors = ([] if len(policies) <= 1 else
              ["the CPU frequency policy changed between passes: " +
               " | ".join(f"{k} (passes {sorted(set(v))})" for k, v in policies.items())])
    notes = [f"{problem} (passes {', '.join(map(str, sorted(ns)))})" for problem, ns in sorted(problems.items())]
    return errors, notes


# --- the measurement calibration (benches/fast/measure.toml) -----------------------------------
#
# The gate's power against a small loss is bounded by each series' noise between passes
# (benches/NOISY.md). `calibrate` sets, per case and from the gate's own passes, what the
# measurement does about it, the same for every side: more executions a process where they are
# cheap (the perf lab's --runs, the DL tools' files a process, a batch of executions a sample
# where every side's perf lab has --batch), and more passes for the cases whose noise needs
# them to catch a TARGET_LOSS with TARGET_POWER (the confirm-on-rerun chain simulated as in
# benches/probes/gate-power.py), within the timed part's budget on the gate's host.

TARGET_LOSS, TARGET_POWER = 0.02, 0.90
QUERY_MS = 2000.0      # a perf-lab process's measured query time, at most (the runs it buys)
DL_MS = 3000.0         # a DL tool's process: its files' time, at most
MAX_RUNS, MAX_REPEAT = 2000, 30
BATCH_SAMPLE_MS = 0.05  # a batched sample lasts at least this (50 µs: the timer's steps far below)
PASS_OVERHEAD_S = 40   # a pass's own time beyond its cases' (start, records, the host's state)


def case_runs(case: dict) -> tuple[int, int]:
    """The perf lab's runs and warm-ups a process as the case's arguments set them."""
    args, runs, warmup = case.get("args", []), 5, 1
    for i, a in enumerate(args[:-1]):
        if a == "--runs":
            runs = int(args[i + 1])
        elif a == "--warmup":
            warmup = int(args[i + 1])
    return runs, warmup


def runs_adjustable(case: dict) -> bool:
    """A perf-lab case timed by its queries' repeated executions (not a first execution, a
    reopened store, a replayed log or a sweep, whose runs are what they measure)."""
    args = case.get("args", [])
    return (case.get("tool", "perf_lab") == "perf_lab" and case.get("metric") == "sum_p50_ms"
            and bool(case.get("queries")) and not case["queries"].startswith("@cache-log")
            and "setup" not in case and "sweep" not in case and "--store" not in args
            and case_runs(case)[1] > 0 and "--clients" not in args and "--commits" not in args)


def judged_noise(rows: list[dict], resolution_ms: float = RESOLUTION["ms"]) -> float:
    """A case's noise for its passes: the largest of its series whose TARGET_LOSS is above the
    timing resolution (a smaller series can't show that loss at any number of passes)."""
    judged = [r["sd"] for r in rows if r["median_ms"] * TARGET_LOSS >= resolution_ms]
    return max(judged, default=0.0)


def power_table(passes: list[int], sims: int = 300) -> dict[int, list[float]]:
    """The chain's power against TARGET_LOSS on gate-power.py's noise grid, per round-0 passes."""
    import importlib.util
    spec = importlib.util.spec_from_file_location("gate_power", ROOT / "benches/probes/gate-power.py")
    gp = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(gp)
    table = {n: [gp.caught(s, 1 + TARGET_LOSS, n, sims, seed=11 + i)["block-confirmed"]
                 for i, s in enumerate(gp.GRID)] for n in passes}
    return {"grid": [float(s) for s in gp.GRID], **{str(n): v for n, v in table.items()}}


def power_at(table: dict, n: int, sigma: float) -> float:
    grid = table["grid"]
    xs = [math.log(s) for s in grid]
    x = math.log(max(sigma, grid[0]))
    ys = table[str(n)]
    if x >= xs[-1]:
        return ys[-1]
    i = max(j for j in range(len(xs)) if xs[j] <= x)
    return ys[i] + (ys[i + 1] - ys[i]) * (x - xs[i]) / (xs[i + 1] - xs[i])


def calibrate(catalog: list[dict], passes: list[dict], budget_min: float, max_passes: int = 16,
              min_passes: int = CAMPAIGN_PASSES, sides: int = 3, table: dict | None = None,
              batch: bool = True) -> dict:
    """Each case's measurement from passes of the gate's host (an A/A run, or a gate's): runs,
    warm-ups, batch and repetitions from its times, passes from its noise. Every case gets
    `min_passes` a side; the passes beyond go where they buy the most power against TARGET_LOSS
    a second of the timed part (weighted by the case's judged series), until `budget_min`. The
    passes' binaries are the sides (an A/A run's one binary counts `sides` times)."""
    cases = {c["name"]: c for c in catalog}
    by_case: dict[str, dict] = {}
    revs = set()
    for p in passes:
        rev = (p["report"].get("binaries") or {}).get("revision") or p["side"]
        revs.add(rev)
        for r in p["report"].get("records", []):
            entry = by_case.setdefault(r["case"], {"seconds": {}, "series": {}, "status": set(), "measured": {}})
            entry["seconds"].setdefault(rev, []).append(r.get("case_s") or 0)
            entry["status"].add(r.get("status"))
            entry["measured"] = r.get("measurement") or entry["measured"]
            if r.get("status") == "ok":
                for s, values in (r.get("samples") or {}).items():
                    if values:
                        entry["series"].setdefault((rev, s), []).append(statistics.median(values))
    evens = list(range(min_passes, max_passes + 1, 2))
    table = table or power_table(evens)
    per_side = sides if len(revs) == 1 else 1
    out, rounds, weights = {}, {}, {}
    for name, e in sorted(by_case.items()):
        case = cases.get(name)
        if case is None:
            continue
        rows = [{"rev": rev, "series": s, "median_ms": statistics.median(m),
                 "sd": statistics.stdev(math.log(v) for v in m)}
                for (rev, s), m in e["series"].items() if len(m) >= 2 and min(m) > 0]
        seconds = {rev: statistics.mean(v) for rev, v in e["seconds"].items()}
        # One execution of every query (a DL tool's file: its phases), on the slowest side that
        # ran the case (a baseline that can't run it has no times).
        per_run = max((sum(r["median_ms"] for r in rows if r["rev"] == rev) for rev in revs), default=0.0)
        added = 0.0  # a process's added measured time, on every side
        entry = {}
        measured_runs = e["measured"].get("runs")
        if runs_adjustable(case) and "ok" in e["status"] and rows:
            runs, warmup = case_runs(case)
            longest = max(r["median_ms"] for r in rows)
            k = 1
            if batch and longest < 1.0:
                # A sample of every query well above the timer's steps (applied where every
                # side's perf lab batches; otherwise the runs alone).
                k = min(1000, math.ceil(BATCH_SAMPLE_MS / max(min(r["median_ms"] for r in rows), 1e-4)))
            # The runs as if unbatched (a gate whose baselines can't batch runs them so); a
            # batching gate's processes take `batch` times as long.
            new_runs = min(MAX_RUNS, max(runs, math.ceil(QUERY_MS / max(per_run, 1e-3))))
            entry.update(runs=new_runs, warmup=max(warmup, 3 if new_runs >= 20 else warmup))
            if k > 1:
                entry["batch"] = k
            added = (new_runs - (measured_runs or runs)) * per_run / 1000
        elif case.get("tool") == "tableau" and "ok" in e["status"] and rows:
            whole = per_run
            repeat = int(case.get("repeat", 1))
            limit = 0.4 * float(case.get("timeout_s", 300)) * 1000 / max(whole, 1e-3)
            new = int(min(MAX_REPEAT, limit, max(repeat, math.ceil(DL_MS / max(whole, 1e-3)))))
            if new > repeat:
                entry["repeat"] = new
                added = (new - (e["measured"].get("repeat") or repeat)) * whole / 1000
        sigma = judged_noise(rows)
        entry.update(noise=round(sigma, 4), passes=min_passes,
                     seconds=round(statistics.mean(seconds.values()) + added, 1))
        rounds[name] = per_side * sum(s + added for s in seconds.values())  # a pass of every side
        weights[name] = max(1, len({r["series"] for r in rows if r["median_ms"] * TARGET_LOSS >= RESOLUTION["ms"]}))
        out[name] = entry

    def minutes(plan: dict) -> float:
        blocks = max((e["passes"] for e in plan.values()), default=0) // 2
        total = 0.0
        for k in range(1, blocks + 1):
            in_block = [rounds[c] for c, e in plan.items() if e["passes"] >= 2 * k]
            total += 2 * (sum(in_block) + sides * PASS_OVERHEAD_S)
        return total / 60

    power = lambda c, n: power_at(table, n, out[c]["noise"])  # noqa: E731
    spent = minutes(out)
    while True:
        top = max(e["passes"] for e in out.values())
        best = None
        for c, e in out.items():
            for n in evens:
                if n <= e["passes"]:
                    continue
                gain = weights[c] * (power(c, n) - power(c, e["passes"]))
                cost = (n - e["passes"]) * rounds[c] * 2 / 60 + max(0, n - top) * sides * PASS_OVERHEAD_S / 60
                if gain > 1e-3 and spent + cost <= budget_min and (best is None or gain / cost > best[0]):
                    best = (gain / cost, c, n, cost)
        if best is None:
            break
        out[best[1]]["passes"] = best[2]
        spent = minutes(out)
    for e in out.values():
        e["power"] = round(power_at(table, e["passes"], e["noise"]), 3)
    return {"cases": out, "minutes": round(minutes(out), 1), "cap": max(e["passes"] for e in out.values()),
            "budget_min": budget_min}


def write_measure(result: dict, source: str) -> str:
    lines = ["# The release gate's measurement per case (fast.py --measure; benches/PROTOCOL.md §5, \"Noise\").",
             "# Written by `release_gate.py calibrate`, not by hand: " + source + ".",
             f"# runs, warmup, batch: the perf lab's a process; repeat: the DL tool's files a process; passes: a side",
             f"# (round 0), for {TARGET_POWER:.0%} power against a {TARGET_LOSS:.0%} loss at the case's noise (the largest",
             "# sd of log pass medians of its series where that loss exceeds the resolution); power: at those passes;",
             f"# seconds: a pass's. Timed part {result['minutes']} min (budget {result['budget_min']}, passes at most "
             f"{result['cap']}).", ""]
    for name, e in result["cases"].items():
        lines.append(f"[case.{name}]")
        lines += [f"{k} = {v}" for k, v in e.items()]
        lines.append("")
    return "\n".join(lines)


def blocks(plan: list[str], measure: dict, min_passes: int = CAMPAIGN_PASSES) -> list[list[str]]:
    """The cases of each A-B-C-C-B-A block: block k (from 1) runs those whose passes a side
    (measure.toml) reach 2k; the first min_passes / 2 blocks run the whole plan."""
    need = {c: max(min_passes, int(measure.get(c, {}).get("passes", min_passes))) for c in plan}
    count = max(need.values(), default=min_passes) // 2
    return [[c for c in plan if need[c] >= 2 * k] for k in range(1, count + 1)]


def read_measure(text: str | None) -> dict:
    return tomllib.loads(text).get("case", {}) if text else {}


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
    lines = [f"{k} = {value(v)}" for k, v in job.items() if k not in ("requires", "inputs", "outputs")]
    lines += ["", "[requires]"] + [f"{k} = {value(v)}" for k, v in job["requires"].items()]
    for table in ("inputs", "outputs"):
        for item in job.get(table, []):
            lines += ["", f"[[{table}]]"] + [f"{k} = {value(v)}" for k, v in item.items()]
    return "\n".join(lines) + "\n"


def jobs(candidate: str, harness: str, checkout: str, prefix: str, passes_per_side: int, cases: str | None,
         base: str = BASE, main: str = MAIN, timed_host: str = "phuoc-yu", checks_host: str = "office-pc",
         max_gb: float = 24, checks: bool = True, big_host: str | None = "khaleesi",
         big_gb: float = 28, measure: dict | None = None, batch: bool = False) -> dict[str, dict]:
    """The gate's fleet jobs: binaries and data, then the timed passes and their report, alone
    on the timed host; the cases whose caps exceed it on the big host (khaleesi, 64 GB) at
    their full caps, on the build's binaries (packed by the build job: the big host builds
    nothing); the W3C suites and the guards on the checks host. No priority is set: each job
    gets its class's (merge-gate). With `measure` (benches/fast/measure.toml) the timed passes
    run in A-B-C-C-B-A blocks, each later block only the cases whose noise needs its passes
    (plan-blocks); `batch`: every side's perf lab batches (fast.py --batch)."""
    env = {"GATE_NAME": f"gate-v2-{candidate[:8]}", "GATE_MAX_GB": str(max_gb), "NRESE_QUIET_SLOT": "0",
           "GATE_MIN_PASSES": str(passes_per_side), **({"GATE_CASES": cases} if cases else {}),
           **({"GATE_BIG_GB": str(big_gb), "GATE_BIG_HOST": big_host} if big_host else {}),
           **({"GATE_BATCH": "1"} if batch else {})}
    source = [{"name": "checkout", "source": {"git": checkout, "rev": harness}}]
    gate = ["bash", "scripts/release-gate.sh"]
    order = schedule(passes_per_side)
    revs = {"base": base, "main": main, "cand": candidate}
    small = bool(cases)
    out = {f"{prefix}-build": {
        "id": f"{prefix}-build", "group": "release", "kind": "owlrs.build", "priority_class": "merge-gate",
        # About 25 minutes a revision's build (skipped when its verified binaries are there).
        "est_min": 25 * len(dict.fromkeys([base, main, candidate, harness])) + (10 if small else 30), "env": env,
        "steps": [[*gate, "build", rev] for rev in dict.fromkeys([base, main, candidate, harness])]
        + [[*gate, "prepare", harness]] + ([[*gate, "pack", base, main, candidate, harness]] if big_host else []),
        "requires": {"hosts": [timed_host], "cores": 16, "ram_gb": 12}, "inputs": source,
        **({"outputs": [{"name": "binaries", "path": "binaries"}]} if big_host else {})}}
    per_pass = {"base": 1, "main": 1, "cand": 1} if small else {"base": 14, "main": 4, "cand": 14}
    if measure:
        # Blocks: the k-th runs the cases needing 2k passes a side; a pass's minutes, its cases'.
        need = {c: max(passes_per_side, int(e.get("passes", passes_per_side))) for c, e in measure.items()}
        count = max(max(need.values()) // 2, passes_per_side // 2)
        full = sum(e.get("seconds", 0) for e in measure.values()) or 1
        steps, est, n = [[*gate, "plan-blocks", harness]], 2.0, 0
        for k in range(1, count + 1):
            share = sum(e.get("seconds", 0) for c, e in measure.items() if need[c] >= 2 * k) / full
            for side in schedule(2):
                n += 1
                steps.append([*gate, "pass", str(n), side, revs[side], harness, f"plan-block-{k}.json"])
                est += (per_pass[side] if small else full * share / 60) + PASS_OVERHEAD_S / 60
    else:
        steps = [[*gate, "pass", str(n), side, revs[side], harness] for n, side in enumerate(order, start=1)]
        est = sum(per_pass[s] for s in order) + 2
    out[f"{prefix}-timed"] = {
        "id": f"{prefix}-timed", "group": "release", "kind": "owlrs.timed", "class": "exclusive",
        "priority_class": "merge-gate", "after": [f"{prefix}-build"],
        "env": {**env, "GATE_HOST": timed_host, "GATE_PREFIX": prefix, "GATE_RAM_GB": str(max_gb)},
        "est_min": round(est), "stall_after_min": 40,
        "steps": steps + [[*gate, "report", harness, f"cand={candidate}", f"base={base}", f"main={main}"],
                          [*gate, "clean"]],
        "requires": {"hosts": [timed_host], "cores": 16, "ram_gb": max_gb}, "inputs": source}
    if big_host:
        big_env = {**env, "GATE_MIN_GB": str(max_gb), "GATE_MAX_GB": str(big_gb), "GATE_HOST": big_host,
                   "GATE_PREFIX": f"{prefix}-big", "GATE_RAM_GB": str(big_gb + 4)}
        big_env.pop("GATE_BIG_GB")
        per_big = 2 if small else 15
        out[f"{prefix}-big"] = {
            "id": f"{prefix}-big", "group": "release", "kind": "owlrs.timed", "class": "exclusive",
            "priority_class": "merge-gate", "after": [f"{prefix}-build"], "env": big_env,
            "est_min": per_big * len(order) + 30, "stall_after_min": 60,
            "steps": [[*gate, "unpack"], [*gate, "prepare", harness]]
            + [[*gate, "pass", str(n), side, revs[side], harness] for n, side in enumerate(order, start=1)]
            + [[*gate, "report", harness, f"cand={candidate}", f"base={base}", f"main={main}"], [*gate, "clean"]],
            "requires": {"hosts": [big_host], "cores": 16, "ram_gb": big_gb + 4},
            "inputs": [*source, {"name": "binaries", "source": {"artifact": f"{prefix}-build/binaries"}}]}
    if checks:
        out[f"{prefix}-checks"] = {
            "id": f"{prefix}-checks", "group": "release", "kind": "owlrs.any", "priority_class": "merge-gate",
            "est_min": 100, "env": {**env, "CARGO_BUILD_JOBS": "16"},
            "steps": [[*gate, "w3c"], [*gate, "guards"], [*gate, "clean"]],
            "requires": {"hosts": [checks_host], "cores": 16, "ram_gb": 12},
            "inputs": [{"name": "checkout", "source": {"git": checkout, "rev": candidate}}]}
    return out


# --- confirm on rerun (the owner's decision, 18 Oct 2026) --------------------------------------
#
# A gate run flags each dimension whose 95 % interval shows a loss, or that it couldn't decide.
# Those cases run again, alone, in a fresh, independent A-B-B-A on the same host (`confirm`).
# A loss blocks only if a second round shows it too, at 95 % on the same, worse side; one that
# doesn't repeat is "not confirmed", reported with every round's interval, and doesn't block.
# A dimension still undecided gets another round; after CONFIRM_ROUNDS it blocks, undecided.

CONFIRM_ROUNDS = 3        # the first run and two confirmations
CONFIRM_PASSES = (4, 24)  # passes a side for a confirmation: its noise's estimate, within these


def rounds_of(reports: list[dict]) -> list[list[dict]]:
    """The reports of each host, first run first (`context.round`)."""
    by_host: dict[str, list[dict]] = {}
    for r in reports:
        host = (r.get("context") or {}).get("host") or ",".join(map(str, r.get("host", [])))
        by_host.setdefault(host, []).append(r)
    return [sorted(rs, key=lambda r: (r.get("context") or {}).get("round", 0)) for rs in by_host.values()]


def dimension_in(report: dict, side: str, case: str, dimension: str) -> dict | None:
    return report.get("comparisons", {}).get(side, {}).get(case, {}).get("dimensions", {}).get(dimension)


def combine(rounds: list[dict]) -> list[dict]:
    """Every dimension the first run flagged, through the confirmation rounds: `confirmed` (two
    rounds show the loss: blocks), `not confirmed` (shown once, then a round without it), `resolved`
    (undecided, then decided without a loss), `pending` (another round is due), `undecided` (no
    round left: blocks), `missing` (a confirmation lacks it: blocks)."""
    out = []
    for flag in rounds[0].get("flagged", []):
        key = (flag["side"], flag["case"], flag["dimension"])
        history, detections, status = [], 0, None
        for k, report in enumerate(rounds):
            r = flag if k == 0 else dimension_in(report, *key)
            if r is None:
                status = "missing"
                break
            if r.get("accepted_tradeoff"):
                history.append({**{f: r.get(f) for f in ("verdict", "ratio", "ci", "passes")}, "verdict": "accepted"})
                status = "resolved"
                break
            history.append({f: r.get(f) for f in ("verdict", "ratio", "ci", "passes", "needs_passes", "noise")})
            if r["verdict"] == "regression":
                detections += 1
                if detections >= 2:
                    status = "confirmed"
                    break
            elif r["verdict"] in ("neutral", "better"):
                status = "not confirmed" if detections else "resolved"
                break
            elif r["verdict"] == "incomplete":
                status = "missing"
                break
        if status is None:
            status = "pending" if len(rounds) < CONFIRM_ROUNDS else "undecided"
        out.append({**dict(zip(("side", "case", "dimension"), key)), "status": status, "rounds": history,
                    "blocks": status in ("confirmed", "pending", "undecided", "missing")})
    return out


def confirm_passes(history: list[dict]) -> int:
    """Passes a side for the next round: the last round's estimate (its interval's half-width
    against the distance to settle), even, within CONFIRM_PASSES."""
    last = history[-1]
    low, high = CONFIRM_PASSES
    if last.get("needs_passes"):
        need = last["needs_passes"]
    elif last["verdict"] == "regression" and last.get("ci") and last.get("ratio"):
        half = (math.log(last["ci"][1]) - math.log(last["ci"][0])) / 2
        gap = abs(math.log(last["ratio"]))
        need = (last.get("passes") or [low])[0] * (half / gap) ** 2 if gap > 0 else high
    else:
        need = high
    n = min(high, max(low, math.ceil(need)))
    return n + n % 2


def confirm_job(rounds: list[dict], checkout: str) -> dict | None:
    """The next confirmation's fleet job for one host, or None when no dimension is pending."""
    states = [s for s in combine(rounds) if s["status"] == "pending"]
    if not states:
        return None
    first = rounds[0]
    ctx = first.get("context") or {}
    cases = sorted({s["case"] for s in states})
    n = max(confirm_passes(s["rounds"]) for s in states)
    order = schedule(n)
    revs = {"cand": first["candidate"]["revision"], **first["baselines"]}
    harness = first["harness"]
    k = len(rounds)
    prefix = ctx.get("prefix") or f"rg-{revs['cand'][:8]}"
    name = f"{prefix}-confirm" if k == 1 else f"{prefix}-confirm{k}"
    seconds = sum(first.get("case_seconds", {}).get(c, {}).get(side, 60) for c in cases for side in order)
    gate = ["bash", "scripts/release-gate.sh"]
    env = {"GATE_NAME": ctx.get("gate_name") or f"gate-v2-{revs['cand'][:8]}", "GATE_HOST": ctx.get("host", ""),
           "GATE_ROUND": str(k), "GATE_PREFIX": prefix, "GATE_MIN_PASSES": str(n), "NRESE_QUIET_SLOT": "0",
           "GATE_PLAN": f"plan-confirm{k}.json", "GATE_MAX_GB": str(ctx.get("max_gb", 24))}
    return {"id": name, "group": "release", "kind": "owlrs.timed", "class": "exclusive",
            "priority_class": "merge-gate", "env": env,
            "est_min": round(seconds / 60 + 0.7 * len(order) + 5), "stall_after_min": 60,
            "steps": [[*gate, "plan-cases", ",".join(cases)]]
            + [[*gate, "pass", str(i), side, revs[side], harness] for i, side in enumerate(order, start=1)]
            + [[*gate, "report", harness, f"cand={revs['cand']}", *(f"{s}={r}" for s, r in first["baselines"].items())],
               [*gate, "clean"]],
            "requires": {"hosts": [ctx.get("host") or "phuoc-yu"], "cores": 16, "ram_gb": ctx.get("ram_gb", 24)},
            "inputs": [{"name": "checkout", "source": {"git": checkout, "rev": harness}}]}


def verdict(timed: list[dict], w3c: dict, guards: dict) -> tuple[bool, str]:
    """Every part together: each timed part passed, every case one part left to another
    in some timed part's plan, the W3C counts and the guards."""
    lines = ["# Release verdict", ""]
    parts_passed = []
    for rounds in rounds_of(timed):
        first = rounds[0]
        states = combine(rounds)
        errors = [e for r in rounds for e in r["errors"]]
        # Blocks no rerun can lift (a failed case, lost answers, a pass that doesn't qualify);
        # confirmations add their own (the case failing there).
        other = [b for r in rounds for b in r.get("other_blocking", [])]
        blocking = [s for s in states if s["blocks"]]
        ok = not errors and not other and not blocking and not any(r["diagnostic"] for r in rounds)
        parts_passed.append(ok)
        host = (first.get("context") or {}).get("host") or ", ".join(map(str, first["host"]))
        lines.append(f"- timed on {host}: {'passed' if ok else 'NOT passed'} ({len(rounds)} round(s); "
                     f"{sum(s['status'] == 'confirmed' for s in states)} confirmed losses, "
                     f"{sum(s['status'] in ('pending', 'undecided', 'missing') for s in states)} undecided, "
                     f"{len(other)} other blocks, {len(errors)} pass errors"
                     f"{', diagnostic' if any(r['diagnostic'] for r in rounds) else ''})")
        for s in states:
            shown = "; ".join(f"round {i}: {h['verdict']} {h.get('ratio') or 0:.3f} "
                              f"[{(h.get('ci') or [0, 0])[0]:.3f}, {(h.get('ci') or [0, 0])[1]:.3f}]"
                              for i, h in enumerate(s["rounds"]))
            lines.append(f"  - {s['case']} against {s['side']}: {s['dimension']}: **{s['status']}** ({shown})")
        # A loss a confirmation shows that the first run didn't: one round, not confirmed, kept.
        first_keys = {(f["side"], f["case"], f["dimension"]) for f in first.get("flagged", [])}
        for k, report in enumerate(rounds[1:], start=1):
            for f in report.get("flagged", []):
                if (f["side"], f["case"], f["dimension"]) not in first_keys:
                    ci = f.get("ci") or [0, 0]
                    lines.append(f"  - {f['case']} against {f['side']}: {f['dimension']}: **not confirmed** (only round "
                                 f"{k}: {f['verdict']} {f.get('ratio') or 0:.3f} [{ci[0]:.3f}, {ci[1]:.3f}]; round 0 "
                                 "didn't show it)")
        lines += [f"  - {b}" for b in other] + [f"  - pass error: {e}" for e in errors]
    lines.append(f"- W3C: {'passed' if w3c['passed'] else 'NOT passed'} ({'; '.join(w3c['errors']) or 'every suite as required'})")
    lines.append(f"- §0 guards: {'passed' if guards['passed'] else 'NOT passed'} "
                 f"({'; '.join(guards['errors'][:5]) or str(guards['guards']) + ' guards ran and passed'})")
    # A case one timed part leaves to another must be in that other's plan.
    covered = {c for t in timed for c in t.get("planned", [])}
    missing = sorted({c for t in timed for c in t.get("elsewhere", {})} - covered)
    if missing:
        lines.append(f"- NOT timed anywhere: {', '.join(missing)} (left to another part, which didn't run them)")
    passed = all(parts_passed) and w3c["passed"] and guards["passed"] and not missing
    lines += ["", f"**{'PASSED' if passed else 'NOT PASSED'}**"]
    return passed, "\n".join(lines) + "\n"


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
    s.add_argument("--min-gb", type=float, default=0, help="only cases above this cap (the big host's share)")
    s.add_argument("--big-gb", type=float, help="cases up to this cap run on the big host instead")
    s.add_argument("--big-host", default="khaleesi")
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
    s.add_argument("--elsewhere", action="append", default=[], metavar="CASE=WHERE",
                   help="cases another timed part of the gate runs (the verdict checks that it did)")
    s.add_argument("--tradeoffs", type=Path, default=ROOT / "benches" / "release-tradeoffs.toml",
                   help="the owner-approved trade-offs: the only way a loss is accepted")
    s.add_argument("--inputs", type=Path, help="the datasets the passes read, as prepare recorded them")
    s.add_argument("--context", action="append", default=[], metavar="KEY=VALUE",
                   help="the job: host (fleet's name), round (0 the first run), prefix, gate_name, max_gb, ram_gb")
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
    s.add_argument("--big-host", default="khaleesi", help="the host of the cases above --max-gb ('' for none)")
    s.add_argument("--big-gb", type=float, default=28)
    s.add_argument("--out", type=Path, default=Path("."))
    s = sub.add_parser("calibrate", help="benches/fast/measure.toml from passes of the gate's host (an A/A run)")
    s.add_argument("--passes", type=Path, nargs="+", required=True, help="pass reports, or directories of them")
    s.add_argument("--budget-min", type=float, default=150, help="the timed part's minutes, at most")
    s.add_argument("--max-passes", type=int, default=16)
    s.add_argument("--no-batch", action="store_true", help="no batched timing for the microsecond cases")
    s.add_argument("--table", type=Path, help="a power table written before (--write-table), not simulated again")
    s.add_argument("--write-table", type=Path)
    s.add_argument("--out", type=Path, default=ROOT / "benches" / "fast" / "measure.toml")
    s = sub.add_parser("blocks", help="each block's plan of a gate (release-gate.sh plan-blocks)")
    s.add_argument("--plan", type=Path, required=True)
    s.add_argument("--measure", type=Path, required=True)
    s.add_argument("--out", type=Path, required=True)
    s = sub.add_parser("confirm", help="the confirmation job of each host whose run flagged a loss or an undecided case")
    s.add_argument("--reports", type=Path, nargs="+", required=True, help="every timed report so far (release-gate.json)")
    s.add_argument("--checkout", default=ROOT.as_posix())
    s.add_argument("--out", type=Path, default=Path("."))
    s = sub.add_parser("verdict")
    s.add_argument("--timed", type=Path, action="append", required=True)
    s.add_argument("--w3c", type=Path, required=True)
    s.add_argument("--guards", type=Path, required=True)
    s.add_argument("--out", type=Path)
    args = p.parse_args()

    if args.command == "plan":
        catalog = cases_registry.load()
        run, not_run, elsewhere = plan(catalog, args.max_gb, args.cases.split(",") if args.cases else None,
                                       args.min_gb, args.big_gb, args.big_host)
        print(json.dumps({"run": run, "not_run": not_run, "elsewhere": elsewhere}, indent=1))
        return 0
    if args.command == "report":
        sides = pairs(args.side, "--side")
        tradeoffs, bad = load_tradeoffs(args.tradeoffs)
        if bad:
            raise SystemExit("benches/release-tradeoffs.toml: " + "; ".join(bad))
        passes = load_passes(args.passes)
        summary = gate(passes, sides, args.candidate, [s for s in args.require.split(",") if s], args.harness_revision,
                       args.min_passes, args.diagnostic, pairs(args.not_run, "--not-run"), tradeoffs,
                       cases_registry.load(), pairs(args.elsewhere, "--elsewhere"),
                       {k: v for k, v in pairs(args.context, "--context").items()})
        for key in ("round", "max_gb", "ram_gb"):
            if key in summary["context"]:
                summary["context"][key] = float(summary["context"][key]) if "gb" in key else int(summary["context"][key])
        if args.inputs and args.inputs.is_file():
            summary["inputs"] = args.inputs.read_text(encoding="utf-8").splitlines()
        prepared = {(p["report"].get("prepared") or {}).get("manifest_sha256") for p in passes}
        summary["prepared_manifest"] = sorted(map(str, prepared))
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
    if args.command == "calibrate":
        files = [f for p in args.passes for f in
                 ((sorted(p.glob("**/pass-*.json")) or sorted(p.glob("*.json"))) if p.is_dir() else [p])]
        found = []
        for i, f in enumerate(files):
            m = PASS_FILE.search(f.name)
            found.append({"n": int(m.group(1)) if m else i, "side": m.group(2) if m else "?", "path": f.name,
                          "report": json.loads(f.read_text(encoding="utf-8"))})
        table = (json.loads(args.table.read_text(encoding="utf-8")) if args.table else
                 power_table(list(range(CAMPAIGN_PASSES, args.max_passes + 1, 2))))
        if args.write_table:
            args.write_table.write_text(json.dumps(table), encoding="utf-8")
        result = calibrate(cases_registry.load(), found, args.budget_min, args.max_passes, table=table,
                           batch=not args.no_batch)
        hosts_ = sorted({str((p["report"].get("machine") or {}).get("host")) for p in found})
        source = f"{len(found)} passes on {', '.join(hosts_)} ({', '.join(sorted({p['path'] for p in found})[:2])} ...)"
        args.out.write_text(write_measure(result, source), encoding="utf-8")
        under = {c: e for c, e in result["cases"].items() if e["power"] < TARGET_POWER}
        print(f"{len(result['cases'])} cases; timed part {result['minutes']} min; passes at most {result['cap']}; "
              f"{len(under)} below {TARGET_POWER:.0%} power at a {TARGET_LOSS:.0%} loss: "
              + ", ".join(f"{c} ({e['noise']:.3f}, {e['power']:.2f})" for c, e in sorted(under.items())))
        return 0
    if args.command == "blocks":
        plan = json.loads(args.plan.read_text(encoding="utf-8"))
        found = blocks(plan["run"], read_measure(args.measure.read_text(encoding="utf-8"))
                       if args.measure.is_file() else {})
        for k, cases in enumerate(found, start=1):
            (args.out / f"plan-block-{k}.json").write_text(json.dumps({"run": cases, "not_run": {}, "elsewhere": {}},
                                                                      indent=1), encoding="utf-8")
        print("blocks: " + ", ".join(f"{k} {len(c)} cases" for k, c in enumerate(found, start=1)))
        return 0
    if args.command == "confirm":
        reports = [json.loads(path.read_text(encoding="utf-8")) for path in args.reports]
        args.out.mkdir(parents=True, exist_ok=True)
        written = 0
        for rounds in rounds_of(reports):
            job = confirm_job(rounds, args.checkout)
            states = combine(rounds)
            print(f"{(rounds[0].get('context') or {}).get('host')}: " +
                  (", ".join(f"{s['case']}/{s['dimension']} against {s['side']}: {s['status']}" for s in states) or
                   "nothing flagged"))
            if job:
                (args.out / f"{job['id']}.toml").write_text(toml_job(job), encoding="utf-8")
                print(f"  {args.out / (job['id'] + '.toml')}: {len(job['steps']) - 3} passes, est {job['est_min']} min")
                written += 1
        return 0
    if args.command == "jobs":
        import subprocess
        rev = lambda r: subprocess.run(["git", "-C", args.checkout, "rev-parse", r], capture_output=True,  # noqa: E731
                                       text=True, check=True).stdout.strip()
        candidate = rev(args.candidate)
        harness = rev(args.harness or candidate)
        # The harness's calibration, and batched timing only where every side's perf lab batches.
        shown = subprocess.run(["git", "-C", args.checkout, "show", f"{harness}:benches/fast/measure.toml"],
                               capture_output=True, text=True)
        measure = read_measure(shown.stdout if shown.returncode == 0 else None)
        batch = all(subprocess.run(["git", "-C", args.checkout, "grep", "-q", "-e", '"--batch"', r, "--",
                                    "crates/nrese-store/examples"], capture_output=True).returncode == 0
                    for r in {candidate, rev(args.base), rev(args.main)})
        found = jobs(candidate, harness, args.checkout, args.id or f"rg-{candidate[:8]}",
                     args.passes, args.cases, rev(args.base), rev(args.main), max_gb=args.max_gb,
                     checks=not args.no_checks, big_host=args.big_host or None, big_gb=args.big_gb,
                     measure=measure, batch=batch)
        args.out.mkdir(parents=True, exist_ok=True)
        for name, job in found.items():
            (args.out / f"{name}.toml").write_text(toml_job(job), encoding="utf-8")
            print(args.out / f"{name}.toml")
        return 0
    parts = [json.loads(path.read_text(encoding="utf-8")) for path in [*args.timed, args.w3c, args.guards]]
    passed, text = verdict(parts[:-2], parts[-2], parts[-1])
    print(text)
    if args.out:
        args.out.write_text(text, encoding="utf-8")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
