#!/usr/bin/env python3
"""The release gate (benches/PROTOCOL.md §5, "The release gate"): does the candidate regress,
beyond noise, on any workload, in time or memory, against each required baseline? And are
the §0 guards green and the W3C suites' counts what they must be?

    release_gate.py plan --max-gb GB [--cases A,B]
        the cases a host giving GB can run at their declared caps; the others, with why
    release_gate.py report --passes DIR --candidate NAME --side NAME=REV ... --require NAME,...
                           --harness-revision REV [--min-passes 4] [--diagnostic]
                           [--not-run CASE=WHY] [--out MD] [--json JSON]
        the timed part: DIR holds the passes, pass-N-SIDE.json, each a full `fast.py run
        --reps 1 --prepared` report of one side's binaries over the same harness and data
    release_gate.py w3c LOGDIR --expected benches/fast/w3c-expected.toml [--run --identity ID] [--json JSON]
        the declared kits (--run: launched here first), each one's outcome and its suites' counts
    release_gate.py guards [--names NAMES] --log LOG [--run --identity ID] [--json JSON]
    release_gate.py verdict --candidate REV [--checkout DIR] --timed JSON [--timed JSON] --w3c JSON
                            --guards JSON [--out MD]
        every part, each bound to REV's tree (a check by its receipt), under the policy at REV
        (benches/release-tradeoffs.toml)

Each subcommand exits 0 only when its part passes; `verdict` only when every part does.
"""
from __future__ import annotations

import argparse
import fnmatch
import hashlib
import json
import math
import os
import platform
import re
import secrets
import statistics
import subprocess
import sys
import tomllib
from collections import Counter
from pathlib import Path
from typing import Callable, NamedTuple

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "benches" / "suite"))
from suitekit import cases as cases_registry  # noqa: E402
from suitekit import driver, gatestats  # noqa: E402
from suitekit.compare import counter_changes  # noqa: E402
from suitekit import hosts as hosts_registry  # noqa: E402
from suitekit.hosts import run_identity  # noqa: E402
from suitekit.checkout import STORED_OBJECTS, checkout_differs  # noqa: E402,F401

CAMPAIGN_BLOCKS = 2  # blocks a round at least (each: every case on every side, twice, as a palindrome)
PASS_FILE = re.compile(r"pass-(\d+)-([A-Za-z0-9_.-]+)\.json$")


# --- plan --------------------------------------------------------------------------------------

def plan(catalog: list[dict], max_gb: float, selection: list[str] | None = None, min_gb: float = 0,
         big_gb: float | None = None, big_host: str = "the big part") -> tuple[list[str], dict, dict]:
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
            elsewhere[case["name"]] = f"its cap is {cap:g} GB: timed in {big_host} (giving {big_gb:g} GB)"
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


def fleet_host_errors(passes: list[dict], host: str) -> list[str]:
    """Why the passes can't be judged as `host`'s, the fleet host the round names (an A-B-B-A's
    manifest, a timed part's report context), whose floor its verdict reads: a pass from another
    fleet host, or one that doesn't say (manifest.machine() `fleet_host`). Review r1 R1-F1: the
    fleet may move a timed job between steps when a host's timing window closes, and passes timed
    elsewhere were judged by the first host's floor, "no loss shown". No host named: nothing to
    hold them to (a run outside a fleet job)."""
    if not host:
        return []
    found = [(p["report"].get("machine") or {}).get("fleet_host") for p in passes]
    out = []
    if unsaid := sum(1 for h in found if not h):
        out.append(f"{unsaid} passes don't say which fleet host ran them (machine.fleet_host): not judged as {host}'s")
    if elsewhere := sorted({h for h in found if h and h != host}):
        out.append(f"passes ran on {', '.join(elsewhere)}, not on {host}, the host the round names and whose floor "
                   "it reads: a job moved between hosts has no verdict")
    return out


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
           "refused": [],
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
        # A query refused at its memory limit (fast.py QUERY_REFUSAL, an invocation's `refused`) has
        # no answer: its checks and routes are unanswered, not failed; the refusal is named instead,
        # and fails the case whatever it declares (state()). The record's own checks are its last
        # invocation's.
        gone = [o.get("refused") or {} for o in observations]
        last = gone[-1] if gone else {}
        checks = [c for c in record.get("checks", []) if not unanswered(c, last)] + \
            [c for o, g in zip(observations, gone) for c in o.get("checks", []) if not unanswered(c, g)]
        routes = [c for c in record.get("routes", []) if not unanswered(c, last)] + \
            [c for o, g in zip(observations, gone) for c in o.get("routes", []) if not unanswered(c, g)]
        out["refused"] += [f"{q} refused at its memory limit ({e[:160]})" for g in gone for q, e in sorted(g.items())
                           if f"{q} refused at its memory limit ({e[:160]})" not in out["refused"]]
        for key, items in (("failed_checks", checks), ("failed_routes", routes)):
            out[key] += [f"{c.get('path')}: expected {c.get('expected')}, got {c.get('got')}"
                         for c in items if c.get("ok") is False and
                         f"{c.get('path')}: expected {c.get('expected')}, got {c.get('got')}" not in out[key]]
        out["unchecked"] += sum(c.get("ok") is None for c in checks + routes)
        out["notes"] += [n for n in record.get("notes", []) if n not in out["notes"]]
        out["estimator"] |= {o.get("estimator", "samples") for o in observations} or {"samples"}
        # A sample the perf lab didn't report is null (fast.py asks each run for the series the case
        # declares: an older perf lab writes no first_ms): such a list is no reading of its series
        # on this pass, which compare_case() then treats as any series a side doesn't report.
        out["samples"].append({s: v for s, v in (record.get("samples") or {}).items() if None not in (v or [])})
        process: dict[str, list[float]] = {}
        unreported = set()  # a null in any observation: the series isn't read on this pass at all
        for o in observations:
            for series, values in (o.get("samples") or {}).items():
                if values and None in values:
                    unreported.add(series)
                elif values:
                    process.setdefault(series, []).append(p50(values))
        out["process"].append({s: v for s, v in process.items() if s not in unreported})
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


def unanswered(check: dict, refused: dict) -> bool:
    """A check (or route) of a query its invocation refused at its memory limit: no answer to judge."""
    path = str(check.get("path", ""))
    return path.startswith("q.") and path.split(".")[1] in refused


def state(e: dict, off_route_ok: bool = False) -> str:
    """ok (every pass ok), known (every pass the declared outcome, not ok), or failed. A
    baseline off the route the harness asserts (an operator it hasn't) with every answer
    right is a valid time: ok with `off_route_ok`. A refused query fails it, whatever the
    case declares: no case declares a refusal (fast.py QUERY_MEMORY_LIMIT)."""
    if e.get("refused"):
        return "failed"
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
    if e["status"] and set(e["status"]) == {"absent"}:
        return f"{side} not run: {e['notes'][-1] if e['notes'] else 'declared absent'}"
    for note in e["notes"]:
        if match := CAPABILITY.search(note.replace("\\n", "\n")):
            return f"{side} can't run it ({match.group(0).strip()}; {statuses})"
    if e.get("refused"):
        return f"{side} refused a query ({e['refused'][0]}; {statuses})"
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
        # A wrong answer is the owner's policy's to accept or block (verdict(), BAR_OUTCOMES), wherever
        # the baselines stand; a declared limit where a baseline completed blocks here.
        if base_state == "ok" and not set(cand["status"]) & BAR_OUTCOMES:
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
    missing = (r.get("shapes") or {}).get("missing")
    lacking = f"; {missing} blocks without readings on one side" if missing else ""
    if r.get("ratio") is None:
        return f"{r.get('blocks', 0)} blocks with values on both sides{lacking}"
    ci = f" [{r['ci'][0]:.4f}, {r['ci'][1]:.4f}]" if r.get("ci") else ""
    flip = f", sign-flip p {r['p_sign_flip']:.3g}" if r.get("p_sign_flip") is not None else ""
    return (f"{fmt(r['base'])} -> {fmt(r['new'])} {r['unit']}, ratio {r['ratio']:.4f}{ci}, one-sided p {r['p']:.3g}"
            f"{flip}, {r['units']} units of {r['blocks']} blocks{lacking}")


def worse_end(r: dict) -> float | None:
    """The worse end of a dimension's interval, as a ratio: above 1 is worse."""
    if not r.get("ci"):
        return None
    return 1 / r["ci"][0] if r.get("higher_is_better") else r["ci"][1]


def block_contrasts(base: list[list[float]], base_blocks: list, new: list[list[float]], new_blocks: list,
                    layouts: dict, step: float = 0.0) -> tuple[list[float], list, float, float, Counter]:
    """Per block present on both sides: log(median of the candidate's values in it / median of the
    baseline's); the blocks' layouts; the medians of all the baseline's and candidate's values; and
    how many blocks had a median below the counter's step (`step`), on which side.

    A median below one step is a reading below the counter's resolution, never a missing one (the
    below-resolution design, 14 Nov 2026: dropping such blocks dropped exactly those where a cost
    appeared on one side). It stands for the middle of [0, step), step / 2, so a block where the
    baseline reads 0.0 and the candidate 5.0 counts as a loss and one where both read 0.0 as none.
    A median of 0 where the counter has no step (qps) is no reading: `zero`. A block of either side
    without a finite reading on both is `missing`, and its series incomplete: a missing reading
    never passes (below-resolution review: an empty list kept as the series' key left the other
    blocks to decide it, by its readings or by the test)."""
    def by_block(values, blocks):
        out: dict = {}
        for vs, b in zip(values, blocks):
            out.setdefault(b, []).extend(v for v in vs if isinstance(v, (int, float)) and math.isfinite(v))
        return out
    b_, n_ = by_block(base, base_blocks), by_block(new, new_blocks)
    contrasts, lays, shapes = [], [], Counter()
    shapes["missing"] = sum(1 for block in set(b_) | set(n_) if not (b_.get(block) and n_.get(block)))
    for block in sorted(set(b_) & set(n_), key=str):
        if b_[block] and n_[block]:
            mb, mn = statistics.median(b_[block]), statistics.median(n_[block])
            below_b, below_n = mb < step, mn < step
            if (mb <= 0 or mn <= 0) and step <= 0:
                shapes["zero"] += 1
                continue
            shapes[{(True, True): "both below", (True, False): "base below",
                    (False, True): "new below", (False, False): "measured"}[(below_b, below_n)]] += 1
            contrasts.append(math.log((step / 2 if below_n else mn) / (step / 2 if below_b else mb)))
            lays.append(layouts.get(block, 1))
    every_b = [v for vs in b_.values() for v in vs]
    every_n = [v for vs in n_.values() for v in vs]
    return contrasts, lays, (statistics.median(every_b) if every_b else None), \
        (statistics.median(every_n) if every_n else None), shapes


# The counters' own resolution (research round 8: not a tolerance; a difference within it the test
# doesn't reject is `unresolved`, never neutral): times to the reported step (quantum(), batches
# dividing it), peaks in bytes where reported, else whole MiB.
COUNTER_STEP = {"ms": 0.001, "MiB": 1.0, "bytes": 1.0, "qps": 0.0}
# Verdicts below the counter's resolution, decided by the readings themselves and kept out of the
# Holm family (an untestable hypothesis there would only lower every other's alpha / m), each
# reported (the below-resolution design, 14 Nov 2026): every block below one step on both sides,
# covered by the case's whole run (COVER) or blocking; every block below it on the candidate alone,
# a gain; every block below it on the baseline alone, `emerged`: a cost appearing where there was
# none to time, which blocks unless the owner's [[emerged]] entry accepts it (no round can confirm
# it statistically in time: 2^-blocks over a family of about 1,200 needs 15 blocks).
OUTSIDE_FAMILY = ("below resolution", "vanished", "emerged")
# The timed series holding every other time series of its case: a classifier process's whole run.
COVER = f"time/{cases_registry.WHOLE_RUN}"


def uncovered(side: str, case: str, d: str, row: dict, declared) -> bool:
    """A series below resolution (both sides below the step in every block) that no measured,
    declared whole run of its case covers: it blocks (gate(); the A/A probe counts it too)."""
    cover = row["dimensions"].get(COVER)
    return row["dimensions"][d]["status"] == "below resolution" and not (
        d.startswith("time/") and d != COVER and hypothesis(side, case, COVER) in declared
        and cover is not None and cover["status"] not in OUTSIDE_FAMILY + ("incomplete",))


def dimension(base: list[list[float]], base_blocks: list, new: list[list[float]], new_blocks: list,
              layouts: dict, unit: str, higher_is_better: bool = False, step: float | None = None,
              floor: float = 0.0) -> dict:
    """One dimension: its block contrasts and the one-sided test of a loss (gatestats.loss_test),
    its spread at least the host's A-A' floor; or, every block below the counter's step on both
    sides or on the candidate alone, its verdict below resolution (OUTSIDE_FAMILY)."""
    step = COUNTER_STEP[unit] if step is None else step
    contrasts, lays, b, n, shapes = block_contrasts(base, base_blocks, new, new_blocks, layouts, step)
    r = gatestats.loss_test(contrasts, lays, higher_is_better, resolution=step, scale=b or 0, floor=floor)
    out = {"unit": unit, "higher_is_better": higher_is_better, "base": b, "new": n, "step": step, **r,
           "contrasts": [round(c, 6) for c in contrasts], "shapes": dict(shapes),
           "ratio": math.exp(r["mean_log"]) if r["mean_log"] is not None else None}
    if r["ci"]:
        # The interval of new/base (gatestats tests the worse direction: undo its sign).
        lo, hi = r["ci"]
        out["ci"] = [math.exp(-hi), math.exp(-lo)] if higher_is_better else [math.exp(lo), math.exp(hi)]
        if higher_is_better:
            out["ratio"] = math.exp(-r["mean_log"])
    below = shapes["both below"] + shapes["base below"] + shapes["new below"]
    if shapes["zero"] or shapes["missing"]:
        out["status"] = "incomplete"         # a zero where the counter has no step, or a block without readings
    elif contrasts and shapes["both below"] == len(contrasts):
        out["status"] = "below resolution"   # OUTSIDE_FAMILY: covered, or it blocks
    elif contrasts and shapes["new below"] == len(contrasts):
        out["status"] = "vanished"           # OUTSIDE_FAMILY: a gain
    elif contrasts and shapes["base below"] == len(contrasts):
        out["status"] = "emerged"            # OUTSIDE_FAMILY: blocks unless the owner accepts it
    elif r["verdict"] == "incomplete":
        out["status"] = "incomplete"
    elif r["p"] <= gatestats.ALPHA * gatestats.PAIR_WEIGHTS[0]:
        out["status"] = "loss shown"         # this round: a confirmation is due (rounds_needed)
    elif r.get("below_resolution") or below:
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


# The phases each classifier tool times (fast.py's samples(): the profile's fields it reports);
# a case's `phases` declares fewer where the tool can't resolve the others (benches/cases/README.md).
DL_TOOL_PHASES = cases_registry.TOOL_PHASES


