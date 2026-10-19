"""The release gate (benches/fast/release_gate.py) and the pass options of fast.py it relies on.

The defects of the 18 Oct 2026 audit each have a test here:
1. a gate passed with one baseline and overlapping, identical timestamps;
2. (compare.judge, test_release_policy.py) and here: identical sides must pass;
3. main's perf lab reports no samples: its side is compared by per-process p50s, its
   unreported values unchecked, never wrong; the data is the gate's, not the side's;
6. one memory policy: the process's own peak judged, the cgroup's (page cache) reported.
"""
from __future__ import annotations

import copy
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches" / "suite"))
sys.path.insert(0, str(ROOT / "benches" / "fast"))
spec = importlib.util.spec_from_file_location("release_gate", ROOT / "benches/fast/release_gate.py")
gate_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate_module)
import fast  # noqa: E402

HARNESS = "h" * 40
REVS = {"cand": "c" * 40, "base": "b" * 40, "main": "m" * 40}
ORDER = ["base", "main", "cand", "cand", "main", "base"] * 2
MACHINE = {"host": "phuoc-yu", "cpu": "Ryzen", "governor": "performance"}


def observation(samples, peak=500, cgroup=900, estimator="samples", rows=None, phases=None):
    return {"identity": {"sweep": 0, "repeat": 0, "cache": "warm"}, "exit": 0, "samples": samples,
            "estimator": estimator, "peak_mib": peak, "phase_peaks": phases or {}, "cgroup_peak_mib": cgroup,
            "rows": rows or {q: 5 for q in samples}, "checks": [], "routes": []}


def record(case="lab-olympics", samples=None, status="ok", expected="ok", **kw):
    samples = samples if samples is not None else {"q01": [10.0, 10.1, 9.9, 10.0, 10.2], "q06": [4.8, 4.9, 4.8]}
    obs = observation(samples, **kw)
    return {"case": case, "rep": 1, "status": status, "expected_outcome": expected, "checks": [], "routes": [],
            "samples": samples, "observations": [obs], "peak_mib": obs["peak_mib"], "notes": [], "counters": {}}


def report(side, n, start, records, options=None):
    rev = REVS[side]
    binaries = {"revision": rev, "verified": True, "sha256": {"release/examples/perf_lab": side * 8}}
    return {"label": f"gate-{n}-{side}", "started": "2026-10-20T01:00:00", "started_ns": start,
            "finished": "2026-10-20T01:10:00", "finished_ns": start + 600, "reps": 1,
            "planned_cases": sorted({r["case"] for r in records}), "records": records,
            "git": {"commit": HARNESS, "dirty": 0}, "git_finished": {"commit": HARNESS, "dirty": 0},
            "binaries": binaries, "binaries_finished": copy.deepcopy(binaries), "machine": MACHINE,
            "runtime": "native", "prepared": {"manifest_sha256": "p" * 64, "files": 3, "changed": []},
            "options": options or {"prepared": True, "routes": True, "empty_queries": False}}


def passes(records_of=None, order=ORDER, starts=None):
    """A campaign's passes: records_of(side, n) gives a pass's records."""
    records_of = records_of or (lambda side, n: [record()])
    starts = starts or [1000 * (i + 1) for i in range(len(order))]
    return [{"n": i + 1, "side": side, "path": f"pass-{i + 1}-{side}.json",
             "report": report(side, i + 1, starts[i], records_of(side, i + 1))}
            for i, side in enumerate(order)]


def run_gate(ps, sides=("cand", "base", "main"), require=("base", "main"), **kw):
    return gate_module.gate(ps, {s: REVS[s] for s in sides}, "cand", list(require), HARNESS, **kw)


class Schedule(unittest.TestCase):
    """Defect 1."""

    def test_identical_sides_in_a_real_a_b_b_a_campaign_pass(self):
        summary = run_gate(passes())
        self.assertEqual(summary["errors"], [])
        self.assertEqual(summary["blocking"], [])
        self.assertTrue(summary["passed"])

    def test_one_baseline_never_passes_a_gate_that_requires_two(self):
        ps = passes(order=["base", "cand", "cand", "base"] * 2)
        summary = run_gate(ps, sides=("cand", "base"))
        self.assertIn("required baselines missing: main", summary["errors"])
        self.assertFalse(summary["passed"])

    def test_overlapping_or_identical_timestamps_refuse_the_passes(self):
        same = passes(starts=[5000] * 12)
        self.assertTrue(any("started at the same time" in e for e in run_gate(same)["errors"]))
        overlap = passes(starts=[1000 + 300 * i for i in range(12)])  # each pass lasts 600
        self.assertTrue(any("overlap" in e for e in run_gate(overlap)["errors"]))
        self.assertFalse(run_gate(overlap)["passed"])

    def test_blocks_of_one_side_are_not_interleaved(self):
        ps = passes(order=["base"] * 4 + ["main"] * 4 + ["cand"] * 4)
        errors = run_gate(ps)["errors"]
        self.assertTrue(any("not a palindrome" in e for e in errors))
        self.assertTrue(any("more than two passes in a row" in e for e in errors))

    def test_fewer_passes_than_the_campaign_are_diagnostic_only(self):
        ps = passes(order=["base", "main", "cand", "cand", "main", "base"])
        summary = run_gate(ps)
        self.assertTrue(any("at least 4 required" in e for e in summary["errors"]))
        diagnostic = run_gate(ps, min_passes=2)
        self.assertEqual(diagnostic["errors"], [])
        self.assertTrue(diagnostic["diagnostic"])
        self.assertFalse(diagnostic["passed"])

    def test_a_pass_must_say_what_it_ran(self):
        ps = passes()
        ps[3]["report"]["git"]["dirty"] = 2
        ps[4]["report"]["binaries"]["revision"] = REVS["base"]
        ps[5]["report"]["prepared"]["changed"] = ["/fast/lubm.nt"]
        del ps[6]["report"]["finished_ns"]
        errors = " ".join(run_gate(ps)["errors"])
        for text in ("dirty 2", "expected " + REVS["main"], "prepared data changed", "unfinished pass"):
            self.assertIn(text, errors)


