#!/usr/bin/env python3
"""The release gate's report (scripts/release-gate.sh; the owner, 13 Oct 2026): the integrated
release candidate against each baseline (the pre-regression 0f46563 and the current main), all
fast cases, from one host's one-repetition passes pooled per side. Nothing is hidden below a
threshold:

- every failed quality check, per side (a case that fails on a baseline isn't compared: said);
- every slowdown, the small ones too: the ratio candidate/baseline of the summed medians with
  its bootstrap 95 % interval, and the verdict compare would give (regression: beyond
  max(10 %, the spread) and 2 ms with the interval above 1);
- memory by the cgroup peak where both sides have one (independent of the harness; the owner,
  14 Oct 2026), else by the process's lifetime peak where both sides measured the same phases,
  a growth beyond max(10 %, 64 MiB) said, an unstable peak said, the lifetime peak shown beside;
- cases that couldn't run on the host (--not-run): said, never passes; the gate waits for them;
- work counters that grew beyond 2 %;
- the drift of each baseline between its first and last passes (the A-B-B-A symmetry cancels
  a linear drift; a large one is said).

    python benches/fast/release_gate.py --candidate C1.json,C2.json,... --baseline NAME=B1.json,B2.json,...
                                        [--baseline NAME=...] [--out REPORT.md] [--json SUMMARY.json]

Exit status 0 only for a complete four-pass campaign with no regression or failed candidate
check. --diagnostic-passes N compares fewer passes without granting release acceptance.
"""
from __future__ import annotations

import argparse
import json
import math
import statistics
import sys
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parents[0] / "suite"))
from suitekit import cases as cases_registry, manifest  # noqa: E402
from fast import case_manifest, select  # noqa: E402
from suitekit.compare import (CACHE_KINDS, UNSTABLE_PEAK, bootstrap_ratio, counter_changes,  # noqa: E402
                              kind_of, load_report, peak_spread, pooled, spread, valid_series, complete_timing)
from suitekit.hosts import run_identity  # noqa: E402

THRESHOLD = 0.10
MIN_MS = 2.0
MEMORY_GROWTH = 0.10
MEMORY_FLOOR_MIB = 64
CAMPAIGN_PASSES = 4  # base, main, cand, cand, main, base, twice: four passes per side.


def expectation(root: Path, harness_revision: str, engine_revision: str, target: Path, selection=None) -> dict:
    """Independent current source and verified build inputs, never inferred from saved reports."""
    source = manifest.git(root)
    build = manifest.binaries(target, engine_revision)
    if not harness_revision or not engine_revision or source["commit"] != harness_revision or source["dirty"] != 0 or not build["verified"]:
        raise ValueError(f"expected clean harness or verified engine build unavailable: source={source}, build={build}")
    catalog = cases_registry.load(root / "benches/cases")
    plan = select(catalog, argparse.Namespace(cases=selection, areas=None))
    return {"harness_revision": harness_revision, "engine_revision": engine_revision,
            "sha256": build["sha256"], "case_manifest": case_manifest(catalog, root),
            "planned_cases": [c["name"] for c in plan], "all_cases": [c["name"] for c in catalog],
            "outcomes": {c["name"]: c.get("outcome", "ok") for c in catalog}}