def family_of(catalog: list[dict], cases: list[str], baselines: tuple[str, ...] = ("base", "main")) -> list[str]:
    """The comparison family, declared before any result (research round 8): every planned
    case's series and gated metrics, against each baseline. A case with a declared outcome is
    checked, not timed: it adds none; nor does a baseline the case declares `absent` (its product
    lacks the capability: named in the verdict, never compared)."""
    import fast
    manifest_ = fast.case_manifest(catalog)
    by = {c["name"]: c for c in catalog}
    out = []
    for name in cases:
        case = by[name]
        if case.get("outcome", "ok") != "ok":
            continue
        tool = case.get("tool", "perf_lab")
        # A classifier's phases and its process's whole run, their cover (COVER).
        series = manifest_["series"].get(name) or (list(fast.DL_PHASES) if tool == "tableau" else
                                                   list(case.get("phases") or (
                                                       [*DL_TOOL_PHASES[tool], cases_registry.WHOLE_RUN]
                                                       if tool in DL_TOOL_PHASES else ["_"])))
        dims = [f"time/{s}" for s in series]
        dims += [f"qps/{n}" for n in manifest_["clients"].get(name, [])]
        dims.append("memory/anon_kernel")
        # The perf lab's peak live bytes exist only where the case counts them (--count-live);
        # --count-allocations alone counts calls and bytes, as the baselines' perf labs do.
        if "--count-live" in case.get("args", []):
            dims.append("memory/live")
        out += [hypothesis(side, name, d) for side in baselines if side not in case.get("absent", {}) for d in dims]
    return out


def declared_absent(catalog: list[dict], policy: "Policy") -> dict[tuple[str, str], str]:
    """Each (case, baseline) a timed block doesn't run, and why: the case's catalogue entry says the
    baseline's product lacks the capability, or the owner's [[absence]] entry names it."""
    out = {}
    for case in catalog:
        for side in ("base", "main"):
            if side in case.get("absent", {}):
                out[(case["name"], side)] = f"{side} has no {case['absent'][side]}"
            elif e := absence_of(policy, side, case["name"]):
                out[(case["name"], side)] = f"absent by the owner's decision: {e['reason']}"
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
    family, rounds_p, where, untested, resolution = [], {}, {}, {}, {}
    for rounds in parts:
        declared = rounds[0].get("family") or list(rounds[0].get("pvalues", {}))
        family += [h for h in declared if h not in where]
        for h in declared:
            where[h] = rounds
        for k, report in enumerate(rounds[:3]):
            for h, p in (report.get("pvalues") or {}).items():
                rounds_p.setdefault(h, [None, None, None])[k] = p
        for report in rounds:
            for h, status in (report.get("resolution") or {}).items():
                resolution.setdefault(h, set()).add(status)
        # A declared hypothesis any round given didn't test, for the reason the gate gave
        # (compare_case), the statistical slots or not: not part of a pass however the rest comes
        # out (review abba-not-compared r1 W-OUT1; release-not-compared r1 W1: a fourth round's).
        for report in rounds:
            for h, why in (report.get("not_compared") or {}).items():
                if h in declared:
                    untested.setdefault(h, why)
    # Decided below the counter's resolution in every round that ran it, tested in none and
    # compared in all (OUTSIDE_FAMILY): out of Holm, where its p of 1 would only lower every
    # other's alpha / m. One that a round tested, or didn't compare, stays in.
    outside = {h: sorted(resolution[h]) for h in family
               if h in resolution and h not in untested and all(p is None for p in rounds_p.get(h, [None]))}
    family = [h for h in family if h not in outside]
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
            "losses": sorted(rejected, key=lambda h: combined[h]), "pending": pending,
            "not_compared": {h: untested[h] for h in family if h in untested}, "outside": outside}


# --- the owner's policy: trade-offs and exceptions (benches/release-tradeoffs.toml) -------------

# The one file that accepts what the bar forbids, read at the candidate's commit by `verdict`
# (release_verdict()), never from a report: a report holds what its harness tree held.
POLICY_FILE = "benches/release-tradeoffs.toml"
# Bar exceptions and trade-offs are Johannes's alone (role.owners); an entry carries his words and
# where he said them (rule.record-owner-intent-verbatim).
OWNER = "Johannes"
AUTHORITY_KEYS = {"approved", "by", "reason", "quote", "source"}
TRADEOFF_KEYS = {"case", "dimension", "max_ratio", "pays_with"} | AUTHORITY_KEYS
EXCEPTION_KEYS = {"case", "outcome"} | AUTHORITY_KEYS
ABSENCE_KEYS = {"side", "port"} | AUTHORITY_KEYS  # and its scope: `cases` or `pattern`, one of them
EMERGED_KEYS = {"case", "dimension", "max"} | AUTHORITY_KEYS
# The outcomes the bar forbids, which only an owner exception accepts: a wrong answer. A declared
# limit (memory-limit, gave-up, timeout, unsupported) blocks where a baseline completes (compare_case)
# and is named where none does; it is neither a wrong answer nor a loss.
BAR_OUTCOMES = {"wrong"}


class Policy(NamedTuple):
    """The owner's approved trade-offs, exceptions and absences, and what's wrong with the file (no
    entry counts while anything is)."""
    tradeoffs: list[dict]
    exceptions: list[dict]
    errors: list[str]
    absences: tuple[dict, ...] = ()
    emerged: tuple[dict, ...] = ()


NO_POLICY = Policy([], [], [])


def authority_error(what: str, e: dict) -> str | None:
    if e["by"] != OWNER:
        return f"{what}: by {e['by']!r}: only {OWNER} approves what the bar forbids"
    if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", str(e["approved"])) or not all(
            isinstance(e[k], str) and e[k].strip() for k in ("reason", "quote", "source")):
        return f"{what}: an approval date (YYYY-MM-DD), a reason, {OWNER}'s quote and its source are required"
    return None


def load_policy(text: str) -> Policy:
    """The policy file's entries, each checked. A trade-off names a case, a dimension, optionally
    the baseline (`against`), the largest ratio its loss may reach (the worse end of its interval),
    and the win that pays for it (`pays_with`: case and dimension, which must show as better in
    the same report). An exception names a case and the outcome it accepts there. An absence names
    the cases (`cases`, or a `pattern` of their names) a baseline (`side`) isn't compared on, why,
    and the `port` planned to compare them. An emerged entry names a case, a dimension, optionally
    the baseline (`against`), and the largest candidate median (`max`, in the dimension's unit) a
    cost appearing from below the counter's step may reach. Each carries the owner's approval:
    `approved` (date), `by`, `reason`, `quote` (his words) and `source`."""
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        return Policy([], [], [f"not TOML: {error}"])
    tables = {"tradeoff", "exception", "absence", "emerged"}
    errors = [f"unknown table {k!r} (tradeoff, exception, absence, emerged)" for k in sorted(set(data) - tables)]
    tradeoffs, exceptions = data.get("tradeoff", []), data.get("exception", [])
    absences, emerged = data.get("absence", []), data.get("emerged", [])
    for key, entries in (("tradeoff", tradeoffs), ("exception", exceptions), ("absence", absences),
                         ("emerged", emerged)):
        if not (isinstance(entries, list) and all(isinstance(e, dict) for e in entries)):
            errors.append(f"{key}: an array of tables ([[{key}]]), not {type(entries).__name__}")
    if errors:
        return Policy([], [], errors)
    for i, e in enumerate(tradeoffs, start=1):
        if missing := TRADEOFF_KEYS - set(e):
            errors.append(f"trade-off {i}: missing {', '.join(sorted(missing))}")
        elif error := authority_error(f"trade-off {i}", e):
            errors.append(error)
        elif not (isinstance(e["max_ratio"], (int, float)) and e["max_ratio"] > 1):
            errors.append(f"trade-off {i}: max_ratio must be a number above 1")
        elif not (isinstance(e["pays_with"], dict) and {"case", "dimension"} <= set(e["pays_with"])):
            errors.append(f"trade-off {i}: pays_with needs the case and dimension of the win")
    for i, e in enumerate(exceptions, start=1):
        if missing := EXCEPTION_KEYS - set(e):
            errors.append(f"exception {i}: missing {', '.join(sorted(missing))}")
        elif error := authority_error(f"exception {i}", e):
            errors.append(error)
        elif e["outcome"] not in BAR_OUTCOMES:
            errors.append(f"exception {i}: outcome {e['outcome']!r}: an exception accepts "
                          f"{', '.join(sorted(BAR_OUTCOMES))} (a declared limit needs none)")
    for i, e in enumerate(absences, start=1):
        scope = [k for k in ("cases", "pattern") if k in e]
        if missing := ABSENCE_KEYS - set(e):
            errors.append(f"absence {i}: missing {', '.join(sorted(missing))}")
        elif error := authority_error(f"absence {i}", e):
            errors.append(error)
        elif not (isinstance(e["side"], str) and e["side"] in cases_registry.BASELINES
                  and isinstance(e["port"], str) and e["port"].strip()):
            errors.append(f"absence {i}: a baseline (base, main) and the port planned to compare it")
        elif not (scope in (["cases"], ["pattern"]) and (
                isinstance(e.get("pattern"), str) and e["pattern"].strip() or
                isinstance(e.get("cases"), list) and e["cases"] and all(isinstance(c, str) for c in e["cases"]))):
            errors.append(f"absence {i}: its cases: a list (`cases`) or a pattern of their names (`pattern`), one")
    for i, e in enumerate(emerged, start=1):
        if missing := EMERGED_KEYS - set(e):
            errors.append(f"emerged {i}: missing {', '.join(sorted(missing))}")
        elif error := authority_error(f"emerged {i}", e):
            errors.append(error)
        elif (isinstance(e["max"], bool) or not isinstance(e["max"], (int, float))
              or not (math.isfinite(e["max"]) and e["max"] > 0)):   # TOML true is no limit
            errors.append(f"emerged {i}: max must be a finite number above 0")
        elif "against" in e and e["against"] not in cases_registry.BASELINES:
            errors.append(f"emerged {i}: against: a baseline (base, main)")
    return Policy([], [], errors) if errors else Policy(tradeoffs, exceptions, [], tuple(absences), tuple(emerged))


def emerged_accepted(policy: Policy, e: dict) -> dict | None:
    """The owner's emerged entry covering this cost (its case, dimension and baseline, the
    candidate's median at most its `max`), if one does."""
    return next((t for t in policy.emerged if (t["case"], t["dimension"]) == (e["case"], e["dimension"])
                 and t.get("against", e["side"]) == e["side"] and e["new"] is not None and e["new"] <= t["max"]),
                None)


def absence_of(policy: Policy, side: str, case: str) -> dict | None:
    """The owner's absence entry naming this case (or a pattern matching it) on this baseline."""
    return next((e for e in policy.absences if e["side"] == side and
                 (case in e.get("cases", []) or "pattern" in e and fnmatch.fnmatchcase(case, e["pattern"]))), None)


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
         catalog: list[dict] | None = None, elsewhere: dict | None = None,
         context: dict | None = None, family: list[str] | None = None, floor: dict | None = None,
         baseline_candidate: bool = False) -> dict:
    """One round of one timed part: the schedule and provenance checked, every comparison's
    blocks tested; each declared hypothesis's p-value (untested: 1). The decision over the family
    and the rounds is verdict()'s, which alone applies the owner's policy (load_policy()), read
    at the candidate's commit. `floor`: the host's A-A' noise floor (read_floor()).
    `baseline_candidate`: an A-A' control's candidate is a baseline, with its own optimizer route."""
    not_run, elsewhere = dict(not_run or {}), dict(elsewhere or {})
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
               "context": context, "flagged": [], "other_blocking": [], "pvalues": {}, "resolution": {}, "emerged": [],
               "not_run": not_run, "elsewhere": elsewhere, "errors": [],
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
    # Fail closed: a part that timed nothing (no pass, or none that planned a case) passes nothing.
    if not planned:
        summary["errors"].append(f"nothing timed: {len(passes)} passes, none planned a case")
    summary["errors"] += provenance_errors(passes, sides, harness, planned)
    summary["errors"] += fleet_host_errors(passes, str(context.get("host") or ""))
    host_errors, summary["host_notes"] = host_states(passes)
    summary["errors"] += host_errors
    summary["host"] = sorted({(p["report"].get("machine") or {}).get("host") for p in passes}, key=str)
    by_side = {s: sorted([p for p in passes if p["side"] == s], key=lambda p: p["n"]) for s in sides}
    checks = {c["name"]: {path.split(".")[1] for path in c.get("check", {}) if path.startswith("q.")}
              for c in (catalog or [])}
    absent = {c["name"]: c.get("absent", {}) for c in (catalog or [])}
    for case in planned:
        cand = evidence(by_side[candidate], case)
        cand_state = state(cand, off_route_ok=baseline_candidate)
        if cand_state == "failed":
            why = cand["refused"][:2] + cand["failed_checks"][:3] + cand["failed_routes"][:2] + cand["notes"][-1:]
            summary["candidate_failures"][case] = f"statuses {', '.join(map(str, cand['status']))}; " + "; ".join(why)
        elif cand_state == "known":
            summary["known_issues"][case] = ", ".join(sorted(set(cand["status"])))
        elif any(x != "ok" for x in cand["expected"]):
            summary["fixed"][case] = ", ".join(sorted(set(cand["expected"]) - {"ok"}))
        if cand["unchecked"] and cand_state != "failed":
            summary["candidate_failures"][case] = f"{cand['unchecked']} checks unchecked on the candidate"
        for side in baselines:
            if side in absent.get(case, {}):
                # The baseline's product lacks it (the catalogue): no hypothesis, named (verdict()).
                summary.setdefault("absent", {}).setdefault(side, {})[case] = absent[case][side]
                continue
            if cand_state == "failed" or side in aliases:
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
                if r["status"] in OUTSIDE_FAMILY:
                    summary["resolution"][h] = r["status"]
                    if uncovered(side, case, d, row, declared):
                        summary["blocking"].append(f"{case} against {side}: {d}: below the counter's step on both "
                                                   f"sides in every block, and no measured {COVER} covers it")
                    if r["status"] == "emerged":
                        summary["emerged"].append({"side": side, "case": case, "dimension": d, "base": r["base"],
                                                   "new": r["new"], "unit": r["unit"], "step": r["step"]})
                    continue
                summary["pvalues"][h] = r["p"]
                if r["status"] == "incomplete":
                    summary["blocking"].append(f"{case} against {side}: {d}: incomplete ({describe(r)})")
                elif r["status"] == "loss shown":
                    summary["flagged"].append({"side": side, "case": case, "dimension": d,
                                               **{k: r.get(k) for k in ("p", "ratio", "ci", "units", "blocks", "unit")}})
    summary["untested"] = sorted(set(declared) - set(summary["pvalues"]) - set(summary["resolution"]))
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
    # A wrong answer the case declares passes no round alone: only the verdict's policy accepts one.
    summary["passed"] = (not summary["diagnostic"] and not summary["errors"] and not summary["blocking"]
                         and not summary["flagged"] and not summary["emerged"]
                         and not any(set(v.split(", ")) & BAR_OUTCOMES for v in summary["known_issues"].values()))
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
    for side, absent in sorted(summary.get("absent", {}).items()):
        lines += [f"Absent by product against {side} ({len(absent)}, not compared): " +
                  ", ".join(f"{k} ({side} has no {v})" for k, v in sorted(absent.items())), ""]
    for side, rows in summary["comparisons"].items():
        counts = summary["counts"].get(side, {})
        lines += [f"## Against {side} {summary['baselines'][side]}", "",
                  f"{len(rows)} cases; dimensions: " + ", ".join(f"{n} {v}" for v, n in sorted(counts.items())), ""]
        unresolved = [f"{case} {d}" for case, row in rows.items() for d, r in row["dimensions"].items()
                      if r["status"] == "unresolved"]
        if unresolved:
            lines += [f"Unresolved (within the counter's step, no loss shown): {', '.join(unresolved)}", ""]
        for status, what in (("below resolution", f"below the counter's step on both sides; {COVER} covers each, or it blocks"),
                             ("vanished", "below the counter's step on the candidate alone, a gain"),
                             ("emerged", "below the counter's step on the baseline alone: blocks unless the owner "
                                         "accepts it")):
            named = [f"{case} {d}" for case, row in rows.items() for d, r in row["dimensions"].items()
                     if r["status"] == status]
            if named:
                lines += [f"{status.capitalize()} ({what}; outside the Holm family): {', '.join(named)}", ""]
        not_compared = {k: v["reason"] for k, v in rows.items() if not v["comparable"]}
        if not_compared:
            lines += [f"Not compared against {side} ({len(not_compared)}):", ""] + \
                     [f"- {k}: {v}" for k, v in not_compared.items()] + [""]
        notes = [(k, n) for k, v in rows.items() for n in v["notes"]]
        if notes:
            lines += ["Notes:", ""] + [f"- {k}: {n}" for k, n in notes] + [""]
    return "\n".join(lines) + "\n"