class Verdicts(unittest.TestCase):
    def test_a_slower_query_against_one_baseline_blocks(self):
        def records_of(side, n):
            samples = {"q01": [10.0, 10.1, 9.9], "q06": [18.0, 18.2, 18.1] if side == "cand" else [4.8, 4.9, 4.8]}
            return [record(samples=samples)]
        summary = run_gate(passes(records_of))
        self.assertFalse(summary["passed"])
        self.assertTrue(any("lab-olympics against base: time/q06: regression" in b for b in summary["blocking"]))
        self.assertTrue(any("lab-olympics against main: time/q06: regression" in b for b in summary["blocking"]))

    def test_the_candidate_failing_a_case_blocks_and_a_baseline_failing_is_reported(self):
        def records_of(side, n):
            if side == "main":
                r = record(status="failed")
                r["notes"] = ['Error: "unknown argument --reason-runs"']
                return [r]
            return [record()]
        summary = run_gate(passes(records_of))
        row = summary["comparisons"]["main"]["lab-olympics"]
        self.assertFalse(row["comparable"])
        self.assertIn("main can't run it (unknown argument --reason-runs", row["reason"])
        self.assertTrue(summary["passed"])
        failing = run_gate(passes(lambda side, n: [record(status="wrong" if side == "cand" else "ok")]))
        self.assertIn("lab-olympics", failing["candidate_failures"])
        self.assertFalse(failing["passed"])

    def test_a_baseline_off_its_route_with_right_answers_is_compared(self):
        def records_of(side, n):
            r = record()
            if side == "base":
                r["status"] = "off-route"
                r["routes"] = [{"path": "q.q06.operators", "expected": "has wcoj", "got": ["join"], "ok": False}]
            return [r]
        summary = run_gate(passes(records_of))
        row = summary["comparisons"]["base"]["lab-olympics"]
        self.assertTrue(row["comparable"])
        self.assertIn("off the route", " ".join(row["notes"]))
        self.assertTrue(summary["passed"])
        candidate_off = run_gate(passes(lambda side, n: records_of("base" if side == "cand" else "cand", n)))
        self.assertIn("lab-olympics", candidate_off["candidate_failures"])

    def test_stopping_at_a_declared_limit_where_a_baseline_finished_blocks(self):
        def records_of(side, n):
            if side == "cand":
                return [record(status="memory-limit", expected="memory-limit")]
            return [record(expected="memory-limit")]
        summary = run_gate(passes(records_of))
        self.assertTrue(any("stops at its declared outcome (memory-limit) where base completed" in b
                            for b in summary["blocking"]))

    def test_cases_not_run_block_and_nothing_waives_them(self):
        summary = run_gate(passes(), not_run={"bsbm-bi-100m": "its cap is 28 GB"})
        self.assertFalse(summary["passed"])
        with self.assertRaises(TypeError):
            run_gate(passes(), waived={"bsbm-bi-100m": "no such thing"})

    def test_cases_left_to_the_big_host_must_be_timed_there(self):
        """The 28 GB cases: the timed host leaves them to khaleesi; the verdict passes only
        when another timed part's plan has them."""
        here = run_gate(passes(), elsewhere={"bsbm-bi-100m": "timed on khaleesi"})
        self.assertTrue(here["passed"])
        ok = {"passed": True, "errors": [], "guards": 81}
        w3c = {"passed": True, "errors": []}
        passed, text = gate_module.verdict([here], w3c, ok)
        self.assertFalse(passed)
        self.assertIn("NOT timed anywhere: bsbm-bi-100m", text)
        big = {**here, "planned": ["bsbm-bi-100m"], "elsewhere": {}}
        self.assertTrue(gate_module.verdict([here, big], w3c, ok)[0])

    def test_a_series_the_candidate_lost_blocks(self):
        def records_of(side, n):
            samples = {"q01": [10.0]} if side == "cand" else {"q01": [10.0], "q06": [4.8]}
            return [record(samples=samples)]
        summary = run_gate(passes(records_of))
        self.assertTrue(any("time/q06: lost" in b for b in summary["blocking"]))