def pass_errors(report: dict, planned: list[str] | None = None, expected: dict | None = None) -> list[str]:
    """A completed one-repetition pass contains exactly its declared case plan."""
    plan = report.get("planned_cases") or []
    errors = []
    if not report.get("finished"):
        errors.append("completion marker missing")
    if not plan or len(plan) != len(set(plan)):
        errors.append("case plan missing, empty or duplicated")
    if planned is not None and set(plan) != set(planned):
        errors.append("case plan differs between passes/sides")
    if report.get("reps") != 1:
        errors.append("a campaign pass must declare one repetition")
    wanted = Counter((case, 1) for case in plan)
    actual = Counter((r.get("case"), r.get("rep")) for r in report.get("records", []))
    if actual != wanted:
        errors.append(f"case/repetition coverage: missing {dict(wanted - actual)}, extra/duplicate {dict(actual - wanted)}")
    if expected is not None:
        if any(report.get(k) != expected[k] for k in ("planned_cases", "all_cases", "case_manifest")):
            errors.append("current harness manifest/selection differs")
        for key in ("git", "git_finished"):
            source = report.get(key) or {}
            if source.get("commit") != expected["harness_revision"] or source.get("dirty") != 0:
                errors.append(f"{key}: missing, changed or dirty harness provenance")
        for key in ("binaries", "binaries_finished"):
            build = report.get(key) or {}
            if not build.get("verified") or build.get("revision") != expected["engine_revision"] or build.get("sha256") != expected["sha256"]:
                errors.append(f"{key}: missing, changed or unverified engine provenance")
        if any(r.get("expected_outcome", "ok") != expected["outcomes"].get(r.get("case")) for r in report.get("records", [])):
            errors.append("declared outcomes differ from current harness")
    for r in report.get("records", []):
        if r.get("status") == "ok" and r.get("expected_outcome", "ok") == "ok" and all(c.get("ok") for c in r.get("checks", []) + r.get("routes", [])):
            value = r.get("metric")
            if type(value) not in (int, float) or not math.isfinite(value) or value < 0 or not valid_series(r.get("samples")):
                errors.append(f"{r.get('case')}: missing/invalid successful timing evidence")
            intended = expected["case_manifest"]["series"].get(r.get("case")) if expected else None
            if intended is not None and set(r.get("samples") or {}) != set(intended):
                errors.append(f"{r.get('case')}: timing series differ from current harness")
    return errors


def pool(paths: list[Path], expected: dict | None = None) -> dict:
    """One-repetition passes as one report (records renumbered as repetitions 1..n)."""
    reports = [load_report(p) for p in paths]
    if not reports:
        return {"records": [], "reps": 0, "complete_passes": 0, "coverage_errors": ["no pass reports"]}
    revisions = {(r.get("binaries") or {}).get("revision") for r in reports}
    if len(revisions) > 1:
        raise SystemExit(f"passes of different revisions pooled: {sorted(map(str, revisions))}")
    harnesses = {(r.get("git") or {}).get("commit") for r in reports}
    if len(harnesses) > 1:
        raise SystemExit(f"passes of different harness revisions pooled: {sorted(map(str, harnesses))}")
    out = {k: v for k, v in reports[0].items() if k != "records"}
    checks = [pass_errors(r, out.get("planned_cases") or [], expected) for r in reports]
    out["complete_passes"] = sum(not errors for errors in checks)
    out["coverage_errors"] = [f"{p.name}: {why}" for p, errors in zip(paths, checks) for why in errors]
    if len({p.resolve() for p in paths}) != len(paths) or len({r.get("label") for r in reports}) != len(reports):
        out["coverage_errors"].append("duplicate pass paths or labels")
    out["reps"] = len(reports)
    out["records"] = [{**record, "rep": n} for n, r in enumerate(reports, start=1) for record in r["records"]]
    out["revision"] = revisions.pop()
    out["harness_revision"] = harnesses.pop()
    out["passes"] = [p.name for p in paths]
    out["pass_reports"] = reports
    # Where and under what each pass ran (the host's key, its configuration then, the runtime).
    out["identities"] = sorted({json.dumps({**run_identity(r.get("machine") or {}, r.get("started", "")),
                                            "runtime": r.get("runtime")}, sort_keys=True) for r in reports})
    return out


def evidence(report: dict) -> dict:
    """Keep invalid raw evidence in the report; avoid feeding it to numeric comparisons."""
    return pooled({**report, "records": [{**r, "samples": r.get("samples") if valid_series(r.get("samples")) else {}}
                                        for r in report["records"]]})


def routes_unchecked(report: dict) -> dict:
    """A baseline whose binaries can't report routes: an off-route status reads as ok and its
    route checks are dropped (answers, checks and times stay)."""
    records = []
    for r in report["records"]:
        r = {**r, "routes": []}
        if r.get("status") == "off-route" and all(c.get("ok") for c in r.get("checks", [])):
            r["status"] = r.get("expected_outcome", "ok")
        records.append(r)
    return {**report, "records": records, "routes_unchecked": True}