# --- W3C and guards ----------------------------------------------------------------------------

class Kit(NamedTuple):
    """A W3C kit the release gate runs (w3c-expected.toml's [kit."CRATE:TEST"]): the suites whose
    counts its own log reports, and the table it writes, if any (NRESE_W3C_OUT, in the logs'
    directory), which reports some of them."""
    name: str
    suites: tuple[str, ...]
    table: str | None

    @property
    def crate(self) -> str:
        return self.name.split(":", 1)[0]

    @property
    def test(self) -> str:
        return self.name.split(":", 1)[1]


def load_w3c_expected(text: str) -> tuple[dict, list[Kit], list[str]]:
    """The suites' expectations, the kits in launch order, and what's wrong with the file: every
    expected suite is one kit's, and every kit's suite has an expectation."""
    data = tomllib.loads(text)
    suites, kits, errors, owners = data.get("suite", {}), [], [], {}
    for name, entry in data.get("kit", {}).items():
        names = entry.get("suites")
        if name.count(":") != 1 or not isinstance(names, list):
            errors.append(f'kit {name!r}: [kit."CRATE:TEST"] with suites = [...]')
            continue
        for suite in names:
            if suite in owners:
                errors.append(f"{suite}: claimed by {owners[suite]} and {name}")
            owners[suite] = name
        kits.append(Kit(name, tuple(names), entry.get("table")))
    errors += [f"{suite}: no kit reports it" for suite in suites if suite not in owners]
    errors += [f"{suite}: {owners[suite]}'s, with no expectation" for suite in owners if suite not in suites]
    if not kits:
        errors.append("no kit declared")
    return suites, kits, errors


def kit_counts(logs: Path, kits: list[Kit]) -> dict[str, dict]:
    """Each suite's counts, from its own kit's log (`-- --nocapture`) and table only: a suite another
    kit's log prints doesn't stand for its kit; every kit's libtest summaries summed ("test results")."""
    out: dict[str, dict] = {}
    for kit in kits:
        log = logs / f"{kit.test}.log"
        counts = log_counts(log.read_text(encoding="utf-8", errors="replace") if log.is_file() else "")
        if kit.table and (logs / kit.table).is_file():
            counts.update(table_counts((logs / kit.table).read_text(encoding="utf-8")))
        for suite, got in counts.items():
            if suite == "test results":
                total = out.setdefault(suite, {"passed": 0, "failed": 0})
                total["passed"], total["failed"] = total["passed"] + got["passed"], total["failed"] + got["failed"]
            elif suite in kit.suites:
                out[suite] = got
    return out


def log_counts(text: str) -> dict[str, dict]:
    """The suites' counts one kit's log reports, and its libtest summaries' sums ("test results")."""
    out: dict[str, dict] = {}
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
    return out


def table_counts(text: str) -> dict[str, dict]:
    """The OWL 2 DL counts of tableau_w3c's table (case, kind, profile, verdict, ...)."""
    verdicts = Counter(line.split("\t")[3] for line in text.splitlines() if line.count("\t") >= 3)
    # open: undecided (gave up, unsupported); every other verdict but a pass or a
    # recorded dispute is a failure (a wrong answer).
    return {"OWL 2 DL": {"passed": verdicts.pop("pass", 0), "disputed": verdicts.pop("disputed", 0),
                         "open": verdicts.pop("open", 0), "failed": sum(verdicts.values()), "other": dict(verdicts)}}


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


# --- what a check launched, and the receipt that binds it to a tree ----------------------------

class Outcome(NamedTuple):
    """One launched unit of a check (a W3C kit, the guards' test run): its exit code, its log's
    SHA-256, and whether the log holds libtest summaries, every one ok."""
    unit: str
    exit: int
    log_sha256: str
    summary: bool


class CheckReceipt(NamedTuple):
    """What a check ran: the checkout's commit and tree (its tracked files unchanged: `dirty`), the
    build identity (scripts/lib/gate-key.sh gate_identity: toolchain, flags, tools), the host and
    fleet job, its step's attempt (attempt_record()), and every unit it launched. `verdict` joins a
    check only through it (release_verdict()); its inputs' pins are the tree's (fetch-w3c-tests.sh,
    release-gate.sh's all.rdf), which the fetch verifies."""
    kind: str
    commit: str
    tree: str
    dirty: bool
    identity: str
    host: str
    job: str
    attempt: str
    outcomes: tuple[Outcome, ...]

    def as_json(self) -> dict:
        return {**self._asdict(), "outcomes": [o._asdict() for o in self.outcomes]}

    @classmethod
    def of(cls, data) -> "CheckReceipt | None":
        """A receipt read back (a result's `receipt`); None when there is none or it isn't one, every
        field of its own type (a "0" is no exit code, a "false" no flag)."""
        try:
            r = cls(**{**data, "outcomes": tuple(Outcome(**o) for o in data["outcomes"])})
        except (TypeError, KeyError):
            return None
        typed = (all(isinstance(getattr(r, k), str) for k in ("kind", "commit", "tree", "identity", "host", "job", "attempt"))
                 and type(r.dirty) is bool
                 and all(type(o.exit) is int and type(o.summary) is bool and isinstance(o.unit, str)
                         and isinstance(o.log_sha256, str) for o in r.outcomes))
        return r if typed else None


SUMMARY = re.compile(r"^test result: (ok|FAILED)\.", re.M)


def run_unit(unit: str, argv: list[str], log: Path, env: dict | None = None) -> Outcome:
    """UNIT run here, its output (stdout and stderr) into LOG, which it replaces; a `bash` in the
    repository's bash (driver.bash(): a bare one on Windows is WSL's launcher)."""
    if argv[0] == "bash":
        argv = [driver.bash(), *argv[1:]]
    log.parent.mkdir(parents=True, exist_ok=True)
    with open(log, "wb") as out:
        code = subprocess.run(argv, stdout=out, stderr=subprocess.STDOUT, env=env).returncode
    data = log.read_bytes()
    summaries = SUMMARY.findall(data.decode("utf-8", errors="replace"))
    return Outcome(unit, code, hashlib.sha256(data).hexdigest(), bool(summaries) and set(summaries) == {"ok"})


def outcome_errors(outcomes: tuple[Outcome, ...], units: list[str]) -> list[str]:
    """Every declared unit launched once and run whole: exit 0 and passing libtest summaries; one
    launched but not declared counts for nothing and is named."""
    by_unit = {o.unit: o for o in outcomes}
    launched = Counter(o.unit for o in outcomes)
    errors = [f"{unit}: launched, but not declared" for unit in by_unit if unit not in units]
    errors += [f"{unit}: launched {n} times" for unit, n in launched.items() if n > 1]
    for unit in units:
        o = by_unit.get(unit)
        if o is None:
            errors.append(f"{unit}: no launch recorded (the gate's step runs it: --run)")
        elif o.exit == 75:
            errors.append(f"{unit}: exited 75: the host's guard held or stopped its build, no verdict")
        elif o.exit != 0:
            errors.append(f"{unit}: exited {o.exit}")
        elif not o.summary:
            errors.append(f"{unit}: exited 0 without passing libtest summaries (killed, truncated or failed)")
        elif not re.fullmatch(r"[0-9a-f]{64}", o.log_sha256):
            errors.append(f"{unit}: no SHA-256 of its log")
    return errors


def checkout_receipt(kind: str, identity: str, attempt: str, outcomes: list[Outcome]) -> CheckReceipt:
    """The receipt of what ran in this directory's checkout (release-gate.sh's checked code checkout)
    as ATTEMPT."""
    def git(*args: str) -> str:
        done = subprocess.run(["git", *args], capture_output=True, text=True, timeout=60)
        return done.stdout.strip() if done.returncode == 0 else ""
    return CheckReceipt(kind, git("rev-parse", "HEAD"), git("rev-parse", "HEAD^{tree}"),
                        bool(git("status", "--porcelain", "--untracked-files=no")), identity,
                        os.environ.get("FLEET_HOST") or platform.node(), os.environ.get("FLEET_JOB", ""), attempt,
                        tuple(outcomes))


# A check's step writes its attempt record first (release-gate.sh w3c, guards; `release_gate.py
# attempt`), before it touches an earlier result: KIND.attempt.SEQUENCE.json beside the result, the
# sequence its name, created exclusively (O_EXCL: two attempts never hold one sequence, however they
# overlap; a taken name sends an attempt to the next), never replaced or removed: no reader holding an
# older record or result can stop it. The record holds a fresh random id, which the result and its
# receipt carry; `verdict` takes a result only of the newest record's id, and nothing at all from a
# history it can't order (a sequence held twice, a name of another form, at any position). The
# orchestrator's design, fix checks r2 and r3, 9 Nov 2026. The run directory is per job: old records stay.
ATTEMPT_SEQUENCE = re.compile(r"(\d+)")
# Sequences an attempt tries past the highest it found before it refuses: as many steps of one kind
# overlapping in one job's directory as this is no gate's (one step a kind runs at a time).
ATTEMPT_TRIES = 64


class AttemptHistory(NamedTuple):
    """KIND's attempt records in a directory: (sequence, path) of each record's name, oldest first;
    every sequence a name there holds, of any form (an attempt allocates above them all); and what
    makes the history unordered (a name of another form, a sequence held twice)."""
    records: list[tuple[int, Path]]
    sequences: list[int]
    ambiguous: list[str]


def attempt_records(where: Path, kind: str) -> AttemptHistory:
    """KIND's attempt history in WHERE (AttemptHistory)."""
    records, sequences, ambiguous = [], [], []
    for path in sorted(where.glob(f"{kind}.attempt.*")):
        rest = path.name[len(f"{kind}.attempt."):]
        number = ATTEMPT_SEQUENCE.match(rest)
        if number:
            sequences.append(int(number[1]))
        if number and rest == f"{int(number[1]):09d}.json":
            records.append((int(number[1]), path))
        else:
            ambiguous.append(f"{path.name}: not an attempt record's name ({kind}.attempt.SEQUENCE.json)")
    ambiguous += [f"sequence {s} held by {n} names" for s, n in sorted(Counter(sequences).items()) if n > 1]
    return AttemptHistory(sorted(records), sequences, ambiguous)


def attempt_record(kind: str, where: Path) -> str:
    """A new attempt of KIND's step in this directory's checkout, recorded in WHERE as the next
    sequence no name there holds (created exclusively: a name taken meanwhile sends it to the next,
    ATTEMPT_TRIES at most); its id."""
    tree = git_line(".", "rev-parse", "HEAD^{tree}")
    if not tree:
        raise SystemExit(f"attempt: no checkout here to name the tree of: no attempt of {kind}")
    attempt = secrets.token_hex(16)
    first = 1 + max(attempt_records(where, kind).sequences, default=0)
    for sequence in range(first, first + ATTEMPT_TRIES):
        try:
            fd = os.open(where / f"{kind}.attempt.{sequence:09d}.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
        except FileExistsError:
            continue  # another attempt holds it: the next
        with os.fdopen(fd, "w", encoding="utf-8") as out:
            out.write(json.dumps({"kind": kind, "attempt": attempt, "tree": tree, "sequence": sequence}))
            out.flush()
            os.fsync(out.fileno())
        return attempt
    raise SystemExit(f"attempt: sequences {first} to {first + ATTEMPT_TRIES - 1} of {kind} in {where} are all "
                     "taken: no attempt recorded")


def newest_attempt(where: Path, kind: str) -> tuple[dict | None, str | None]:
    """KIND's newest attempt record in WHERE, by sequence, and what's wrong with it: (None, None) when
    there is none; an unordered history (attempt_records()), or a newest record that isn't one (its
    content not its name's sequence and kind, no random id or tree), is no record."""
    history = attempt_records(where, kind)
    if history.ambiguous:
        return None, f"its attempt history is ambiguous ({'; '.join(history.ambiguous)})"
    if not history.records:
        return None, None
    sequence, path = history.records[-1]
    try:
        record = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        return None, f"its newest attempt record {path.name} can't be read ({error})"
    if not (isinstance(record, dict) and record.get("kind") == kind and record.get("sequence") == sequence
            and isinstance(record.get("attempt"), str) and re.fullmatch(r"[0-9a-f]{32}", record["attempt"])
            and isinstance(record.get("tree"), str) and record["tree"]):
        return None, f"its newest attempt record {path.name} isn't one"
    return record, None


# The checks' exits: 0 passed and 1 not passed, each with its own published result (the steps keep
# those for the verdict); CANT_INVALIDATE when an earlier result can't be removed, so no run happened.
CANT_INVALIDATE = 3


def publish(path: Path, data: dict) -> None:
    """DATA as PATH's JSON, whole or not at all: written beside it, then renamed over it."""
    part = path.with_name(path.name + ".partial")
    part.write_text(json.dumps(data, indent=1), encoding="utf-8")
    os.replace(part, path)


def run_w3c(logs: Path, kits: list[Kit], identity: str, attempt: str) -> CheckReceipt:
    """Every declared kit, in order, through this checkout's scripts/cargo-guarded.sh; each kit's log
    and table its own, an earlier attempt's removed first."""
    outcomes = []
    for kit in kits:
        table = logs / kit.table if kit.table else None
        if table is not None:
            table.unlink(missing_ok=True)
        env = {**os.environ, **({"NRESE_W3C_OUT": str(table)} if table is not None else {})}
        o = run_unit(kit.name, ["bash", "scripts/cargo-guarded.sh", "test", "--release", "--locked", "-p", kit.crate,
                                "--test", kit.test, "--", "--nocapture"], logs / f"{kit.test}.log", env)
        print(f"w3c {kit.name}: exit {o.exit}{'' if o.summary else ', no passing summary'}", file=sys.stderr)
        outcomes.append(o)
    return checkout_receipt("w3c", identity, attempt, outcomes)


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
                    if values and None not in values:  # a null sample: not reported (evidence())
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
        # ran the case (a baseline that can't run it has no times). A classifier process's whole
        # run (its phases and every --repeat run) is no part of one execution.
        once = [r for r in rows if r["series"] != cases_registry.WHOLE_RUN]
        per_run = max((sum(r["median_ms"] for r in once if r["rev"] == rev) for rev in revs), default=0.0)
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
        elif case.get("tool") in ("tableau", "classify-horn") and "ok" in e["status"] and once:
            # Files a process (the tableau's), or classifications whose median it reports
            # (the context classifier's).
            whole = per_run if case["tool"] == "tableau" else max(r["median_ms"] for r in once)
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
    """Stops `process` and everything it started, at once (benches/suite/measurement_queue.py's stop, without its
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


def measurement_inputs(checkout: str, rev: str, revs: list[str], sides: dict[str, str] | None = None) -> list[dict]:
    """The measurement commits of these sides that a job's checkout of `rev` doesn't reach, as its
    git inputs: the checkout holds only `rev`'s history, and release-gate.sh build needs them in it.
    One that `rev` reaches is staged with it already (and the fleet takes one git input, F150)."""
    import subprocess
    sides = measurement_sides() if sides is None else sides
    inputs = []
    for side in dict.fromkeys(revs):
        full = subprocess.run(["git", "-C", checkout, "rev-parse", "--verify", f"{side}^{{commit}}"],
                              capture_output=True, text=True, timeout=60).stdout.strip()
        if full not in sides:
            continue
        # As stored: a replace ref or graft here would reach it only in this repository.
        reached = subprocess.run(["git", "-C", checkout, "merge-base", "--is-ancestor", sides[full], rev],
                                 capture_output=True, text=True, timeout=60, env={**os.environ, **STORED_OBJECTS})
        if reached.returncode != 0:  # not reached, or not known here: kept, for the build to find or report
            inputs.append({"name": f"measure-{full[:8]}", "source": {"git": checkout, "rev": sides[full]}})
    return inputs


def spec(job: dict) -> dict:
    """The job as the fleet takes it: without the planner's own keys (`_plan`)."""
    return {k: v for k, v in job.items() if not k.startswith("_")}


def toml_value(v) -> str:
    """A value as TOML writes it inline (strings JSON-escaped, which TOML's basic strings read)."""
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, (int, float)):
        return repr(v)
    if isinstance(v, list):
        return "[" + ", ".join(toml_value(x) for x in v) + "]"
    if isinstance(v, dict):
        return "{ " + ", ".join(f"{k} = {toml_value(x)}" for k, x in v.items()) + " }"
    return json.dumps(v)


