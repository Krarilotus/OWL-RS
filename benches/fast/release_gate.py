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
import os
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
from suitekit import gatestats  # noqa: E402
from suitekit.compare import counter_changes  # noqa: E402
from suitekit import hosts as hosts_registry  # noqa: E402
from suitekit.hosts import run_identity  # noqa: E402
from suitekit.checkout import STORED_OBJECTS, checkout_differs  # noqa: E402,F401

CAMPAIGN_BLOCKS = 2  # blocks a round at least (each: every case on every side, twice, as a palindrome)
PASS_FILE = re.compile(r"pass-(\d+)-([A-Za-z0-9_.-]+)\.json$")


# --- plan --------------------------------------------------------------------------------------

def plan(catalog: list[dict], max_gb: float, selection: list[str] | None = None, min_gb: float = 0,
         big_gb: float | None = None, big_host: str = "khaleesi-wsl") -> tuple[list[str], dict, dict]:
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




def schedule_errors(passes: list[dict], sides: list[str], min_blocks: int,
                    status_only: set[str] | None = None, layouts: int = 1) -> list[str]:
    """What the passes' schedule must be. A pass is one side's run of one block (fast.py block:
    pass-<n>-<side>.json with its block, run and layout). Every block has every side's two runs;
    within a block each case ran on the sides as a palindrome (A B C C B A, or a rotation), its six
    runs back to back (no other case between them: each side's two runs are then centred on the
    same instant whatever the runs' durations, research round 8); blocks didn't overlap. Every case
    is in at least `min_blocks` blocks (a declared outcome's in one, `status_only`) and, with
    `layouts` > 1, its blocks cover every layout, each as often to within one."""
    errors = []
    blocks: dict[int, list[dict]] = {}
    for p in passes:
        r = p["report"]
        if not isinstance(r.get("block"), int) or r.get("run") not in (1, 2):
            errors.append(f"{p['path']}: no block and run (not a pass of fast.py block)")
            continue
        if not r.get("finished") or not isinstance(r.get("finished_ns"), int):
            errors.append(f"{p['path']}: no end time (unfinished pass)")
        blocks.setdefault(r["block"], []).append(p)
    spans = []
    case_blocks: dict[str, list[int]] = {}
    case_layouts: dict[str, Counter] = {}
    for b, members in sorted(blocks.items()):
        runs = Counter((p["side"], p["report"]["run"]) for p in members)
        if set(runs) != {(s, r) for s in sides for r in (1, 2)} or any(n != 1 for n in runs.values()):
            errors.append(f"block {b}: its runs are {sorted(runs)}, not each side's two")
            continue
        layout = {p["report"].get("layout", 1) for p in members}
        if len(layout) != 1:
            errors.append(f"block {b}: its sides ran different layouts {sorted(layout)}")
        records = sorted(((rec.get("started_ns") or 0, rec.get("finished_ns") or 0, rec["case"], p["side"])
                          for p in members for rec in p["report"].get("records", [])), key=lambda x: x[0])
        if not records:
            continue
        spans.append((records[0][0], max(r[1] for r in records), b))
        i = 0
        while i < len(records):
            case = records[i][2]
            run = records[i:i + 2 * len(sides)]
            if [r[2] for r in run] != [case] * (2 * len(sides)):
                errors.append(f"block {b}: {case}'s runs aren't back to back")
                break
            seq = [r[3] for r in run]
            if seq != seq[::-1] or sorted(seq[:len(sides)]) != sorted(sides):
                errors.append(f"block {b}: {case} ran {'-'.join(r[3] for r in run)}, not a palindrome")
            case_blocks.setdefault(case, []).append(b)
            case_layouts.setdefault(case, Counter())[next(iter(layout))] += 1
            i += 2 * len(sides)
    spans.sort()
    for (s1, e1, a), (s2, e2, b) in zip(spans, spans[1:]):
        if s2 < e1:
            errors.append(f"blocks {a} and {b} overlap")
    for case, seen in sorted(case_blocks.items()):
        least = 1 if case in (status_only or set()) else min_blocks
        if len(seen) < least:
            errors.append(f"{case}: {len(seen)} blocks, at least {least}")
        counts = case_layouts[case]
        if layouts > 1 and case not in (status_only or set()):
            want = set(range(1, layouts + 1))
            if len(seen) >= layouts and set(counts) != want:
                errors.append(f"{case}: its blocks ran layouts {sorted(counts)}, not all of 1..{layouts}")
            if counts and max(counts.values()) - min(counts.get(k, 0) for k in want) > 1:
                errors.append(f"{case}: layouts unevenly covered {dict(sorted(counts.items()))}")
    return errors


def provenance_errors(passes: list[dict], sides: dict[str, str], harness: str, planned: list[str]) -> list[str]:
    """What each pass must say about itself: complete, one repetition of exactly the planned
    cases, the clean harness revision at start and end, its side's verified binaries (the same
    in every pass of the side), the same prepared data unchanged, one host and configuration."""
    errors, binaries, prepared, identities, options, layouts = [], {}, set(), set(), {}, {}
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
        if not (r.get("instance") or {}).get("built"):
            # Blocks timing one cached instance of a build share its offset (lks-aa-20): no
            # independent units (fast.py block --fresh-copies).
            errors.append(f"{name}: timed its side's build in place, not a copy made for its block")
        layout = (r.get("binaries") or {}).get("layout", 1)
        binaries.setdefault((p["side"], layout), set()).add(
            json.dumps((r.get("binaries") or {}).get("sha256"), sort_keys=True))
        layouts.setdefault(p["side"], Counter())[layout] += 1
        info = r.get("prepared") or {}
        if not info:
            errors.append(f"{name}: not a prepared pass (fast.py run --prepared)")
        elif info.get("changed"):
            errors.append(f"{name}: prepared data changed during the pass: {info['changed'][:3]}")
        prepared.add(info.get("manifest_sha256"))
        options.setdefault(p["side"], set()).add(json.dumps(r.get("options"), sort_keys=True))
        identities.add(json.dumps({**run_identity(r.get("machine") or {}, r.get("started", "")),
                                   "runtime": r.get("runtime")}, sort_keys=True))
    errors += [f"{side}: different binaries in its passes" + (f" of layout {layout}" if layout > 1 else "")
               for (side, layout), shas in binaries.items() if len(shas) > 1]
    # Builds of several layouts (release-gate.sh GATE_LAYOUTS): every side the same layouts as
    # often, so that a layout's luck falls on all alike.
    if len({json.dumps(sorted(c.items())) for c in layouts.values()}) > 1:
        errors.append("the sides ran different layouts: " +
                      "; ".join(f"{s} {dict(sorted(c.items()))}" for s, c in sorted(layouts.items())))
    errors += [f"{side}: different options in its passes" for side, opts in options.items() if len(opts) > 1]
    # The measurement (runs, warm-ups, repetitions: --measure's file) is the same for every side.
    measures = {(p["report"].get("options") or {}).get("measure") for p in passes}
    if len(measures) > 1:
        errors.append("the passes measured with different calibrations (fast.py --measure)")
    # Every side's perf lab keeps its timed window alike (a thread started in it on one side
    # only is a bias of microseconds: PROTOCOL.md §5, measurement-sides.toml).
    timings = {(p["report"].get("options") or {}).get("timing") for p in passes}
    if len(timings) > 1:
        errors.append("the sides' perf labs time differently: " + ", ".join(sorted(map(str, timings))))
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
    """One side's evidence of a case, pass by pass, each pass's block and layout beside it."""
    out = {"status": [], "expected": [], "failed_checks": [], "failed_routes": [], "unchecked": 0, "notes": [],
           "samples": [], "blocks": [], "layouts": [],
           "process": [], "estimator": set(), "lifetime": [], "anon_kernel": [], "live": [], "kinds": set(),
           "phases": [], "cgroup": [], "qps": [],
           "client_peaks": [], "cache_peaks": [], "rows": [], "counters": {}, "batched": {}}
    for p in passes:
        if case not in (p["report"].get("planned_cases") or [case]):
            continue  # a later block's pass, without this case (its noise needs no more)
        record = next((r for r in p["report"]["records"] if r.get("case") == case), None)
        out["blocks"].append(p["report"].get("block", p["n"]))
        out["layouts"].append(p["report"].get("layout", 1))
        if record is None:
            out["status"].append("missing")
            out["expected"].append("ok")
            for key in ("samples", "process", "lifetime", "anon_kernel", "live", "phases", "cgroup", "qps",
                        "client_peaks", "cache_peaks", "rows"):
                out[key].append({} if key in ("samples", "process", "phases", "qps", "client_peaks", "cache_peaks",
                                              "rows") else None)
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
        out["kinds"] |= {o.get("peak_kind") for o in observations if o.get("peak_kind")}
        for key, field in (("anon_kernel", "anon_kernel_peak_bytes"), ("live", "peak_live_requested_bytes")):
            values = [o.get(field) for o in observations if isinstance(o.get(field), (int, float))]
            out[key].append(max(values) if values else None)
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
            # A series batched in every observation: its smallest batch (perf_lab --batch).
            for q, k in (o.get("batched") or {}).items():
                out["batched"][q] = min(out["batched"].get(q, k), k)
        for q in [q for q in out["batched"] if not all(q in (o.get("batched") or {}) for o in observations)]:
            del out["batched"][q]
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
    """A dimension's values per pass, each a list."""
    out = []
    for v in values:
        v = v.get(key) if key is not None else v
        out.append(v if isinstance(v, list) else ([] if v is None else [v]))
    return out


def quantum(sides: list[list[list[float]]]) -> float:
    """The coarsest step every value of a series is a whole multiple of (1, 0.1, 0.01 ms; else
    0): a tool that reports milliseconds to one decimal (the EL classifier's phases), or a time
    derived from seconds to three (an older perf lab's open_s), moves in such steps."""
    values = [v for side in sides for vs in side for v in vs]
    for q in (1.0, 0.1, 0.01):
        if values and all(abs(v / q - round(v / q)) < 1e-6 for v in values):
            return q
    return 0.0