def failures(entry: dict) -> list[str]:
    """What failed in a case's repetitions: a status other than the case's declared outcome
    (ok, or a known issue's: a known issue failing as declared isn't a new failure), and a
    failed check or route where the declared outcome is ok."""
    out = []
    # A known issue that now passes (ok where a failure was declared) is fixed, not failed:
    # said apart (fixed()), its declaration to update (13 Oct 2026: rdf12-join-2g on v2.0 core).
    statuses = [s for s, e in zip(entry["status"], entry["expected"]) if s != e and s != "ok"]
    if statuses:
        out.append(f"status {', '.join(sorted(set(statuses)))} in {len(statuses)} of {len(entry['status'])} repetitions"
                   f" (declared: {', '.join(sorted(set(entry['expected'])))})")
    if entry["invalid"] and all(e == "ok" for e in entry["expected"]):
        out.append("a check or route failed")
    return out


def fixed(entry: dict) -> str | None:
    """A case declared a known issue that passed in every repetition: the issue is fixed."""
    declared = set(entry["expected"]) - {"ok"}
    if declared and entry["status"] and all(s == "ok" for s in entry["status"]):
        return ", ".join(sorted(declared))
    return None


def known_issue(entry: dict) -> str | None:
    """A case declared a known issue (its expected outcome isn't ok), failing as declared."""
    declared = set(entry["expected"]) - {"ok"}
    if declared and not failures(entry):
        return ", ".join(sorted(declared))
    return None


def checks_failed(report: dict, case: str) -> list[str]:
    seen = []
    for r in report["records"]:
        if r["case"] != case:
            continue
        for c in r.get("checks", []) + r.get("routes", []):
            if not c.get("ok"):
                text = f"{c.get('path') or c.get('name') or '?'}: expected {c.get('expected', '?')}, got {c.get('got', '?')}"
                if text not in seen:
                    seen.append(text)
    return seen


def measured(report: dict, case: str) -> tuple[frozenset, list]:
    """What a side's perf_lab measured of a case's memory: the phases it gave a peak window
    (phase_peaks) and the cgroup peaks of its scope or container."""
    phases, cgroups = set(), []
    for r in report["records"]:
        if r["case"] == case:
            phases |= set(r.get("phase_peaks") or {})
            if r.get("cgroup_peak_mib"):
                cgroups.append(r["cgroup_peak_mib"])
    return frozenset(phases), cgroups