def side_inputs(checkout: str, rev: str, revs: list[str]) -> list[dict]:
    """The git inputs a job building `revs` from a checkout of `rev` needs: the sides' measurement
    commits (measurement_inputs()), and each side that `rev`'s history doesn't hold, so that its
    build finds it (Sol r1 S4, 2 Nov 2026: a base off the change's history was built unstaged)."""
    import subprocess
    inputs = measurement_inputs(checkout, rev, revs)
    for side in dict.fromkeys(revs):
        held = subprocess.run(["git", "-C", checkout, "merge-base", "--is-ancestor", side, rev],
                              capture_output=True, text=True, timeout=60)
        if held.returncode == 1:  # not an ancestor; another code (no such commit) is the build's to report
            inputs.append({"name": f"side-{side[:8]}", "source": {"git": checkout, "rev": side}})
    return inputs


def toml_job(job: dict) -> str:
    job = spec(job)
    lines = [f"{k} = {toml_value(v)}" for k, v in job.items() if k not in ("requires", "inputs", "outputs")]
    lines += ["", "[requires]"] + [f"{k} = {toml_value(v)}" for k, v in job["requires"].items()]
    for table in ("inputs", "outputs"):
        for item in job.get(table, []):
            lines += ["", f"[[{table}]]"] + [f"{k} = {toml_value(v)}" for k, v in item.items()]
    return "\n".join(lines) + "\n"


def toml_set(head: dict, members: list[dict]) -> str:
    """One fleet set (fleet-jobs skill, "A set": one wait, one cancel, one result): `head` its
    [set] table, each member a [[job]] with its requires, inputs and outputs."""
    lines = ["[set]"] + [f"{k} = {toml_value(v)}" for k, v in head.items()]
    for job in map(spec, members):
        lines += ["", "[[job]]"] + [f"{k} = {toml_value(v)}" for k, v in job.items()
                                     if k not in ("requires", "inputs", "outputs")]
        lines += ["[job.requires]"] + [f"{k} = {toml_value(v)}" for k, v in job["requires"].items()]
        for table in ("inputs", "outputs"):
            for item in job.get(table, []):
                lines += [f"[[job.{table}]]"] + [f"{k} = {toml_value(v)}" for k, v in item.items()]
    return "\n".join(lines) + "\n"



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


# What a timed part asks, never a host (rule.hosts-belong-to-the-fleet; the brief of 2 Nov 2026): a
# timing host that also builds, since each part builds its sides where it times them (its first step,
# the host held from there) and so needs no binaries staged from elsewhere.
TIMING_ROLES = ["measure", "build"]
BUILD_MIN = 25  # a revision's build, at most; one of the same identity on that host is reused
ASSETS = "owlrs-bench-assets"
# The SPARQL data step's maker: the catalogue's [makers] entry with `sparql` (SPARQL_PREPARE, below,
# its one source for planning and preparation alike: sol-loop r6 S4); this where a catalogue names none.
SPARQL_DEFAULT = "benches/sparql/prepare.sh"


def volume_files(case: dict) -> list[str]:
    """The frozen files a case reads (`volume:` specs, a concatenation's parts included)."""
    specs = [case.get("data", ""), *case.get("extra", [])]
    specs += [p for s in specs if s.startswith("concat:") for p in s[len("concat:"):].split("+")]
    return [s[len("volume:"):] for s in specs if s.startswith("volume:")]


def frozen(files: set[str], datasets: Path) -> dict[str, dict]:
    """benches/datasets.toml's entries, read only when there are frozen files to look up: a
    checkout without the file (a test's own repository) plans its generated-data cases (Sol
    loop proofs 1 and 3, 2 Nov 2026: read for every plan, it failed where no case reads one)."""
    if not files:
        return {}
    return {d["file"]: d for d in tomllib.loads(datasets.read_text(encoding="utf-8"))["dataset"]}


def catalogue_makers(makers: dict) -> tuple[dict[str, list[str]], set[str], str, dict[str, tuple]]:
    """A catalogue's [makers] (benches/datasets.toml): (DATASET_MAKERS, FORM_ONLY, SPARQL_PREPARE,
    MAKER_CONTAINERS). Declared data, read as data: their one source, for this tree's data step and
    for a harness's (harness_tables(); review r2 of pkg/maker-containers, R2-W1: tables in code can
    be changed by code, which no reader can follow without running it). ValueError where it isn't
    their shape."""
    def valid(start, maker):
        if not (isinstance(start, str) and start.strip() == start and start and isinstance(maker, dict)):
            return False  # sol-loop r6 S7: the entry's type before its fields
        container, needs = maker.get("container"), maker.get("needs", [])
        return (set(maker) <= set(MAKER_KEYS) and isinstance(needs, list) and all(isinstance(n, str) for n in needs)
                and isinstance(maker.get("form_only", False), bool) and isinstance(maker.get("sparql", False), bool)
                and (container is None or (isinstance(container, list) and len(container) == 2
                                           and all(type(v) in (int, float) and math.isfinite(v) and v > 0
                                                   for v in container))))
    if not isinstance(makers, dict) or not all(valid(s, m) for s, m in makers.items()):
        raise ValueError("[makers] isn't a table of makers (needs, form_only, sparql, container [finite, positive]"
                         ", note)")
    # A file's maker is the start its made_by begins with (maker_of()); two starts one of which begins the
    # other would make the answer the order they're listed in, which a save re-sorts (sol-loop r6 S6).
    if overlap := sorted(f"{a} / {b}" for a in makers for b in makers if a != b and b.startswith(a + " ")):
        raise ValueError(f"[makers] has starts one of which begins another ({overlap[0]}): a file's maker is ambiguous")
    sparql = [s for s, m in makers.items() if m.get("sparql")]
    if len(sparql) > 1 or (sparql and not sparql[0].startswith("bash ")):
        raise ValueError("[makers] names more than one SPARQL data step, or one that isn't `bash SCRIPT`")
    return ({s: list(m.get("needs", [])) for s, m in makers.items() if not m.get("sparql")},
            {s for s, m in makers.items() if m.get("form_only")},
            sparql[0][len("bash "):] if sparql else SPARQL_DEFAULT,
            {s: tuple(m["container"]) for s, m in makers.items() if "container" in m})


MAKER_KEYS = ("needs", "form_only", "sparql", "container", "note")
# The makers a job runs where its host lacks a frozen file (Johannes, 2 Nov 2026, decisions 1 and 8:
# "a job sets up what it needs where it's missing", never a blanket copy; orchestrator, 7 Nov):
# a `made_by` command's start -> what the job asks the fleet for to run it. Each writes into a
# directory aside (NRESE_DATASET_DIR; the Docker makers bind-mount it instead of the shared volume),
# checked against its pins before it enters the cache (data_state()). Every other maker's files
# are the verified bench assets'. FORM_ONLY: makers whose output a copy can meet only by its form
# (riot labels the blank nodes afresh on every make): their files count as makeable only with a form
# pin (`rdfc_sha256`); one without keeps the assets (sol-loop r1 S1: owl2bench-ql-1.nt). Each in
# benches/datasets.toml [makers], with its reasons.
_CATALOGUE = ROOT / "benches" / "datasets.toml"
DATASET_MAKERS, FORM_ONLY, SPARQL_PREPARE, MAKER_CONTAINERS = catalogue_makers(
    tomllib.loads(_CATALOGUE.read_text(encoding="utf-8")).get("makers", {}) if _CATALOGUE.is_file() else {})
SUPPORT_STRESS = "perf_lab generate support-stress"


def maker_of(entry: dict, tables: "DataTables | None" = None) -> tuple[str, list[str]] | None:
    """The job-run maker of a frozen file (its DATASET_MAKERS start and what it needs), or None: none
    there, or one whose copies can't meet the file's pins. By `tables` (a harness's: harness_tables();
    default this tree's)."""
    makers, form_only = (DATASET_MAKERS, FORM_ONLY) if tables is None else (tables.makers, tables.form_only)
    made = entry.get("made_by", "")
    found = next(((start, needs) for start, needs in makers.items() if made.startswith(start + " ")), None)
    if found and found[0] in form_only and not entry.get("rdfc_sha256"):
        return None
    return found


# MAKER_CONTAINERS (above, from [makers] `container`): the container each maker that starts containers
# runs them in, one at a time (memory GB, CPUs): what a job that may run it declares to the fleet and
# holds the maker's containers to (data_job(); the master, 8 Nov 2026: the fleet refused #129's timing
# jobs, their makers' containers undeclared). Its figures' measured reasons are their [makers] notes;
# benches/sparql/prepare.sh's is its own CAP (test_release_gate holds the two alike).


class DataTables(NamedTuple):
    """A tree's dataset metadata, as its data step uses it (release_gate.py data_state() and
    make_dataset() in that tree): its catalogue (benches/datasets.toml, by file; None: this tree has
    none, a test's own repository; a harness's is always there: harness_tables()) and the tables above,
    from its [makers]. `held`: the maker scripts there whose containers take the job's label and
    limits (they call scripts/lib/ns.sh's ns_maker_args, and ns.sh defines it)."""
    catalogue: dict[str, dict] | None
    makers: dict[str, list[str]]
    form_only: set[str]
    sparql: str
    containers: dict[str, tuple]
    held: frozenset[str] = frozenset()


NS_HELPER = "scripts/lib/ns.sh"


def held_makers(read, starts) -> frozenset[str]:
    """Of the makers `starts` (their `bash SCRIPT` commands), the scripts `read` (path -> text, None:
    not there) shows held to a job's declaration: ns.sh defines ns_maker_args, the script calls it."""
    if "ns_maker_args()" not in (read(NS_HELPER) or ""):
        return frozenset()
    scripts = {start.split()[1] for start in starts if start.startswith("bash ")}
    return frozenset(s for s in scripts if "ns_maker_args" in (read(s) or ""))


def local_tables() -> DataTables:
    """This tree's: for its own data step (data_state()) and for tests of the ask alone."""
    path = ROOT / "benches" / "datasets.toml"
    catalogue = {d["file"]: d for d in tomllib.loads(path.read_text(encoding="utf-8"))["dataset"]} \
        if path.is_file() else None
    read = lambda rel: (ROOT / rel).read_text(encoding="utf-8") if (ROOT / rel).is_file() else None  # noqa: E731
    return DataTables(catalogue, DATASET_MAKERS, FORM_ONLY, SPARQL_PREPARE, MAKER_CONTAINERS,
                      held_makers(read, MAKER_CONTAINERS))


class HarnessRefused(ValueError):
    """A plan whose harness can't make, or wouldn't declare, the data the plan counts on."""


def harness_tables(checkout: str | Path, harness: str) -> DataTables:
    """The dataset metadata of the harness at commit `harness` of `checkout`: what its data step will
    make and how, since a job's data is made there (release-gate.sh prepare, in the harness's
    worktree), not by the tree that plans it (review r1 R1 of pkg/maker-containers, 8 Nov 2026; PR
    #139). Its catalogue by tomllib, its makers' tables from the catalogue's [makers]
    (catalogue_makers(): declared data, nothing in its code can change what the job is told; review
    r2 R2-W1), and which of its makers' scripts are held to a declaration (held_makers()).

    Refused (HarnessRefused), naming the remedy: no catalogue at that commit (none there, or no such
    commit in `checkout`), one that doesn't read, a malformed [makers], or none. A harness without
    [makers] (before pkg/maker-containers, PR #129's included) is refused whatever its code says, and
    nothing of its release_gate.py is read: a reader of tables kept as code sees what one literal
    says, while the harness's code can change it (fix check r3 R3-W1 of pkg/maker-containers: an alias
    added Docker to LUBM's needs, planned with no container; the master, 8 Nov 2026: "remove the legacy
    reader"). Merging engine-v2, which every timing plan does, gives a harness [makers]."""
    import subprocess

    def at(path: str) -> str | None:
        done = subprocess.run(["git", "-C", str(checkout), "show", f"{harness}:{path}"], capture_output=True,
                              text=True, encoding="utf-8", errors="replace", timeout=60)
        return done.stdout if done.returncode == 0 else None

    remedy = "merge refactor/engine-v2 (with [makers]) into the harness, or pass --harness at a commit that has them"

    def refuse(why: str):
        raise HarnessRefused(f"the harness {harness[:12]}'s {why}, so what its data step makes, and in which "
                             f"containers, can't be read from it: {remedy}")

    text = at("benches/datasets.toml")
    if text is None:
        refuse("benches/datasets.toml can't be shown (check --harness names a commit of this checkout), nor its "
               "dataset makers ([makers])")
    try:
        whole = tomllib.loads(text)
        catalogue = {d["file"]: d for d in whole["dataset"]}
    except (tomllib.TOMLDecodeError, KeyError, TypeError) as error:
        refuse(f"benches/datasets.toml doesn't read ({error})")
    if "makers" not in whole:
        refuse("benches/datasets.toml has no [makers], its dataset makers and their containers as data "
               "(engine-v2 has them since pkg/maker-containers)")
    try:
        makers, form_only, sparql, containers = catalogue_makers(whole["makers"])
    except ValueError as error:
        refuse(f"benches/datasets.toml {error}")
    return DataTables(catalogue, makers, form_only, sparql, containers, held_makers(at, containers))