class MainAdapter(unittest.TestCase):
    """Defect 3: main f9b515e8's perf lab emits no samples_ms."""

    def main_result(self, p50=4.8):
        return {"label": "main", "load_s": 0.2, "peak_mib": 511, "sum_p50_ms": 15.0,
                "queries": [{"name": "q01", "rows": 5, "p50_ms": 10.0, "min_ms": 9.9, "max_ms": 10.2},
                            {"name": "q06", "rows": 1033, "p50_ms": p50, "min_ms": p50, "max_ms": p50}]}

    def test_fast_takes_main_s_p50_as_one_value_per_process(self):
        case = {"name": "lab-olympics", "metric": "sum_p50_ms", "queries": "@olympics"}
        result = self.main_result()
        self.assertEqual(fast.samples(case, result), {"q01": [10.0], "q06": [4.8]})
        self.assertEqual(fast.estimator(result), "process-p50")
        self.assertEqual(fast.estimator({"queries": [{"name": "q", "p50_ms": 1, "samples_ms": [1]}]}), "samples")

    def test_without_routes_unreported_values_are_unchecked_not_wrong(self):
        result = self.main_result()
        with patch.dict(fast.OPTIONS, {"routes": False}):
            value = fast.check("q.q06.value", 1033, result, {}, {})
            rows = fast.check("q.q06.rows", 1033, result, {}, {})
            route = fast.route("q.q06.operators", "has dictionary string test", result)
        self.assertIsNone(value["ok"])
        self.assertTrue(rows["ok"])
        self.assertIsNone(route["ok"])
        self.assertFalse(fast.check("q.q06.value", 1033, result, {}, {})["ok"])  # with routes: wrong

    def test_main_is_compared_by_process_p50s_on_both_sides(self):
        def records_of(side, n):
            if side == "main":
                return [record(samples={"q01": [10.0], "q06": [4.8 + 0.01 * n]}, estimator="process-p50")]
            return [record(samples={"q01": [10.0, 10.1, 9.9], "q06": [17.0, 18.0, 30.0]})]
        summary = run_gate(passes(records_of))
        row = summary["comparisons"]["main"]["lab-olympics"]
        self.assertIn("one p50 per process", " ".join(row["notes"]))
        q06 = row["dimensions"]["time/q06"]
        self.assertEqual(q06["verdict"], "regression")
        self.assertAlmostEqual(q06["new"], 18.0)  # the candidate's process p50, not its pooled samples
        self.assertEqual(summary["comparisons"]["base"]["lab-olympics"]["dimensions"]["time/q06"]["verdict"], "neutral")

    def test_answers_that_differ_from_a_baseline_block_unless_a_check_pins_them(self):
        def records_of(side, n):
            return [record(rows={"q01": 5, "q06": 1000 if side == "main" else 1033})]
        unpinned = run_gate(passes(records_of))
        self.assertTrue(any("q06: rows [1033] here, [1000] on main: answers differ" in b for b in unpinned["blocking"]))
        pinned = run_gate(passes(records_of), catalog=[{"name": "lab-olympics", "check": {"q.q06.rows": 1033}}])
        self.assertTrue(pinned["passed"])
        self.assertIn("q06", " ".join(pinned["comparisons"]["main"]["lab-olympics"]["notes"]))

    def test_a_prepared_pass_generates_nothing(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(fast, "FAST_DATA", Path(tmp)), \
                patch.object(fast, "RUNTIME", "native"), patch.dict(fast.OPTIONS, {"prepared": True}), \
                patch.object(fast, "container") as container, \
                patch.object(fast, "native_dirs", lambda: {"/fast": Path(tmp), "/target": Path(tmp)}):
            with self.assertRaisesRegex(RuntimeError, "missing input: social people=10 was not prepared"):
                fast.ensure_data("social people=10")
            container.assert_not_called()

    def test_a_query_set_kept_with_the_data_is_in_the_case_manifest_by_its_name(self):
        # 18 Oct 2026: a whole pass failed (relative_to) on WatDiv's sets, kept outside the tree.
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "watdiv-10m").mkdir()
            (Path(tmp) / "watdiv-10m" / "C1-1.rq").write_text("SELECT * { ?s ?p ?o }", encoding="utf-8")
            case = {"name": "watdiv", "queries": "@watdiv-10m", "metric": "sum_p50_ms", "data": "none"}
            with patch.dict("os.environ", {"NRESE_FAST_QUERY_SETS": tmp}), patch.object(fast, "RUNTIME", "native"):
                found = fast.case_manifest([case])
        self.assertEqual(found["series"]["watdiv"], ["C1-1"])

    def test_prepared_files_are_checked_by_size_and_time(self):
        with tempfile.TemporaryDirectory() as tmp:
            data = Path(tmp) / "d.nt"
            data.write_text("x")
            stat = data.stat()
            files = {data.as_posix(): {"bytes": stat.st_size, "mtime_ns": stat.st_mtime_ns, "sha256": "-"}}
            self.assertEqual(fast.changed_inputs(files), [])
            data.write_text("xy")
            self.assertEqual(fast.changed_inputs(files), [data.as_posix()])


class Memory(unittest.TestCase):
    """Defect 6: one policy. The process's own peak is judged; the cgroup's, which counts the
    page cache charged to the case's reads, is reported beside it and never judged."""

    def test_a_cgroup_peak_of_page_cache_is_reported_not_judged(self):
        summary = run_gate(passes(lambda side, n: [record(cgroup=5000 if side == "cand" else 1000)]))
        self.assertTrue(summary["passed"])
        row = summary["comparisons"]["base"]["lab-olympics"]
        self.assertEqual(row["reported"]["cgroup_peak_mib"], [1000, 5000])
        self.assertNotIn("memory/cgroup", row["dimensions"])

    def test_more_process_memory_beyond_margin_and_floor_blocks(self):
        summary = run_gate(passes(lambda side, n: [record(peak=800 if side == "cand" else 500)]))
        self.assertTrue(any("memory/lifetime: regression" in b for b in summary["blocking"]))
        # No MiB tolerance: 4 % more blocks; within the 2 MiB of rounding it doesn't.
        more = run_gate(passes(lambda side, n: [record(peak=520 if side == "cand" else 500)]))
        self.assertTrue(any("memory/lifetime: regression" in b for b in more["blocking"]))
        rounding = run_gate(passes(lambda side, n: [record(peak=501 if side == "cand" else 500)]))
        self.assertTrue(rounding["passed"])

    def test_a_tool_without_a_process_peak_on_either_side_is_noted_not_blocked(self):
        # The dry run of 18 Oct 2026: tableau_consistency reports no peak (dl-roles).
        summary = run_gate(passes(lambda side, n: [record(peak=None)]))
        row = summary["comparisons"]["base"]["lab-olympics"]
        self.assertNotIn("memory/lifetime", row["dimensions"])
        self.assertIn("no process peak on either side", " ".join(row["notes"]))
        self.assertTrue(summary["passed"])
        lost = run_gate(passes(lambda side, n: [record(peak=None if side == "cand" else 500)]))
        self.assertTrue(any("memory/lifetime: incomplete" in b for b in lost["blocking"]))

    def test_phase_peaks_are_judged_where_both_sides_report_them(self):
        def records_of(side, n):
            phases = {"load": 900 if side == "cand" else 400}
            return [record(phases=None if side == "main" else phases)]
        summary = run_gate(passes(records_of))
        self.assertIn("memory/phase/load", summary["comparisons"]["base"]["lab-olympics"]["dimensions"])
        self.assertTrue(any("against base: memory/phase/load: regression" in b for b in summary["blocking"]))
        self.assertIn("phase peaks: main's perf lab reports none", " ".join(
            summary["comparisons"]["main"]["lab-olympics"]["notes"]))