def compare_case(base: dict, new: dict, base_measured: tuple | None = None,
                 new_measured: tuple | None = None) -> dict:
    """One case, a baseline against the candidate. Memory is judged by the cgroup peak wherever
    both sides have one (the owner, 14 Oct 2026: it doesn't depend on the harness or the
    binary's own accounting), else by the process's lifetime peak, which compares only where
    both sides' perf_lab measured the same phases: before e0f84185 (v2/memory-variance) a
    phase's window reset the peak, and a phase without a window of its own (the classification)
    went unseen (classify-store-horn 266 -> 3,390 MiB, its cgroup peak 3,283 -> 3,427)."""
    row: dict = {}
    if base["reps"] and new["reps"]:
        point, low, high = bootstrap_ratio(base["reps"], new["reps"])
        s = max(spread(base), spread(new))
        threshold = max(THRESHOLD, s)
        b_ms = statistics.median(base["metrics"]) if base["metrics"] else None
        n_ms = statistics.median(new["metrics"]) if new["metrics"] else None
        verdict = "tie"
        if low > 1.0 and point >= 1 + threshold and (b_ms is None or n_ms is None or n_ms - b_ms >= MIN_MS):
            verdict = "regression"
        elif high < 1.0 and point <= 1 / (1 + threshold) and (b_ms is None or n_ms is None or b_ms - n_ms >= MIN_MS):
            verdict = "win"
        row["time"] = {"ratio": point, "ci": [low, high], "threshold": threshold, "verdict": verdict,
                       "base_metric": b_ms, "new_metric": n_ms}
    kinds = (kind_of(base), kind_of(new))
    memory: dict = {}
    if base["peaks"] and new["peaks"]:
        b, n = statistics.median(base["peaks"]), statistics.median(new["peaks"])
        comparable = kinds[0] == kinds[1] and kinds[0] not in CACHE_KINDS and "/" not in kinds[0]
        lifetime = {"kind": kinds[1] if kinds[0] == kinds[1] else f"{kinds[0]} vs {kinds[1]}",
                    "base_mib": b, "new_mib": n, "ratio": n / b if b else None}
        if base_measured and new_measured and base_measured[0] != new_measured[0]:
            comparable = False
            lifetime["phases"] = {"base": sorted(base_measured[0]), "new": sorted(new_measured[0])}
        memory = {**lifetime, "lifetime": {**lifetime, "comparable": comparable}, "comparable": comparable,
                  "spread": max(peak_spread(base["peaks"]), peak_spread(new["peaks"]))}
    if base_measured and new_measured and base_measured[1] and new_measured[1]:
        b, n = statistics.median(base_measured[1]), statistics.median(new_measured[1])
        memory |= {"kind": "cgroup", "base_mib": b, "new_mib": n, "ratio": n / b if b else None,
                   "cgroup_ratio": n / b if b else None, "comparable": True,
                   "spread": max(peak_spread(base_measured[1]), peak_spread(new_measured[1]))}
    if memory:
        b, n = memory["base_mib"], memory["new_mib"]
        memory["judged_by"] = "cgroup" if memory["kind"] == "cgroup" else "lifetime"
        memory["grew"] = memory["comparable"] and n > b * (1 + MEMORY_GROWTH) and n - b > MEMORY_FLOOR_MIB
        memory["unstable"] = memory.pop("spread") > UNSTABLE_PEAK
        row["memory"] = memory
    grew, notes = counter_changes(base["counters"], new["counters"])
    if notes:
        row["counters"] = {"more_work": grew, "notes": notes}
    return row


def drift(entries: dict[str, dict]) -> dict[str, float]:
    """Per case, a baseline's last half of passes over its first half (time)."""
    out = {}
    for case, entry in entries.items():
        reps = entry["reps"]
        half = len(reps) // 2
        if half >= 1:
            point, _, _ = bootstrap_ratio(reps[:half], reps[-half:], rounds=200)
            if point == point:
                out[case] = point
    return out