def compare_case(cand: dict, base: dict, side: str, checked_queries: set[str], floor: dict | None = None) -> dict:
    """The candidate's case against one baseline's, dimension by dimension, by blocks; `floor`:
    the case's A-A' noise floor on this host, per dimension (read_floor()). `not_compared`: each
    dimension deliberately not tested, with its reason (one this baseline doesn't support, or one
    whose loss of evidence blocks); gate() holds every other untested hypothesis an error."""
    floor = floor or {}
    row = {"comparable": True, "dimensions": {}, "notes": [], "blocks": [], "reported": {}, "not_compared": {}}
    base_state = state(base, off_route_ok=True)
    if state(base) == "known" and state(cand) == "known":
        return {**row, "comparable": False, "reason": "a known issue on both sides (no timing verdict)"}
    if state(cand) == "known":
        if base_state == "ok":
            row["blocks"].append(f"stops at its declared outcome ({', '.join(sorted(set(cand['status'])))}) "
                                 f"where {side} completed")
        return {**row, "comparable": False, "reason": "the candidate's known issue"}
    if base_state != "ok":
        return {**row, "comparable": False, "reason": why_not(base, side)}
    if base["failed_routes"]:
        row["notes"].append(f"{side} is off the route the harness asserts ({base['failed_routes'][0]}); "
                            "its answers are right, its time compared")
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
            row["not_compared"][f"time/{q}"] = f"answers differ from {side}'s; the candidate's match the pinned ones"
        else:
            row["blocks"].append(f"{text}: answers differ and no check pins them")
            row["not_compared"][f"time/{q}"] = "answers differ (blocks)"
    layouts = {b: l for e in (cand, base) for b, l in zip(e["blocks"], e["layouts"])}
    bb, cb = base["blocks"], cand["blocks"]

    def judge_key(name, key, unit, higher=False, sub=None, step=None):
        bv, cv = per_pass(base[key], sub), per_pass(cand[key], sub)
        row["dimensions"][name] = dimension(bv, bb, cv, cb, layouts, unit, higher, step, floor.get(name, 0.0))

    adapted = "process-p50" in cand["estimator"] | base["estimator"]
    if adapted:
        row["notes"].append(f"times as one p50 per process on both sides ({side}'s perf lab reports no samples)")
    key = "process" if adapted else "samples"
    series = sorted({s for rep in cand[key] + base[key] for s in rep})
    for s in series:
        if s in excluded:
            continue
        on_cand = all(s in rep for rep in cand[key])
        on_base = all(s in rep for rep in base[key])
        if not on_cand and on_base:
            row["blocks"].append(f"time/{s}: lost (on {side}, not on every pass of the candidate)")
            row["not_compared"][f"time/{s}"] = "lost on the candidate (blocks)"
            continue
        if not on_base:
            row["notes"].append(f"time/{s}: not on every pass of {side}, not compared")
            row["not_compared"][f"time/{s}"] = f"not on every pass of {side} (unsupported by this baseline)"
            continue
        k = min(cand["batched"].get(s, 1), base["batched"].get(s, 1))
        step = max(COUNTER_STEP["ms"] / k, quantum([per_pass(base[key], s), per_pass(cand[key], s)]))
        judge_key(f"time/{s}", key, "ms", sub=s, step=step)
    if not series:
        row["blocks"].append("no timing evidence")
    # Memory (research round 8): judged where both sides report them, the scope's anonymous and
    # kernel memory (Linux: its cgroup's anon + kernel, sampled; the file cache left out) and the
    # perf lab's peak live requested bytes; the process's own peak (VmHWM on Linux, peak private
    # commit on Windows: its kind is named) and the phases' are reported, as is the cgroup's.
    # A baseline that reports none doesn't support it (an older perf lab, another system): stated.
    # The candidate must then report it on every pass: a reading lost on the candidate blocks
    # (Sol, 20 Oct 2026: it left the hypothesis untested and the gate passed a confirmed loss).
    for name, field in (("memory/anon_kernel", "anon_kernel"), ("memory/live", "live")):
        on_base = [v is not None for v in base[field]]
        on_cand = [v is not None for v in cand[field]]
        if not any(on_base):
            row["not_compared"][name] = f"{side} reports none (unsupported by this baseline)"
            if any(on_cand):
                row["notes"].append(f"{name}: {side} reports none, not compared")
        elif not all(on_cand):
            where = "on no pass" if not any(on_cand) else f"on {sum(on_cand)} of {len(on_cand)} passes"
            row["blocks"].append(f"{name}: the candidate reports it {where}, {side} does (its evidence lost)")
            row["not_compared"][name] = "missing on the candidate (blocks)"
        else:
            judge_key(name, field, "bytes")
    kinds = sorted(cand["kinds"] | base["kinds"]) or ["vmhwm"]
    for name, key_, unit, higher in (("qps", "qps", "qps", True),):
        for k in sorted({k for rep in cand[key_] + base[key_] for k in rep}):
            on_cand, on_base = (all(k in rep for rep in e[key_]) for e in (cand, base))
            if on_base and not on_cand:
                row["blocks"].append(f"{name}/{k}: lost (on {side}, not on every pass of the candidate)")
                row["not_compared"][f"{name}/{k}"] = "lost on the candidate (blocks)"
            elif on_base:
                judge_key(f"{name}/{k}", key_, unit, higher, sub=k)
            else:
                row["not_compared"][f"{name}/{k}"] = f"not on every pass of {side} (unsupported by this baseline)"
    reported = {}
    if any(base["lifetime"]) and any(cand["lifetime"]):
        reported[f"memory/{'+'.join(kinds)}"] = dimension(per_pass(base["lifetime"]), bb, per_pass(cand["lifetime"]),
                                                          cb, layouts, "MiB")
    for k in sorted({k for rep in cand["phases"] + base["phases"] for k in rep}):
        if all(k in rep for rep in base["phases"]) and all(k in rep for rep in cand["phases"]):
            reported[f"memory/phase/{k}"] = dimension(per_pass(base["phases"], k), bb, per_pass(cand["phases"], k),
                                                      cb, layouts, "MiB")
    row["reported"]["memory"] = {k: {f: v.get(f) for f in ("ratio", "ci", "p", "status")} for k, v in reported.items()}
    cgroups = [[v for v in e["cgroup"] if v] for e in (base, cand)]
    row["reported"]["cgroup_peak_mib"] = [statistics.median(v) if v else None for v in cgroups]
    common = {k: v for k, v in base["counters"].items() if k in cand["counters"]}
    grew, changes = counter_changes(common, {k: cand["counters"][k] for k in common})
    if changes:
        row["reported"]["work_counters"] = changes
    return row


def describe(r: dict) -> str:
    if r.get("ratio") is None:
        return f"{r.get('blocks', 0)} blocks with values on both sides"
    ci = f" [{r['ci'][0]:.4f}, {r['ci'][1]:.4f}]" if r.get("ci") else ""
    flip = f", sign-flip p {r['p_sign_flip']:.3g}" if r.get("p_sign_flip") is not None else ""
    return (f"{fmt(r['base'])} -> {fmt(r['new'])} {r['unit']}, ratio {r['ratio']:.4f}{ci}, one-sided p {r['p']:.3g}"
            f"{flip}, {r['units']} units of {r['blocks']} blocks")


def worse_end(r: dict) -> float | None:
    """The worse end of a dimension's interval, as a ratio: above 1 is worse."""
    if not r.get("ci"):
        return None
    return 1 / r["ci"][0] if r.get("higher_is_better") else r["ci"][1]


def block_contrasts(base: list[list[float]], base_blocks: list, new: list[list[float]], new_blocks: list,
                    layouts: dict) -> tuple[list[float], list, float, float]:
    """Per block present on both sides: log(median of the candidate's values in it / median of the
    baseline's); the blocks' layouts; the medians of all the baseline's and candidate's values."""
    def by_block(values, blocks):
        out: dict = {}
        for vs, b in zip(values, blocks):
            out.setdefault(b, []).extend(v for v in vs if isinstance(v, (int, float)) and math.isfinite(v))
        return out
    b_, n_ = by_block(base, base_blocks), by_block(new, new_blocks)
    contrasts, lays = [], []
    for block in sorted(set(b_) & set(n_), key=str):
        if b_[block] and n_[block]:
            mb, mn = statistics.median(b_[block]), statistics.median(n_[block])
            if mb > 0 and mn > 0:
                contrasts.append(math.log(mn / mb))
                lays.append(layouts.get(block, 1))
    every_b = [v for vs in b_.values() for v in vs]
    every_n = [v for vs in n_.values() for v in vs]
    return contrasts, lays, (statistics.median(every_b) if every_b else None), \
        (statistics.median(every_n) if every_n else None)


# The counters' own resolution (research round 8: not a tolerance; a difference within it the test
# doesn't reject is `unresolved`, never neutral): times to the reported step (quantum(), batches
# dividing it), peaks in bytes where reported, else whole MiB.
COUNTER_STEP = {"ms": 0.001, "MiB": 1.0, "bytes": 1.0, "qps": 0.0}


def dimension(base: list[list[float]], base_blocks: list, new: list[list[float]], new_blocks: list,
              layouts: dict, unit: str, higher_is_better: bool = False, step: float | None = None,
              floor: float = 0.0) -> dict:
    """One dimension: its block contrasts and the one-sided test of a loss (gatestats.loss_test),
    its spread at least the host's A-A' floor."""
    contrasts, lays, b, n = block_contrasts(base, base_blocks, new, new_blocks, layouts)
    step = COUNTER_STEP[unit] if step is None else step
    r = gatestats.loss_test(contrasts, lays, higher_is_better, resolution=step, scale=b or 0, floor=floor)
    out = {"unit": unit, "higher_is_better": higher_is_better, "base": b, "new": n, "step": step, **r,
           "contrasts": [round(c, 6) for c in contrasts],
           "ratio": math.exp(r["mean_log"]) if r["mean_log"] is not None else None}
    if r["ci"]:
        # The interval of new/base (gatestats tests the worse direction: undo its sign).
        lo, hi = r["ci"]
        out["ci"] = [math.exp(-hi), math.exp(-lo)] if higher_is_better else [math.exp(lo), math.exp(hi)]
        if higher_is_better:
            out["ratio"] = math.exp(-r["mean_log"])
    if r["verdict"] == "incomplete":
        out["status"] = "incomplete"
    elif r["p"] <= gatestats.ALPHA * gatestats.PAIR_WEIGHTS[0]:
        out["status"] = "loss shown"         # this round: a confirmation is due (rounds_needed)
    elif r.get("below_resolution"):
        out["status"] = "unresolved"         # within the counter's step, no loss shown
    else:
        out["status"] = "no loss shown"
    return out


def better_shown(r: dict) -> bool:
    """A win: the whole interval on the better side."""
    end = worse_end(r)
    return end is not None and end < 1


def hypothesis(side: str, case: str, dimension_: str) -> str:
    return f"{side}|{case}|{dimension_}"


# The phases each classifier tool times (fast.py's samples(): the profile's fields it reports).
DL_TOOL_PHASES = {"classify-el": ("read", "normalise", "prepare", "saturate", "assemble", "write"),
                  "classify-horn": ("read", "owl", "write")}


def family_of(catalog: list[dict], cases: list[str], baselines: tuple[str, ...] = ("base", "main")) -> list[str]:
    """The comparison family, declared before any result (research round 8): every planned
    case's series and gated metrics, against each baseline. A case with a declared outcome is
    checked, not timed: it adds none."""
    import fast
    manifest_ = fast.case_manifest(catalog)
    by = {c["name"]: c for c in catalog}
    out = []
    for name in cases:
        case = by[name]
        if case.get("outcome", "ok") != "ok":
            continue
        tool = case.get("tool", "perf_lab")
        series = manifest_["series"].get(name) or (list(fast.DL_PHASES) if tool == "tableau" else
                                                   list(DL_TOOL_PHASES.get(tool, ["_"])))
        dims = [f"time/{s}" for s in series]
        dims += [f"qps/{n}" for n in manifest_["clients"].get(name, [])]
        dims.append("memory/anon_kernel")
        if "--count-allocations" in case.get("args", []):
            dims.append("memory/live")
        out += [hypothesis(side, name, d) for side in baselines for d in dims]
    return out


def block_layout(block: int, layouts: int) -> int:
    """The layout block `block` (from 1) runs: they rotate, so any K blocks in a row cover all K
    (research round 8: the old rotation never ran a third of the layouts when 3 divided K)."""
    return (block - 1) % layouts + 1 if layouts > 1 else 1


def block_step(gate_: list[str], n: int, harness: str, layout: int, plan_: str | None, revs: dict) -> list[str]:
    return [*gate_, "block", str(n), harness, str(layout), plan_ or "-",
            *(f"{side}={revs[side]}" for side in ("base", "main", "cand"))]