def data_job(cases: list[dict], requires: dict, *, harness: tuple[str, str] | None,
             containers_declared: bool = False) -> tuple[dict, dict, dict]:
    """The one data ask of a timing job of `cases` (abba.py jobs(), jobs() part()), its `requires`
    (cores, ram_gb) given: (requires, the job's own fields, its env), all of it from the dataset
    metadata of `harness` (checkout, commit), whose data step makes the data (harness_tables(); None:
    this tree's, for a test of the ask alone). What the host must have or run (`has`, data_needs())
    and the disk to make it in (data_disk_gb()). Where a maker it may run starts containers (one that
    asks for Docker), the fleet's declared containers (F61, job spec 9: `spec`, `containers`, Linux):
    one container, as large as the largest of its makers' (the harness's MAKER_CONTAINERS), since they
    run one at a time; `ram_gb` grows by its memory, which the fleet takes out of every step's slice,
    so the builds and blocks keep what they asked; and the env its makers' containers take their
    limits from (scripts/lib/ns.sh ns_maker_args). A job that runs no such maker is asked as before,
    nothing declared. With `containers_declared` (spec 9 for containers of its own: a suite cell), every
    maker that can start a container and has a [makers] container is declared, whatever `has` says:
    the SPARQL maker asks the host nothing, yet starts one wherever Docker is (#132). Refused
    (HarnessRefused): a harness without a readable catalogue with [makers] (harness_tables(): PR #139's
    harness without dataset makers among them, whose prepare doesn't make
    what the plan counts on), a case file its catalogue doesn't name, and makers that start containers
    it has no size for or doesn't hold to the declaration."""
    files = {f for c in cases for f in volume_files(c)}
    tables = local_tables() if harness is None or not files else harness_tables(*harness)
    if harness is not None and files:
        named = harness[1][:12]
        if unknown := sorted(files - set(tables.catalogue)):  # sol-loop r3 S4
            raise HarnessRefused(f"the harness {named}'s catalogue has no {unknown[0]}"
                                 + (f" and {len(unknown) - 1} more" if len(unknown) > 1 else "")
                                 + ", so its data step can neither make nor check it: catalogue it in that "
                                 "harness's benches/datasets.toml, or time those cases with a harness that has it")
    has, disk = data_needs(cases, tables), data_disk_gb(cases, tables)
    requires = {**requires, **({"has": has} if has else {}), **({"disk_gb": disk} if disk else {})}
    if "docker" not in has and not containers_declared:
        return requires, {}, {}
    starts = {found[0] for f in files if (found := maker_of(tables.catalogue.get(f, {}), tables)) and found[1]}
    starts |= {f"bash {tables.sparql}" for f in files
               if tables.sparql in tables.catalogue.get(f, {}).get("made_by", "")}
    if not starts:
        return requires, {}, {}
    sizes = {start: tables.containers.get(start) for start in starts}
    named = harness[1][:12] if harness else "here"
    # Every maker of a declared job runs in the one declared container, SPARQL's too (sol-loop r3 S3).
    if unsized := sorted(s for s, size in sizes.items() if size is None):
        raise HarnessRefused(
            f"the harness {named}'s makers start containers it declares no size for ({', '.join(unsized)}: no "
            "container in its benches/datasets.toml [makers]), so the fleet would stop its data step at them: "
            "merge engine-v2 into it, or give --harness at or after pkg/maker-containers")
    # Their containers take the job's label and limits only where the harness's scripts do (sol-loop r3 S1).
    if loose := sorted(s.split()[1] for s in starts if s.split()[1] not in tables.held):
        raise HarnessRefused(
            f"the harness {named}'s makers don't hold their containers to the job's declaration ({', '.join(loose)}, "
            f"{NS_HELPER}: no ns_maker_args), so the fleet would stop its data step at them: merge engine-v2 "
            "into it, or give --harness at or after pkg/maker-containers")
    memory, cpus = max(m for m, _ in sizes.values()), max(c for _, c in sizes.values())
    requires |= {"os": "linux", "ram_gb": requires["ram_gb"] + memory}
    return (requires, {"spec": 9, "containers": [{"name": "maker", "memory_gb": memory, "cpus": cpus}]},
            {"NRESE_MAKER_MEMORY_GB": f"{memory:g}", "NRESE_MAKER_CPUS": f"{cpus:g}"})


def data_needs(cases: list[dict], tables: DataTables | None = None) -> list[str]:
    """What a host must hold or run for the cases' frozen inputs (fleet `has`). The job makes what
    benches/sparql/prepare.sh and the DATASET_MAKERS can (Johannes, 2 Nov 2026: "Data is the job's
    responsibility, not the fleet's"), asking what its makers need (Docker) and its disk
    (data_disk_gb()); the verified bench assets (hosts.toml `inputs`) only for a file no maker
    here makes, an ORE ontology or a reference taxonomy. So a plan the fleet takes places the job
    where each case's data is there or can be made (REAS, 7 Nov 2026, failed at its data step
    after its plan passed). By `tables` (the harness's: data_job(); default this tree's)."""
    tables = tables or local_tables()
    entries = tables.catalogue or {}
    out = set()
    for c in cases:
        for f in volume_files(c):
            entry = entries.get(f, {})
            if tables.sparql in entry.get("made_by", ""):
                continue
            maker = maker_of(entry, tables)
            out.update(maker[1] if maker else [ASSETS])
        if any(s.startswith("ore:") for s in [c.get("data", ""), *c.get("extra", [])]) or c.get("reference"):
            out.add(ASSETS)
    return sorted(out)


def pin_problem(path: Path, entry: dict, rdfc) -> str | None:
    """Why the file at `path` isn't its freeze (None: it is): its bytes' SHA-256 is its `sha256`, or,
    for a file pinned by form, its RDFC-1.0 canonical N-Quads' SHA-256 (`rdfc(path)`) is its
    `rdfc_sha256`. The datasets' one check, for the data view and for a made file alike. riot labels
    univ-bench.nt's blank nodes afresh on every make (stage-data-probe-1, 7 Nov 2026); a form pin
    with no `rdfc` to compute the form fails closed."""
    with open(path, "rb") as f:
        sha = hashlib.file_digest(f, "sha256").hexdigest()
    if sha == entry.get("sha256"):
        return None
    said = f"sha256 {sha[:12]}, pinned {str(entry.get('sha256', 'none'))[:12]}"
    form = entry.get("rdfc_sha256")
    if not form:
        return said
    if rdfc is None:
        return f"{said}; pinned by form (rdfc_sha256) too, but nothing here computes it (NRESE_RDFC)"
    got = rdfc(path)
    return None if got == form else f"{said}; RDFC-1.0 sha256 {got[:12]}, pinned {form[:12]}"


def dataset_source(name: str, entry: dict, dirs: list[Path], rdfc) -> tuple[Path | None, str | None]:
    """The first copy of `name` in `dirs` (the host's assets, then its dataset cache) that is its
    pin (pin_problem()), and None; or, none being, the first copy there and why it isn't (None, None:
    no copy). A copy at another size than its pin's is an older release (lubm-10, lubm-100 became
    their sorted distinct lines, 7 Nov 2026), skipped without hashing it."""
    copies = [d / name for d in dirs if d and (d / name).is_file()]
    for path in copies:
        if entry.get("bytes") and path.stat().st_size != entry["bytes"]:
            continue
        if pin_problem(path, entry, rdfc) is None:
            return path, None
    if not copies:
        return None, None
    first = copies[0]
    if entry.get("bytes") and first.stat().st_size != entry["bytes"]:
        return first, f"{first.stat().st_size} bytes, pinned {entry['bytes']}"
    return first, pin_problem(first, entry, rdfc)


def rdfc_hash(binary: str | None):
    """The SHA-256 of a file's RDFC-1.0 canonical N-Quads, by the harness's `nrese-rdf-io` example
    `rdfc` at `binary` (None: none here). A failed run gives a value no pin has."""
    import subprocess
    if not binary:
        return None

    def run(path: Path) -> str:
        done = subprocess.run([binary, str(path)], capture_output=True, timeout=3600)
        if done.returncode != 0:
            return f"rdfc failed (exit {done.returncode}): {done.stderr.decode(errors='replace').strip()[-120:]}"
        return hashlib.sha256(done.stdout).hexdigest()
    return run


def sparql_data() -> Path:
    """The SPARQL workloads' data cache on this host (benches/hosts.toml [caches.sparql-data])."""
    return Path(os.environ.get("SPARQL_DATA") or Path.home() / "owlrs" / "sparql-data")


def data_disk_gb(cases: list[dict], tables: DataTables | None = None) -> float:
    """The disk a job may fill making its cases' data where the host lacks it: each frozen file
    benches/sparql/prepare.sh or a DATASET_MAKERS maker makes, twice (its source and the file), in GB.
    By `tables` (the harness's: data_job(); default this tree's)."""
    files = {f for c in cases for f in volume_files(c)}
    if not files:
        return 0
    tables = tables or local_tables()
    entries = tables.catalogue or {}
    made = [entries[f] for f in files if f in entries
            and (tables.sparql in entries[f].get("made_by", "") or maker_of(entries[f], tables))]
    return math.ceil(2 * sum(d.get("bytes", 0) for d in made) / 1e9)


def query_set_problems(directory: Path, pinned: dict[str, str]) -> list[str]:
    """Why the query set in `directory` isn't its pins (empty: it is): every pinned file, by exact
    name, with its SHA-256, and no other .rq file (review r1 R1-F2: a count of files passed a set of
    wrong contents, or of other names)."""
    if not directory.is_dir():
        return [f"no {directory}"]
    have = {p.name: p for p in directory.glob("*.rq")}
    out = []
    if missing := sorted(set(pinned) - set(have)):
        out.append(f"{len(missing)} pinned files missing (e.g. {missing[0]})")
    if extra := sorted(set(have) - set(pinned)):
        out.append(f"{len(extra)} files not pinned (e.g. {extra[0]})")
    wrong = sorted(n for n in set(pinned) & set(have)
                   if hashlib.sha256(have[n].read_bytes()).hexdigest() != pinned[n])
    if wrong:
        out.append(f"{len(wrong)} files differ from their SHA-256 pins (e.g. {wrong[0]})")
    return out


def data_state(cases: list[dict], make: bool = False, root: Path | None = None,
               inputs_repo: Path | None = None) -> tuple[list[str], bool]:
    """What this host holds of the cases' frozen inputs, and whether all of it is as pinned: the
    one validator of query sets, which release-gate.sh prepare runs with `make` and stops on.
    - Datasets: each file in the bench assets (hosts.toml inputs.general) or the SPARQL data cache;
      with `make`, one benches/sparql/prepare.sh makes is made into the cache first, its failure
      failing the data. Their one gate is prepare's freeze check (their SHA-256), which links and
      converts them: here the probe reports a size other than the frozen one, and with `make` leaves
      the verdict to that check (one validator each, not two; review r1).
    - Query sets (@watdiv...): exact pinned names and SHA-256 (benches/sparql/queries.sha256), in the
      cache and as delivered. WatDiv's generator seeds from the clock, so a set that isn't its pins
      comes from the private bench-inputs repository's commit, a git input of the job
      (BENCH_INPUTS_REV): extracted aside, checked, and only then put in place; a delivery that
      isn't the pins fails and leaves the cache as it was.
    Lines, and whether everything is there and as pinned."""
    root = root or ROOT
    general, cache = hosts_registry.input_path("general"), sparql_data()
    files = sorted({f for c in cases for f in volume_files(c)})  # a concatenation's parts too (Sol r2 T2)
    entries = frozen(set(files), root / "benches" / "datasets.toml")
    lines, ok = [], True
    asides: dict[str, Path] = {}
    try:
        for name in files:
            entry = entries.get(name, {})
            found = next((d / name for d in (general, cache) if d and (d / name).is_file()), None)
            if make and maker_of(entry) and entry.get("bytes"):
                # A copy at another size than the pin is an older release: one this job can make is
                # made rather than stopped on (the view takes the copy that is its pin).
                found = next((d / name for d in (general, cache) if d and (d / name).is_file()
                              and (d / name).stat().st_size == entry["bytes"]), None)
            if found is None and make and maker_of(entry):
                found = make_dataset(name, entry, root, cache, asides, lines)
                if found is None:
                    ok = False
                    continue
            ok = probe_dataset(name, entry, found, make, cache, root, lines) and ok
    finally:
        import shutil
        for aside in asides.values():
            shutil.rmtree(aside, ignore_errors=True)
    return query_sets_state(cases, make, root, cache, inputs_repo, lines, ok)


def make_dataset(name: str, entry: dict, root: Path, cache: Path, asides: dict[str, Path],
                 lines: list[str]) -> Path | None:
    """`name` made by its DATASET_MAKERS maker into a directory aside (one run per maker command;
    support-stress's maker makes the whole workload at once), checked against its pin
    (pin_problem()), and only then renamed into the cache: its path there, or None (why, in
    `lines`). A file that isn't its pin stays out, and the cache is as it was."""
    import subprocess
    import sys
    import tempfile
    import time
    command = entry["made_by"]
    key = SUPPORT_STRESS if command.startswith(SUPPORT_STRESS + " ") else command
    if key not in asides:
        cache.mkdir(parents=True, exist_ok=True)
        aside = asides[key] = Path(tempfile.mkdtemp(prefix=".make-", dir=cache))
        if key == SUPPORT_STRESS:
            perf_lab = os.environ.get("NRESE_PERF_LAB", "")
            argv = [sys.executable, (root / "benches/fast/prepare-support-stress.py").as_posix(), perf_lab,
                    aside.as_posix(), (aside / "answers.json").as_posix()] if perf_lab else []
        else:
            import shutil
            words = command.split()  # bash SCRIPT ARGS; the bash on PATH (Windows' own is WSL's launcher)
            argv = [shutil.which("bash") or "bash", (root / words[1]).as_posix(), *words[2:]]
        if not argv:
            lines.append(f"FAILED making `{key}`: no perf_lab here (NRESE_PERF_LAB)")
        else:
            start = time.monotonic()
            done = subprocess.run(argv, cwd=root, env={**os.environ, "NRESE_DATASET_DIR": aside.as_posix()})
            lines.append(f"ran `{key}` aside: exit {done.returncode}, {time.monotonic() - start:.0f} s")
    made = asides[key] / name
    if not made.is_file():
        lines.append(f"FAILED {name}: `{command}` made no {name}")
        return None
    if problem := pin_problem(made, entry, rdfc_hash(os.environ.get("NRESE_RDFC"))):
        lines.append(f"FAILED {name}: made by `{command}`, not as pinned: {problem}")
        return None
    target = cache / name
    target.parent.mkdir(parents=True, exist_ok=True)
    made.replace(target)
    lines.append(f"made {name} by `{command}`, as pinned: {target}, {target.stat().st_size} bytes")
    return target


def probe_dataset(name: str, entry: dict, found: Path | None, make: bool, cache: Path, root: Path,
                  lines: list[str]) -> bool:
    """One frozen file as the host holds it (a line in `lines`); with `make`, a missing one
    benches/sparql/prepare.sh makes is made into the cache first. Whether it may stand."""
    import subprocess
    ok = True
    if found is None and make and SPARQL_PREPARE in entry.get("made_by", ""):
        dataset = entry["made_by"].split(SPARQL_PREPARE, 1)[1].split()
        done = subprocess.run(["bash", str(root / SPARQL_PREPARE), *dataset, "--out", str(cache)], cwd=root)
        lines.append(f"made {name} ({' '.join(dataset)}): exit {done.returncode}")
        if done.returncode != 0:
            ok = False
        found = cache / name if (cache / name).is_file() else None
    if found is None:
        how = (f"made by `{entry['made_by']}`" if entry.get("made_by") else "no recorded maker")
        lines.append(f"MISSING {name}: {how}")
        return False
    if entry.get("bytes") and found.stat().st_size != entry["bytes"]:
        # The probe's diagnosis; in prepare (`make`), the freeze check that follows refuses it by
        # its SHA-256, the datasets' one gate, so this step doesn't second-guess it.
        lines.append(f"DIFFERS {name}: {found} is {found.stat().st_size} bytes, frozen {entry['bytes']}"
                     + (" (prepare's freeze check decides)" if make else ""))
        return ok and make
    lines.append(f"present {name}: {found}, its frozen size (prepare checks its SHA-256)")
    return ok