def gate(candidate: dict, baselines: dict[str, dict], not_run: dict[str, str] | None = None,
         diagnostic_passes: int | None = None, expectations: dict | None = None) -> dict:
    """`not_run`: cases that couldn't run on the gate's host (case -> why, e.g. its data isn't
    there): neither failures nor passes, never compared; while any remain the gate isn't passed
    (the owner, 14 Oct 2026: not green while they're unrun)."""
    if "candidate" in baselines:
        raise ValueError("candidate is a reserved baseline name")
    cand = evidence(candidate)
    not_run = dict(not_run or {})
    summary = {"candidate": candidate.get("revision"), "harness_revision": candidate.get("harness_revision"),
               "host": (candidate.get("machine") or {}).get("host"),
               "cases": sorted(cand), "candidate_failures": {}, "baselines": {}, "not_run": not_run}
    summary["known_issues"] = {}
    summary["fixed"] = {}
    summary["diagnostic"] = diagnostic_passes is not None
    summary["required_passes"] = CAMPAIGN_PASSES
    summary["coverage"] = {}
    for name, report in [("candidate", candidate), *baselines.items()]:
        errors = list(report.get("coverage_errors", []))
        if not expectations or set(expectations) != {"candidate", *baselines}:
            errors.append("expected campaign sides/provenance missing or mismatched: diagnostic only")
        elif name in expectations:
            # Recheck each original pass even if callers pooled without expected inputs.
            errors.extend(why for r in report.get("pass_reports", []) for why in pass_errors(r, expected=expectations[name]))
            if not report.get("pass_reports"):
                errors.append("original pass evidence missing")
        expected = diagnostic_passes if diagnostic_passes is not None else CAMPAIGN_PASSES
        if report.get("complete_passes", 0) != expected:
            errors.append(f"{report.get('complete_passes', 0)} complete passes; expected {expected} (campaign requires {CAMPAIGN_PASSES})")
        if set(report.get("planned_cases") or []) != set(candidate.get("planned_cases") or []):
            errors.append("case plan differs between sides")
        if set(report.get("planned_cases") or []) != set(report.get("all_cases") or []):
            errors.append("selected/missing full case catalog: diagnostic only")
        summary["coverage"][name] = errors
    for case in sorted(set(cand) - set(not_run)):
        failed = failures(cand[case])
        if failed:
            summary["candidate_failures"][case] = failed + checks_failed(candidate, case)
        elif fixed(cand[case]):
            summary["fixed"][case] = fixed(cand[case])
        elif known_issue(cand[case]):
            summary["known_issues"][case] = known_issue(cand[case])
    for name, report in baselines.items():
        base = evidence(report)
        side = {"revision": report.get("revision"), "harness_revision": report.get("harness_revision"),
                "failures": {}, "missing": sorted(set(cand) - set(base)),
                "cases": {}, "drift": drift(base)}
        for case in sorted(set(cand) & set(base)):
            if case in not_run:
                side["cases"][case] = {"compared": False, "not_run": not_run[case]}
                continue
            failed = failures(base[case])
            if failed:
                side["failures"][case] = failed + checks_failed(report, case)
            if failed or failures(cand[case]) or known_issue(base[case]) or known_issue(cand[case]):
                side["cases"][case] = {"compared": False}
                continue
            intended = (expectations or {}).get("candidate", {}).get("case_manifest", {}).get("series", {}).get(case)
            if not complete_timing(base[case], cand[case], series=intended):
                summary["coverage"][name].append(f"{case}: missing/invalid timing series or repetitions")
                side["cases"][case] = {"compared": False}
                continue
            side["cases"][case] = {"compared": True, **compare_case(base[case], cand[case], measured(report, case),
                                                                      measured(candidate, case))}
        summary["baselines"][name] = side
    # One host, one configuration: every pass of every side alike.
    identities = sorted(set(candidate.get("identities", [])).union(
        *(set(r.get("identities", [])) for r in baselines.values())))
    summary["identities"] = identities
    summary["harnesses"] = sorted({candidate.get("harness_revision"),
                                  *(r.get("harness_revision") for r in baselines.values())}, key=str)
    summary["passed"] = not summary["diagnostic"] and bool(baselines) and not any(summary["coverage"].values()) and len(identities) <= 1 and len(summary["harnesses"]) <= 1 and not summary["candidate_failures"] and not not_run and not any(
        row.get("time", {}).get("verdict") == "regression" or row.get("memory", {}).get("grew")
        for side in summary["baselines"].values() for row in side["cases"].values())
    return summary