def decide(parts: list[list[dict]]) -> dict:
    """Every declared hypothesis of every timed part through its rounds: its p-value per round, the
    combined p (gatestats.combine_rounds), and Holm over the whole family. `losses`: rejected
    (blocks unless an approved trade-off covers it); `pending`: a round that could still reject it
    hasn't run (round 2 for a round-1 p within 0.9 alpha; round 3 for one measured in round 2 that
    round 3 could still get rejected, whether or not round 1 flagged it: a loss first seen while
    confirming another is confirmed too, not dropped); everything else passes at the end of its
    budget."""
    family, rounds_p, where = [], {}, {}
    for rounds in parts:
        declared = rounds[0].get("family") or list(rounds[0].get("pvalues", {}))
        family += [h for h in declared if h not in where]
        for h in declared:
            where[h] = rounds
        for k, report in enumerate(rounds[:3]):
            for h, p in (report.get("pvalues") or {}).items():
                rounds_p.setdefault(h, [None, None, None])[k] = p
    combined = {h: gatestats.combine_rounds(rounds_p.get(h, [None, None, None])) for h in family}
    rejected = gatestats.holm(combined)
    # Round 3 where it could still decide: Holm rejects it if every hypothesis measured in round 2
    # came out of round 3 at p = 0 (Holm only rejects more as p-values fall, so nothing outside
    # that set ever can). With a large family that takes a round-2 p near alpha / m, not 0.1 alpha.
    third = {h for h in family
             if h not in rejected and len(where[h]) == 2 and rounds_p.get(h, [None] * 3)[1] is not None}
    best = gatestats.holm({h: gatestats.combine_rounds([*rounds_p[h][:2], 0.0]) if h in third else p
                           for h, p in combined.items()})
    pending = []
    for h in family:
        if h in rejected:
            continue
        ps = rounds_p.get(h, [None, None, None])
        if ps[0] is not None and ps[0] <= gatestats.ALPHA * gatestats.PAIR_WEIGHTS[0] and len(where[h]) < 2:
            pending.append(h)
        elif h in third and h in best:
            pending.append(h)
    return {"family": family, "m": len(family), "combined": combined, "rounds": rounds_p,
            "losses": sorted(rejected, key=lambda h: combined[h]), "pending": pending}


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


def accepted(tradeoffs: list[dict], side: str, case: str, dimension_: str, r: dict, comparisons: dict) -> dict | None:
    """The approved trade-off covering this loss, if one does: its case, dimension and baseline,
    the loss's interval within its max_ratio, and its paying win there."""
    for t in tradeoffs:
        if (t["case"], t["dimension"]) != (case, dimension_) or t.get("against", side) != side:
            continue
        end = worse_end(r)
        if end is None or end > t["max_ratio"]:
            continue
        win = comparisons.get(side, {}).get(t["pays_with"]["case"], {}).get("dimensions", {}).get(
            t["pays_with"]["dimension"])
        if win and better_shown(win):
            return t
    return None


def noise_floor(summary: dict) -> dict:
    """The host's noise floor from an A-A' gate (every side the same revision; fast.py block's copies
    make them different instances): per case and dimension, the sd of one block's log contrast,
    pooled over the baselines. lks-aa-20 (20 Oct 2026): the instances' offsets reached 0.045 on
    creaders' read p50 against 0.0075 between the blocks of one instance."""
    revisions = {summary["candidate"]["revision"], *summary["baselines"].values()}
    if len(revisions) != 1:
        raise ValueError(f"not an A-A' run: the sides are {len(revisions)} revisions")
    if summary["errors"]:
        raise ValueError("its passes don't qualify: " + "; ".join(summary["errors"][:3]))
    pooled: dict = {}
    for rows in summary["comparisons"].values():
        for case, row in rows.items():
            for d, r in row["dimensions"].items():
                c = r.get("contrasts") or []
                if len(c) >= 2:
                    pooled.setdefault(case, {}).setdefault(d, []).append((statistics.variance(c), len(c) - 1))
    cases = {case: {d: math.sqrt(sum(v * df for v, df in vs) / sum(df for _, df in vs)) for d, vs in dims.items()}
             for case, dims in pooled.items()}
    blocks = len({p["block"] for p in summary["passes"]})
    return {"host": ", ".join(map(str, summary["host"])), "blocks": blocks, "cases": cases}


def write_floor(floor: dict, source: str) -> str:
    lines = [f"# The A-A' noise floor of {floor['host']} (release_gate.py floor; benches/PROTOCOL.md §5):",
             "# per case and dimension, the sd of one block's log contrast between copies of one build.",
             f"# From {source}, {floor['blocks']} blocks. Written by the tool, not by hand.",
             f'source = "{source}, {floor["blocks"]} blocks"', ""]
    for case, dims in sorted(floor["cases"].items()):
        lines.append(f'[cases."{case}"]')
        lines += [f'"{d}" = {sd:.6f}' for d, sd in sorted(dims.items())] + [""]
    return "\n".join(lines)


def read_floor(path: Path) -> dict | None:
    """A host's floor (noise-floor/<host>.toml), or None where none was measured."""
    if not path.is_file():
        return None
    data = tomllib.loads(path.read_text(encoding="utf-8"))
    return {"source": f"{path.name}: {data.get('source', '?')}", "cases": data.get("cases", {})}


FLOORS = ROOT / "benches" / "fast" / "noise-floor"


def quiet_verdicts(directory: Path) -> tuple[dict, list[str]]:
    """Each block's quiet-watcher verdict (release-gate.sh block: quiet-block-<n>.json), and a
    note for each block work that isn't the job's own disturbed (scripts/lib/quiet-watch.py
    suspends only the job's own: other work is left running and its load recorded), or whose
    suspensions weren't all resumed. A disturbed block was left out and run once more
    (quiet-block-<n>-disturbed-<t>.json, its reports disturbed-*); a repeat disturbed again
    was kept ("repeat")."""
    verdicts, notes = {}, []
    for n in sorted({int(m.group(1)) for p in directory.glob("quiet-block-*-disturbed-*.json")
                     if (m := re.search(r"quiet-block-(\d+)-disturbed-\d+\.json$", p.name))}):
        notes.append(f"block {n}: disturbed: left out of the statistics (both sides, kept as disturbed-*) "
                     "and run once more")
    found = [(int(m.group(1)), p) for p in directory.glob("quiet-block-*.json")
             if (m := re.search(r"quiet-block-(\d+)\.json$", p.name))]
    for n, path in sorted(found):
        try:
            v = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            notes.append(f"block {n}: its quiet verdict can't be read ({path.name})")
            continue
        verdicts[n] = v
        if v.get("verdict") == "disturbed":
            notes.append(f"block {n}: {'its repeat ' if v.get('repeat') else ''}disturbed"
                         f"{', kept' if v.get('repeat') else ''}: work that isn't this job's used more than a core in "
                         f"{v.get('unowned_share', 0):.0%} of {v.get('samples', 0)} samples (left running, never suspended)")
        if v.get("not_resumed"):
            notes.append(f"block {n}: suspensions not resumed: {v['not_resumed']}")
    return verdicts, notes


def fmt(x) -> str:
    return "-" if x is None else f"{x:.4g}"


# --- the timed part ----------------------------------------------------------------------------

def gate(passes: list[dict], sides: dict[str, str], candidate: str, required: list[str], harness: str,
         min_blocks: int = CAMPAIGN_BLOCKS, diagnostic: bool = False, not_run: dict | None = None,
         tradeoffs: list[dict] | None = None, catalog: list[dict] | None = None, elsewhere: dict | None = None,
         context: dict | None = None, family: list[str] | None = None, floor: dict | None = None) -> dict:
    """One round of one timed part: the schedule and provenance checked, every comparison's
    blocks tested; each declared hypothesis's p-value (untested: 1). The decision over the family
    and the rounds is verdict()'s. `floor`: the host's A-A' noise floor (read_floor())."""
    not_run, tradeoffs, elsewhere = dict(not_run or {}), list(tradeoffs or []), dict(elsewhere or {})
    if candidate not in sides:
        raise ValueError(f"the candidate {candidate} is no declared side")
    baselines = [s for s in sides if s != candidate]
    # A baseline of an earlier baseline's revision (a confirmation job's main that is its base):
    # one build, timed once as that one (release-gate.sh block); it is that side here, and its
    # hypotheses are that side's, not declared twice.
    aliases = {}
    for k, s in enumerate(baselines):
        same = next((t for t in baselines[:k] if sides[t] == sides[s] and t not in aliases), None)
        if same is not None:
            aliases[s] = same
    context = dict(context or {})
    summary = {"candidate": {"name": candidate, "revision": sides[candidate]},
               "baselines": {s: sides[s] for s in baselines}, "harness": harness,
               "policy": {"alpha": gatestats.ALPHA, "pair_weights": gatestats.PAIR_WEIGHTS,
                          "test": "one-sided t on log block contrasts, layouts the outer unit, the spread at "
                                  "least the host's A-A' floor; Holm over the family",
                          "floor": (floor or {}).get("source")},
               "passes": [{"n": p["n"], "side": p["side"], "file": p["path"], "block": p["report"].get("block"),
                           "run": p["report"].get("run"), "layout": p["report"].get("layout")}
                          for p in sorted(passes, key=lambda p: p["n"])],
               "diagnostic": diagnostic or min_blocks < CAMPAIGN_BLOCKS, "required": required,
               "context": context, "flagged": [], "other_blocking": [], "pvalues": {},
               "not_run": not_run, "elsewhere": elsewhere, "tradeoffs": tradeoffs, "accepted": [], "errors": [],
               "candidate_failures": {}, "known_issues": {}, "fixed": {}, "comparisons": {}, "blocking": []}
    if missing := [s for s in required if s not in baselines]:
        summary["errors"].append(f"required baselines missing: {', '.join(missing)}")
    if len(baselines) < 1:
        summary["errors"].append("no baseline")
    status_only = {c["name"] for c in (catalog or []) if c.get("outcome", "ok") != "ok"}
    summary["aliases"] = aliases
    summary["errors"] += schedule_errors(passes, [s for s in sides if s not in aliases], min_blocks, status_only,
                                         int(context.get("layouts", 1) or 1))
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
            if state(cand) == "failed" or side in aliases:
                continue
            row = compare_case(cand, evidence(by_side[side], case), side, checks.get(case, set()),
                               ((floor or {}).get("cases") or {}).get(case))
            summary["comparisons"].setdefault(side, {})[case] = row
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
    declared = [h for h in family if h.split("|", 1)[0] not in aliases] if family is not None else [
        hypothesis(side, case, d) for side, rows in summary["comparisons"].items()
        for case, row in rows.items() for d in row["dimensions"]]
    summary["family"] = declared
    for side, rows in summary["comparisons"].items():
        for case, row in rows.items():
            summary["blocking"] += [f"{case} against {side}: {b}" for b in row["blocks"]]
            for d, r in row["dimensions"].items():
                h = hypothesis(side, case, d)
                if h not in declared:
                    row["notes"].append(f"{d}: not in the declared family, reported only")
                    continue
                summary["pvalues"][h] = r["p"]
                if r["status"] == "incomplete":
                    summary["blocking"].append(f"{case} against {side}: {d}: incomplete ({describe(r)})")
                elif r["status"] == "loss shown":
                    summary["flagged"].append({"side": side, "case": case, "dimension": d,
                                               **{k: r.get(k) for k in ("p", "ratio", "ci", "units", "blocks", "unit")}})
    summary["untested"] = sorted(set(declared) - set(summary["pvalues"]))
    # Fail closed: a declared hypothesis of a case this round ran that has no test and no stated
    # reason (the candidate failing the case, a baseline that can't run it or doesn't support the
    # metric, evidence lost on the candidate, which blocks) has lost its candidate evidence.
    summary["not_compared"] = {}
    for h in summary["untested"]:
        side, case, d = h.split("|", 2)
        row = summary["comparisons"].get(side, {}).get(case)
        if case not in planned or case in summary["candidate_failures"] or side not in sides:
            continue
        if row is not None and not row["comparable"]:
            summary["not_compared"][h] = row.get("reason", "not comparable")
        elif row is not None and d in row["not_compared"]:
            summary["not_compared"][h] = row["not_compared"][d]
        else:
            summary["errors"].append(f"{h}: declared, but no evidence from the candidate and no reason")
    summary["other_blocking"] = list(summary["blocking"])
    summary["counts"] = {side: dict(Counter(r["status"] for row in rows.values() for r in row["dimensions"].values()))
                         for side, rows in summary["comparisons"].items()}
    # One round decides nothing alone: a loss needs two (verdict()). This round passes on its
    # own only when nothing is flagged, failed or missing.
    summary["passed"] = (not summary["diagnostic"] and not summary["errors"] and not summary["blocking"]
                         and not summary["flagged"])
    return summary