def query_sets_state(cases: list[dict], make: bool, root: Path, cache: Path, inputs_repo: Path | None,
                     lines: list[str], ok: bool) -> tuple[list[str], bool]:
    """data_state()'s query sets (@watdiv...): exact pinned names and SHA-256, taken from the bench
    inputs where they aren't (its docstring)."""
    pin_file = root / "benches/sparql/queries.sha256"
    pins = [line.split("  ", 1) for line in (pin_file.read_text(encoding="utf-8").splitlines()
                                             if pin_file.is_file() else []) if "  " in line]
    rev = os.environ.get("BENCH_INPUTS_REV", "")
    for qset in sorted({c["queries"][1:] for c in cases if str(c.get("queries", "")).startswith("@watdiv")}):
        pinned = {name.split("/", 1)[1]: sha for sha, name in pins if name.startswith(f"{qset}/")}
        target = cache / "queries" / qset
        if not pinned:
            ok = False
            lines.append(f"MISSING pins for query set {qset} in {pin_file}")
            continue
        problems = query_set_problems(target, pinned)
        if problems and make and rev:
            problems = extract_query_set(qset, rev, inputs_repo or root, cache, pinned, lines)
        if problems:
            ok = False
            lines.append(f"MISSING query set {qset}: {'; '.join(problems)}. Generated with a clock seed, it can't be "
                         f"made again: it comes from the bench inputs repository (benches/hosts.toml [sources]), a "
                         f"git input of the job ({'BENCH_INPUTS_REV ' + rev[:12] if rev else 'none given'})")
        else:
            lines.append(f"held query set {qset}: {len(pinned)} files, each as pinned")
    return lines, ok


def extract_query_set(qset: str, rev: str, repo: Path, cache: Path, pinned: dict[str, str],
                      lines: list[str]) -> list[str]:
    """The query set `qset` out of the bench-inputs commit `rev` (in `repo`, where the fleet pushed
    it) by `git archive`, into a directory aside, checked against its pins, and only then in place
    of the cache's. Its problems (empty: in place); a delivery that isn't the pins leaves the cache
    as it was."""
    import io
    import shutil
    import subprocess
    import tarfile
    import tempfile
    done = subprocess.run(["git", "-C", str(repo), "archive", "--format=tar", rev, f"queries/{qset}"],
                          capture_output=True, timeout=300)
    if done.returncode != 0:
        why = done.stderr.decode(errors="replace").strip()[-200:]
        lines.append(f"FAILED taking query set {qset} from the bench inputs ({rev[:12]}): {why}")
        return [f"not in the bench inputs at {rev[:12]}"]
    (cache / "queries").mkdir(parents=True, exist_ok=True)
    aside = Path(tempfile.mkdtemp(prefix=f".{qset}-", dir=cache / "queries"))
    try:
        with tarfile.open(fileobj=io.BytesIO(done.stdout)) as archive:
            archive.extractall(aside, filter="data")
        delivered = aside / "queries" / qset
        if problems := query_set_problems(delivered, pinned):
            lines.append(f"FAILED: the bench inputs at {rev[:12]} deliver a query set {qset} that isn't its pins: "
                         + "; ".join(problems))
            return [f"the bench inputs at {rev[:12]} aren't its pins"]
        target = cache / "queries" / qset
        shutil.rmtree(target, ignore_errors=True)
        delivered.rename(target)
        lines.append(f"copied query set {qset} from the bench inputs ({rev[:12]}): {len(pinned)} files, each as pinned")
        return []
    finally:
        shutil.rmtree(aside, ignore_errors=True)


BENCH_INPUTS_INPUT = "BENCH_INPUTS"


def bench_inputs(cases: list[dict]) -> list[dict]:
    """The private input repository as a git input (benches/hosts.toml [sources] bench-inputs, its
    variable overriding), where the cases read a pinned query set that can't be made again (WatDiv's):
    the job fetches it, nothing a host must hold (the orchestrator's decision (b), 2 Nov 2026). None
    where no case needs it, or the repository isn't there: the data probe then says what's missing."""
    import subprocess
    if not any(str(c.get("queries", "")).startswith("@watdiv") for c in cases):
        return []
    source = hosts_registry.load().get("sources", {}).get("bench-inputs", {})
    repo = os.environ.get(source.get("env") or "BENCH_INPUTS_GIT") or source.get("git")
    if not repo or not Path(repo).is_dir():
        return []
    rev = subprocess.run(["git", "-C", repo, "rev-parse", "HEAD"], capture_output=True, text=True, timeout=60)
    if rev.returncode != 0:
        return []
    return [{"name": BENCH_INPUTS_INPUT, "source": {"git": Path(repo).as_posix(), "rev": rev.stdout.strip()}}]


def case_seconds(measure: dict | None, case: str) -> float | None:
    """A case's seconds a pass by the calibration, None where it has none."""
    value = (measure or {}).get(case, {}).get("seconds")
    return float(value) if value else None


def jobs(candidate: str, harness: str, checkout: str, prefix: str, blocks_per_round: int, cases: str | None,
         base: str = BASE, main: str = MAIN, max_gb: float = 24, checks: bool = True,
         big_gb: float | None = 28, measure: dict | None = None, batch: bool = False,
         layouts: int = 1, catalog: list[dict] | None = None) -> dict[str, dict]:
    """The gate's fleet jobs, one set: its timed part (the cases up to `max_gb`) and its big part
    (those up to `big_gb`), each one owlrs.timed job on a timing host the fleet picks by capability
    (TIMING_ROLES, Linux: ELF binaries, never native Windows; the memory; the data its cases read):
    each builds every side and prepares the data there first, then times round 1's
    blocks alone and reports; and the W3C suites and the guards on any test host. With `measure` the
    k-th block runs the cases needing k blocks; blocks rotate through `layouts` builds. Each job's
    `est_min` is its planned minutes (plan_minutes())."""
    catalog = cases_registry.load() if catalog is None else catalog
    selection = cases.split(",") if cases else None
    run, _, elsewhere = plan(catalog, max_gb, selection, 0, big_gb)
    by_name = {c["name"]: c for c in catalog}
    # Each part's mutable gate state (release-gate.sh GATE: its plan, data view, passes) is the
    # request's and the part's own: placed by capability, two requests at one candidate, or a part and
    # the checks, may meet on one host (review r1 R1-F3). The shared binaries and data caches stay
    # shared (GATE_BIN, SPARQL_DATA).
    request = json.dumps([candidate, harness, base, main, cases, blocks_per_round, max_gb, big_gb, layouts, batch,
                          prefix])
    gate_name = f"gate-v2-{candidate[:8]}-{hashlib.sha256(request.encode()).hexdigest()[:8]}"
    env = {"GATE_NAME": gate_name, "GATE_MAX_GB": str(max_gb), "NRESE_QUIET_SLOT": "0",
           "GATE_MIN_BLOCKS": str(blocks_per_round), **({"GATE_CASES": cases} if cases else {}),
           **({"GATE_BIG_GB": str(big_gb)} if big_gb else {}),
           **({"GATE_BATCH": "1"} if batch else {}), **({"GATE_LAYOUTS": str(layouts)} if layouts > 1 else {})}
    source = [{"name": "checkout", "source": {"git": checkout, "rev": checkout_rev(checkout, candidate, harness)}}]
    gate_ = ["bash", "scripts/release-gate.sh"]
    revs = {"base": base, "main": main, "cand": candidate}
    revisions = list(dict.fromkeys([base, main, candidate, harness]))
    inputs = source + side_inputs(checkout, source[0]["source"]["rev"], revisions)
    # Every build and the data's preparation in one first step, exclusive like every step: the fleet
    # keeps an exclusive job's host only before an exclusive step and while the host takes exclusive
    # work, so the part starts only where it can time and every step runs where these binaries are
    # (Sol r1 S1; 2 Nov 2026: an untimed first step built on a host outside its timing window).
    builds = [["bash", "-c", 'set -e; harness=$1; shift; for rev in "$@"; do bash scripts/release-gate.sh build "$rev"; '
               'done; bash scripts/release-gate.sh prepare "$harness"', "prepare", harness, *revisions]]
    # the sides timed (a main that is the base isn't timed again)
    sides = 2 + (main != base)
    # Whole Latin squares (whole_squares()): every timed side opens a case's blocks alike.
    env["GATE_SQUARE"] = str(sides)
    blocks_per_round = whole_squares(blocks_per_round, sides)
    env["GATE_MIN_BLOCKS"] = str(blocks_per_round)
    need = {c: whole_squares(int(e.get("blocks", blocks_per_round)), sides) for c, e in (measure or {}).items()}
    count = max([blocks_per_round, *need.values()]) if measure else blocks_per_round
    report = [[*gate_, "report", harness, f"cand={candidate}", f"base={base}", f"main={main}"], [*gate_, "clean"]]

    def part(job_id: str, part_env: dict, planned: list[str], timed_steps: list[list[str]], ram_gb: float,
             block_cases: list[list[str]]) -> dict:
        steps = builds + timed_steps + report
        mine = [by_name[c] for c in planned if c in by_name]
        # Its data's ask, its makers' containers declared, by the harness whose prepare makes the data:
        # data_job(), as abba.py's jobs.
        try:
            requires, data_fields, data_env = data_job(
                mine, {"roles": TIMING_ROLES, "os": "linux", "cores": 16, "ram_gb": ram_gb},
                harness=(checkout, harness))
        except HarnessRefused as refused:
            raise SystemExit(str(refused)) from None
        part_env = {**part_env, **data_env}
        fetched = bench_inputs(mine)
        if fetched:
            part_env = {**part_env, "BENCH_INPUTS_REV": fetched[0]["source"]["rev"]}
        est = plan_minutes(len(revisions), sides, block_cases, measure)
        return {"id": job_id, "group": "release", "kind": "owlrs.timed", "class": "exclusive", **data_fields,
                "priority_class": "merge-gate", "env": part_env, "stall_after_min": 90,
                "est_min": round(est["total_min"]), "_plan": est,
                "steps": steps,
                # Builds ask the part's whole memory: a host too small for its blocks would end the hold
                # after them (Sol r2 S1).
                "step_deadline_min": BUILD_MIN * len(revisions) + 120,
                "requires": requires, "inputs": inputs + fetched}

    timed_steps = [[*gate_, "plan-blocks", harness]] if measure else []
    block_cases = []
    for k in range(1, count + 1):
        timed_steps.append(block_step(gate_, k, harness, block_layout(k, layouts),
                                      f"plan-block-{k}.json" if measure else None, revs))
        block_cases.append([c for c in run if not measure or k == 1 or need.get(c, blocks_per_round) >= k])
    out = {f"{prefix}-timed": part(f"{prefix}-timed", {**env, "GATE_NAME": f"{gate_name}-timed", "GATE_PREFIX": prefix,
                                                           "GATE_RAM_GB": str(max_gb)},
                                   run, timed_steps, max_gb, block_cases)}
    if big_gb and elsewhere:
        # Its own gate directory (GATE_NAME): placed by capability it may share a host with the timed
        # part, whose plan and passes would be overwritten (Sol r1 S3); a confirmation keeps it.
        big_env = {**env, "GATE_MIN_GB": str(max_gb), "GATE_MAX_GB": str(big_gb), "GATE_NAME": f"{gate_name}-big",
                   "GATE_PREFIX": f"{prefix}-big", "GATE_RAM_GB": str(big_gb + 4)}
        big_env.pop("GATE_BIG_GB")
        big = sorted(elsewhere)
        out[f"{prefix}-big"] = part(
            f"{prefix}-big", big_env, big,
            [block_step(gate_, k, harness, block_layout(k, layouts), None, revs) for k in range(1, blocks_per_round + 1)],
            big_gb + 4, [big] * blocks_per_round)
    if checks:
        out[f"{prefix}-checks"] = {
            "id": f"{prefix}-checks", "group": "release", "kind": "owlrs.any", "priority_class": "merge-gate",
            "est_min": 100, "env": {**env, "GATE_NAME": f"{gate_name}-checks", "CARGO_BUILD_JOBS": "16"},
            "steps": [[*gate_, "w3c"], [*gate_, "guards"], [*gate_, "clean"]],
            "requires": {"roles": ["test"], "os": "linux", "cores": 16, "ram_gb": 12},
            "inputs": [{"name": "checkout", "source": {"git": checkout, "rev": candidate}}]}
    return out


def plan_minutes(revisions: int, sides: int, block_cases: list[list[str]], measure: dict | None) -> dict:
    """A timed part's minutes: its builds and data (at most BUILD_MIN a revision, shareable), and
    its blocks alone on its host, each every timed side twice over the block's cases (their
    calibrated seconds a pass, else a minute) plus a pass's own overhead. `guessed`: the cases
    without a calibration."""
    guessed = sorted({c for cases in block_cases for c in cases if case_seconds(measure, c) is None})
    timed = sum(2 * sides * (sum(case_seconds(measure, c) or 60.0 for c in cases) + PASS_OVERHEAD_S) / 60
                for cases in block_cases)
    built = BUILD_MIN * revisions + 10
    return {"build_min": built, "timed_min": round(timed, 1), "total_min": built + timed + 3, "guessed": guessed}


FLEET_DEADLINE_S = 120  # a fleet call (check, submit): stopped past this, its reading then missing


def fleet_call(*args: str) -> tuple[int, str]:
    """The fleet CLI (`fleet` on PATH, else FLEET_BIN) under a deadline that stops it
    (rule.remote-calls-have-deadlines): its exit and output; (-1, why) when it gave no answer, which
    a caller treats as "not checked", never as taken."""
    import shutil
    import subprocess
    binary = os.environ.get("FLEET_BIN") or shutil.which("fleet")
    if not binary:
        return -1, "fleet: not on PATH (set FLEET_BIN)"
    try:
        done = subprocess.run([binary, *args], capture_output=True, text=True, encoding="utf-8",
                              errors="replace", timeout=FLEET_DEADLINE_S)
    except subprocess.TimeoutExpired:
        return -1, f"fleet {args[0]}: no answer in {FLEET_DEADLINE_S} s (stopped)"
    return done.returncode, (done.stdout + done.stderr).strip()