def render(summary: dict) -> str:
    lines = [f"# Release gate: {summary['candidate']}", "",
             f"Harness revision: {summary.get('harness_revision') or 'unrecorded'}; engine revision: {summary['candidate']}.", "",
             f"Host {summary['host']}; {len(summary['cases'])} cases; "
             + ("one host and configuration for every pass; " if len(summary.get("identities", [])) <= 1 else
                f"NOT ONE CONFIGURATION ({' | '.join(summary['identities'])}); ")
             +
             f"{'PASSED' if summary['passed'] else 'NOT PASSED'} (requires no regression, no memory growth, no failed check "
             "on the candidate, every case run and four complete passes per side).", ""]
    if summary.get("diagnostic"):
        lines += ["DIAGNOSTIC ONLY — NOT RELEASE ACCEPTED; campaign requires four complete passes per side.", ""]
    if any(summary.get("coverage", {}).values()):
        lines += ["## Incomplete evidence", ""] + [
            f"- {name}: {why}" for name, errors in summary["coverage"].items() for why in errors] + [""]
    if len(summary.get("harnesses", [])) > 1:
        lines += ["NOT ONE HARNESS: " + ", ".join(str(r) for r in summary["harnesses"]), ""]
    if summary.get("not_run"):
        lines += ["## Not run", "", f"{len(summary['not_run'])} cases didn't run here, so the gate isn't passed "
                  "until they have:", ""]
        lines += [f"- {case}: not run ({why})" for case, why in summary["not_run"].items()] + [""]
    lines += ["## Failed quality checks on the candidate", ""]
    lines += [f"- {case}: " + "; ".join(why) for case, why in summary["candidate_failures"].items()] or ["- none"]
    if summary.get("fixed"):
        lines += ["", "Known issues the candidate passes (fixed: update the case's declared outcome): "
                  + ", ".join(f"{c} (declared {o})" for c, o in summary["fixed"].items())]
    if summary.get("known_issues"):
        lines += ["", "Known issues failing as declared (no timing verdict): "
                  + ", ".join(f"{c} ({o})" for c, o in summary["known_issues"].items())]
    for name, side in summary["baselines"].items():
        rows = side["cases"]
        timed = {c: r["time"] for c, r in rows.items() if r.get("time")}
        slower = sorted(((c, t) for c, t in timed.items() if t["ratio"] > 1.0), key=lambda ct: -ct[1]["ratio"])
        faster = sum(1 for t in timed.values() if t["ratio"] < 1.0)
        lines += ["", f"## Against {name} ({side['revision']})", "",
                  f"{len(timed)} cases timed; {len(slower)} slower (every one listed, below the threshold too), "
                  f"{faster} faster; {sum(t['verdict'] == 'regression' for t in timed.values())} regressions, "
                  f"{sum(t['verdict'] == 'win' for t in timed.values())} wins.", "",
                  "| Case | Candidate / baseline | 95 % interval | Threshold | Verdict |", "|---|---|---|---|---|"]
        for case, t in slower:
            lines.append(f"| {case} | {t['ratio']:.3f} | [{t['ci'][0]:.3f}, {t['ci'][1]:.3f}] | "
                         f"{t['threshold']:.0%} | {t['verdict']} |")
        wins = [(c, t) for c, t in timed.items() if t["verdict"] == "win"]
        if wins:
            lines += ["", "Wins: " + ", ".join(f"{c} {t['ratio']:.2f}" for c, t in sorted(wins, key=lambda ct: ct[1]['ratio']))]
        not_compared = [c for c, r in rows.items() if not r.get("compared")]
        lines += ["", f"Not compared ({len(not_compared)}): a known issue, a check failed on a side, the case "
                      "is missing there, or it didn't run."]
        for case, why in side["failures"].items():
            if case in summary.get("not_run", {}):
                continue
            lines.append(f"- {case} on {name}: " + "; ".join(why))
        if side["missing"]:
            lines.append(f"- missing on {name}: {', '.join(side['missing'])}")
        memory = [(c, r["memory"]) for c, r in rows.items() if r.get("memory")]
        moved = [(c, m) for c, m in memory if m["grew"] or (m["comparable"] and m["unstable"])
                 or " vs " in m["kind"] or (m["ratio"] and abs(m["ratio"] - 1) > MEMORY_GROWTH)]
        moved += [(c, m) for c, m in memory if (c, m) not in moved and (l := m.get("lifetime"))
                  and (not l["comparable"] or (l["ratio"] and abs(l["ratio"] - 1) > MEMORY_GROWTH))]
        lines += ["", "Memory, judged by the cgroup peak where both sides have one (it doesn't depend on the "
                      "harness), else by the process's lifetime peak where both measured the same phases:", "",
                  "| Case | Judged by | Baseline MiB | Candidate MiB | Ratio | Lifetime peak | Note |",
                  "|---|---|---|---|---|---|---|"]
        for case, m in sorted(moved, key=lambda cm: -(cm[1]["ratio"] or 0)):
            note = ", ".join(n for n, on in (("GREW", m["grew"]), ("not comparable", not m["comparable"]),
                                              ("unstable peak", m["unstable"])) if on)
            if m.get("phases"):
                note += (f"{'; ' if note else ''}lifetime peaks over other phases: baseline "
                         f"{', '.join(m['phases']['base']) or 'none'}, candidate {', '.join(m['phases']['new']) or 'none'}")
            life = m.get("lifetime")
            shown = (f"{life['base_mib']:.0f} → {life['new_mib']:.0f} ({life['ratio']:.2f}x"
                     f"{', not comparable' if not life['comparable'] else ''})" if life and life["ratio"] else "-")
            lines.append(f"| {case} | {m['judged_by']} | {m['base_mib']:.0f} | {m['new_mib']:.0f} | "
                         f"{m['ratio']:.2f} | {shown} | {note} |")
        lines.append(f"\n{len(memory) - len(moved)} other cases within {MEMORY_GROWTH:.0%}.")
        work = [(c, r["counters"]) for c, r in rows.items() if r.get("counters", {}).get("more_work")]
        if work:
            lines += ["", "More work (counters beyond 2 %):"] + [f"- {c}: " + "; ".join(w["notes"]) for c, w in work]
        d = side["drift"]
        if d:
            far = sorted(((c, v) for c, v in d.items() if abs(v - 1) > 0.05), key=lambda cv: -abs(cv[1] - 1))
            lines += ["", f"Drift of {name} between its first and last passes: median "
                          f"{statistics.median(d.values()):.3f}; beyond 5 %: "
                          + (", ".join(f"{c} {v:.2f}" for c, v in far) or "none")]
    return "\n".join(lines) + "\n"