def render(summary: dict) -> str:
    c = summary["candidate"]
    verdict_ = "PASSED" if summary["passed"] else ("DIAGNOSTIC ONLY, not a release verdict" if summary["diagnostic"]
                                                   else "NOT PASSED on its own (verdict combines the rounds)")
    blocks = sorted({p["block"] for p in summary["passes"] if p.get("block") is not None}, key=str)
    lines = [f"# Release gate round {summary['context'].get('round', 0)}: {c['name']} {c['revision']}", "",
             f"Against {', '.join(f'{s} {r}' for s, r in summary['baselines'].items())}; harness {summary['harness']}; "
             f"host {', '.join(map(str, summary['host']))}; {len(blocks)} blocks, {len(summary['passes'])} side-runs."
             + "".join(f" {s} is {t}'s build (one revision): timed once, as {t}; its hypotheses are {t}'s."
                       for s, t in summary.get("aliases", {}).items()), "",
             f"**{verdict_}.** Each declared hypothesis (case x series or metric x baseline: {len(summary['family'])}) "
             "gets a one-sided p-value of a loss from its block contrasts (layouts the outer unit); a loss counts "
             f"when two consecutive rounds show it and Holm over the family rejects at {gatestats.ALPHA} "
             "(benches/PROTOCOL.md §5). Flagged here: due for confirmation.", ""]
    if summary["errors"]:
        lines += ["## The passes don't qualify", ""] + [f"- {e}" for e in summary["errors"]] + [""]
    floor = summary["policy"].get("floor")
    lines += [f"Noise floor: {floor}." if floor else
              "Noise floor: none measured for this host (release_gate.py floor): each test against its "
              "blocks' own spread alone.", ""]
    if summary.get("inputs") or summary.get("prepared_manifest"):
        prepared = ", ".join(summary.get("prepared_manifest", []))
        lines += ["## Inputs", "", f"Prepared data: PREPARED.json sha256 {prepared}, the same in every pass.", ""]
        lines += [f"- {line}" for line in summary.get("inputs", [])] + [""]
    lines += ["## Blocking (not statistical)", ""] + ([f"- {b}" for b in summary["blocking"]] or ["- nothing"]) + [""]
    if summary["flagged"]:
        lines += ["## Flagged: a loss shown this round, a confirmation due", "",
                  "| baseline | case / dimension | ratio | interval | p | units |", "|---|---|---|---|---|---|"]
        for f in sorted(summary["flagged"], key=lambda f: f["p"]):
            ci = f"{f['ci'][0]:.4f}-{f['ci'][1]:.4f}" if f.get("ci") else "-"
            lines.append(f"| {f['side']} | {f['case']} / {f['dimension']} | {fmt(f['ratio'])} | {ci} | {f['p']:.3g} | "
                         f"{f['units']} |")
        lines.append("")
    if summary.get("host_notes"):
        lines += ["## The host as the passes met it (scripts/hosts/hygiene.py)", ""] + \
                 [f"- {n}" for n in summary["host_notes"]] + [""]
    if summary.get("untested"):
        lines += [f"Declared but untested here ({len(summary['untested'])}, p = 1): " +
                  ", ".join(summary["untested"][:40]) + (" ..." if len(summary["untested"]) > 40 else ""), ""]
    if summary["known_issues"]:
        lines += ["Known issues as declared (no timing verdict): " +
                  ", ".join(f"{k} ({v})" for k, v in summary["known_issues"].items()), ""]
    if summary["fixed"]:
        lines += ["Declared issues the candidate now passes (update their declaration): " +
                  ", ".join(f"{k} ({v})" for k, v in summary["fixed"].items()), ""]
    for side, rows in summary["comparisons"].items():
        counts = summary["counts"].get(side, {})
        lines += [f"## Against {side} {summary['baselines'][side]}", "",
                  f"{len(rows)} cases; dimensions: " + ", ".join(f"{n} {v}" for v, n in sorted(counts.items())), ""]
        unresolved = [f"{case} {d}" for case, row in rows.items() for d, r in row["dimensions"].items()
                      if r["status"] == "unresolved"]
        if unresolved:
            lines += [f"Unresolved (within the counter's step, no loss shown): {', '.join(unresolved)}", ""]
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
    return errors, notes + stall_notes(passes)


STALL_SHARE = 0.02  # a process that waited this share of its case's time met what others didn't


def stall_notes(passes: list[dict]) -> list[str]:
    """The cases whose processes waited, for a CPU, I/O or memory (their scope's pressure
    stalls, fast.py `stall_ms`), more than STALL_SHARE of the case's time: where a pass that
    stands apart met another process, a missing page cache or reclaim."""
    out = []
    for p in sorted(passes, key=lambda p: p["n"]):
        for r in p["report"].get("records", []):
            wall_ms = (r.get("wall_s") or r.get("case_s") or 0) * 1000
            stalls: dict[str, float] = {}
            for o in r.get("observations") or []:
                for kind, ms in (o.get("stall_ms") or {}).items():
                    stalls[kind] = stalls.get(kind, 0) + ms
            high = {k: v for k, v in stalls.items() if wall_ms and v > STALL_SHARE * wall_ms}
            if high:
                out.append(f"{r['case']} in pass {p['n']} ({p['side']}) waited " +
                           ", ".join(f"{k} {v:.0f} ms" for k, v in sorted(high.items())) +
                           f" of {wall_ms / 1000:.1f} s")
    return out


# --- the measurement calibration (benches/fast/measure.toml) -----------------------------------
#
# The gate's power against a small loss is bounded by each series' noise between passes
# (benches/NOISY.md). `calibrate` sets, per case and from the gate's own passes, what the
# measurement does about it, the same for every side: more executions a process where they are
# cheap (the perf lab's --runs, the DL tools' files a process, a batch of executions a sample
# where every side's perf lab has --batch), and more blocks for the cases whose noise needs
# them to catch a TARGET_LOSS with TARGET_POWER a round (the one-sided t at alpha / m, analytic:
# gatestats.power_at, benches/probes/gate-power.py), within the timed part's budget.

TARGET_LOSS, TARGET_POWER = 0.02, 0.90 ** 0.5  # a round's power: two rounds give 90 % together
QUERY_MS = 2000.0      # a perf-lab process's measured query time, at most (the runs it buys)
DL_MS = 3000.0         # a DL tool's process: its files' time, at most
MAX_RUNS, MAX_REPEAT = 2000, 30
PROCESS_S = 15.0       # a case's fresh processes a pass, at most this long together ...
MAX_PROCESSES = 5      # ... and at most this many (a process that started differently, outvoted)
PROCESS_BLOCKS = 15    # only where this many blocks alone don't reach the target
PROCESS_TOOLS = ("perf_lab", "tableau", "classify-el", "classify-horn")
KEPT = ("runs", "warmup", "batch", "batch_queries", "repeat", "processes")
BATCH_SAMPLE_MS = 0.05  # a batched sample lasts at least this (50 µs: the timer's steps far below)
BATCH_BELOW_MS = 0.25   # queries batched: those whose 2 % loss is below the 5 µs resolution
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


def judged_noise(rows: list[dict], resolution_ms: float = 0.001) -> float:
    """A case's noise for its blocks: the largest of its series whose TARGET_LOSS is above the
    timing resolution and the series' reporting step (a smaller series can't show that loss at
    any number of passes)."""
    judged = [r["sd"] for r in rows if r["median_ms"] * TARGET_LOSS >= max(resolution_ms, r.get("quantum", 0.0))]
    return max(judged, default=0.0)