class Tradeoffs(unittest.TestCase):
    """Only an owner-approved trade-off accepts a loss (benches/release-tradeoffs.toml)."""

    def campaign(self):
        def records_of(side, n):
            slow = {"q01": [10.0, 10.1, 9.9], "q06": [5.2, 5.25, 5.2] if side == "cand" else [4.8, 4.85, 4.8]}
            r = record(samples=slow)
            fast_ = record(case="kernel-intersect",
                           samples={"triangles": [50.0, 50.2] if side == "cand" else [140.0, 140.5]})
            return [r, fast_]
        return passes(records_of)

    def entry(self, **changes):
        return {"case": "lab-olympics", "dimension": "time/q06", "max_ratio": 1.12, "approved": "2026-10-20",
                "by": "the owner", "reason": "text index: 8 % slower filters for 2.8x faster triangles",
                "pays_with": {"case": "kernel-intersect", "dimension": "time/triangles"}, **changes}

    def test_without_an_entry_the_loss_blocks(self):
        summary = run_gate(self.campaign())
        self.assertTrue(any("lab-olympics against base: time/q06: regression" in b for b in summary["blocking"]))
        self.assertFalse(summary["passed"])

    def test_an_approved_trade_off_with_its_paying_win_is_reported_as_accepted(self):
        summary = run_gate(self.campaign(), tradeoffs=[self.entry()])
        self.assertTrue(summary["passed"])
        self.assertEqual(len(summary["accepted"]), 2)  # against base and against main
        self.assertIn("accepted trade-off", summary["accepted"][0])
        self.assertIn("accepted trade-off", gate_module.render(summary))

    def test_a_trade_off_covers_only_its_bound_its_baseline_and_with_its_win(self):
        for entry in (self.entry(max_ratio=1.05),                                          # the loss is larger
                      self.entry(pays_with={"case": "lab-olympics", "dimension": "time/q01"}),  # no win there
                      self.entry(dimension="time/q01")):                                   # another dimension
            self.assertFalse(run_gate(self.campaign(), tradeoffs=[entry])["passed"], entry)
        only_main = run_gate(self.campaign(), tradeoffs=[self.entry(against="main")])
        self.assertTrue(any("against base: time/q06" in b for b in only_main["blocking"]))
        self.assertFalse(any("against main: time/q06" in b for b in only_main["blocking"]))

    def test_the_list_starts_empty_and_a_malformed_entry_is_refused(self):
        entries, errors = gate_module.load_tradeoffs(ROOT / "benches/release-tradeoffs.toml")
        self.assertEqual((entries, errors), ([], []))
        with tempfile.TemporaryDirectory() as tmp:
            bad = Path(tmp) / "t.toml"
            bad.write_text('[[tradeoff]]\ncase = "x"\ndimension = "time/q"\nmax_ratio = 1.1\n', encoding="utf-8")
            entries, errors = gate_module.load_tradeoffs(bad)
            self.assertEqual(entries, [])
            self.assertIn("missing approved, by, pays_with, reason", errors[0])


class ConfirmOnRerun(unittest.TestCase):
    """The owner's decision (18 Oct 2026): a flagged loss or undecided case runs again in a fresh,
    independent A-B-B-A; a loss blocks only if that confirmation shows it too."""

    W3C = {"passed": True, "errors": []}
    GUARDS = {"passed": True, "errors": [], "guards": 81}

    def gate_run(self, q06_cand, q06_base=(4.8, 4.85, 4.8), round_=0, order=ORDER):
        def records_of(side, n):
            q06 = list(q06_cand) if side == "cand" else list(q06_base)
            return [record(samples={"q01": [10.0, 10.1, 9.9], "q06": q06})]
        return run_gate(passes(records_of, order=order), context={"host": "phuoc-yu", "round": round_,
                                                                    "prefix": "rg-cccccccc", "max_gb": 24.0,
                                                                    "ram_gb": 24.0})

    def test_a_first_run_loss_is_flagged_not_final(self):
        first = self.gate_run((5.2, 5.25, 5.2))
        flagged = {(f["side"], f["dimension"], f["verdict"]) for f in first["flagged"]}
        self.assertEqual(flagged, {("base", "time/q06", "regression"), ("main", "time/q06", "regression")})
        passed, text = gate_module.verdict([first], self.W3C, self.GUARDS)
        self.assertFalse(passed)  # pending its confirmation
        self.assertIn("**pending**", text)

    def test_a_loss_the_confirmation_repeats_blocks(self):
        first, again = self.gate_run((5.2, 5.25, 5.2)), self.gate_run((5.21, 5.22, 5.2), round_=1)
        states = gate_module.combine([first, again])
        self.assertEqual({s["status"] for s in states}, {"confirmed"})
        passed, text = gate_module.verdict([first, again], self.W3C, self.GUARDS)
        self.assertFalse(passed)
        self.assertIn("**confirmed**", text)

    def test_a_loss_that_doesnt_repeat_is_reported_with_both_intervals_and_doesnt_block(self):
        first, again = self.gate_run((5.2, 5.25, 5.2)), self.gate_run((4.8, 4.86, 4.8), round_=1)
        states = gate_module.combine([first, again])
        self.assertEqual({s["status"] for s in states}, {"not confirmed"})
        self.assertEqual([h["verdict"] for h in states[0]["rounds"]], ["regression", "neutral"])
        passed, text = gate_module.verdict([first, again], self.W3C, self.GUARDS)
        self.assertTrue(passed)
        line = next(x for x in text.splitlines() if "not confirmed" in x)
        self.assertIn("round 0: regression", line)
        self.assertIn("round 1: neutral", line)

    def test_undecided_then_shown_once_needs_another_round_and_blocks_at_the_last(self):
        first = self.gate_run((5.2, 5.25, 5.2))
        flag = first["flagged"][0]
        undecided = {**flag, "verdict": "suspect", "needs_passes": 10}
        first = {**first, "flagged": [undecided]}
        shown = self.gate_run((5.21, 5.22, 5.2), round_=1)
        self.assertEqual(gate_module.combine([first, shown])[0]["status"], "pending")  # one detection
        twice = self.gate_run((5.2, 5.2, 5.22), round_=2)
        self.assertEqual(gate_module.combine([first, shown, twice])[0]["status"], "confirmed")
        still = {**shown, "comparisons": {}}  # the confirmation lacks the dimension
        self.assertEqual(gate_module.combine([first, still])[0]["status"], "missing")

    def test_the_confirmation_job_runs_just_the_flagged_cases_on_the_same_host(self):
        first = self.gate_run((5.2, 5.25, 5.2))
        job = gate_module.confirm_job([first], "C:/repo")
        self.assertEqual(job["id"], "rg-cccccccc-confirm")
        self.assertEqual((job["kind"], job["class"], job["requires"]["hosts"]), ("owlrs.timed", "exclusive", ["phuoc-yu"]))
        self.assertEqual(job["steps"][0][2:], ["plan-cases", "lab-olympics"])
        sides = [s[4] for s in job["steps"] if s[2] == "pass"]
        self.assertEqual(sides, sides[::-1])  # palindromic
        n = sides.count("cand")
        self.assertTrue(4 <= n <= 24 and n % 2 == 0)
        self.assertEqual(job["env"]["GATE_ROUND"], "1")
        self.assertEqual(job["env"]["GATE_MIN_PASSES"], str(n))
        self.assertEqual([s[2] for s in job["steps"][-2:]], ["report", "clean"])
        import tomllib
        self.assertEqual(tomllib.loads(gate_module.toml_job(job))["steps"], job["steps"])
        again = self.gate_run((5.21, 5.22, 5.2), round_=1)
        self.assertIsNone(gate_module.confirm_job([first, again], "C:/repo"))  # decided: confirmed
        undecided = {**first, "flagged": [{**f, "verdict": "suspect", "needs_passes": 9} for f in first["flagged"]]}
        second = gate_module.confirm_job([undecided, again], "C:/repo")
        self.assertEqual(second["id"], "rg-cccccccc-confirm2")

    def test_nothing_flagged_needs_no_confirmation(self):
        clean = self.gate_run((4.8, 4.85, 4.8))
        self.assertEqual(clean["flagged"], [])
        self.assertIsNone(gate_module.confirm_job([clean], "C:/repo"))
        self.assertTrue(gate_module.verdict([clean], self.W3C, self.GUARDS)[0])

    def test_a_loss_only_the_confirmation_shows_is_kept_as_not_confirmed(self):
        first = self.gate_run((5.2, 5.25, 5.2))
        def records_of(side, n):
            q01 = [10.6, 10.7, 10.6] if side == "cand" else [10.0, 10.1, 9.9]
            return [record(samples={"q01": q01, "q06": [5.2, 5.2, 5.21] if side == "cand" else [4.8, 4.85, 4.8]})]
        again = run_gate(passes(records_of), context={"host": "phuoc-yu", "round": 1})
        passed, text = gate_module.verdict([first, again], self.W3C, self.GUARDS)
        self.assertIn("time/q01: **not confirmed** (only round 1", text)