def main() -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--candidate", help="the candidate's pass reports, comma-separated")
    p.add_argument("--baseline", action="append", metavar="NAME=PASS,PASS,...")
    p.add_argument("--check-pass", type=Path, help="check completion and exact coverage of one campaign pass")
    p.add_argument("--harness-root", type=Path, default=HERE.parents[1])
    p.add_argument("--harness-revision", required=True, help="expected clean candidate harness revision")
    p.add_argument("--engine", action="append", required=True, metavar="NAME=REV=DIR", help="expected side and verified build bundle")
    p.add_argument("--cases", help="current selection, diagnostic only if not the full catalog")
    p.add_argument("--diagnostic-passes", type=int, help="compare N passes per side diagnostically; never release accepted")
    p.add_argument("--routes-unchecked", action="append", default=[], metavar="NAME",
                   help="a baseline whose binaries predate the harness's route assertions (main at f9b515e8): "
                        "its off-route status and route checks don't count; answers and times do")
    p.add_argument("--not-run", action="append", default=[], metavar="CASE,CASE,...=WHY",
                   help="cases that couldn't run on this host (their data isn't there): reported as not run, "
                        "never as passes; the gate isn't passed while any remain")
    p.add_argument("--out", type=Path)
    p.add_argument("--json", type=Path)
    args = p.parse_args()
    expectations = {}
    for spec in args.engine:
        name, revision, directory = spec.split("=", 2)
        if not name or not revision or name in expectations:
            p.error("nonempty, unique expected engine sides required")
        expectations[name] = expectation(args.harness_root, args.harness_revision, revision, Path(directory), args.cases)
    if args.check_pass:
        if len(expectations) != 1:
            p.error("one expected engine required for check-pass")
        errors = pass_errors(load_report(args.check_pass), expected=next(iter(expectations.values())))
        print("\n".join(errors))
        return int(bool(errors))
    if args.candidate is None or not args.baseline or (args.diagnostic_passes is not None and args.diagnostic_passes < 1):
        p.error("candidate and baseline reports required; diagnostic pass count must be positive")
    candidate = pool([Path(x) for x in args.candidate.split(",") if x], expectations.get("candidate"))
    baselines = {}
    for spec in args.baseline:
        name, _, paths = spec.partition("=")
        if not name or name == "candidate" or name in baselines:
            p.error("nonempty, unique baseline names required; candidate is reserved")
        baselines[name] = pool([Path(x) for x in paths.split(",") if x], expectations.get(name))
        if name in args.routes_unchecked:
            baselines[name] = routes_unchecked(baselines[name])
    not_run = {}
    for spec in args.not_run:
        cases, _, why = spec.partition("=")
        not_run |= {c: why or "not run" for c in cases.split(",") if c}
    summary = gate(candidate, baselines, not_run, args.diagnostic_passes, expectations)
    text = render(summary)
    print(text)
    if args.out:
        args.out.write_text(text, encoding="utf-8")
    if args.json:
        args.json.write_text(json.dumps(summary, indent=1, default=str), encoding="utf-8")
    return 0 if summary["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