def set_plan(found: dict[str, dict], prefix: str, candidate: str, harness: str, base: str, main: str,
              measure: dict | None) -> str:
    """The gate's plan before anything is queued: each part's cases, blocks, minutes and asks, and
    the whole in host-hours (intent.bench-by-purpose: "estimated time value ... how much it would
    block")."""
    lines = [f"set:{prefix}: cand {candidate[:12]} against base {base[:12]} and main {main[:12]}, harness {harness[:12]}"]
    total = exclusive = 0.0
    for job in found.values():
        asks = job["requires"]
        blocks_ = sum(1 for s in job["steps"] if len(s) > 2 and s[2] == "block")
        timed = job["kind"] == "owlrs.timed"
        lines.append(f"  {job['id']}: {job['est_min']} min" + (f", {blocks_} blocks" if timed else "")
                     + f"; asks {job['kind']}" + (f", roles {'+'.join(asks['roles'])}" if asks.get("roles") else "")
                     + f", {asks.get('os', 'any OS')}, {asks['cores']} cores, {asks['ram_gb']} GB"
                     + (f", has {'+'.join(asks['has'])}" if asks.get("has") else ""))
        total += job["est_min"]
        exclusive += job["est_min"] if timed else 0  # a timed part holds its host from its first step
    guessed = sorted({c for job in found.values() for c in (job.get("_plan") or {}).get("guessed", [])})
    if guessed:
        lines.append(f"  not calibrated (benches/fast/measure.toml at the harness), a minute a pass assumed: {len(guessed)} "
                     f"cases: {', '.join(guessed)}")
    lines.append(f"  estimate: {total / 60:.1f} host-hours in all, {exclusive / 60:.1f} of them timing hosts held "
                 "alone (builds included: a build of the same identity is reused); queue time and confirmation "
                 "rounds come on top")
    return "\n".join(lines)


# --- rounds and the verdict (research round 8: prespecified rounds, Holm over the family) ------




def rounds_of(reports: list[dict]) -> list[list[dict]]:
    """The reports of each timed part, first round first (`context.round`). A part is its job's
    prefix (GATE_PREFIX: the timed part's, the big part's; a confirmation keeps its part's), else its
    host: placed by capability, two parts may run on one host, and they aren't rounds of each other."""
    by_part: dict[str, list[dict]] = {}
    for r in reports:
        ctx = r.get("context") or {}
        part = ctx.get("prefix") or ctx.get("host") or ",".join(map(str, r.get("host", [])))
        by_part.setdefault(part, []).append(r)
    return [sorted(rs, key=lambda r: (r.get("context") or {}).get("round", 0)) for rs in by_part.values()]


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
    if not ctx.get("host"):
        raise SystemExit(f"{ctx.get('prefix') or 'a timed part'}: its report names no host (GATE_HOST, else FLEET_HOST): "
                         "a confirmation runs where round 1 ran (its binaries, its floor), so none is written")
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
            "requires": {"hosts": [ctx["host"]], "roles": TIMING_ROLES, "os": "linux", "cores": 16,
                         "ram_gb": ctx.get("ram_gb", 24)},
            "pin_reason": "a confirmation round runs on its first round's host: its builds there, its floor, its rounds combined",
            "inputs": [{"name": "checkout", "source": {"git": checkout,
                                                       "rev": checkout_rev(checkout, revs["cand"], harness)}}]}


def known_lines(timed: list[dict], policy: Policy) -> tuple[list[str], list[str]]:
    """The candidate's known issues under the owner's policy: the verdict's lines, and what blocks.
    A wrong answer passes only through an exception naming its case and outcome; a declared limit
    is named; an exception nothing used is named too."""
    known: dict[str, set[str]] = {}
    for report in timed:
        for case, statuses in report.get("known_issues", {}).items():
            known.setdefault(case, set()).update(s.strip() for s in statuses.split(","))
    lines, blocking, used = [], [], set()
    for case in sorted(known):
        for status in sorted(known[case]):
            if status not in BAR_OUTCOMES:
                lines.append(f"- known issue: {case}: {status} (declared; it blocks where a baseline completes)")
                continue
            e = next((e for e in policy.exceptions if (e["case"], e["outcome"]) == (case, status)), None)
            if e is None:
                blocking.append(f"{case}: {status}, no owner exception ({POLICY_FILE} at the candidate)")
            else:
                used.add((case, status))
                lines.append(f"- owner exception: {case} {status}, approved {e['approved']} by {e['by']} "
                             f"({e['source']}): \"{e['quote']}\"")
    lines += [f"- owner exception not used: {e['case']} {e['outcome']} (the candidate doesn't show it)"
              for e in policy.exceptions if (e["case"], e["outcome"]) not in used]
    return lines, blocking


def verdict(timed: list[dict], w3c: dict, guards: dict, policy: Policy = NO_POLICY,
            evidence: list[str] | None = None) -> tuple[bool, str]:
    """Every part together: the family over every timed part decided (decide()), the other blocks
    of every round, the cases one part left to another, the W3C counts and the guards, under the
    owner's POLICY (none: nothing the bar forbids is accepted). EVIDENCE: what binding the parts to
    the candidate found (release_verdict(), the command's only way here)."""
    parts = rounds_of(timed)
    d = decide(parts)
    lines = ["# Release verdict", "",
             f"Family: {d['m']} hypotheses declared (case x series or metric x baseline, every timed part); "
             f"Holm at {gatestats.ALPHA} over p = min(max(p1, p2) / 0.9, max(p2, p3) / 0.1)"
             + (f"; {len(d['outside'])} more decided below the counter's resolution, outside it" if d["outside"] else "")
             + ".", ""]
    lines += [f"- decided below the counter's step: {h.split('|')[1]} against {h.split('|')[0]}: {h.split('|', 2)[2]}: "
              f"{', '.join(s)}" for h, s in d["outside"].items()]
    evidence = list(evidence or [])
    lines += [f"- **not the candidate's**: {e}" for e in evidence]
    lines += [f"- **policy**: {POLICY_FILE}: {e}" for e in policy.errors]
    known, wrong = known_lines(timed, policy)
    lines += [f"- **wrong answer**: {w}" for w in wrong] + known
    errors = [e for rounds in parts for r in rounds for e in r["errors"]]
    other = [b for rounds in parts for r in rounds for b in r.get("other_blocking", [])]
    diagnostic = any(r["diagnostic"] for rounds in parts for r in rounds)
    blocking_losses = []
    for h in d["losses"]:
        side, case, dim = h.split("|")
        ps = d["rounds"].get(h, [])
        # The approval must cover the loss in every round that shows it (its worse end within the bound,
        # its win shown there): one round within the bound never waives another beyond it.
        covers = [accepted(policy.tradeoffs, side, case, dim, r, report.get("comparisons", {}))
                  for rounds in parts for report in rounds
                  if (r := report.get("comparisons", {}).get(side, {}).get(case, {}).get("dimensions", {}).get(dim))
                  is not None]
        accepted_ = covers[0] if covers and all(covers) else None
        text = f"{case} against {side}: {dim}: p per round {ps}, combined {d['combined'][h]:.3g}"
        if accepted_:
            lines.append(f"- accepted trade-off: {text} (at most {accepted_['max_ratio']}, approved "
                         f"{accepted_['approved']} by {accepted_['by']}, {accepted_['source']})")
        else:
            blocking_losses.append(text)
    lines += [f"- **confirmed loss**: {t}" for t in blocking_losses]
    # A cost appearing from below the counter's step, in any round: no statistics to wait for.
    blocking_emerged = []
    for e in (e for rounds in parts for r in rounds for e in r.get("emerged", [])):
        text = (f"{e['case']} against {e['side']}: {e['dimension']}: {fmt(e['base'])} -> {fmt(e['new'])} {e['unit']} "
                f"(below its step of {e['step']:g} on {e['side']} in every block)")
        if t := emerged_accepted(policy, e):
            lines.append(f"- accepted emerged cost: {text} (at most {t['max']}, approved {t['approved']} by {t['by']}, "
                         f"{t['source']})")
        else:
            blocking_emerged.append(text)
    lines += [f"- **emerged**: {t}" for t in blocking_emerged]
    lines += [f"- **pending** (a round that could reject it hasn't run): {h}" for h in d["pending"]]
    # A baseline absent by product (the harness's catalogue, in each report) declared no hypothesis;
    # one the owner's absence entry names (the candidate's policy) is not compared by his decision;
    # any other hypothesis not compared blocks.
    absent = {(side, case): what for rounds in parts for r in rounds
              for side, cases_ in (r.get("absent") or {}).items() for case, what in cases_.items()}
    decided, not_compared = {}, {}
    for h, why in d["not_compared"].items():
        side, case, _ = h.split("|", 2)
        if e := absence_of(policy, side, case):
            decided[(side, case)] = e
        else:
            not_compared[h] = why
    lines += [f"- not compared: {case} against {side}: {side} has no {what}"
              for (side, case), what in sorted(absent.items())]
    lines += [f"- not compared by the owner's decision: {case} against {side}: {e['reason']} (port: {e['port']}; "
              f"approved {e['approved']} by {e['by']}, {e['source']})" for (side, case), e in sorted(decided.items())]
    lines += [f"- **not compared**: {h.split('|')[1]} against {h.split('|')[0]}: {h.split('|', 2)[2]}: {why}"
              for h, why in not_compared.items()]
    if absent or decided:
        counts = []
        for side in sorted({h.split("|", 1)[0] for h in d["family"]} | {s for s, _ in absent}):
            declared = {h.split("|")[1] for h in d["family"] if h.split("|", 1)[0] == side}
            blocked = {h.split("|")[1] for h in not_compared if h.split("|", 1)[0] == side}
            by_owner = {c for s, c in decided if s == side}
            # Compared: a round tested one of its hypotheses (a p-value), none left blocking or waived.
            tested = {h.split("|")[1] for h in d["family"] if h.split("|", 1)[0] == side
                      and any(p is not None for p in d["rounds"].get(h, []))}
            compared = tested - blocked - by_owner
            uncompared = declared - compared - by_owner
            counts.append(f"{side}: {len(compared)} compared, "
                          f"{sum(s == side for s, _ in absent)} absent by product, {len(by_owner)} absent by the "
                          f"owner's decision" + (f", {len(uncompared)} not compared" if uncompared else ""))
        lines.append("- cases per baseline: " + "; ".join(counts))
    lines += [f"- {b}" for b in other] + [f"- pass error: {e}" for e in errors]
    covered = {c for t in timed for c in t.get("planned", [])}
    missing = sorted({c for t in timed for c in t.get("elsewhere", {})} - covered)
    if missing:
        lines.append(f"- NOT timed anywhere: {', '.join(missing)} (left to another part, which didn't run them)")
    # A timed report that planned no case (one written before gate() refused it) timed nothing.
    empty = [t for t in timed if not t.get("planned")]
    lines += [f"- **nothing timed**: round {t.get('context', {}).get('round', '?')} planned no case" for t in empty]
    # A check passes only by saying so exactly (True, no errors); a "false" or a missing field is no pass.
    w3c_ok, guards_ok = (part.get("passed") is True and not part.get("errors") for part in (w3c, guards))
    lines.append(f"- W3C: {'passed' if w3c_ok else 'NOT passed'} "
                 f"({'; '.join(map(str, w3c.get('errors') or [])) or 'every suite as required'})")
    lines.append(f"- §0 guards: {'passed' if guards_ok else 'NOT passed'} "
                 f"({'; '.join(map(str, (guards.get('errors') or [])[:5])) or str(guards.get('guards')) + ' guards ran and passed'})")
    passed = (not blocking_losses and not blocking_emerged and not d["pending"] and not not_compared and not other
              and not errors
              and not diagnostic and not missing and not empty and w3c_ok and guards_ok
              and not evidence and not policy.errors and not wrong)
    lines += ["", f"**{'PASSED' if passed else 'NOT PASSED'}**"]
    return passed, "\n".join(lines) + "\n"


class Candidate(NamedTuple):
    """The candidate a verdict is for: its commit and tree, and the tree of any revision as the
    repository it was named in holds it (None: not there)."""
    commit: str
    tree: str
    tree_of: Callable[[str], "str | None"]


CONTRACT_FILES = ("benches/fast/w3c-expected.toml", "docs/design/performance.md")


class Contract(NamedTuple):
    """What the candidate's tree requires of its checks (CONTRACT_FILES, read at the candidate's
    commit): the W3C suites' counts and the kits that report them, and the §0 guards; and what's
    wrong with it (nothing passes while anything is)."""
    suites: dict
    kits: list[Kit]
    guards: list[str]
    errors: list[str]


def load_contract(w3c_expected: str | None, performance_md: str | None) -> Contract:
    """The contract from the two files' texts (None: not in the candidate's tree)."""
    errors, suites, kits, guards = [], {}, [], []
    if w3c_expected is None:
        errors.append(f"{CONTRACT_FILES[0]}: not in the candidate's tree")
    else:
        try:
            suites, kits, bad = load_w3c_expected(w3c_expected)
        except tomllib.TOMLDecodeError as error:
            bad = [f"not TOML: {error}"]
        errors += [f"{CONTRACT_FILES[0]}: {b}" for b in bad]
    if performance_md is None or "## 0." not in performance_md:
        errors.append(f"{CONTRACT_FILES[1]}: no §0 in the candidate's tree")
    elif not (guards := guard_names(performance_md)):
        errors.append(f"{CONTRACT_FILES[1]}: §0 names no guard")
    return Contract(suites, kits, guards, errors)


def claim_errors(kind: str, result: dict) -> list[str]:
    """A result's own claim: passed, exactly true, with no errors; what it says otherwise."""
    if result.get("passed") is True and result.get("errors") == []:
        return []
    return [f"{kind}: its result doesn't say passed with no errors "
            f"(passed {result.get('passed')!r}; {'; '.join(map(str, result.get('errors') or [])) or 'no errors listed'})"]


def w3c_result_errors(result: dict, receipt: CheckReceipt, contract: Contract) -> list[str]:
    """A W3C result against the contract: every declared kit launched once and run whole, every
    suite's counts as required (the producer's own checks, outcome_errors() and w3c_check())."""
    counts = result.get("counts")
    return (outcome_errors(receipt.outcomes, [kit.name for kit in contract.kits])
            + (w3c_check(counts, contract.suites) if isinstance(counts, dict) else ["w3c: its result holds no counts"]))


def guards_result_errors(result: dict, receipt: CheckReceipt, contract: Contract) -> list[str]:
    """A guards result against the contract: the one test run launched and run whole, and every §0
    guard among its results, each ok."""
    results = result.get("results") if isinstance(result.get("results"), dict) else {}
    errors = outcome_errors(receipt.outcomes, ["guards"])
    for name in contract.guards:
        got = results.get(name)
        if not (isinstance(got, list) and got):
            errors.append(f"{name}: no test of that name ran (named in performance.md §0)")
        elif any(s != "ok" for s in got):
            errors.append(f"{name}: {', '.join(map(str, got))}")
    return errors


RESULT_ERRORS = {"w3c": w3c_result_errors, "guards": guards_result_errors}


def attempt_errors(kind: str, result: dict | None, newest: tuple[dict | None, str | None],
                   candidate: Candidate) -> list[str]:
    """A check's result against its step's newest attempt record (newest_attempt()): one there, of the
    candidate's tree, and the result (and its receipt) that attempt's."""
    record, wrong = newest
    if wrong:
        return [f"{kind}: {wrong}"]
    if record is None:
        return [f"{kind}: no attempt record ({kind}.attempt.*.json beside its result): no step stands for it"]
    if result is None:
        return [f"{kind}: no result for its latest attempt {record['attempt']}"]
    errors = []
    carried = {result.get("attempt"), (result.get("receipt") or {}).get("attempt")}
    if carried != {record["attempt"]}:
        errors.append(f"{kind}: stale result from an earlier attempt (it carries "
                      f"{', '.join(sorted(map(str, carried)))}; the latest attempt is {record['attempt']})")
    if record["tree"] != candidate.tree:
        errors.append(f"{kind}: its latest attempt ran tree {record['tree']}, not the candidate's {candidate.tree}")
    return errors