class Parts(unittest.TestCase):
    def test_plan_never_lowers_a_cap(self):
        catalog = [{"name": "small", "cap_gb": 4}, {"name": "bsbm-bi-100m", "cap_gb": 28},
                   {"name": "huge", "cap_gb": 60}]
        run, not_run, elsewhere = gate_module.plan(catalog, 24)
        self.assertEqual(run, ["small"])
        self.assertIn("its cap is 28 GB", not_run["bsbm-bi-100m"])
        run, not_run, elsewhere = gate_module.plan(catalog, 24, big_gb=28)
        self.assertEqual((run, sorted(not_run)), (["small"], ["huge"]))
        self.assertIn("timed on khaleesi", elsewhere["bsbm-bi-100m"])
        run, not_run, elsewhere = gate_module.plan(catalog, 28, min_gb=24)  # the big host's share
        self.assertEqual(run, ["bsbm-bi-100m"])

    def test_guards_are_read_from_performance_md_and_must_each_pass(self):
        names = gate_module.guard_names((ROOT / "docs/design/performance.md").read_text(encoding="utf-8"))
        self.assertIn("ordered_dedup_uses_any_complete_sort_order_without_gathering", names)
        self.assertIn("cycles_are_counted_without_their_solutions", names)
        self.assertNotIn("nrese-exec", names)
        log = ("test a::ordered_dedup_uses_any_complete_sort_order_without_gathering ... ok\n"
               "test cycles_are_counted_without_their_solutions ... ignored\n")
        errors, _ = gate_module.guards_check(["ordered_dedup_uses_any_complete_sort_order_without_gathering",
                                              "cycles_are_counted_without_their_solutions", "a_missing_guard_name"], log)
        self.assertEqual(len(errors), 2)
        self.assertIn("ignored", errors[0])
        self.assertIn("no test of that name ran", errors[1])

    def test_w3c_counts_are_required_per_suite(self):
        with tempfile.TemporaryDirectory() as tmp:
            logs = Path(tmp)
            (logs / "w3c_owl2_rl.log").write_text(
                "W3C OWL 2 RL test cases (RDF-based semantics): 86 passed, 0 failed (0 expected), 3 skipped\n"
                "test result: ok. 1 passed; 0 failed; 0 ignored\n")
            (logs / "w3c_shacl.log").write_text("W3C SHACL Core: 98 of 98 pass\n")
            (logs / "w3c_sparql11.log").write_text(
                "W3C SPARQL 1.1 on engine v2: passed / failed / skipped (failures shared with the reference)\n"
                "  QueryEvaluation: 239 / 0 / 0 (0)\n  total: 505 / 0 / 0 (0)\n")
            (logs / "tableau-w3c.tsv").write_text("a\tConsistencyTest\tAL\tpass\t0\n" * 410 +
                                                  "b\tInconsistencyTest\tAL\tdisputed\t0\n")
            counts = gate_module.w3c_counts(logs)
            expected = {"OWL 2 RL": {"passed": 86, "skipped": 3}, "SHACL Core": {"passed": 98},
                        "SPARQL 1.1": {"passed": 505}, "OWL 2 DL": {"passed": 410, "disputed": 1},
                        "SHACL-SPARQL": {"passed": 22}}
            errors = gate_module.w3c_check(counts, expected)
            self.assertEqual(errors, ["SHACL-SPARQL: no counts (its kit didn't run, or its data was missing)"])
            counts["OWL 2 RL"]["skipped"] = 89
            self.assertIn("OWL 2 RL: 89 skipped, at most 3 known", gate_module.w3c_check(counts, expected))
            # The dry run of 18 Oct 2026: two DL cases undecided are open, not failed.
            (logs / "tableau-w3c.tsv").write_text("a\tConsistencyTest\tAL\tpass\t0\n" * 408 +
                                                  "b\tInconsistencyTest\tAL\tdisputed\t0\n" +
                                                  "c\tPositiveEntailmentTest\tAL\topen\t0\tunsupported\n" * 2)
            dl = gate_module.w3c_counts(logs)["OWL 2 DL"]
            self.assertEqual((dl["passed"], dl["open"], dl["failed"]), (408, 2, 0))
            errors = gate_module.w3c_check({"OWL 2 DL": dl}, {"OWL 2 DL": expected["OWL 2 DL"]})
            self.assertEqual(errors, ["OWL 2 DL: 408 passed, at least 410 required", "OWL 2 DL: 2 open, at most 0 known"])

    def test_the_expected_w3c_counts_cover_the_merge_gate_state(self):
        import tomllib
        expected = tomllib.loads((ROOT / "benches/fast/w3c-expected.toml").read_text(encoding="utf-8"))["suite"]
        self.assertEqual(expected["OWL 2 DL"], {"passed": 410, "disputed": 1})
        self.assertEqual(expected["OWL 2 RL"]["passed"], 86)
        self.assertEqual(expected["OWL 2 RL"]["skipped"], 3)

    def test_the_jobs_run_the_whole_gate_on_its_hosts_without_a_priority_of_their_own(self):
        found = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 4, None)
        build, timed, checks = found["rg-test-build"], found["rg-test-timed"], found["rg-test-checks"]
        self.assertEqual((build["kind"], build["requires"]["hosts"]), ("owlrs.build", ["phuoc-yu"]))
        self.assertEqual((timed["kind"], timed["class"], timed["after"]), ("owlrs.timed", "exclusive", ["rg-test-build"]))
        self.assertEqual(checks["requires"]["hosts"], ["office-pc"])
        sides = [step[4] for step in timed["steps"] if step[2] == "pass"]
        self.assertEqual(sides, ORDER)
        self.assertEqual(timed["steps"][-2][2:4], ["report", "c" * 40])
        self.assertEqual([s[2] for s in checks["steps"]], ["w3c", "guards", "clean"])
        for job in found.values():
            self.assertNotIn("priority", job)
            self.assertEqual(job["priority_class"], "merge-gate")
        text = gate_module.toml_job(timed)
        import tomllib
        self.assertEqual(tomllib.loads(text)["steps"], timed["steps"])
        with self.assertRaises(ValueError):
            gate_module.schedule(3)

    def test_the_big_cases_run_on_khaleesi_on_the_builds_binaries(self):
        found = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 4, None)
        build, big, timed = found["rg-test-build"], found["rg-test-big"], found["rg-test-timed"]
        self.assertEqual(build["steps"][-1][2], "pack")
        self.assertEqual(build["outputs"], [{"name": "binaries", "path": "binaries"}])
        self.assertEqual((big["kind"], big["class"], big["requires"]["hosts"]), ("owlrs.timed", "exclusive", ["khaleesi"]))
        self.assertGreaterEqual(big["requires"]["ram_gb"], 28)
        self.assertEqual([s[2] for s in big["steps"][:2]], ["unpack", "prepare"])
        self.assertNotIn("build", [s[2] for s in big["steps"]])  # khaleesi builds nothing
        self.assertIn({"name": "binaries", "source": {"artifact": "rg-test-build/binaries"}}, big["inputs"])
        self.assertEqual((big["env"]["GATE_MIN_GB"], big["env"]["GATE_MAX_GB"]), ("24", "28"))
        self.assertEqual((timed["env"]["GATE_BIG_GB"], timed["env"]["GATE_BIG_HOST"]), ("28", "khaleesi"))
        self.assertEqual([s[4] for s in big["steps"] if s[2] == "pass"], ORDER)
        import tomllib
        self.assertEqual(tomllib.loads(gate_module.toml_job(build))["outputs"], build["outputs"])
        self.assertNotIn("rg-test-big", gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 4, None,
                                                         big_host=None))

    def test_the_command_line_plans_and_splits_into_blocks(self):
        # 19 Oct 2026: a local named `plan` in main() broke `release_gate.py plan` (a gate's prepare).
        import subprocess
        script = ROOT / "benches/fast/release_gate.py"
        done = subprocess.run([sys.executable, "-B", str(script), "plan", "--max-gb", "24"], capture_output=True,
                              text=True, encoding="utf-8")
        self.assertEqual(done.returncode, 0, done.stderr[-500:])
        plan = json.loads(done.stdout)
        self.assertTrue(plan["run"])
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "plan.json").write_text(done.stdout, encoding="utf-8")
            (Path(tmp) / "measure.toml").write_text(f'[case.{plan["run"][0]}]\npasses = 6\n', encoding="utf-8")
            done = subprocess.run([sys.executable, "-B", str(script), "blocks", "--plan", str(Path(tmp) / "plan.json"),
                                   "--measure", str(Path(tmp) / "measure.toml"), "--out", tmp],
                                  capture_output=True, text=True, encoding="utf-8")
            self.assertEqual(done.returncode, 0, done.stderr[-500:])
            third = json.loads((Path(tmp) / "plan-block-3.json").read_text(encoding="utf-8"))
        self.assertEqual(third["run"], plan["run"][:1])

    def test_an_empty_case_selection_is_refused_never_every_case(self):
        from types import SimpleNamespace
        with self.assertRaises(SystemExit):
            fast.select([{"name": "a", "area": "query"}], SimpleNamespace(cases="", areas=None))
        self.assertEqual(len(fast.select([{"name": "a", "area": "query"}], SimpleNamespace(cases=None, areas=None))), 1)

    def test_layouts_rotate_within_each_side_and_every_side_runs_them_alike(self):
        found = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 4, None, big_host=None, layouts=2)
        timed = found["rg-test-timed"]
        passes_ = [s for s in timed["steps"] if s[2] == "pass"]
        self.assertEqual(found["rg-test-build"]["env"]["GATE_LAYOUTS"], "2")
        by_side = {}
        for s in passes_:
            by_side.setdefault(s[4], []).append(int(s[8]) if len(s) > 8 else 1)
        # Each block of two passes a side runs both layouts, the first alternating.
        self.assertEqual(by_side, {"base": [1, 2, 2, 1], "main": [1, 2, 2, 1], "cand": [1, 2, 2, 1]})
        self.assertEqual(passes_[0][2:], ["pass", "1", "base", gate_module.BASE, "c" * 40])

        def records_of(side, n):
            return [record()]
        ps = passes(records_of)
        for p, layout in zip(ps, gate_module.layouts_of(ORDER, 2)):
            p["report"]["binaries"] = {**p["report"]["binaries"], "layout": layout,
                                       "sha256": {"release/examples/perf_lab": f"{p['side']}{layout}"}}
            p["report"]["binaries_finished"] = copy.deepcopy(p["report"]["binaries"])
        self.assertEqual(run_gate(ps)["errors"], [])
        ps[2]["report"]["binaries"]["layout"] = ps[2]["report"]["binaries_finished"]["layout"] = 2
        errors = run_gate(ps)["errors"]
        self.assertTrue(any("different layouts" in e for e in errors), errors)

    def test_report_writes_markdown_and_json(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            for p in passes():
                (directory / p["path"]).write_text(json.dumps(p["report"]), encoding="utf-8")
            loaded = gate_module.load_passes(directory)
            self.assertEqual([p["side"] for p in sorted(loaded, key=lambda p: p["n"])], ORDER)
            summary = run_gate(loaded)
            text = gate_module.render(summary)
            self.assertIn("**PASSED.**", text)
            self.assertIn("## Against main", text)



class NoiseReduction(unittest.TestCase):
    """benches/NOISY.md: the noise reduced in the harness, not tolerated (18 Oct 2026)."""

    def blocks_passes(self, extra_case="lab-yago-tiny", sides_in_block3=("base", "main", "cand")):
        """Two blocks of both cases, a third of one case only (its noise needs 6 passes a side)."""
        order = ORDER + ["base", "main", "cand", "cand", "main", "base"]

        def records_of(side, n):
            out = [record()]
            if n <= 12 or side in sides_in_block3:
                out.append(record(case=extra_case))
            return out
        return passes(records_of, order=order)

    def test_a_case_whose_noise_needs_more_passes_is_judged_on_its_block_s_passes(self):
        summary = run_gate(self.blocks_passes())
        self.assertEqual(summary["errors"], [])
        self.assertTrue(summary["passed"])
        row = summary["comparisons"]["base"]
        self.assertEqual(row["lab-yago-tiny"]["dimensions"]["time/q01"]["passes"], [6, 6])
        self.assertEqual(row["lab-olympics"]["dimensions"]["time/q01"]["passes"], [6, 6])
        # A pass without the case is no missing record of it.
        self.assertEqual(gate_module.evidence(self.blocks_passes()[12:], "lab-yago-tiny")["status"],
                         ["ok"] * 6)

    def test_a_later_block_must_give_every_side_the_case_alike(self):
        summary = run_gate(self.blocks_passes(sides_in_block3=("cand",)))
        self.assertTrue(any(e.startswith("lab-yago-tiny: ") for e in summary["errors"]), summary["errors"])
        self.assertFalse(summary["passed"])

    def test_blocks_follow_the_calibration_and_the_jobs_run_them(self):
        measure = {"a": {"passes": 4, "seconds": 10}, "b": {"passes": 8, "seconds": 5}, "c": {"passes": 6, "seconds": 1}}
        self.assertEqual(gate_module.blocks(["a", "b", "c"], measure), [["a", "b", "c"], ["a", "b", "c"], ["b", "c"], ["b"]])
        found = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 4, None, measure=measure)
        timed = found["rg-test-timed"]
        self.assertEqual(timed["steps"][0][2:], ["plan-blocks", "c" * 40])
        pass_steps = [s for s in timed["steps"] if s[2] == "pass"]
        self.assertEqual([s[4] for s in pass_steps], ORDER * 2)
        self.assertEqual({s[7] for s in pass_steps[18:]}, {"plan-block-4.json"})
        self.assertNotIn("GATE_BATCH", timed["env"])
        self.assertEqual(gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "x", 4, None, measure=measure,
                                          batch=True)["x-timed"]["env"]["GATE_BATCH"], "1")

    def test_the_passes_measure_alike_and_the_host_s_policy_holds(self):
        ps = passes()
        ps[3]["report"]["options"] = {**ps[3]["report"]["options"], "measure": "a" * 64}
        self.assertIn("the passes measured with different calibrations (fast.py --measure)", run_gate(ps)["errors"])
        ps = passes()
        state = {"frequency": {"driver": ["amd-pstate-epp"], "governor": ["performance"], "boost": "1"},
                 "problems": ["other users busy: gnome-shell 1.0"]}
        for p in ps:
            p["report"]["host_state"] = copy.deepcopy(state)
        ps[5]["report"]["host_state"]["frequency"]["governor"] = ["powersave"]
        summary = run_gate(ps)
        self.assertTrue(any("frequency policy changed" in e for e in summary["errors"]))
        self.assertEqual(summary["host_notes"], ["other users busy: gnome-shell (passes 1, 2, 3, 4, 5, 6, 7, 8, 9, "
                                                 "10, 11, 12)"])
        self.assertIn("## The host as the passes met it", gate_module.render(summary))
        # A process that waited for a CPU or I/O (its scope's pressure stalls) is named.
        ps = passes()
        ps[8]["report"]["records"][0].update(wall_s=10.0)
        ps[8]["report"]["records"][0]["observations"][0]["stall_ms"] = {"cpu": 900.0, "io": 3.0, "memory": 0.0}
        self.assertEqual(run_gate(ps)["host_notes"], ["lab-olympics in pass 9 (cand) waited cpu 900 ms of 10.0 s"])
        self.assertEqual(fast.scope_stalls_ms("x\nfast-scope-pressure cpu=1500 io=250000 memory=\n"),
                         {"cpu": 1.5, "io": 250.0})

    def test_a_series_batched_on_both_sides_is_judged_at_its_finer_resolution(self):
        def records_of(side, n):
            r = record(samples={"q01": [0.0090, 0.0091, 0.0090] if side == "cand" else [0.0088, 0.0088, 0.0089]})
            if side != "main":
                r["observations"][0]["batched"] = {"q01": 50}
            return [r]
        ps = passes(records_of)
        summary = run_gate(ps)
        # 2 % slower, 0.2 µs: below the unit's 5 µs; batched 50 times on v1.5's side and the
        # head's, a loss; main can't batch, so against main the 5 µs stays.
        self.assertEqual(summary["comparisons"]["base"]["lab-olympics"]["dimensions"]["time/q01"]["verdict"],
                         "regression")
        self.assertEqual(summary["comparisons"]["main"]["lab-olympics"]["dimensions"]["time/q01"]["verdict"],
                         "neutral")

    def test_a_series_in_coarse_steps_is_judged_at_its_step(self):
        # An older perf lab's open_s (seconds to three decimals): 4 or 5 ms, one step apart.
        def records_of(side, n):
            return [record(samples={"open": [5.0, 5.0, 4.0] if side == "cand" else [4.0, 4.0, 5.0]})]
        summary = run_gate(passes(records_of))
        self.assertEqual(summary["comparisons"]["base"]["lab-olympics"]["dimensions"]["time/open"]["verdict"],
                         "neutral")
        self.assertEqual(gate_module.quantum([[[4.0, 5.0]], [[17.4, 5.5]]]), 0.1)
        self.assertEqual(gate_module.quantum([[[0.0091, 4.0]]]), 0.0)

    def test_calibrate_buys_runs_where_cheap_and_passes_where_noise_needs_them_within_budget(self):
        # Power at 4 passes falls from 1 (sd 0.1 %) to 0 (sd 10 %); 16 passes hold it to 2.5 %.
        table = {"grid": [0.001, 0.1], **{str(n): [1.0, max(0.0, (n - 4) / 12 * 0.9)] for n in range(4, 17, 2)}}
        quiet = [10.0, 10.01, 9.99, 10.0]
        noisy = [10.0, 12.0, 8.5, 11.0]

        def records_of(side, n):
            i = (n - 1) % 4
            return [record(case="lab-olympics", samples={"q01": [quiet[i]] * 5}),
                    record(case="q-paths", samples={"broader-closure": [noisy[i]] * 5})]
        ps = passes(records_of)
        for p in ps:
            for r in p["report"]["records"]:
                r["case_s"] = 10.0
        catalog = [{"name": "lab-olympics", "metric": "sum_p50_ms", "queries": "@olympics", "args": []},
                   {"name": "q-paths", "metric": "sum_p50_ms", "queries": "q-paths", "args": ["--runs", "10"]}]
        # Four passes a side of both cases: 2 blocks x 2 x (3 sides x 20 s + 3 x 40 s) = 12 min.
        cheap = gate_module.calibrate(catalog, ps, budget_min=13, table=table, batch=False)["cases"]
        # Batched: only a short query of a case with long ones, its samples 50 µs at least.
        short = [record(case="lab-olympics", samples={"q01": [10.0] * 5, "q02": [0.008] * 5})]
        batched = gate_module.calibrate(catalog[:1], passes(lambda side, n: short), budget_min=60, table=table)
        self.assertEqual({k: batched["cases"]["lab-olympics"][k] for k in ("batch", "batch_queries")},
                         {"batch": 7, "batch_queries": ["q02"]})
        self.assertEqual(cheap["lab-olympics"]["runs"], 200)   # 2 s of 10 ms executions
        self.assertEqual(cheap["lab-olympics"]["warmup"], 3)
        self.assertEqual(cheap["lab-olympics"]["passes"], 4)
        self.assertEqual(cheap["q-paths"]["passes"], 4)        # 13 min: no room beyond four
        rich = gate_module.calibrate(catalog, ps, budget_min=120, table=table, batch=False)
        self.assertEqual(rich["cases"]["lab-olympics"]["passes"], 4)
        self.assertGreater(rich["cases"]["q-paths"]["passes"], 4)
        self.assertLessEqual(rich["minutes"], 120)
        text = gate_module.write_measure(rich, "a test")
        import tomllib
        self.assertEqual(tomllib.loads(text)["case"]["q-paths"]["passes"], rich["cases"]["q-paths"]["passes"])

    def test_fast_applies_the_calibration_alike_and_pools_repeated_processes(self):
        case = {"name": "x", "tool": "perf_lab", "args": ["--runs", "5", "--qerror"], "repeat": 1}
        with patch.dict(fast.MEASURE, {"x": {"runs": 41, "warmup": 3, "batch": 7, "repeat": 2}}, clear=True),                 patch.dict(fast.OPTIONS, {"batch": False}):
            got = fast.measured(case)
        self.assertEqual(got["args"], ["--runs", "41", "--qerror", "--warmup", "3"])
        self.assertEqual(got["repeat"], 2)
        with patch.dict(fast.MEASURE, {"x": {"batch": 7}}, clear=True), patch.dict(fast.OPTIONS, {"batch": True}):
            self.assertEqual(fast.measured(case)["args"][-2:], ["--batch", "7"])
        with patch.dict(fast.MEASURE, {"x": {"batch": 7, "batch_queries": ["q01", "q07"]}}, clear=True),                 patch.dict(fast.OPTIONS, {"batch": True}):
            self.assertEqual(fast.measured(case)["args"][-4:], ["--batch", "7", "--batch-queries", "q01,q07"])
        unlisted = {"name": "y", "args": ["--runs", "5"]}
        with patch.dict(fast.MEASURE, {}, clear=True):
            self.assertIs(fast.measured(unlisted), unlisted)
        result = {"queries": [{"name": "q", "samples_ms": [3.0]}],
                  "_repeats": [{"queries": [{"name": "q", "samples_ms": [1.0, 2.0]}]},
                               {"queries": [{"name": "q", "samples_ms": [3.0]}]}]}
        self.assertEqual(fast.samples({"metric": "sum_p50_ms"}, result), {"q": [1.0, 2.0, 3.0]})

if __name__ == "__main__":
    unittest.main()