def calibrate(catalog: list[dict], passes: list[dict], budget_min: float, max_blocks: int = 30,
              min_blocks: int = CAMPAIGN_BLOCKS, sides: int = 3, m: int | None = None,
              batch: bool = True, keep: dict | None = None, side_weights: dict | None = None) -> dict:
    """Each case's measurement from passes of the gate's host (an A/A run, or a gate's): runs,
    warm-ups, batch and repetitions from its times, blocks from its noise. Every case gets
    `min_blocks`; the blocks beyond go where they buy the most power against TARGET_LOSS a second
    of the timed part (weighted by the case's judged series, at most `max_blocks`, power at the
    family's size `m`), until `budget_min`. The passes' binaries are the sides (an A/A run's one
    binary counts `sides` times). `keep`: the calibration the passes were measured with: a case's
    settings there stay (the passes' times include them), only its noise and blocks are new.
    `side_weights`: per case, a gate pass of every side in units of the candidate's (from a real
    gate's passes, `side_weights()`; an older baseline runs fewer cases, or faster): an A/A run's
    times are the candidate's alone."""
    cases = {c["name"]: c for c in catalog}
    by_case: dict[str, dict] = {}
    revs = set()
    if m is None:
        present = sorted({r["case"] for p in passes for r in p["report"].get("records", []) if r["case"] in cases})
        m = max(1, len(family_of(catalog, present)))
    for p in passes:
        rev = (p["report"].get("binaries") or {}).get("revision") or p["side"]
        revs.add(rev)
        for r in p["report"].get("records", []):
            entry = by_case.setdefault(r["case"], {"seconds": {}, "series": {}, "status": set(), "measured": {},
                                                   "quanta": {}})
            # A case stopped at its limit and run once more past it (frontier) took that rerun's
            # time too, which a gate's pass doesn't spend: its own measurement's then.
            entry["seconds"].setdefault(rev, []).append((r.get("wall_s") if r.get("frontier") else r.get("case_s")) or 0)
            entry["status"].add(r.get("status"))
            entry["measured"] = r.get("measurement") or entry["measured"]
            if r.get("status") == "ok":
                for s, values in (r.get("samples") or {}).items():
                    if values:
                        entry["series"].setdefault((rev, s), []).append(statistics.median(values))
                        entry["quanta"].setdefault((rev, s), []).append(values)
    per_side = sides if len(revs) == 1 else 1
    out, rounds, weights, processes = {}, {}, {}, {}
    for name, e in sorted(by_case.items()):
        case = cases.get(name)
        if case is None:
            continue
        rows = [{"rev": rev, "series": s, "median_ms": statistics.median(m),
                 "sd": statistics.stdev(math.log(v) for v in m), "quantum": quantum([e["quanta"].get((rev, s), [])])}
                for (rev, s), m in e["series"].items() if len(m) >= 2 and min(m) > 0]
        seconds = {rev: statistics.mean(v) for rev, v in e["seconds"].items()}
        # One execution of every query (a DL tool's file: its phases), on the slowest side that
        # ran the case (a baseline that can't run it has no times).
        per_run = max((sum(r["median_ms"] for r in rows if r["rev"] == rev) for rev in revs), default=0.0)
        added = 0.0  # a process's added measured time, on every side
        entry = {}
        measured_runs = e["measured"].get("runs")
        kept = {k: v for k, v in (keep or {}).get(name, {}).items() if k in KEPT}
        if kept:
            entry.update(kept)
        elif runs_adjustable(case) and "ok" in e["status"] and rows:
            runs, warmup = case_runs(case)
            # The short queries batched, each sample well above the timer's steps (applied
            # where every side's perf lab batches; otherwise the runs alone).
            short = sorted({r["series"] for r in rows if r["median_ms"] < BATCH_BELOW_MS})
            k = 1
            if batch and short:
                fastest = min(r["median_ms"] for r in rows if r["series"] in short)
                k = min(1000, math.ceil(BATCH_SAMPLE_MS / max(fastest, 1e-4)))
            # The runs as if unbatched (a gate whose baselines can't batch runs them so); a
            # batching gate's processes take `batch` times as long.
            new_runs = min(MAX_RUNS, max(runs, math.ceil(QUERY_MS / max(per_run, 1e-3))))
            entry.update(runs=new_runs, warmup=max(warmup, 3 if new_runs >= 20 else warmup))
            if k > 1:
                entry["batch"] = k
                if len(short) < len({r["series"] for r in rows}):
                    entry["batch_queries"] = short
            added = (new_runs - (measured_runs or runs)) * per_run / 1000
        elif case.get("tool") in ("tableau", "classify-horn") and "ok" in e["status"] and rows:
            # Files a process (the tableau's), or classifications whose median it reports
            # (the context classifier's).
            whole = per_run if case["tool"] == "tableau" else max(r["median_ms"] for r in rows)
            repeat = int(case.get("repeat", 1))
            limit = 0.4 * float(case.get("timeout_s", 300)) * 1000 / max(whole, 1e-3)
            new = int(min(MAX_REPEAT, limit, max(repeat, math.ceil(DL_MS / max(whole, 1e-3)))))
            if new > repeat:
                entry["repeat"] = new
                added = (new - (e["measured"].get("repeat") or repeat)) * whole / 1000
        process_s = 0.0
        if (not kept and "ok" in e["status"] and rows and case.get("tool", "perf_lab") in PROCESS_TOOLS
                and gatestats.power_at(PROCESS_BLOCKS, judged_noise(rows), m) < TARGET_POWER):
            # Fresh processes a pass where passes alone won't do: a pass's value the median
            # over several processes, one that started differently (another memory layout)
            # outvoted. The perf lab's are `repeat`; a DL tool's `processes` (its `repeat`
            # repeats inside one process).
            key = "repeat" if case.get("tool", "perf_lab") == "perf_lab" else "processes"
            have = int(case.get(key, 1))
            process_s = (statistics.mean(seconds.values()) + added) / max(e["measured"].get(key) or have, 1)
            new = int(min(MAX_PROCESSES, max(have, PROCESS_S // max(process_s, 0.1))))
            if new > have:
                entry[key] = new
                added += (new - have) * process_s
        sigma = judged_noise(rows)
        status = case.get("outcome", "ok") != "ok"
        entry.update(noise=round(sigma, 4), blocks=1 if status else min_blocks,
                     seconds=round(statistics.mean(seconds.values()) + added, 1))
        rounds[name] = per_side * sum(s + added for s in seconds.values())  # a pass of every side
        if side_weights and len(revs) == 1 and name in side_weights:
            rounds[name] = rounds[name] / sides * side_weights[name]
        key = "repeat" if case.get("tool", "perf_lab") == "perf_lab" else "processes"
        processes[name] = (key, int(case.get(key, 1)), process_s * per_side * len(seconds))
        weights[name] = max(1, len({r["series"] for r in rows if r["median_ms"] * TARGET_LOSS >= 0.001}))
        out[name] = entry

    def minutes(plan: dict) -> float:
        count = max((e["blocks"] for e in plan.values()), default=0)
        total = 0.0
        for k in range(1, count + 1):
            in_block = [rounds[c] for c, e in plan.items() if e["blocks"] >= k]
            total += 2 * sum(in_block) + PASS_OVERHEAD_S  # each side twice, then the block's own time
        return total / 60

    # Over the budget at the least passes: the added processes go first, the costliest first.
    while minutes(out) > budget_min:
        grown = [c for c, e in out.items() if processes[c][2] and e.get(processes[c][0], 0) > processes[c][1]]
        if not grown:
            break
        c = max(grown, key=lambda c: (out[c][processes[c][0]] - processes[c][1]) * processes[c][2])
        key, have, cost = processes[c]
        out[c][key] -= 1
        rounds[c] -= cost
        out[c]["seconds"] = round(out[c]["seconds"] - cost / per_side / max(len(revs), 1), 1)
        if out[c][key] == have:
            del out[c][key]
    power = lambda c, n: gatestats.power_at(n, out[c]["noise"], m)  # noqa: E731
    spent = minutes(out)
    while True:
        top = max(e["blocks"] for e in out.values())
        best = None
        for c, e in out.items():
            if e["blocks"] < min_blocks:
                continue  # a status-only case (a declared outcome) isn't timed
            for n in range(e["blocks"] + 1, max_blocks + 1):
                gain = weights[c] * (power(c, n) - power(c, e["blocks"]))
                cost = (n - e["blocks"]) * 2 * rounds[c] / 60 + max(0, n - top) * PASS_OVERHEAD_S / 60
                if gain > 1e-3 and spent + cost <= budget_min and (best is None or gain / cost > best[0]):
                    best = (gain / cost, c, n, cost)
                if power(c, n) >= TARGET_POWER:
                    break
        if best is None:
            break
        out[best[1]]["blocks"] = best[2]
        spent = minutes(out)
    for e in out.values():
        if e["blocks"] >= min_blocks:
            e["power"] = round(power(next(c for c in out if out[c] is e), e["blocks"]), 3)
    return {"cases": out, "minutes": round(minutes(out), 1), "cap": max(e["blocks"] for e in out.values()),
            "budget_min": budget_min, "m": m}


def side_weights(passes: list[dict], candidate: str = "cand") -> dict[str, float]:
    """Per case, the mean time of a pass of every side over the candidate's (a real gate's
    passes: base, main and cand)."""
    times: dict[str, dict[str, list[float]]] = {}
    for p in passes:
        for r in p["report"].get("records", []):
            times.setdefault(r["case"], {}).setdefault(p["side"], []).append(r.get("case_s") or 0)
    out = {}
    for case, by_side in times.items():
        cand = statistics.mean(by_side.get(candidate) or [0])
        if cand > 0:
            out[case] = sum(statistics.mean(v) for v in by_side.values()) / cand
    return out


def write_measure(result: dict, source: str) -> str:
    lines = ["# The release gate's measurement per case (fast.py --measure; benches/PROTOCOL.md §5, \"Noise\").",
             "# Written by `release_gate.py calibrate`, not by hand: " + source + ".",
             "# runs, warmup, batch: the perf lab's a process; repeat, processes: the DL tool's; blocks: a round's,",
             f"# for {TARGET_POWER:.1%} power a round against a {TARGET_LOSS:.0%} loss (one-sided at alpha / m, m = "
             f"{result.get('m')}) at the case's noise (the largest",
             "# sd of log pass medians of its series where that loss exceeds the resolution); power: at those passes;",
             f"# seconds: a side's run. Timed part {result['minutes']} min (budget {result['budget_min']}, blocks at most "
             f"{result['cap']}).", ""]
    for name, e in result["cases"].items():
        lines.append(f"[case.{name}]")
        lines += [f"{k} = {json.dumps(v) if isinstance(v, list) else v}" for k, v in e.items()]
        lines.append("")
    return "\n".join(lines)


def whole_squares(n: int, square: int) -> int:
    """n blocks rounded up to whole Latin squares of `square` blocks (fast.py palindrome(): one per
    timed side). Only whole squares give every side the first place alike; a case in 2 blocks of
    three sides leaves one side never opening (tf2-gate-1, 20 Oct 2026: a first run cost 15.8 %)."""
    return -(-n // square) * square if square > 1 else n


def blocks(plan_: list[str], measure: dict, min_blocks: int = CAMPAIGN_BLOCKS, *, square: int,
           checked: set | frozenset = frozenset()) -> list[list[str]]:
    """The cases of each block: block k (from 1) runs those whose blocks (measure.toml, at least the
    round's `min_blocks`) reach k, each case's count in whole squares of `square` blocks
    (whole_squares(): the timed sides); a declared outcome's case (`checked`) only the first, where
    it is checked, not timed. `square` has no default (Sol r1, 20 Oct 2026: a call without it
    planned 2 and 8 blocks for three sides). Sol r2: the plan files are the job's block steps
    (jobs() counts with the same minimum), and a declared outcome isn't rounded up to a square."""
    need = {c: 1 if c in checked else whole_squares(max(int(measure.get(c, {}).get("blocks", min_blocks)), min_blocks),
                                                    square) for c in plan_}
    count = max([whole_squares(min_blocks, square), *need.values()])
    return [[c for c in plan_ if need[c] >= k or k == 1] for k in range(1, count + 1)]


def read_measure(text: str | None) -> dict:
    return tomllib.loads(text).get("case", {}) if text else {}


# --- the fleet jobs ----------------------------------------------------------------------------

# Runs and notes from before 26 Oct 2026 name these sides by their ids before that day's history
# rewrite, 0f465633 and f9b515e8: the same trees.
BASE = "d348a8a1a9e40cd23381df1faa726d3ffa44948c"  # v1.5, the last recorded run's base
MAIN = "a0020134c20a33eabe8ead0402f814585b9c7fd0"  # main

# The sides measured at a reviewed commit on top of them (benches/PROTOCOL.md §5, "No thread
# started in a timed window"): a side's revision → that commit.
MEASUREMENT_SIDES = HERE / "measurement-sides.toml"
# All a measurement commit may change: the perf lab's sources.
PERF_LAB = "crates/nrese-store/examples/"


def measurement_table(path: Path = MEASUREMENT_SIDES) -> dict[str, dict]:
    """The sides table, by side. Sides and commits are full revisions: an abbreviated side would
    miss the lookup by full revision, and its side would be built as itself (Sol, T0 refs r1 S1)."""
    table = tomllib.loads(path.read_text(encoding="utf-8"))["side"]
    full = re.compile(r"[0-9a-f]{40}")
    bad = [rev for rev, side in table.items() if not full.fullmatch(rev) or not full.fullmatch(str(side.get("commit")))]
    if bad:
        raise SystemExit(f"{path.name}: a side and its commit are full revisions, not: {', '.join(bad)}")
    return table


def measurement_sides(path: Path = MEASUREMENT_SIDES) -> dict[str, str]:
    return {rev: side["commit"] for rev, side in measurement_table(path).items()}


def measured_at(repo: str, rev: str, sides: dict[str, str] | None = None) -> str:
    """The commit a side is built and timed from: its revision, or its measurement commit. That
    commit's only parent is the revision, and it changes nothing outside the perf lab's sources:
    the engine, its lockfile and its toolchain are the side's. Anything else is refused. It reads
    the objects as stored (STORED_OBJECTS), as the gate's checkout does."""
    import subprocess

    def git(*args: str) -> str:
        # Bytes git doesn't promise as UTF-8 (a commit message in another encoding, a path) stay
        # readable (Sol, PR #54 R1 fix review r3 D4).
        done = subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True, encoding="utf-8",
                              errors="surrogateescape", timeout=60, env={**os.environ, **STORED_OBJECTS})
        if done.returncode != 0:
            raise SystemExit(f"git {' '.join(args)}: {done.stderr.strip()}")
        return done.stdout
    rev = git("rev-parse", "--verify", f"{rev}^{{commit}}").strip()
    sides = measurement_sides() if sides is None else sides
    # A measurement commit is built only as its side's, under one name (its build is the side's).
    of = [side for side, commit in sides.items() if commit == rev]
    if of:
        raise SystemExit(f"{rev[:12]} is the measurement commit of {of[0][:12]}: build {of[0][:12]}")
    commit = sides.get(rev)
    if commit is None:
        return rev
    try:
        # The parents its object names: a graft or a shallow boundary changes what a walk reports
        # (Sol, PR #54 R1 fix review D3).
        header = git("cat-file", "commit", f"{commit}^{{commit}}").split("\n\n", 1)[0]
        parents = [line.split()[1] for line in header.splitlines() if line.startswith("parent ")]
    except SystemExit as e:
        raise SystemExit(f"the measurement commit {commit[:12]} of {rev[:12]} isn't in this checkout ({e}): "
                         "a job that builds it takes it as an input (measurement_inputs)") from None
    if parents != [rev]:
        raise SystemExit(f"the measurement commit {commit[:12]} isn't a commit on {rev[:12]} alone: its parents "
                         f"are {', '.join(p[:12] for p in parents) or 'none'}")
    # Every path the two trees differ in, by plumbing: no diff configuration of the user's may
    # hide one (Sol, PR #54 r1 R1: diff.ignoreSubmodules=all dropped an added gitlink).
    changed = git("diff-tree", "-r", "-z", "--no-renames", "--ignore-submodules=none", "--name-only", rev, commit)
    outside = [p for p in changed.split("\0") if p and not p.startswith(PERF_LAB)]
    if outside:
        raise SystemExit(f"the measurement commit {commit[:12]} of {rev[:12]} changes more than its perf lab "
                         f"({PERF_LAB}): {', '.join(outside)}")
    return commit


def stop_tree(process) -> str | None:
    """Stops `process` and everything it started, at once (benches/suite/queue.py's stop, without its
    grace): on Windows taskkill /T, elsewhere its process group, which it must lead; then the
    process itself, whatever the tree's stop did. What may still run, or None (Sol, S2 D2)."""
    import subprocess
    left = None
    if os.name == "nt":
        if process.poll() is not None:  # ended at the deadline: taskkill /T can't find its tree
            left = f"git (pid {process.pid}) ended at the deadline; a process it started may still run"
        else:
            try:
                done = subprocess.run(["taskkill", "/T", "/F", "/PID", str(process.pid)], capture_output=True,
                                      timeout=60)
                if done.returncode != 0:
                    why = done.stderr.decode("utf-8", "replace").strip() or f"exit {done.returncode}"
                    left = f"pid {process.pid}'s process tree may still run (taskkill: {why})"
            except subprocess.TimeoutExpired:
                left = f"pid {process.pid}'s process tree may still run (taskkill didn't finish in 60 s)"
    else:
        import signal
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    process.kill()
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        return "; ".join(filter(None, [f"git (pid {process.pid}) still runs after its stop", left]))
    return left


def published(repo: str, rev: str, remote: str | None, deadline_s: float = 60) -> str | None:
    """The branch of `remote` whose head has `rev` in its history: the heads the remote names now
    (git ls-remote), read through this checkout's objects as stored. None if no head has it. Only
    the remote's word counts: a refs/remotes/* ref is anyone's to write here (Sol, PR #54 fix check
    1 S2). No remote, a failure or no answer within the deadline (its process tree stopped) is a
    SystemExit; a head this checkout hasn't fetched proves nothing."""
    import subprocess
    import tempfile
    if not remote:
        raise SystemExit("no remote to ask whether the side is published (.githooks/pre-push names it)")
    # Its output into files, not pipes: a transport helper's process left behind (an ssh master,
    # say) may hold them open after git has answered, and only git's own exit is waited for (Sol,
    # S2 r1 D1).
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        asking = subprocess.Popen(["git", "-C", repo, "ls-remote", "--heads", remote], stdout=out, stderr=err,
                                  env={**os.environ, "GIT_TERMINAL_PROMPT": "0"}, start_new_session=os.name != "nt")
        try:
            asking.wait(timeout=deadline_s)
        except subprocess.TimeoutExpired:
            left = stop_tree(asking)
            raise SystemExit("; ".join(filter(None, [
                f"{remote} didn't say within {deadline_s:.0f} s which branches it has", left]))) from None
        out.seek(0)
        err.seek(0)
        heads, error = (f.read().decode("utf-8", "replace") for f in (out, err))
    if asking.returncode != 0:
        raise SystemExit(f"{remote} couldn't be asked which branches it has: {error.strip()}")
    stored = {**os.environ, **STORED_OBJECTS}
    for line in heads.splitlines():
        head, _, branch = line.partition("\t")
        here = subprocess.run(["git", "-C", repo, "cat-file", "-e", f"{head}^{{commit}}"], capture_output=True,
                              timeout=60, env=stored)
        if here.returncode == 0 and subprocess.run(["git", "-C", repo, "merge-base", "--is-ancestor", rev, head],
                                                   capture_output=True, timeout=60, env=stored).returncode == 0:
            return branch
    return None


def measurement_ref(repo: str, ref: str, commit: str, remote: str | None,
                    path: Path = MEASUREMENT_SIDES) -> str:
    """A pushed refs/measure/* ref (.githooks/pre-push, check.sh push): the ref a side of the table
    declares, named for that side, whose side is already on a branch of the remote it goes to, at
    that side's declared commit, which measured_at admits for it. The side; anything else is
    refused."""
    table = measurement_table(path)
    declaring = [rev for rev, side in table.items() if side.get("ref") == ref]
    if not declaring:
        raise SystemExit(f"{ref}: no side of {path.name} declares this measurement ref")
    rev = declaring[0]
    if not ref.endswith(f"-{rev[:8]}"):
        raise SystemExit(f"{ref}: its name doesn't name its side {rev[:8]} (refs/measure/<name>-{rev[:8]})")
    import subprocess
    # The ref's push sends its side's history along, so only a side that is already out may go (T0,
    # 28 Oct 2026: main's and v1.5's sides were ids from before the history rewrite, on no remote).
    if published(repo, rev, remote) is None:
        raise SystemExit(f"{ref}: its side {rev[:12]} is on no branch of {remote} (as fetched here): "
                         "the side must be published")
    stored = {**os.environ, **STORED_OBJECTS}
    done = subprocess.run(["git", "-C", repo, "rev-parse", "--verify", f"{commit}^{{commit}}"],
                          capture_output=True, text=True, timeout=60, env=stored)
    if done.returncode != 0 or done.stdout.strip() != table[rev]["commit"]:
        raise SystemExit(f"{ref}: pushes {commit[:12]}, but {path.name} declares {table[rev]['commit'][:12]}")
    if measured_at(repo, rev, {rev: table[rev]["commit"]}) != table[rev]["commit"]:
        raise SystemExit(f"{ref}: {table[rev]['commit'][:12]} isn't admitted as the measurement commit of {rev[:12]}")
    return rev


def measurement_inputs(checkout: str, revs: list[str], sides: dict[str, str] | None = None) -> list[dict]:
    """The measurement commits of these sides as a job's git inputs: a job's checkout holds only
    its own revision's history, and release-gate.sh build needs them in it."""
    import subprocess
    sides = measurement_sides() if sides is None else sides
    inputs = []
    for rev in dict.fromkeys(revs):
        full = subprocess.run(["git", "-C", checkout, "rev-parse", "--verify", f"{rev}^{{commit}}"],
                              capture_output=True, text=True, timeout=60).stdout.strip()
        if full in sides:
            inputs.append({"name": f"measure-{full[:8]}", "source": {"git": checkout, "rev": sides[full]}})
    return inputs


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


def linux_host(name: str, registry: dict | None = None) -> None:
    """The gate's binaries are Linux ELF (built on office-pc): a timed host must be a Linux one in
    benches/hosts.toml (khaleesi-wsl, not khaleesi). Anything else is refused, never routed."""
    host = (registry or hosts_registry.load()).get("host", {}).get(name)
    if not host or host.get("os") != "linux":
        raise SystemExit(f"{name}: not a Linux host in benches/hosts.toml "
                         f"(os {host.get('os') if host else 'unknown'}): the gate's ELF binaries can't run there")


def checkout_rev(checkout: str, candidate: str, harness: str) -> str:
    """The revision a gate job's checkout takes, so that release-gate.sh can name every revision
    it builds: the harness when it contains the candidate (a gate of a head the harness is on), the
    candidate when it contains the harness (a branch rebased on it: keepers, 20 Oct 2026, whose
    candidate builds failed at the harness's checkout); neither containing the other is no job."""
    if candidate == harness:
        return harness
    import subprocess

    def contains(rev: str, other: str) -> bool:
        done = subprocess.run(["git", "-C", checkout, "merge-base", "--is-ancestor", other, rev],
                              capture_output=True, text=True)
        if done.returncode not in (0, 1):
            raise SystemExit(f"{checkout}: can't compare {other[:8]} and {rev[:8]}: {done.stderr.strip()}")
        return done.returncode == 0
    if contains(harness, candidate):
        return harness
    if contains(candidate, harness):
        return candidate
    raise SystemExit(f"neither the candidate {candidate[:8]} nor the harness {harness[:8]} contains the other: "
                     "a job's checkout must hold both (rebase the candidate onto the harness, or gate with "
                     "--harness the candidate)")


def jobs(candidate: str, harness: str, checkout: str, prefix: str, blocks_per_round: int, cases: str | None,
         base: str = BASE, main: str = MAIN, timed_host: str = "phuoc-yu", checks_host: str = "office-pc",
         max_gb: float = 24, checks: bool = True, big_host: str | None = "khaleesi-wsl",
         big_gb: float = 28, measure: dict | None = None, batch: bool = False,
         layouts: int = 1) -> dict[str, dict]:
    """The gate's fleet jobs: binaries and data, then round 1's blocks and its report, alone on
    the timed host; the cases whose caps exceed it on the big host, a Linux endpoint (khaleesi-wsl:
    ELF binaries run there, never on native Windows; unpack refuses another), on the build's
    binaries; the W3C suites and the guards on the checks host. With `measure` the k-th block runs
    the cases needing k blocks; blocks rotate through `layouts` builds."""
    linux_host(timed_host)
    if big_host:
        linux_host(big_host)
    env = {"GATE_NAME": f"gate-v2-{candidate[:8]}", "GATE_MAX_GB": str(max_gb), "NRESE_QUIET_SLOT": "0",
           "GATE_MIN_BLOCKS": str(blocks_per_round), **({"GATE_CASES": cases} if cases else {}),
           **({"GATE_BIG_GB": str(big_gb), "GATE_BIG_HOST": big_host} if big_host else {}),
           **({"GATE_BATCH": "1"} if batch else {}), **({"GATE_LAYOUTS": str(layouts)} if layouts > 1 else {})}
    source = [{"name": "checkout", "source": {"git": checkout, "rev": checkout_rev(checkout, candidate, harness)}}]
    gate_ = ["bash", "scripts/release-gate.sh"]
    revs = {"base": base, "main": main, "cand": candidate}
    small = bool(cases)
    out = {f"{prefix}-build": {
        "id": f"{prefix}-build", "group": "release", "kind": "owlrs.build", "priority_class": "merge-gate",
        "est_min": 25 * len(dict.fromkeys([base, main, candidate, harness])) + (10 if small else 30), "env": env,
        "steps": [[*gate_, "build", rev] for rev in dict.fromkeys([base, main, candidate, harness])]
        + [[*gate_, "prepare", harness]] + ([[*gate_, "pack", base, main, candidate, harness]] if big_host else []),
        "requires": {"hosts": [timed_host], "cores": 16, "ram_gb": 12},
        "inputs": source + measurement_inputs(checkout, [base, main, candidate, harness]),
        **({"outputs": [{"name": "binaries", "path": "binaries"}]} if big_host else {})}}
    # minutes, the three sides' two runs of every case (a main that is the base isn't timed again)
    sides = 2 + (main != base)
    per_block = (2 if small else 60) * sides / 3
    # Whole Latin squares (whole_squares()): every timed side opens a case's blocks alike.
    env["GATE_SQUARE"] = str(sides)
    blocks_per_round = whole_squares(blocks_per_round, sides)
    env["GATE_MIN_BLOCKS"] = str(blocks_per_round)
    count = blocks_per_round
    steps = [[*gate_, "plan-blocks", harness]] if measure else []
    if measure:
        need = {c: whole_squares(int(e.get("blocks", blocks_per_round)), sides) for c, e in measure.items()}
        count = max([blocks_per_round, *need.values()])
        full = sum(e.get("seconds", 0) for e in measure.values()) or 1
    est = 2.0
    for k in range(1, count + 1):
        steps.append(block_step(gate_, k, harness, block_layout(k, layouts),
                                f"plan-block-{k}.json" if measure else None, revs))
        share = (sum(e.get("seconds", 0) for c, e in measure.items() if need[c] >= k or k == 1) / full) if measure else 1
        est += per_block * share + 2
    out[f"{prefix}-timed"] = {
        "id": f"{prefix}-timed", "group": "release", "kind": "owlrs.timed", "class": "exclusive",
        "priority_class": "merge-gate", "after": [f"{prefix}-build"],
        "env": {**env, "GATE_HOST": timed_host, "GATE_PREFIX": prefix, "GATE_RAM_GB": str(max_gb)},
        "est_min": round(est), "stall_after_min": 60,
        "steps": steps + [[*gate_, "report", harness, f"cand={candidate}", f"base={base}", f"main={main}"],
                          [*gate_, "clean"]],
        "requires": {"hosts": [timed_host], "cores": 16, "ram_gb": max_gb}, "inputs": source}
    if big_host:
        big_env = {**env, "GATE_MIN_GB": str(max_gb), "GATE_MAX_GB": str(big_gb), "GATE_HOST": big_host,
                   "GATE_PREFIX": f"{prefix}-big", "GATE_RAM_GB": str(big_gb + 4)}
        big_env.pop("GATE_BIG_GB")
        out[f"{prefix}-big"] = {
            "id": f"{prefix}-big", "group": "release", "kind": "owlrs.timed", "class": "exclusive",
            "priority_class": "merge-gate", "after": [f"{prefix}-build"], "env": big_env,
            "est_min": (6 if small else 45) * blocks_per_round + 30, "stall_after_min": 90,
            "steps": [[*gate_, "unpack"], [*gate_, "prepare", harness]]
            + [block_step(gate_, k, harness, block_layout(k, layouts), None, revs) for k in range(1, blocks_per_round + 1)]
            + [[*gate_, "report", harness, f"cand={candidate}", f"base={base}", f"main={main}"], [*gate_, "clean"]],
            "requires": {"hosts": [big_host], "cores": 16, "ram_gb": big_gb + 4},
            "inputs": [*source, {"name": "binaries", "source": {"artifact": f"{prefix}-build/binaries"}}]}
    if checks:
        out[f"{prefix}-checks"] = {
            "id": f"{prefix}-checks", "group": "release", "kind": "owlrs.any", "priority_class": "merge-gate",
            "est_min": 100, "env": {**env, "CARGO_BUILD_JOBS": "16"},
            "steps": [[*gate_, "w3c"], [*gate_, "guards"], [*gate_, "clean"]],
            "requires": {"hosts": [checks_host], "cores": 16, "ram_gb": 12},
            "inputs": [{"name": "checkout", "source": {"git": checkout, "rev": candidate}}]}
    return out


# --- rounds and the verdict (research round 8: prespecified rounds, Holm over the family) ------




def rounds_of(reports: list[dict]) -> list[list[dict]]:
    """The reports of each timed part (host), first round first (`context.round`)."""
    by_host: dict[str, list[dict]] = {}
    for r in reports:
        host = (r.get("context") or {}).get("host") or ",".join(map(str, r.get("host", [])))
        by_host.setdefault(host, []).append(r)
    return [sorted(rs, key=lambda r: (r.get("context") or {}).get("round", 0)) for rs in by_host.values()]


def dimension_in(report: dict, side: str, case: str, dimension: str) -> dict | None:
    return report.get("comparisons", {}).get(side, {}).get(case, {}).get("dimensions", {}).get(dimension)


CONFIRM_BLOCKS = (2, 30)  # blocks of a confirmation round: its cases' calibrated blocks, within these


def confirm_job(rounds: list[dict], checkout: str, measure: dict | None = None) -> dict | None:
    """The next round's fleet job for one timed part: the cases of its pending hypotheses, each
    for its calibrated blocks (measure.toml), layouts rotating; None when nothing is pending."""
    pending = decide([rounds])["pending"]
    if not pending:
        return None
    first = rounds[0]
    ctx = first.get("context") or {}
    cases = sorted({h.split("|")[1] for h in pending})
    low, high = CONFIRM_BLOCKS
    revs = {"cand": first["candidate"]["revision"], **first["baselines"]}
    # In whole Latin squares of the timed sides: the candidate always, and each baseline that isn't
    # an earlier one's build (gate()'s aliases; release-gate.sh skips only such a main). Sol r2, 20
    # Oct 2026: counting revisions dropped a candidate that shares base's, so three timed sides got 2.
    timed = 1 + len(set(first["baselines"].values()))
    n = whole_squares(min(high, max([low, *(int((measure or {}).get(c, {}).get("blocks", low)) for c in cases)])), timed)
    harness = first["harness"]
    k = len(rounds)
    prefix = ctx.get("prefix") or f"rg-{revs['cand'][:8]}"
    name = f"{prefix}-confirm" if k == 1 else f"{prefix}-confirm{k}"
    seconds = sum(first.get("case_seconds", {}).get(c, {}).get(side, 60) for c in cases for side in revs) * 2
    gate_ = ["bash", "scripts/release-gate.sh"]
    layouts = int(ctx.get("layouts", 1) or 1)
    env = {"GATE_NAME": ctx.get("gate_name") or f"gate-v2-{revs['cand'][:8]}", "GATE_HOST": ctx.get("host", ""),
           "GATE_ROUND": str(k), "GATE_PREFIX": prefix, "GATE_MIN_BLOCKS": str(n), "NRESE_QUIET_SLOT": "0",
           "GATE_PLAN": f"plan-confirm{k}.json", "GATE_MAX_GB": str(ctx.get("max_gb", 24)),
           **({"GATE_LAYOUTS": str(layouts)} if layouts > 1 else {})}
    return {"id": name, "group": "release", "kind": "owlrs.timed", "class": "exclusive",
            "priority_class": "merge-gate", "env": env,
            "est_min": round(n * (seconds / 60 + 2) + 5), "stall_after_min": 60,
            "steps": [[*gate_, "plan-cases", ",".join(cases)]]
            + [block_step(gate_, b, harness, block_layout(b, layouts), None, revs) for b in range(1, n + 1)]
            + [[*gate_, "report", harness, f"cand={revs['cand']}", *(f"{s}={r}" for s, r in first["baselines"].items())],
               [*gate_, "clean"]],
            "requires": {"hosts": [ctx.get("host") or "phuoc-yu"], "cores": 16, "ram_gb": ctx.get("ram_gb", 24)},
            "inputs": [{"name": "checkout", "source": {"git": checkout,
                                                       "rev": checkout_rev(checkout, revs["cand"], harness)}}]}


def verdict(timed: list[dict], w3c: dict, guards: dict) -> tuple[bool, str]:
    """Every part together: the family over every timed part decided (decide()), the other blocks
    of every round, the cases one part left to another, the W3C counts and the guards."""
    parts = rounds_of(timed)
    d = decide(parts)
    lines = ["# Release verdict", "",
             f"Family: {d['m']} hypotheses declared (case x series or metric x baseline, every timed part); "
             f"Holm at {gatestats.ALPHA} over p = min(max(p1, p2) / 0.9, max(p2, p3) / 0.1).", ""]
    errors = [e for rounds in parts for r in rounds for e in r["errors"]]
    other = [b for rounds in parts for r in rounds for b in r.get("other_blocking", [])]
    diagnostic = any(r["diagnostic"] for rounds in parts for r in rounds)
    blocking_losses = []
    for h in d["losses"]:
        side, case, dim = h.split("|")
        ps = d["rounds"].get(h, [])
        accepted_ = None
        for rounds in parts:
            for report in rounds:
                r = report.get("comparisons", {}).get(side, {}).get(case, {}).get("dimensions", {}).get(dim)
                if r is not None:
                    t = accepted(report.get("tradeoffs", []), side, case, dim, r, report.get("comparisons", {}))
                    accepted_ = accepted_ or t
        text = f"{case} against {side}: {dim}: p per round {ps}, combined {d['combined'][h]:.3g}"
        if accepted_:
            lines.append(f"- accepted trade-off: {text} (at most {accepted_['max_ratio']}, approved "
                         f"{accepted_['approved']} by {accepted_['by']})")
        else:
            blocking_losses.append(text)
    lines += [f"- **confirmed loss**: {t}" for t in blocking_losses]
    lines += [f"- **pending** (a round that could reject it hasn't run): {h}" for h in d["pending"]]
    lines += [f"- {b}" for b in other] + [f"- pass error: {e}" for e in errors]
    covered = {c for t in timed for c in t.get("planned", [])}
    missing = sorted({c for t in timed for c in t.get("elsewhere", {})} - covered)
    if missing:
        lines.append(f"- NOT timed anywhere: {', '.join(missing)} (left to another part, which didn't run them)")
    lines.append(f"- W3C: {'passed' if w3c['passed'] else 'NOT passed'} ({'; '.join(w3c['errors']) or 'every suite as required'})")
    lines.append(f"- §0 guards: {'passed' if guards['passed'] else 'NOT passed'} "
                 f"({'; '.join(guards['errors'][:5]) or str(guards['guards']) + ' guards ran and passed'})")
    passed = (not blocking_losses and not d["pending"] and not other and not errors and not diagnostic
              and not missing and w3c["passed"] and guards["passed"])
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
    s.add_argument("--big-host", default="khaleesi-wsl")
    s.add_argument("--cases")
    s.add_argument("--baselines", default="base,main", help="the family's baselines (declared before any result)")
    s = sub.add_parser("report")
    s.add_argument("--passes", type=Path, required=True)
    s.add_argument("--candidate", required=True)
    s.add_argument("--side", action="append", required=True, metavar="NAME=REV")
    s.add_argument("--require", required=True, help="the baselines the gate needs, comma-separated")
    s.add_argument("--harness-revision", required=True)
    s.add_argument("--min-blocks", type=int, default=CAMPAIGN_BLOCKS)
    s.add_argument("--family", type=Path, help="the declared family (plan.json's), else the tested dimensions")
    s.add_argument("--diagnostic", action="store_true")
    s.add_argument("--not-run", action="append", default=[], metavar="CASE=WHY")
    s.add_argument("--elsewhere", action="append", default=[], metavar="CASE=WHERE",
                   help="cases another timed part of the gate runs (the verdict checks that it did)")
    s.add_argument("--tradeoffs", type=Path, default=ROOT / "benches" / "release-tradeoffs.toml",
                   help="the owner-approved trade-offs: the only way a loss is accepted")
    s.add_argument("--inputs", type=Path, help="the datasets the passes read, as prepare recorded them")
    s.add_argument("--context", action="append", default=[], metavar="KEY=VALUE",
                   help="the job: host (fleet's name), round (0 the first run), prefix, gate_name, max_gb, ram_gb, "
                        "layouts")
    s.add_argument("--floor", type=Path, help="the host's A-A' noise floor (default noise-floor/HOST.toml, "
                                               "HOST the context's)")
    s.add_argument("--out", type=Path)
    s.add_argument("--json", type=Path)
    s = sub.add_parser("floor", help="a host's noise floor (noise-floor/HOST.toml) from an A-A' gate's report")
    s.add_argument("report", type=Path, help="its release-gate.json (every side one revision)")
    s.add_argument("--host", required=True, help="fleet's name of the host")
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
    s.add_argument("--blocks", type=int, default=CAMPAIGN_BLOCKS, help="blocks of round 1 at least (fewer than 2: diagnostic)")
    s.add_argument("--cases", help="a selection: a dry run, diagnostic only")
    s.add_argument("--base", default=BASE)
    s.add_argument("--main", default=MAIN)
    s.add_argument("--max-gb", type=float, default=24)
    s.add_argument("--no-checks", action="store_true", help="without the W3C and guards job")
    s.add_argument("--big-host", default="khaleesi-wsl",
                   help="the Linux host of the cases above --max-gb ('' for none; never native Windows)")
    s.add_argument("--big-gb", type=float, default=28)
    s.add_argument("--layouts", type=int, default=1, help="builds a side in different layouts (GATE_LAYOUTS)")
    s.add_argument("--out", type=Path, default=Path("."))
    s = sub.add_parser("calibrate", help="benches/fast/measure.toml from passes of the gate's host (an A/A run)")
    s.add_argument("--passes", type=Path, nargs="+", required=True, help="pass reports, or directories of them")
    s.add_argument("--budget-min", type=float, default=150, help="the timed part's minutes, at most")
    s.add_argument("--max-blocks", type=int, default=30)
    s.add_argument("--family-size", type=int, help="m, the declared family's size (default: the calibrated cases')")
    s.add_argument("--no-batch", action="store_true", help="no batched timing for the microsecond cases")
    s.add_argument("--keep", type=Path, help="the calibration the passes ran with: its runs stay, only blocks are new")
    s.add_argument("--sides", type=Path, help="a real gate's passes: each case's time on every side, for the budget")
    s.add_argument("--out", type=Path, default=ROOT / "benches" / "fast" / "measure.toml")
    s = sub.add_parser("blocks", help="each block's plan of a gate (release-gate.sh plan-blocks)")
    s.add_argument("--plan", type=Path, required=True)
    s.add_argument("--measure", type=Path, required=True)
    s.add_argument("--square", type=int, required=True, help="the timed sides: each case's blocks in whole Latin squares")
    s.add_argument("--min-blocks", type=int, default=CAMPAIGN_BLOCKS, help="the round's least blocks a timed case (GATE_MIN_BLOCKS)")
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
    s = sub.add_parser("measured-at", help="the commit a side is built and timed from (release-gate.sh build)")
    s.add_argument("rev")
    s = sub.add_parser("measurement-ref", help="a pushed refs/measure/* ref and its commit, checked (check.sh push)")
    s.add_argument("ref")
    s.add_argument("commit")
    s.add_argument("--remote", help="the remote the ref is pushed to, whose branches must have its side")
    args = p.parse_args()

    if args.command == "measured-at":
        print(measured_at(str(ROOT), args.rev))
        return 0
    if args.command == "measurement-ref":
        side = measurement_ref(str(ROOT), args.ref, args.commit, args.remote)
        print(f"{args.ref}: the measurement commit of {side[:12]}, admitted (benches/fast/measurement-sides.toml)")
        return 0
    if args.command == "plan":
        catalog = cases_registry.load()
        run, not_run, elsewhere = plan(catalog, args.max_gb, args.cases.split(",") if args.cases else None,
                                       args.min_gb, args.big_gb, args.big_host)
        family = family_of(catalog, run, tuple(b for b in args.baselines.split(",") if b))
        print(json.dumps({"run": run, "not_run": not_run, "elsewhere": elsewhere, "family": family}, indent=1))
        return 0
    if args.command == "report":
        sides = pairs(args.side, "--side")
        tradeoffs, bad = load_tradeoffs(args.tradeoffs)
        if bad:
            raise SystemExit("benches/release-tradeoffs.toml: " + "; ".join(bad))
        passes = load_passes(args.passes)
        family = json.loads(args.family.read_text(encoding="utf-8")).get("family") if args.family else None
        context = {k: v for k, v in pairs(args.context, "--context").items()}
        floor_file = args.floor or (FLOORS / f"{context['host']}.toml" if context.get("host") else None)
        summary = gate(passes, sides, args.candidate, [s for s in args.require.split(",") if s], args.harness_revision,
                       args.min_blocks, args.diagnostic, pairs(args.not_run, "--not-run"), tradeoffs,
                       cases_registry.load(), pairs(args.elsewhere, "--elsewhere"), context, family,
                       read_floor(floor_file) if floor_file else None)
        for key in ("round", "max_gb", "ram_gb", "layouts"):
            if key in summary["context"]:
                summary["context"][key] = float(summary["context"][key]) if "gb" in key else int(summary["context"][key])
        if args.inputs and args.inputs.is_file():
            summary["inputs"] = args.inputs.read_text(encoding="utf-8").splitlines()
        summary["quiet"], notes = quiet_verdicts(args.passes)
        summary["host_notes"] = [*summary.get("host_notes", []), *notes]
        prepared = {(p["report"].get("prepared") or {}).get("manifest_sha256") for p in passes}
        summary["prepared_manifest"] = sorted(map(str, prepared))
        text = render(summary)
        print(text)
        if args.out:
            args.out.write_text(text, encoding="utf-8")
        if args.json:
            args.json.write_text(json.dumps(summary, indent=1, default=str), encoding="utf-8")
        return 0 if summary["passed"] else 1
    if args.command == "floor":
        summary = json.loads(args.report.read_text(encoding="utf-8"))
        floor = noise_floor(summary)
        FLOORS.mkdir(exist_ok=True)
        out = FLOORS / f"{args.host}.toml"
        source = f"{summary['context'].get('gate_name') or args.report.parent.name} round {summary['context'].get('round', 0)}"
        out.write_text(write_floor(floor, source), encoding="utf-8")
        print(f"{out}: {sum(len(d) for d in floor['cases'].values())} dimensions of {len(floor['cases'])} cases, "
              f"{floor['blocks']} blocks")
        return 0
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
        result = calibrate(cases_registry.load(), found, args.budget_min, args.max_blocks, m=args.family_size,
                           batch=not args.no_batch,
                           keep=read_measure(args.keep.read_text(encoding="utf-8")) if args.keep else None,
                           side_weights=side_weights(load_passes(args.sides) or [
                               {"n": i, "side": f.stem.rsplit("-", 1)[-1], "path": f.name,
                                "report": json.loads(f.read_text(encoding="utf-8"))}
                               for i, f in enumerate(sorted(args.sides.glob("*.json")))]) if args.sides else None)
        hosts_ = sorted({str((p["report"].get("machine") or {}).get("host")) for p in found})
        source = f"{len(found)} passes on {', '.join(hosts_)} ({', '.join(sorted({p['path'] for p in found})[:2])} ...)"
        args.out.write_text(write_measure(result, source), encoding="utf-8")
        under = {c: e for c, e in result["cases"].items() if e.get("power", 1) < TARGET_POWER}
        print(f"{len(result['cases'])} cases, family m = {result['m']}; timed part {result['minutes']} min; "
              f"blocks at most {result['cap']}; "
              f"{len(under)} below {TARGET_POWER:.0%} power at a {TARGET_LOSS:.0%} loss: "
              + ", ".join(f"{c} ({e['noise']:.3f}, {e['power']:.2f})" for c, e in sorted(under.items())))
        return 0
    if args.command == "blocks":
        gate_plan = json.loads(args.plan.read_text(encoding="utf-8"))
        found = blocks(gate_plan["run"], read_measure(args.measure.read_text(encoding="utf-8"))
                       if args.measure.is_file() else {}, args.min_blocks, square=args.square,
                       checked={c["name"] for c in cases_registry.load() if c.get("outcome", "ok") != "ok"})
        for k, cases in enumerate(found, start=1):
            (args.out / f"plan-block-{k}.json").write_text(json.dumps({"run": cases, "not_run": {}, "elsewhere": {}},
                                                                      indent=1), encoding="utf-8")
        print("blocks: " + ", ".join(f"{k} {len(c)} cases" for k, c in enumerate(found, start=1)))
        return 0
    if args.command == "confirm":
        reports = [json.loads(path.read_text(encoding="utf-8")) for path in args.reports]
        args.out.mkdir(parents=True, exist_ok=True)
        written = 0
        shown = (ROOT / "benches" / "fast" / "measure.toml")
        measure = read_measure(shown.read_text(encoding="utf-8")) if shown.is_file() else {}
        for rounds in rounds_of(reports):
            job = confirm_job(rounds, args.checkout, measure)
            d = decide([rounds])
            print(f"{(rounds[0].get('context') or {}).get('host')}: {len(d['losses'])} losses, "
                  f"{len(d['pending'])} pending of {d['m']}" + (": " + ", ".join(d["pending"][:20]) if d["pending"] else ""))
            if job:
                (args.out / f"{job['id']}.toml").write_text(toml_job(job), encoding="utf-8")
                print(f"  {args.out / (job['id'] + '.toml')}: {len(job['steps']) - 3} blocks, est {job['est_min']} min")
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
                     args.blocks, args.cases, rev(args.base), rev(args.main), max_gb=args.max_gb,
                     checks=not args.no_checks, big_host=args.big_host or None, big_gb=args.big_gb,
                     measure=measure, batch=batch, layouts=args.layouts)
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