def evidence_errors(timed: list[dict], checks: dict[str, dict | None], candidate: Candidate,
                    contract: Contract, attempts: dict[str, tuple[dict | None, str | None]]) -> list[str]:
    """What isn't evidence of the candidate's tree: a timed part that timed another tree, a check
    without a receipt (one from before receipts, or not the gate's), with a receipt of another tree,
    of a checkout with changes, or whose commit isn't of the tree it names; a check that isn't its
    step's latest attempt (ATTEMPTS: each kind's record, attempt_errors()); and a check that isn't
    the run the candidate's CONTRACT requires (its own `passed` decides nothing). A tree, not a
    commit: the same tree under a merge commit is the same candidate
    (rule.owl-rs-results-keyed-by-tree)."""
    errors = [f"the candidate's contract: {e}" for e in contract.errors]
    for kind, result in checks.items():
        errors += attempt_errors(kind, result, attempts.get(kind, (None, None)), candidate)
    checks = {kind: result for kind, result in checks.items() if result is not None}
    for report in timed:
        rev = str((report.get("candidate") or {}).get("revision") or "")
        tree = candidate.tree_of(rev) if rev else None
        if tree != candidate.tree:
            c = report.get("context") or {}
            errors.append(f"timed part {c.get('prefix') or '?'} on {c.get('host') or '?'}, round {c.get('round', '?')}: "
                          f"timed {rev[:12] or 'no candidate'} (tree {tree or 'unknown here'}), "
                          f"not the candidate's tree {candidate.tree}")
    for kind, result in checks.items():
        r = CheckReceipt.of(result.get("receipt"))
        if r is None:
            errors.append(f"{kind}: no receipt (a result from before receipts, or not the gate's): "
                          "it can't be bound to the candidate")
            continue
        if r.kind != kind:
            errors.append(f"{kind}: the receipt of a {r.kind} check")
        if not (r.identity and r.host and r.outcomes):
            errors.append(f"{kind}: its receipt names no build identity, host or launched unit")
        if r.dirty:
            errors.append(f"{kind}: ran a checkout whose tracked files had changes")
        if r.tree != candidate.tree:
            errors.append(f"{kind}: ran tree {r.tree or 'none'}, not the candidate's {candidate.tree}")
        elif candidate.tree_of(r.commit) != r.tree:
            errors.append(f"{kind}: its commit {r.commit[:12] or 'none'} isn't of tree {r.tree} here")
        errors += claim_errors(kind, result)
        if not contract.errors:
            errors += [f"{kind}: {e}" for e in RESULT_ERRORS[kind](result, r, contract)]
    return errors


def release_verdict(timed: list[dict], w3c: dict | None, guards: dict | None, candidate: Candidate,
                    policy: Policy, contract: Contract,
                    attempts: dict[str, tuple[dict | None, str | None]]) -> tuple[bool, str]:
    """`verdict`: the parts bound to the candidate, each check to its step's latest attempt (ATTEMPTS)
    and to its tree's contract (evidence_errors()), under its tree's policy. A check without a result
    (None) is no pass."""
    evidence = evidence_errors(timed, {"w3c": w3c, "guards": guards}, candidate, contract, attempts)
    passed, text = verdict(timed, w3c or {}, guards or {}, policy, evidence)
    head, rest = text.split("\n", 1)
    return passed, f"{head}\n\nCandidate: {candidate.commit} (tree {candidate.tree})\n{rest}"


def git_line(checkout: str, *args: str) -> str | None:
    """A git command's output in CHECKOUT, as UTF-8 (the repository's files are; never the host's
    locale codec), stripped; None when it fails."""
    done = subprocess.run(["git", "-C", checkout, *args], capture_output=True, encoding="utf-8", timeout=60)
    return done.stdout.strip() if done.returncode == 0 else None


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
    s.add_argument("--big-host", default="the big part", help="its name in the plan's reasons")
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
    s = sub.add_parser("w3c", help="the W3C kits' outcomes and their suites' counts (release-gate.sh w3c)")
    s.add_argument("logs", type=Path, nargs="?", help="the kits' logs and tables, and receipt.json")
    s.add_argument("--expected", type=Path, required=True, help="the kits and the suites' counts (w3c-expected.toml)")
    s.add_argument("--list-kits", action="store_true", help="the declared kits, in launch order")
    s.add_argument("--run", action="store_true", help="launch every declared kit in this checkout first, each "
                                                      "outcome recorded in LOGS/receipt.json")
    s.add_argument("--identity", help="with --run: the build identity (scripts/lib/gate-key.sh gate_identity)")
    s.add_argument("--attempt", help="with --run: its step's attempt (release_gate.py attempt), in result and receipt")
    s.add_argument("--json", type=Path)
    s = sub.add_parser("guards", help="the §0 guards, run (release-gate.sh guards)")
    s.add_argument("--names", type=Path, help="one guard per line; default: performance.md §0's")
    s.add_argument("--list-names", action="store_true")
    s.add_argument("--log", type=Path)
    s.add_argument("--run", action="store_true", help="run the guards' tests in this checkout first, into --log")
    s.add_argument("--identity", help="with --run: the build identity (scripts/lib/gate-key.sh gate_identity)")
    s.add_argument("--attempt", help="with --run: its step's attempt (release_gate.py attempt), in result and receipt")
    s.add_argument("--json", type=Path)
    s = sub.add_parser("attempt", help="a new attempt of a check's step, recorded beside its result; prints its id")
    s.add_argument("kind", choices=("w3c", "guards"))
    s.add_argument("where", type=Path, help="the step's results directory (KIND.attempt.SEQUENCE.json)")
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
    s.add_argument("--big-gb", type=float, default=28,
                   help="the cases above --max-gb up to this run in the big part (a timing host with that memory)")
    s.add_argument("--no-big", action="store_true", help="without the big part")
    s.add_argument("--plan", action="store_true", help="the plan and fleet's check of the set only: nothing queued")
    s.add_argument("--submit", action="store_true", help="submit the set (fleet submit) once fleet's check takes it")
    s.add_argument("--layouts", type=int, default=1, help="builds a side in different layouts (GATE_LAYOUTS)")
    s.add_argument("--out", type=Path, default=Path("."))
    s = sub.add_parser("data", help="what this host holds of the cases' frozen inputs (exit 1: something missing)")
    s.add_argument("--cases", help="comma-separated; default every registry case")
    s.add_argument("--make", action="store_true", help="make what benches/sparql/prepare.sh can into SPARQL_DATA first")
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
    s = sub.add_parser("verdict", help="every part together, for one candidate (release-gate.sh verdict)")
    s.add_argument("--candidate", required=True, help="the candidate: a revision of --checkout; every part must "
                                                      "be its tree's, and its tree's policy applies")
    s.add_argument("--checkout", default=ROOT.as_posix(), help="the repository the candidate is named in")
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
    if args.command == "data":
        wanted = set(args.cases.split(",")) if args.cases else None
        lines, ok = data_state([c for c in cases_registry.load() if wanted is None or c["name"] in wanted], args.make)
        print("\n".join(lines) or "no frozen inputs")
        return 0 if ok else 1
    if args.command == "plan":
        catalog = cases_registry.load()
        run, not_run, elsewhere = plan(catalog, args.max_gb, args.cases.split(",") if args.cases else None,
                                       args.min_gb, args.big_gb, args.big_host)
        family = family_of(catalog, run, tuple(b for b in args.baselines.split(",") if b))
        print(json.dumps({"run": run, "not_run": not_run, "elsewhere": elsewhere, "family": family}, indent=1))
        return 0
    if args.command == "report":
        sides = pairs(args.side, "--side")
        passes = load_passes(args.passes)
        family = json.loads(args.family.read_text(encoding="utf-8")).get("family") if args.family else None
        context = {k: v for k, v in pairs(args.context, "--context").items()}
        floor_file = args.floor or (FLOORS / f"{context['host']}.toml" if context.get("host") else None)
        summary = gate(passes, sides, args.candidate, [s for s in args.require.split(",") if s], args.harness_revision,
                       args.min_blocks, args.diagnostic, pairs(args.not_run, "--not-run"),
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
    if args.command in ("w3c", "guards") and args.run:
        # A run's earlier result (and the W3C receipt) goes before anything that can fail, its setup
        # included: a run stopped at any point leaves none, never one that speaks for what it didn't run.
        # Its own is published only once it is whole (publish()).
        for earlier in (args.json, args.logs / "receipt.json" if args.command == "w3c" and args.logs else None):
            if earlier is None:
                continue
            try:
                earlier.unlink(missing_ok=True)
            except OSError as error:
                # Not a judged result (0 passed, 1 not passed: the step keeps those): a crash, which no
                # step reads as one (fix check r1: an exit 1 beside the earlier file passed it).
                print(f"{args.command}: the earlier result {earlier} can't be removed ({error}): no run",
                      file=sys.stderr)
                return CANT_INVALIDATE
    if args.command == "attempt":
        args.where.mkdir(parents=True, exist_ok=True)
        print(attempt_record(args.kind, args.where))
        return 0
    if args.command == "w3c":
        expected, kits, bad = load_w3c_expected(args.expected.read_text(encoding="utf-8"))
        if bad:
            raise SystemExit(f"{args.expected}: " + "; ".join(bad))
        if args.list_kits:
            print("\n".join(kit.name for kit in kits))
            return 0
        if args.logs is None or (args.run and not (args.identity and args.attempt)):
            p.error("w3c: LOGDIR, and with --run its --identity and --attempt")
        recorded = args.logs / "receipt.json"
        if args.run:
            receipt = run_w3c(args.logs, kits, args.identity, args.attempt)
            publish(recorded, receipt.as_json())
        else:
            receipt = CheckReceipt.of(json.loads(recorded.read_text(encoding="utf-8"))) if recorded.is_file() else None
        counts = kit_counts(args.logs, kits)
        errors = outcome_errors(receipt.outcomes if receipt else (), [kit.name for kit in kits]) + w3c_check(counts, expected)
        # Each recorded launch's log is the one it wrote (a re-check reads logs a later run may have replaced).
        logs_of = {kit.name: args.logs / f"{kit.test}.log" for kit in kits}
        errors += [f"{o.unit}: its log isn't the one its launch recorded" for o in (receipt.outcomes if receipt else ())
                   if o.unit in logs_of and (not logs_of[o.unit].is_file()
                                             or hashlib.sha256(logs_of[o.unit].read_bytes()).hexdigest() != o.log_sha256)]
        out = {"counts": counts, "expected": expected, "errors": errors, "passed": not errors,
               **({"attempt": receipt.attempt, "receipt": receipt.as_json()} if receipt else {})}
        print(json.dumps(out, indent=1))
        if args.json:
            publish(args.json, out)
        return 0 if not errors else 1
    if args.command == "guards":
        names = (args.names.read_text(encoding="utf-8").split() if args.names else
                 guard_names((ROOT / "docs/design/performance.md").read_text(encoding="utf-8")))
        if args.list_names:
            print("\n".join(names))
            return 0
        if args.log is None or (args.run and not (args.identity and args.attempt)):
            p.error("guards: --log, and with --run its --identity and --attempt")
        receipt = None
        if args.run:
            receipt = checkout_receipt("guards", args.identity, args.attempt, [run_unit("guards", [
                "bash", "scripts/cargo-guarded.sh", "test", "--release", "--locked", "--workspace", "--no-fail-fast",
                "--lib", "--bins", "--tests", "--", *names], args.log)])
        errors, results = guards_check(names, args.log.read_text(encoding="utf-8", errors="replace")
                                       if args.log.is_file() else "")
        errors = outcome_errors(receipt.outcomes if receipt else (), ["guards"]) + errors
        out = {"guards": len(names), "results": results, "errors": errors, "passed": not errors,
               **({"attempt": receipt.attempt, "receipt": receipt.as_json()} if receipt else {})}
        print("\n".join(errors) or f"all {len(names)} guards ran and passed")
        if args.json:
            publish(args.json, out)
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
        prefix = args.id or f"rg-{candidate[:8]}"
        found = jobs(candidate, harness, args.checkout, prefix,
                     args.blocks, args.cases, rev(args.base), rev(args.main), max_gb=args.max_gb,
                     checks=not args.no_checks, big_gb=None if args.no_big else args.big_gb,
                     measure=measure, batch=batch, layouts=args.layouts,
                     catalog=cases_registry.load_at(args.checkout, harness))
        args.out.mkdir(parents=True, exist_ok=True)
        path = args.out / f"{prefix}.set.toml"
        path.write_bytes(toml_set({"id": prefix, "fail": "continue"}, list(found.values())).encode("utf-8"))
        print(set_plan(found, prefix, candidate, harness, rev(args.base), rev(args.main), measure))
        print(f"  set file: {path}")
        if not (args.plan or args.submit):
            print(f"  not checked or submitted: --plan asks fleet where it can run, --submit submits it")
            return 0
        code, said = fleet_call("check", str(path))
        print("  fleet check" + (f" (exit {code})" if code else "") + ":\n    " + "\n    ".join(said.splitlines() or ["-"]))
        if not args.submit:
            # A plan the fleet refuses exits as refused (abba.py run --plan's sibling, review
            # release-not-compared r1 OUT1).
            print(f"not submitted (--plan): fleet submit {path}, or this command with --submit"
                  + ("; fleet check refuses it (above)" if code else ""))
            return 1 if code else 0
        if code != 0:
            print("not submitted: fleet check didn't take the set (above)", file=sys.stderr)
            return 1
        code, said = fleet_call("submit", str(path))
        print(said)
        if code == 0:
            print(f"wait: fleet wait set:{prefix}; then fetch each part (fleet results ID --to DIR) and "
                  f"release-gate.sh verdict {candidate} TIMED_DIR... CHECKS_DIR")
        return 0 if code == 0 else 1
    commit = git_line(args.checkout, "rev-parse", "--verify", "--quiet", f"{args.candidate}^{{commit}}")
    if not commit:
        p.error(f"verdict: {args.candidate} is no commit of {args.checkout}")
    # The policy as the candidate's tree holds it; a tree without the file accepts nothing.
    policy = load_policy(git_line(args.checkout, "show", f"{commit}:{POLICY_FILE}") or "")
    candidate = Candidate(commit, git_line(args.checkout, "rev-parse", f"{commit}^{{tree}}") or "",
                          lambda rev: git_line(args.checkout, "rev-parse", "--verify", "--quiet", f"{rev}^{{tree}}"))
    # What the candidate's tree requires of its checks: its kits, suites and guards.
    contract = load_contract(*(git_line(args.checkout, "show", f"{commit}:{rel}") for rel in CONTRACT_FILES))
    timed = [json.loads(path.read_text(encoding="utf-8")) for path in args.timed]
    # Each check's result (None: there is none, as when its producer never published) and its step's
    # newest attempt record beside it (attempt_errors()).
    checks, attempts = {}, {}
    for kind, path in (("w3c", args.w3c), ("guards", args.guards)):
        checks[kind] = json.loads(path.read_text(encoding="utf-8")) if path.is_file() else None
        attempts[kind] = newest_attempt(path.parent, kind)
    if not all(isinstance(part, dict) for part in [*timed, *(c for c in checks.values() if c is not None)]):
        p.error("verdict: every part is a JSON object (release-gate.json, w3c.json, guards.json)")
    passed, text = release_verdict(timed, checks["w3c"], checks["guards"], candidate, policy, contract, attempts)
    print(text)
    if args.out:
        args.out.write_text(text, encoding="utf-8")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
