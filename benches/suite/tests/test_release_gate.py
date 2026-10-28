"""The release gate (benches/fast/release_gate.py, suitekit/gatestats.py) and the block runs of
fast.py it relies on.

Each defect of the audits has a test here. 18 Oct 2026: a gate passed with one baseline and
overlapping timestamps; main's perf lab reports no samples (its side compared by per-process
p50s); the cgroup's peak counts page cache. Research round 8 (19 Oct 2026): a palindrome of
whole passes isn't time-balanced; the layout rotation skipped layouts; layouts weren't the
statistical unit; a loss first seen while confirming another was dropped; a difference within
the counter's step was neutral, untested; the Windows peak was mislabelled; the big host was
native Windows. lks (20 Oct 2026): blocks timing one cached instance of a build share its
offset, so a round's blocks weren't independent; the order was fixed in advance; no test knew the
host's noise between instances.
"""
from __future__ import annotations

import copy
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
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
from suitekit import gatestats  # noqa: E402

HARNESS = "h" * 40
REVS = {"cand": "c" * 40, "base": "b" * 40, "main": "m" * 40}
SIDES = ("base", "main", "cand")
MACHINE = {"host": "phuoc-yu", "cpu": "Ryzen", "governor": "performance"}
CONTEXT = {"host": "phuoc-yu", "round": 0, "prefix": "rg-cccccccc", "max_gb": 24.0, "ram_gb": 24.0}


def observation(samples, peak=500, cgroup=900, estimator="samples", rows=None, phases=None, anon=None):
    out = {"identity": {"sweep": 0, "repeat": 0, "cache": "warm"}, "exit": 0, "samples": samples,
           "estimator": estimator, "peak_mib": peak, "phase_peaks": phases or {}, "cgroup_peak_mib": cgroup,
           "rows": rows or {q: 5 for q in samples}, "checks": [], "routes": []}
    if anon is not None:
        out["anon_kernel_peak_bytes"] = anon
    return out


def record(case="lab-olympics", samples=None, status="ok", expected="ok", **kw):
    samples = samples if samples is not None else {"q01": [10.0, 10.1, 9.9, 10.0, 10.2], "q06": [4.8, 4.9, 4.8]}
    obs = observation(samples, **kw)
    return {"case": case, "rep": 1, "status": status, "expected_outcome": expected, "checks": [], "routes": [],
            "samples": samples, "observations": [obs], "peak_mib": obs["peak_mib"], "notes": [], "counters": {}}


def report(side, n, records, options=None):
    rev = REVS[side]
    binaries = {"revision": rev, "verified": True, "sha256": {"release/examples/perf_lab": side * 8}}
    return {"label": f"gate-{n}-{side}", "started": "2026-10-20T01:00:00", "finished": "2026-10-20T01:10:00",
            "reps": 1, "planned_cases": sorted({r["case"] for r in records}), "records": records,
            "git": {"commit": HARNESS, "dirty": 0}, "git_finished": {"commit": HARNESS, "dirty": 0},
            "binaries": binaries, "binaries_finished": copy.deepcopy(binaries), "machine": MACHINE,
            "instance": {"built": f"/gate/bin-{side}", "inode": n},
            "runtime": "native", "prepared": {"manifest_sha256": "p" * 64, "files": 3, "changed": []},
            "options": options or {"prepared": True, "routes": True, "empty_queries": False}}


def passes(records_of=None, blocks=2, layouts=None, sides=SIDES, order_of=None):
    """A round as fast.py block writes it: per block one report per side and run, each case's six
    runs back to back as a palindrome (`order_of(block)` another order). records_of(side, n) gives
    a run's records, n its pass number (6 (block - 1) + its place)."""
    records_of = records_of or (lambda side, n: [record()])
    out, t = [], 1000
    for b in range(1, blocks + 1):
        layout = layouts[b - 1] if layouts else 1
        order = order_of(b) if order_of else fast.palindrome(list(sides), b)
        runs = {}
        for i, (side, run) in enumerate(order):
            n = len(order) * (b - 1) + i + 1
            runs[(side, run)] = (n, report(side, n, records_of(side, n)))
        for case in dict.fromkeys(r["case"] for n, rep in runs.values() for r in rep["records"]):
            for side, run in order:
                for rec in runs[(side, run)][1]["records"]:
                    if rec["case"] == case:
                        rec.update(started_ns=t, finished_ns=t + 10, block=b, layout=layout, side=side, run=run)
                        t += 10
        for (side, run), (n, rep) in runs.items():
            rep.update(block=b, run=run, layout=layout, side=side,
                       started_ns=min(r["started_ns"] for r in rep["records"]),
                       finished_ns=max(r["finished_ns"] for r in rep["records"]))
            if layout > 1:
                rep["binaries"] = {**rep["binaries"], "layout": layout,
                                   "sha256": {"release/examples/perf_lab": f"{side}{layout}"}}
                rep["binaries_finished"] = copy.deepcopy(rep["binaries"])
            out.append({"n": n, "side": side, "path": f"pass-{n}-{side}.json", "report": rep})
        t += 100
    return sorted(out, key=lambda p: p["n"])


def run_gate(ps, sides=("cand", "base", "main"), require=("base", "main"), **kw):
    kw.setdefault("context", CONTEXT)
    return gate_module.gate(ps, {s: REVS[s] for s in sides}, "cand", list(require), HARNESS, **kw)


def worse(factor, n):
    """The candidate's run n: `factor` give or take a few percent of it, so blocks' contrasts
    differ (identical ones tie: their p is 2^-blocks); no difference stays none."""
    return factor ** (1 + 0.05 * ((n * 7) % 5 - 2))


def slower(factor, series="q06", base=(4.8, 4.85, 4.8)):
    """records_of: the candidate's `series` about `factor` times the baselines'."""
    def records_of(side, n):
        values = [v * (worse(factor, n) if side == "cand" else 1) for v in base]
        return [record(samples={"q01": [10.0, 10.1, 9.9], series: values})]
    return records_of


class Schedule(unittest.TestCase):
    def test_identical_sides_pass(self):
        summary = run_gate(passes())
        self.assertEqual((summary["errors"], summary["blocking"], summary["flagged"]), ([], [], []))
        self.assertTrue(summary["passed"])

    def test_one_baseline_never_passes_a_gate_that_requires_two(self):
        summary = run_gate(passes(sides=("base", "cand")), sides=("cand", "base"))
        self.assertIn("required baselines missing: main", summary["errors"])
        self.assertFalse(summary["passed"])

    def test_each_case_s_runs_are_a_back_to_back_palindrome(self):
        """Round 8: a palindrome of whole passes isn't time-balanced (a case's runs on each side
        sit where the other cases' durations put them); its six runs back to back, mirrored, are."""
        def two(side, n):
            return [record(), record(case="kernel-intersect", samples={"triangles": [50.0]})]
        whole_passes = passes(two)
        t = 10 ** 6
        for p in whole_passes:  # each run runs every case before the next run starts
            for r in p["report"]["records"]:
                r.update(started_ns=t, finished_ns=t + 10)
                t += 10
            p["report"].update(started_ns=p["report"]["records"][0]["started_ns"], finished_ns=t)
        self.assertTrue(any("aren't back to back" in e for e in run_gate(whole_passes)["errors"]))
        cyclic = passes(order_of=lambda b: [(s, 1) for s in SIDES] + [(s, 2) for s in SIDES])
        self.assertTrue(any("not a palindrome" in e for e in run_gate(cyclic)["errors"]))

    def test_overlapping_blocks_are_refused(self):
        ps = passes()
        for p in ps[6:]:
            for r in p["report"]["records"]:
                r["started_ns"] -= 130
                r["finished_ns"] -= 130
        self.assertIn("blocks 1 and 2 overlap", run_gate(ps)["errors"])

    def test_fewer_blocks_than_the_round_are_diagnostic_only(self):
        one = passes(blocks=1)
        self.assertTrue(any("1 blocks, at least 2" in e for e in run_gate(one)["errors"]))
        diagnostic = run_gate(one, min_blocks=1)
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
        for text in ("dirty 2", "expected " + REVS[ps[4]["side"]], "prepared data changed", "unfinished pass"):
            self.assertIn(text, errors)


class Layouts(unittest.TestCase):
    def test_any_k_blocks_in_a_row_run_every_layout(self):
        # Round 8: the old rotation never ran a third of the layouts when 3 divided K.
        for k in range(2, 10):
            for first in range(1, k + 1):
                self.assertEqual({gate_module.block_layout(b, k) for b in range(first, first + k)},
                                 set(range(1, k + 1)), (k, first))

    def test_the_layout_is_the_unit_and_one_build_s_blocks_are_their_own(self):
        """Round 8: blocks of one layout aren't independent (its effect, tau^2 / K, stays however
        many repeat it): its blocks' mean is one unit. One build per side: each block is one."""
        contrasts = [0.020, 0.021, -0.010, -0.011]
        self.assertEqual(gatestats.loss_test(contrasts, [1, 1, 2, 2])["units"], 2)
        self.assertEqual(gatestats.loss_test(contrasts, [1, 2, 3, 4])["units"], 4)
        self.assertEqual(gatestats.loss_test(contrasts, [1, 1, 1, 1])["units"], 4)

    def test_layout_components_are_a_revision_s_own(self):
        """Round 8: gate-power.py's layout split pooled revisions, so their difference passed for a
        layout's."""
        spec_ = importlib.util.spec_from_file_location("gate_power", ROOT / "benches/probes/gate-power.py")
        probe = importlib.util.module_from_spec(spec_)
        spec_.loader.exec_module(probe)
        runs = {("a" * 40, 1): [10.0, 10.1], ("a" * 40, 2): [10.0, 10.1],
                ("b" * 40, 1): [20.0, 20.1], ("b" * 40, 2): [10.0, 10.1]}
        with tempfile.TemporaryDirectory() as tmp:
            paths = []
            for (rev, layout), values in runs.items():
                for i, v in enumerate(values):
                    path = Path(tmp) / f"{rev[0]}{layout}{i}.json"
                    path.write_text(json.dumps({"binaries": {"revision": rev, "layout": layout}, "records": [
                        {"case": "lab-olympics", "status": "ok", "samples": {"q01": [v]}}]}), encoding="utf-8")
                    paths.append(path)
            rows = {r["binary"]: r["layout_sd"] for r in probe.layout_components(paths, 0.001)}
        self.assertLess(rows["aaaaaaaa"], 0.001)
        self.assertGreater(rows["bbbbbbbb"], 0.3)


class Rounds(unittest.TestCase):
    W3C = {"passed": True, "errors": []}
    GUARDS = {"passed": True, "errors": [], "guards": 81}

    def round_(self, factor, k=0, **kw):
        return run_gate(passes(slower(factor), **kw), context={**CONTEXT, "round": k})

    def test_a_loss_needs_two_rounds_and_the_family_s_holm(self):
        first = self.round_(1.08)
        self.assertEqual({(f["side"], f["dimension"]) for f in first["flagged"]},
                         {("base", "time/q06"), ("main", "time/q06")})
        passed, text = gate_module.verdict([first], self.W3C, self.GUARDS)
        self.assertFalse(passed)
        self.assertIn("**pending**", text)
        with patch.object(gate_module, "checkout_rev", lambda checkout, cand, harness: harness):  # no repository
            job = gate_module.confirm_job([first], "C:/repo")
        self.assertEqual((job["id"], job["requires"]["hosts"]), ("rg-cccccccc-confirm", ["phuoc-yu"]))
        self.assertEqual(job["steps"][0][2:], ["plan-cases", "lab-olympics"])
        self.assertEqual({s[2] for s in job["steps"][1:-2]}, {"block"})
        confirmed, text = gate_module.verdict([first, self.round_(1.08, 1)], self.W3C, self.GUARDS)
        self.assertFalse(confirmed)
        self.assertIn("**confirmed loss**: lab-olympics against base: time/q06", text)
        self.assertTrue(gate_module.verdict([first, self.round_(1.0, 1)], self.W3C, self.GUARDS)[0])

    def test_a_loss_first_seen_while_confirming_another_is_confirmed_not_dropped(self):
        """Round 8: combine() followed only round 1's flags, so a loss round 2 showed passed."""
        family = ["base|a|time/q", "base|b|time/q"]
        r1 = {"family": family, "pvalues": {family[0]: 0.001, family[1]: 0.5}}
        r2 = {"pvalues": {family[0]: 0.001, family[1]: 0.0001}}
        d = gate_module.decide([[r1, r2]])
        self.assertEqual((d["losses"], d["pending"]), ([family[0]], [family[1]]))
        d = gate_module.decide([[r1, r2, {"pvalues": {family[1]: 0.0002}}]])
        self.assertEqual(sorted(d["losses"]), family)
        # Round 3 only where it could still decide: in a family of a thousand, a round-2 p of
        # 0.004 can't reach Holm's alpha / m.
        many = family + [f"base|c{i}|time/q" for i in range(1000)]
        r1 = {"family": many, "pvalues": {**{h: 0.5 for h in many}, family[0]: 0.001}}
        d = gate_module.decide([[r1, {"pvalues": {family[0]: 0.5, family[1]: 0.004}}]])
        self.assertEqual(d["pending"], [])

    def test_the_test_is_one_sided_holm_and_the_rounds_as_declared(self):
        self.assertAlmostEqual(gatestats.t_sf(2.015048, 5), 0.05, places=5)
        self.assertAlmostEqual(gatestats.t_sf(6.313752, 1), 0.05, places=5)
        self.assertAlmostEqual(gatestats.t_ppf(0.95, 5), 2.015048, places=4)
        faster = self.round_(0.8)
        q06 = faster["comparisons"]["base"]["lab-olympics"]["dimensions"]["time/q06"]
        self.assertGreater(q06["p"], 0.95)
        self.assertEqual(faster["flagged"], [])
        self.assertEqual(gatestats.holm({"a": 0.01, "b": 0.02, "c": 0.03}), {"a", "b", "c"})
        self.assertEqual(gatestats.holm({"a": 0.01, "b": 0.04, "c": 0.04}), {"a"})
        self.assertAlmostEqual(gatestats.combine_rounds([0.01, 0.02]), 0.02 / 0.9)
        self.assertAlmostEqual(gatestats.combine_rounds([0.5, 0.001, 0.001]), 0.01)

    def test_a_confirmation_runs_whole_squares_of_its_timed_sides(self):
        """Sol r2: the candidate is timed whatever revision it shares; only a main that is base's
        build isn't. Counting revisions gave three timed sides 2 blocks."""
        for cand, main, sides in (("b", "m", 3), ("m", "m", 3), ("b", "b", 2), ("c", "m", 3), ("c", "b", 2)):
            with self.subTest(cand=cand, main=main), patch.dict(REVS, {"cand": cand * 40, "base": "b" * 40, "main": main * 40}):
                summary = run_gate(passes(slower(1.08), blocks=6))
                for calibrated in (None, 3):
                    measure = {"lab-olympics": {"blocks": calibrated}} if calibrated else None
                    with patch.object(gate_module, "checkout_rev", return_value="x"):
                        job = gate_module.confirm_job([summary], "C:/repo", measure=measure)
                    n = len([s for s in job["steps"] if s[2] == "block"])
                    self.assertEqual(n % sides, 0, (cand, main, calibrated, n))
                    self.assertGreaterEqual(n, calibrated or gate_module.CONFIRM_BLOCKS[0])

    def test_nothing_flagged_needs_no_confirmation(self):
        clean = self.round_(1.0)
        self.assertIsNone(gate_module.confirm_job([clean], "C:/repo"))
        self.assertTrue(gate_module.verdict([clean], self.W3C, self.GUARDS)[0])


class Verdicts(unittest.TestCase):
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

    def test_stopping_at_a_declared_limit_where_a_baseline_finished_blocks(self):
        def records_of(side, n):
            if side == "cand":
                return [record(status="memory-limit", expected="memory-limit")]
            return [record(expected="memory-limit")]
        summary = run_gate(passes(records_of), min_blocks=1)
        self.assertTrue(any("stops at its declared outcome (memory-limit) where base completed" in b
                            for b in summary["blocking"]))

    def test_cases_not_run_block_and_those_left_to_the_big_host_must_be_timed_there(self):
        self.assertFalse(run_gate(passes(), not_run={"bsbm-bi-100m": "its cap is 28 GB"})["passed"])
        here = run_gate(passes(), elsewhere={"bsbm-bi-100m": "timed on khaleesi-wsl"})
        ok = {"passed": True, "errors": [], "guards": 81}
        w3c = {"passed": True, "errors": []}
        passed, text = gate_module.verdict([here], w3c, ok)
        self.assertFalse(passed)
        self.assertIn("NOT timed anywhere: bsbm-bi-100m", text)
        big = {**here, "planned": ["bsbm-bi-100m"], "elsewhere": {}, "context": {**CONTEXT, "host": "khaleesi-wsl"}}
        self.assertTrue(gate_module.verdict([here, big], w3c, ok)[0])

    def test_a_series_the_candidate_lost_blocks(self):
        def records_of(side, n):
            samples = {"q01": [10.0]} if side == "cand" else {"q01": [10.0], "q06": [4.8]}
            return [record(samples=samples)]
        self.assertTrue(any("time/q06: lost" in b for b in run_gate(passes(records_of))["blocking"]))

    def test_answers_that_differ_from_a_baseline_block_unless_a_check_pins_them(self):
        def records_of(side, n):
            return [record(rows={"q01": 5, "q06": 1000 if side == "main" else 1033})]
        unpinned = run_gate(passes(records_of))
        self.assertTrue(any("q06: rows [1033] here, [1000] on main: answers differ" in b for b in unpinned["blocking"]))
        pinned = run_gate(passes(records_of), catalog=[{"name": "lab-olympics", "check": {"q.q06.rows": 1033}}])
        self.assertTrue(pinned["passed"])


class MainAdapter(unittest.TestCase):
    """main f9b515e8's perf lab emits no samples_ms."""

    def main_result(self, p50=4.8):
        return {"label": "main", "load_s": 0.2, "peak_mib": 511, "sum_p50_ms": 15.0,
                "queries": [{"name": "q01", "rows": 5, "p50_ms": 10.0, "min_ms": 9.9, "max_ms": 10.2},
                            {"name": "q06", "rows": 1033, "p50_ms": p50, "min_ms": p50, "max_ms": p50}]}

    def test_fast_takes_main_s_p50_as_one_value_per_process(self):
        case = {"name": "lab-olympics", "metric": "sum_p50_ms", "queries": "@olympics"}
        self.assertEqual(fast.samples(case, self.main_result()), {"q01": [10.0], "q06": [4.8]})
        self.assertEqual(fast.estimator(self.main_result()), "process-p50")

    def test_without_routes_unreported_values_are_unchecked_not_wrong(self):
        result = self.main_result()
        with patch.dict(fast.OPTIONS, {"routes": False}):
            value = fast.check("q.q06.value", 1033, result, {}, {})
            route = fast.route("q.q06.operators", "has dictionary string test", result)
        self.assertIsNone(value["ok"])
        self.assertIsNone(route["ok"])
        self.assertFalse(fast.check("q.q06.value", 1033, result, {}, {})["ok"])  # with routes: wrong

    def test_main_is_compared_by_process_p50s_on_both_sides(self):
        def records_of(side, n):
            if side == "main":
                return [record(samples={"q01": [10.0], "q06": [4.8]}, estimator="process-p50")]
            return [record(samples={"q01": [10.0, 10.1, 9.9], "q06": [17.0, 18.0, 30.0]})]
        row = run_gate(passes(records_of))["comparisons"]["main"]["lab-olympics"]
        self.assertIn("one p50 per process", " ".join(row["notes"]))
        self.assertAlmostEqual(row["dimensions"]["time/q06"]["new"], 18.0)  # the process p50, not the pool

    def test_a_prepared_run_generates_nothing(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(fast, "FAST_DATA", Path(tmp)), \
                patch.object(fast, "RUNTIME", "native"), patch.dict(fast.OPTIONS, {"prepared": True}), \
                patch.object(fast, "container") as container, \
                patch.object(fast, "native_dirs", lambda: {"/fast": Path(tmp), "/target": Path(tmp)}):
            with self.assertRaisesRegex(RuntimeError, "missing input: social people=10 was not prepared"):
                fast.ensure_data("social people=10")
            container.assert_not_called()

    def test_a_query_set_kept_with_the_data_is_in_the_case_manifest_by_its_name(self):
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
    def test_the_scope_s_anon_and_kernel_memory_is_gated_its_page_cache_reported(self):
        summary = run_gate(passes(lambda side, n: [record(cgroup=5000 if side == "cand" else 1000,
                                                          anon=int((worse(2, n) if side == "cand" else 1) * 10 ** 9))]))
        row = summary["comparisons"]["base"]["lab-olympics"]
        self.assertEqual(row["reported"]["cgroup_peak_mib"], [1000, 5000])
        self.assertNotIn("memory/cgroup", row["dimensions"])
        self.assertIn(("base", "memory/anon_kernel"), {(f["side"], f["dimension"]) for f in summary["flagged"]})
        self.assertEqual(fast.anon_kernel_bytes("anon 1000\nfile 99999\nkernel 200\n"), 1200)
        self.assertEqual(fast.anon_kernel_bytes("anon 10\nfile 5\nslab 5\nkernel_stack 2\n"), 17)

    def test_a_difference_within_the_counter_s_step_is_tested_never_neutral(self):
        """Round 8: below the resolution the old judge said neutral without a test. A consistent
        loss of one step is a loss; an inconsistent one is `unresolved`, reported."""
        def dim(cand):
            blocks = list(range(1, len(cand) + 1))
            return gate_module.dimension([[500.0]] * len(cand), blocks, [[v] for v in cand], blocks, {}, "MiB")
        self.assertEqual(dim([501.0] * 6)["status"], "loss shown")  # tied: p = 2^-6
        self.assertEqual(dim([501.0] * 2)["p"], 0.25)  # never 0 (two tied blocks were p = 0 in both rounds)
        unresolved = dim([501.0, 500.0, 500.0])
        self.assertEqual(unresolved["status"], "unresolved")
        self.assertGreater(unresolved["p"], 0.045)


class Evidence(unittest.TestCase):
    def test_a_declared_hypothesis_without_candidate_evidence_fails_closed(self):
        """Sol, 20 Oct 2026: with the baseline's memory reading and none from the candidate, the
        hypothesis went untested and the gate passed a confirmed loss. A reading lost on the
        candidate blocks; a baseline without the metric is stated, not missing."""
        dims = ("time/q01", "time/q06", "memory/anon_kernel")
        family = [f"{s}|lab-olympics|{d}" for s in ("base", "main") for d in dims]
        rows = [  # case, the baseline's reading, the candidate's per pass n, the declared family's extra
            ("complete", 1_000_000, lambda n: 2_000_000, []),
            ("absent", 1_000_000, lambda n: None, []),
            ("partial", 1_000_000, lambda n: None if n % 3 == 0 else 2_000_000, []),
            ("unsupported by the baseline", None, lambda n: 2_000_000, []),
            ("declared, never measured", 1_000_000, lambda n: 1_000_000, ["base|lab-olympics|time/q99"]),
        ]
        out = {}
        for name, base_anon, cand_anon, extra in rows:
            def records_of(side, n, base_anon=base_anon, cand_anon=cand_anon):
                anon = cand_anon(n) if side == "cand" else base_anon
                return [record(samples={"q01": [10.0, 10.1, 9.9], "q06": [4.8, 4.85, 4.8]}, anon=anon)]
            s = run_gate(passes(records_of, blocks=4), family=family + extra)
            memory = "base|lab-olympics|memory/anon_kernel"
            out[name] = (memory in s["pvalues"], any("memory/anon_kernel" in b for b in s["blocking"]),
                         any("no evidence from the candidate" in e for e in s["errors"]),
                         "unsupported" in s["not_compared"].get(memory, ""))
        self.assertEqual(out, {
            "complete": (True, False, False, False),
            "absent": (False, True, False, False),
            "partial": (False, True, False, False),
            "unsupported by the baseline": (False, False, False, True),
            "declared, never measured": (True, False, True, False),
        })


class Parts(unittest.TestCase):
    def test_plan_never_lowers_a_cap(self):
        catalog = [{"name": "small", "cap_gb": 4}, {"name": "bsbm-bi-100m", "cap_gb": 28},
                   {"name": "huge", "cap_gb": 60}]
        run, not_run, elsewhere = gate_module.plan(catalog, 24)
        self.assertEqual(run, ["small"])
        self.assertIn("its cap is 28 GB", not_run["bsbm-bi-100m"])
        run, not_run, elsewhere = gate_module.plan(catalog, 24, big_gb=28)
        self.assertEqual((run, sorted(not_run)), (["small"], ["huge"]))
        self.assertIn("timed on khaleesi-wsl", elsewhere["bsbm-bi-100m"])
        run, not_run, elsewhere = gate_module.plan(catalog, 28, min_gb=24)  # the big host's share
        self.assertEqual(run, ["bsbm-bi-100m"])

    def test_guards_are_read_from_performance_md_and_must_each_pass(self):
        names = gate_module.guard_names((ROOT / "docs/design/performance.md").read_text(encoding="utf-8"))
        self.assertIn("ordered_dedup_uses_any_complete_sort_order_without_gathering", names)
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

    def test_the_jobs_run_the_whole_gate_in_blocks_on_its_hosts(self):
        found = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 2, None)
        timed, checks = found["rg-test-timed"], found["rg-test-checks"]
        self.assertEqual((timed["kind"], timed["class"], timed["after"]), ("owlrs.timed", "exclusive", ["rg-test-build"]))
        self.assertEqual(checks["requires"]["hosts"], ["office-pc"])
        blocks_ = [s for s in timed["steps"] if s[2] == "block"]
        self.assertEqual([s[3] for s in blocks_], ["1", "2", "3"])  # 2 asked: one whole square of three sides
        self.assertEqual(blocks_[0][2:], ["block", "1", "c" * 40, "1", "-", f"base={gate_module.BASE}",
                                          f"main={gate_module.MAIN}", f"cand={'c' * 40}"])
        self.assertEqual(timed["env"]["GATE_MIN_BLOCKS"], "3")
        for job in found.values():
            self.assertNotIn("priority", job)
            self.assertEqual(job["priority_class"], "merge-gate")
        import tomllib
        self.assertEqual(tomllib.loads(gate_module.toml_job(timed))["steps"], timed["steps"])

    def test_the_big_cases_run_on_a_linux_host_never_native_windows(self):
        """Round 8: the big job went to khaleesi, native Windows, with the build's ELF binaries."""
        found = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 2, None)
        big = found["rg-test-big"]
        self.assertEqual(big["requires"]["hosts"], ["khaleesi-wsl"])
        self.assertEqual([s[2] for s in big["steps"][:2]], ["unpack", "prepare"])
        self.assertIn({"name": "binaries", "source": {"artifact": "rg-test-build/binaries"}}, big["inputs"])
        with self.assertRaises(SystemExit):
            gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 2, None, big_host="khaleesi")
        # And the step itself: on another system than Linux, unpack refuses.
        import shutil
        import subprocess
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("no bash")
        with tempfile.TemporaryDirectory() as tmp:
            shim = Path(tmp) / "shim"
            shim.mkdir()
            (shim / "uname").write_text("#!/bin/sh\necho MINGW64_NT-10.0\n", encoding="utf-8", newline="\n")
            (shim / "uname").chmod(0o755)
            env = {**__import__("os").environ, "NRESE_GATE_DIR": f"{tmp}/gate", "GATE_BIN": f"{tmp}/bin",
                   "FLEET_OUT": f"{tmp}/out", "FLEET_INPUT_BINARIES": tmp}
            env["PATH"] = str(shim) + __import__("os").pathsep + env["PATH"]
            done = subprocess.run([bash, "-c", f'PATH="$(cygpath -u "{shim}" 2>/dev/null || echo "{shim}"):$PATH" '
                                   f'exec bash scripts/release-gate.sh unpack'], cwd=ROOT, env=env,
                                  capture_output=True, text=True, encoding="utf-8")
        # The gate's own checkout (of this checkout's commit) was under the scratch directory.
        subprocess.run(["git", "worktree", "prune"], cwd=ROOT, capture_output=True)
        self.assertEqual(done.returncode, 2, done.stdout + done.stderr)
        self.assertIn("is not Linux", done.stdout)

    def test_a_block_of_two_sides_is_complete_with_its_four_reports(self):
        """20 Oct 2026: release-gate.sh block counted six reports a block whatever the sides, so an
        A-B-B-A of two (the agents' confirmation rounds) never completed."""
        import shutil
        import subprocess
        # Every python3 call the gate makes goes to this test's own interpreter: a gate host's is
        # 3.11 or later (tomllib), a machine running the suite may have an older python3.
        bash, python = shutil.which("bash"), sys.executable
        if bash is None:
            self.skipTest("no bash")
        head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip()
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp).as_posix()
            # fast.py block as it numbers its reports (2 per side a block); every other python3 call the real one.
            shim = Path(tmp) / "shim"
            shim.mkdir()
            (shim / "python3").write_text(f"""#!/usr/bin/env bash
case " $* " in
  *" benches/fast/fast.py block "*)
    n=0; out=; sides=()
    while [ $# -gt 0 ]; do case $1 in --block) n=$2 ;; --out) out=$2 ;; --side) sides+=("${{2%%:*}}") ;; esac; shift; done
    per=$(( 2 * ${{#sides[@]}} ))
    for k in $(seq 1 $per); do echo '{{"finished_ns": 1}}' > "$out/pass-$(( per * (n - 1) + k ))-x$k.json"; done ;;
  *) exec "{Path(python).as_posix()}" "$@" ;;
esac
""", encoding="utf-8", newline="\n")
            (shim / "python3").chmod(0o755)
            setup = f'''
. scripts/lib/gate-publish.sh
GATE={t}/gate GATE_BIN={t}/bin
b=$GATE_BIN/bin-test; mkdir -p "$b/release" "$GATE" {t}/out
echo tool > "$b/release/tool"; echo {head} > "$b/REVISION"; echo timing=watchdog > "$b/CAPABILITIES"
(cd "$b" && sha256sum release/tool > SHA256SUMS)
point {head} "$b"
echo '{{"run": ["lab-olympics"]}}' > "$GATE/plan.json"
'''
            env = {**os.environ, "NRESE_GATE_DIR": f"{t}/gate", "GATE_BIN": f"{t}/bin", "FLEET_OUT": f"{t}/out",
                   "GATE_QUIET": "0"}
            subprocess.run([bash, "-c", setup], cwd=ROOT, env=env, check=True, capture_output=True)
            done = subprocess.run([bash, "-c", f'PATH="$(cygpath -u "{shim}" 2>/dev/null || echo "{shim}"):$PATH" '
                                   f'exec bash scripts/release-gate.sh block 2 {head} 1 - base={head} main={head} '
                                   f'cand={head}'],
                                  cwd=ROOT, env=env, capture_output=True, text=True, encoding="utf-8")
            # An A-B-B-A's A' (benches/suite/abba.py) is A's build by design: timed, never skipped.
            aa = subprocess.run([bash, "-c", f'PATH="$(cygpath -u "{shim}" 2>/dev/null || echo "{shim}"):$PATH" '
                                 f'exec bash scripts/release-gate.sh block 3 {head} 1 - a={head} a2={head} b={head}'],
                                cwd=ROOT, env=env, capture_output=True, text=True, encoding="utf-8")
            reports = len(list(Path(tmp).glob("out/passes/pass-*.json"))) - 4  # block 2's: base and cand
            subprocess.run(["git", "worktree", "remove", "--force", f"{t}/gate/src-{head[:8]}"], cwd=ROOT,
                           capture_output=True)
        subprocess.run(["git", "worktree", "prune"], cwd=ROOT, capture_output=True)
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn("block 2: done", done.stdout)
        self.assertIn("main is the same build as another side", done.stdout)  # timed once, as base
        self.assertEqual((aa.returncode, reports), (0, 6), aa.stdout + aa.stderr)  # a, a2, b: twice each
        self.assertNotIn("same build", aa.stdout)

    def test_a_main_that_is_the_base_is_timed_once_and_declared_once(self):
        """Orchestrator, 20 Oct 2026: the jobs require a main, so a confirmation passes the base
        twice and every block timed it twice. The gate takes the base's runs for it, and its
        hypotheses aren't declared beside the base's."""
        sides = {"cand": REVS["cand"], "base": REVS["base"], "main": REVS["base"]}
        ps = passes(sides=("base", "cand"))
        for p in ps:
            if p["side"] == "base":
                for key in ("binaries", "binaries_finished"):
                    p["report"][key]["revision"] = REVS["base"]
        family = [f"{s}|lab-olympics|time/{q}" for s in ("base", "main") for q in ("q01", "q06")]
        s = gate_module.gate(ps, sides, "cand", ["base", "main"], HARNESS, context=CONTEXT, family=family)
        self.assertEqual((s["errors"], s["aliases"], sorted(s["family"])),
                         ([], {"main": "base"}, sorted(h for h in family if h.startswith("base|"))))
        self.assertTrue(s["passed"])
        self.assertIn("main is base's build", gate_module.render(s))

    def test_a_job_s_checkout_holds_the_candidate_and_the_harness_or_there_is_no_job(self):
        """Keepers, 20 Oct 2026: a confirmation's candidate rebased onto the harness was built in a
        checkout at the harness, which doesn't contain it, and the build failed with exit 2."""
        import subprocess
        with tempfile.TemporaryDirectory() as tmp:
            git = lambda *a: subprocess.run(["git", "-C", tmp, *a], capture_output=True, text=True,  # noqa: E731
                                            check=True).stdout.strip()
            git("init", "-q")
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "base")
            root = git("rev-parse", "HEAD")
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "harness")
            harness = git("rev-parse", "HEAD")
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "rebased candidate")
            rebased = git("rev-parse", "HEAD")
            git("checkout", "-q", "-b", "elsewhere", root)
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "diverged")
            diverged = git("rev-parse", "HEAD")

            def checkout_of(candidate):
                found = gate_module.jobs(candidate, harness, tmp, "rg-test", 2, None, base=root, main=root,
                                         checks=False, big_host=None)
                return found["rg-test-build"]["inputs"][0]["source"]["rev"]
            self.assertEqual((checkout_of(rebased), checkout_of(root)), (rebased, harness))
            with self.assertRaises(SystemExit) as refused:
                checkout_of(diverged)
            self.assertIn("contains the other", str(refused.exception))

    def test_a_cached_build_counts_only_with_its_whole_identity(self):
        """Round 8: binaries from before counted by revision alone, whatever toolchain, flags or
        linker made them."""
        import shutil
        import subprocess
        bash = shutil.which("bash")
        if bash is None or shutil.which("rustc") is None:
            self.skipTest("no bash or rustc")
        script = r'''
. scripts/lib/gate-publish.sh
GATE_BIN=$1 GATE=$1/gate-x
a=$(identity_dir HEAD 1)
[ "$a" = "$(identity_dir HEAD 1)" ] && echo same
[ "$a" != "$(RUSTFLAGS="-C opt-level=2" identity_dir HEAD 1)" ] && echo flags
[ "$a" != "$(identity_dir HEAD 2)" ] && echo layout
d=$1/build; mkdir -p "$d/release"
printf "x Linker: LLD 22.1.8 y" > "$d/release/tool"; lld_of "$d"
printf "no linker" > "$d/release/other"; lld_of "$d" 2>/dev/null || echo not-lld
'''
        with tempfile.TemporaryDirectory() as tmp:
            done = subprocess.run([bash, "-c", script, "-", Path(tmp).as_posix()], cwd=ROOT,
                                  capture_output=True, text=True, encoding="utf-8")
        self.assertEqual(done.stdout.split(), ["same", "flags", "layout", "LLD", "22.1.8", "not-lld"],
                         done.stderr[-500:])

    def test_the_command_line_plans_the_family_and_splits_into_blocks(self):
        import subprocess
        script = ROOT / "benches/fast/release_gate.py"
        done = subprocess.run([sys.executable, "-B", str(script), "plan", "--max-gb", "24"], capture_output=True,
                              text=True, encoding="utf-8")
        self.assertEqual(done.returncode, 0, done.stderr[-500:])
        plan = json.loads(done.stdout)
        self.assertTrue(plan["run"] and plan["family"])
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "plan.json").write_text(done.stdout, encoding="utf-8")
            (Path(tmp) / "measure.toml").write_text(f'[case.{plan["run"][0]}]\nblocks = 3\n', encoding="utf-8")
            done = subprocess.run([sys.executable, "-B", str(script), "blocks", "--plan", str(Path(tmp) / "plan.json"),
                                   "--measure", str(Path(tmp) / "measure.toml"), "--square", "1", "--out", tmp],
                                  capture_output=True, text=True, encoding="utf-8")
            self.assertEqual(done.returncode, 0, done.stderr[-500:])
            third = json.loads((Path(tmp) / "plan-block-3.json").read_text(encoding="utf-8"))
        self.assertEqual(third["run"], plan["run"][:1])

    def test_an_empty_case_selection_is_refused_never_every_case(self):
        from types import SimpleNamespace
        with self.assertRaises(SystemExit):
            fast.select([{"name": "a", "area": "query"}], SimpleNamespace(cases="", areas=None))

    def test_a_block_that_work_not_its_own_disturbed_is_noted_in_the_report(self):
        """COT-13 (20 Oct 2026): the watcher no longer stops work that isn't the job's own; it
        leaves it running and records it, and the report says which blocks it disturbed."""
        with tempfile.TemporaryDirectory() as tmp:
            for n, verdict in ((1, {"verdict": "quiet", "samples": 30, "unowned_share": 0.0}),
                               (2, {"verdict": "disturbed", "samples": 30, "unowned_share": 0.4,
                                    "not_resumed": ["pid 51"]})):
                (Path(tmp) / f"quiet-block-{n}.json").write_text(json.dumps(verdict), encoding="utf-8")
            verdicts, notes = gate_module.quiet_verdicts(Path(tmp))
        self.assertEqual(sorted(verdicts), [1, 2])
        self.assertEqual(len(notes), 2)
        self.assertTrue(notes[0].startswith("block 2: disturbed") and "40%" in notes[0], notes)
        self.assertIn("not resumed", notes[1])

    @unittest.skipUnless(sys.platform.startswith("linux"), "the watcher watches /proc (Linux)")
    def test_a_blocks_attempts_from_every_crash_point_one_repeat_at_most_a_verdict_before_acceptance(self):
        """The orchestrator, 20 Oct 2026: a disturbed block is left out of the statistics, both
        sides, and repeated once; a disturbed repeat is kept and listed. Sol, 20 Oct 2026: a restart
        accepted a disturbed block, or ran a third attempt. One table over the states a crash leaves
        a block in: what the restarted step runs and keeps. git, the watcher and fast.py are faked
        (the watcher's --resume is the real one); no repository is touched."""
        import shutil
        import subprocess
        bash, python = shutil.which("bash"), sys.executable  # as above: this test's own interpreter
        head = "a" * 40
        reports = [f"pass-{k}-x{k}.json" for k in range(1, 5)]
        complete = {r: '{"finished_ns": 1}' for r in reports}
        archived = {f"disturbed-{r[:-5]}-9.json": "{}" for r in reports}
        quiet, disturbed = '{"verdict": "quiet"}', '{"verdict": "disturbed"}'
        # case, the state file, files in passes, an earlier watcher's record, the verdicts of the
        # attempts it runs -> exit, attempts run, pass-*, disturbed-pass-*, final state, a report
        # note; and a fault: mv failing on paths that contain a text, or a directory where the
        # state is written (its temporary file) or renamed to
        faults = {
            "a state that can't be written: nothing run":
                (None, {}, None, ["quiet"], (2, 0, 0, 0, None, None), ("squat", "block-1.state.new")),
            "a state that can't be renamed: nothing run":
                (None, {}, None, ["quiet"], (2, 0, 0, 0, "squatted", None), ("squat", "block-1.state")),
            "an acceptance that can't be saved: not accepted":
                ("1:ran", {**complete, "quiet-block-1.json": quiet}, None, [], (2, 0, 4, 0, "1:ran", None),
                 ("squat", "block-1.state.new")),
            "an archival move that fails: no repeat":
                ("1:ran", {**complete, "quiet-block-1.json": disturbed}, None, ["quiet"], (2, 0, 4, 0, "1:archiving", None),
                 ("mv", "disturbed-pass")),
        }
        rows = [
            ("fresh, quiet", None, {}, None, ["quiet"], (0, 1, 4, 0, "1:accepted", None)),
            ("fresh, disturbed then quiet", None, {}, None, ["disturbed", "quiet"],
             (0, 2, 4, 4, "2:accepted", "left out of the statistics")),
            ("fresh, disturbed twice: kept", None, {}, None, ["disturbed", "disturbed"],
             (0, 2, 4, 4, "2:accepted", "its repeat disturbed, kept")),
            ("a crash during attempt 1: resumed", "1:running", dict(list(complete.items())[:2]), None, ["quiet"],
             (0, 1, 4, 0, "1:accepted", None)),
            ("attempt 1 run, its verdict not recorded: run again", "1:ran", complete, None, ["quiet"],
             (0, 1, 4, 0, "1:accepted", None)),
            ("attempt 1 disturbed, a crash before archiving", "1:ran", {**complete, "quiet-block-1.json": disturbed},
             None, ["quiet"], (0, 1, 4, 4, "2:accepted", "left out of the statistics")),
            ("a crash midway through archiving", "1:archiving",
             {**dict(list(complete.items())[2:]), **dict(list(archived.items())[:2]), "quiet-block-1.json": disturbed},
             None, ["quiet"], (0, 1, 4, 4, "2:accepted", "left out of the statistics")),
            ("archived, a crash before the repeat", "2:start", archived, None, ["quiet"], (0, 1, 4, 4, "2:accepted", None)),
            ("a crash during the repeat: resumed, never a third", "2:running",
             {**archived, **dict(list(complete.items())[:1])}, None, ["disturbed"],
             (0, 1, 4, 4, "2:accepted", "its repeat disturbed, kept")),
            ("the repeat disturbed and recorded: kept, nothing run", "2:ran",
             {**archived, **complete, "quiet-block-1.json": disturbed}, None, [], (0, 0, 4, 4, "2:accepted", None)),
            ("accepted before: nothing run", "1:accepted", {**complete, "quiet-block-1.json": quiet}, None, [],
             (0, 0, 4, 0, "1:accepted", None)),
            ("complete and disturbed, no state, an earlier record unresolved: stopped", None,
             {**complete, "quiet-block-1.json": disturbed}, "pid 51\n", [], (2, 0, 4, 0, None, None)),
            ("a third attempt: never started", "3:start", {}, None, [], (2, 0, 0, 0, "3:start", None)),
        ]
        rows = [(*row, None) for row in rows] + [(case, *rest) for case, rest in faults.items()]
        for case, state, files, frozen, verdicts, expected, fault in rows:
            with self.subTest(case), tempfile.TemporaryDirectory() as tmp:
                t = Path(tmp).as_posix()
                shim = Path(tmp) / "shim"
                shim.mkdir()
                (Path(tmp) / "verdicts").write_text("".join(f"{v}\n" for v in verdicts), encoding="utf-8")
                # fast.py block writes the block's reports (each run counted); quiet-watch.py writes
                # the next verdict when it is stopped; its --resume is the real watcher's.
                (shim / "python3").write_text(f"""#!/usr/bin/env bash
case " $* " in
  *" benches/fast/fast.py block "*)
    echo run >> {t}/runs
    n=0; out=; sides=()
    while [ $# -gt 0 ]; do case $1 in --block) n=$2 ;; --out) out=$2 ;; --side) sides+=("${{2%%:*}}") ;; esac; shift; done
    per=$(( 2 * ${{#sides[@]}} ))
    for k in $(seq 1 $per); do echo '{{"finished_ns": 1}}' > "$out/pass-$(( per * (n - 1) + k ))-x$k.json"; done
    sleep 1 ;;  # a block takes time: the watcher is up before it ends
  *"quiet-watch.py"*" --resume "*) exec "{Path(python).as_posix()}" "{(ROOT / 'scripts/lib/quiet-watch.py').as_posix()}" "$2" --resume ;;
  *"quiet-watch.py"*)
    state=$2
    verdict() {{
      v=$(head -1 {t}/verdicts); sed -i 1d {t}/verdicts
      printf '{{"verdict": "%s", "samples": 9, "unowned_share": 0.5}}\\n' "$v" > "$state/verdict.json"
      echo "$v" > "$state/watch.log"; exit 0
    }}
    trap verdict TERM
    while :; do sleep 0.2; done ;;
  *) exec "{Path(python).as_posix()}" "$@" ;;
esac
""", encoding="utf-8", newline="\n")
                (shim / "python3").chmod(0o755)
                # git, faked: the revision is a fixed id, its tree empty, a worktree an empty
                # directory; anything else the step asks fails loudly (no repository is touched).
                (shim / "git").write_text(f"""#!/usr/bin/env bash
while [ "$1" = -c ] || [ "$1" = -C ]; do shift 2; done
case "$1 $2" in
  "rev-parse --short=8") echo "${{3:0:8}}" ;;
  "rev-parse --show-object-format") echo sha1 ;;
  "rev-parse HEAD") echo {head} ;;
  "ls-tree -r") ;;
  "worktree add") mkdir -p "${{@: -2:1}}" ;;
  "status --porcelain") ;;
  *) if [ "$1" = rev-parse ] && [ $# -eq 2 ]; then echo "$2"; else echo "fake git: $*" >&2; exit 99; fi ;;
esac
""", encoding="utf-8", newline="\n")
                (shim / "git").chmod(0o755)
                if fault and fault[0] == "mv":
                    (shim / "mv").write_text(f'#!/bin/sh\ncase "$*" in *{fault[1]}*) echo "injected mv failure" >&2; exit 1 ;; '
                                             'esac\nexec /bin/mv "$@"\n', encoding="utf-8", newline="\n")
                    (shim / "mv").chmod(0o755)
                setup = f'''
. scripts/lib/gate-publish.sh
GATE={t}/gate GATE_BIN={t}/bin
b=$GATE_BIN/bin-test; mkdir -p "$b/release" "$GATE" {t}/out/passes
echo tool > "$b/release/tool"; echo {head} > "$b/REVISION"; echo timing=watchdog > "$b/CAPABILITIES"
(cd "$b" && sha256sum release/tool > SHA256SUMS)
point {head} "$b"
echo '{{"run": ["lab-olympics"]}}' > "$GATE/plan.json"
'''
                env = {**os.environ, "NRESE_GATE_DIR": f"{t}/gate", "GATE_BIN": f"{t}/bin", "FLEET_OUT": f"{t}/out",
                       "PATH": f"{shim}{os.pathsep}{os.environ['PATH']}"}
                subprocess.run([bash, "-c", setup], cwd=ROOT, env=env, check=True, capture_output=True)
                passes = Path(tmp) / "out" / "passes"
                for name, content in files.items():
                    (passes / name).write_text(content, encoding="utf-8")
                if state:
                    attempt, stage = state.split(":")
                    (passes / "block-1.state").write_text(f"attempt={attempt}\nstage={stage}\n", encoding="utf-8")
                if fault and fault[0] == "squat":
                    (passes / fault[1]).mkdir()  # a directory where the state is written or renamed to
                if frozen:
                    (Path(tmp) / "gate" / "quiet").mkdir(parents=True)
                    (Path(tmp) / "gate" / "quiet" / "frozen").write_text(frozen, encoding="utf-8")
                done = subprocess.run([bash, "-c", f'PATH="{shim}:$PATH" exec bash scripts/release-gate.sh block 1 {head} 1 - '
                                       f'base={head} cand={head}'], cwd=ROOT, env=env, capture_output=True, text=True)
                runs = Path(tmp) / "runs"
                final = passes / "block-1.state"
                found = (done.returncode, len(runs.read_text().split()) if runs.exists() else 0,
                         len(list(passes.glob("pass-*.json"))), len(list(passes.glob("disturbed-pass-*.json"))),
                         "squatted" if final.is_dir() else
                         ":".join(line.split("=", 1)[1] for line in final.read_text().split()) if final.exists() else None)
                self.assertEqual(found, expected[:5], done.stdout + done.stderr)
                if frozen:
                    self.assertEqual((Path(tmp) / "gate" / "quiet" / "frozen").read_text(), frozen)
                notes = gate_module.quiet_verdicts(passes)[1]
                if expected[5]:
                    self.assertTrue(any(expected[5] in note for note in notes), notes)

    def test_report_writes_markdown_and_json(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            for p in passes():
                (directory / p["path"]).write_text(json.dumps(p["report"]), encoding="utf-8")
            summary = run_gate(gate_module.load_passes(directory))
            text = gate_module.render(summary)
            self.assertIn("**PASSED.**", text)
            self.assertIn("## Against main", text)


class Tradeoffs(unittest.TestCase):
    """Only an owner-approved trade-off accepts a loss (benches/release-tradeoffs.toml)."""

    def rounds(self, tradeoffs=()):
        def records_of(side, n):
            cand = side == "cand"
            return [record(samples={"q01": [10.0, 10.1, 9.9],
                                    "q06": [v * (worse(1.08, n) if cand else 1) for v in [4.8, 4.85, 4.8]]}),
                    record(case="kernel-intersect",
                           samples={"triangles": [v * (worse(0.36, n) if cand else 1) for v in [140.0, 140.5]]})]
        # Three blocks: with two, the 8 % loss sat on Holm's threshold (combined p 0.0082 against
        # 0.0083), and which runs a side got in the block's order decided it.
        return [run_gate(passes(records_of, blocks=3), tradeoffs=list(tradeoffs), context={**CONTEXT, "round": k})
                for k in (0, 1)]

    def entry(self, **changes):
        return {"case": "lab-olympics", "dimension": "time/q06", "max_ratio": 1.12, "approved": "2026-10-20",
                "by": "the owner", "reason": "text index: 8 % slower filters for 2.8x faster triangles",
                "pays_with": {"case": "kernel-intersect", "dimension": "time/triangles"}, **changes}

    def verdict(self, *entries):
        return gate_module.verdict(self.rounds(entries), {"passed": True, "errors": []},
                                   {"passed": True, "errors": [], "guards": 81})

    def test_a_confirmed_loss_blocks_unless_an_approved_trade_off_with_its_win_covers_it(self):
        self.assertFalse(self.verdict()[0])
        passed, text = self.verdict(self.entry())
        self.assertTrue(passed, text)
        self.assertIn("accepted trade-off", text)
        for entry in (self.entry(max_ratio=1.05), self.entry(pays_with={"case": "lab-olympics", "dimension": "time/q01"}),
                      self.entry(dimension="time/q01")):
            self.assertFalse(self.verdict(entry)[0], entry)

    def test_the_list_starts_empty_and_a_malformed_entry_is_refused(self):
        entries, errors = gate_module.load_tradeoffs(ROOT / "benches/release-tradeoffs.toml")
        self.assertEqual((entries, errors), ([], []))
        with tempfile.TemporaryDirectory() as tmp:
            bad = Path(tmp) / "t.toml"
            bad.write_text('[[tradeoff]]\ncase = "x"\ndimension = "time/q"\nmax_ratio = 1.1\n', encoding="utf-8")
            entries, errors = gate_module.load_tradeoffs(bad)
            self.assertEqual(entries, [])
            self.assertIn("missing approved, by, pays_with, reason", errors[0])


class Measurement(unittest.TestCase):
    """benches/NOISY.md: the noise reduced in the harness, not tolerated."""

    def test_a_case_whose_noise_needs_more_blocks_is_tested_on_them_and_the_jobs_run_them(self):
        measure = {"a": {"blocks": 2}, "b": {"blocks": 4}, "c": {"blocks": 3}}
        self.assertEqual(gate_module.blocks(["a", "b", "c"], measure, square=1),  # one side: no squares
                         [["a", "b", "c"], ["a", "b", "c"], ["b", "c"], ["b"]])
        with self.assertRaises(TypeError):  # Sol r1: no square given planned 2 and 8 blocks for three sides
            gate_module.blocks(["a"], measure)
        # Sol r2: the plan files follow the job's minimum (jobs() with 4 blocks a round and a case
        # needing 2: 6 block steps, the plan 3), and a declared outcome's case keeps its one block.
        plan = gate_module.blocks(["a", "b", "c"], measure, 6, square=3, checked={"c"})
        self.assertEqual([sorted(cases) for cases in plan], [["a", "b", "c"]] + [["a", "b"]] * 5)
        # Whole Latin squares of the timed sides (tf2-gate-1: a first run's cost; a case in 2
        # blocks of three sides leaves one side never opening): 2 -> 3, 4 -> 6, 3 stays.
        self.assertEqual(gate_module.blocks(["a", "b", "c"], measure, square=3),
                         [["a", "b", "c"]] * 3 + [["b"]] * 3)
        timed = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 2, None, measure=measure)["rg-test-timed"]
        self.assertEqual(timed["steps"][0][2:], ["plan-blocks", "c" * 40])
        self.assertEqual([s[6] for s in timed["steps"] if s[2] == "block"], [f"plan-block-{k}.json" for k in range(1, 7)])
        self.assertEqual((timed["env"]["GATE_SQUARE"], timed["env"]["GATE_MIN_BLOCKS"]), ("3", "3"))
        two = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 2, None, measure=measure,
                               main=gate_module.BASE)["rg-test-timed"]  # main is base's build: two sides
        self.assertEqual(len([s for s in two["steps"] if s[2] == "block"]), 4)

        def records_of(side, n):
            return [record()] + ([record(case="lab-yago-tiny")] if n <= 12 else [])
        summary = run_gate(passes(lambda side, n: [record(), record(case="lab-yago-tiny")], blocks=3)[:12] +
                           [p for p in passes(records_of, blocks=3) if p["n"] > 12])
        self.assertEqual(summary["errors"], [])
        row = summary["comparisons"]["base"]
        self.assertEqual(row["lab-olympics"]["dimensions"]["time/q01"]["units"], 3)
        self.assertEqual(row["lab-yago-tiny"]["dimensions"]["time/q01"]["units"], 2)

    def test_the_passes_measure_alike_and_the_host_s_policy_holds(self):
        ps = passes()
        ps[3]["report"]["options"] = {**ps[3]["report"]["options"], "measure": "a" * 64}
        self.assertIn("the passes measured with different calibrations (fast.py --measure)", run_gate(ps)["errors"])
        ps = passes()
        state = {"frequency": {"driver": ["amd-pstate-epp"], "governor": ["performance"], "boost": "1"}}
        for p in ps:
            p["report"]["host_state"] = copy.deepcopy(state)
        ps[5]["report"]["host_state"]["frequency"]["governor"] = ["powersave"]
        self.assertTrue(any("frequency policy changed" in e for e in run_gate(ps)["errors"]))
        self.assertEqual(fast.scope_stalls_ms("x\nfast-scope-pressure cpu=1500 io=250000 memory=\n"),
                         {"cpu": 1.5, "io": 250.0})

    def test_a_series_in_coarse_steps_is_tested_at_its_step(self):
        self.assertEqual(gate_module.quantum([[[4.0, 5.0]], [[17.4, 5.5]]]), 0.1)
        self.assertEqual(gate_module.quantum([[[0.0091, 4.0]]]), 0.0)

    def test_calibrate_buys_runs_where_cheap_and_blocks_where_noise_needs_them_within_budget(self):
        quiet = [10.0, 10.01, 9.99, 10.0, 10.0, 10.01]
        noisy = [10.0, 10.4, 9.7, 10.2, 9.9, 10.3]

        def records_of(side, n):
            i = (n - 1) % 6
            out = [record(case="lab-olympics", samples={"q01": [quiet[i]] * 5}),
                   record(case="q-paths", samples={"broader-closure": [noisy[i]] * 5})]
            for r in out:
                r["case_s"] = 10.0
            return out
        ps = passes(records_of)
        catalog = [{"name": "lab-olympics", "metric": "sum_p50_ms", "queries": "@olympics", "args": []},
                   {"name": "q-paths", "metric": "sum_p50_ms", "queries": "q-paths", "args": ["--runs", "10"]}]
        cheap = gate_module.calibrate(catalog, ps, budget_min=1, m=10, batch=False)["cases"]
        self.assertEqual((cheap["lab-olympics"]["runs"], cheap["lab-olympics"]["warmup"]), (200, 3))
        self.assertEqual((cheap["lab-olympics"]["blocks"], cheap["q-paths"]["blocks"]), (2, 2))
        rich = gate_module.calibrate(catalog, ps, budget_min=240, m=10, batch=False)
        # Two blocks are one degree of freedom (t at alpha / m: about 64): even the quiet case buys one.
        self.assertEqual(rich["cases"]["lab-olympics"]["blocks"], 3)
        self.assertGreater(rich["cases"]["q-paths"]["blocks"], 3)
        self.assertLessEqual(rich["minutes"], 240)
        import tomllib
        self.assertEqual(tomllib.loads(gate_module.write_measure(rich, "a test"))["case"]["q-paths"]["blocks"],
                         rich["cases"]["q-paths"]["blocks"])

    def test_fast_applies_the_calibration_alike_and_pools_repeated_processes(self):
        case = {"name": "x", "tool": "perf_lab", "args": ["--runs", "5", "--qerror"], "repeat": 1}
        with patch.dict(fast.MEASURE, {"x": {"runs": 41, "warmup": 3, "batch": 7, "repeat": 2}}, clear=True), \
                patch.dict(fast.OPTIONS, {"batch": False}):
            got = fast.measured(case)
        self.assertEqual(got["args"], ["--runs", "41", "--qerror", "--warmup", "3"])
        self.assertEqual(got["repeat"], 2)
        with patch.dict(fast.MEASURE, {"x": {"batch": 7, "batch_queries": ["q01", "q07"]}}, clear=True), \
                patch.dict(fast.OPTIONS, {"batch": True}):
            self.assertEqual(fast.measured(case)["args"][-4:], ["--batch", "7", "--batch-queries", "q01,q07"])
        result = {"queries": [{"name": "q", "samples_ms": [3.0]}],
                  "_repeats": [{"queries": [{"name": "q", "samples_ms": [1.0, 2.0]}]},
                               {"queries": [{"name": "q", "samples_ms": [3.0]}]}]}
        self.assertEqual(fast.samples({"metric": "sum_p50_ms"}, result), {"q": [1.0, 2.0, 3.0]})


class Instances(unittest.TestCase):
    """lks-abba-11 and -13 timed the same two files: creaders' read p50 x1.032 and x0.951, each at
    p = 0.001; lks-aa-20 timed a build against a byte-identical copy: x1.048 in one session, x0.983
    in the next. The offset is the cached file instance's, the same in every block that times it."""

    def test_every_block_times_a_fresh_copy_and_the_gate_refuses_a_build_timed_in_place(self):
        with tempfile.TemporaryDirectory() as tmp:
            built = Path(tmp) / "bin-cccccccc-l1"
            (built / "release" / "examples").mkdir(parents=True)
            (built / "release" / "examples" / "perf_lab").write_bytes(b"\x7fELF perf lab")
            (built / ".refs").mkdir()
            side = {"name": "cand", "bin": built.as_posix()}
            first, = fast.fresh_copies([side], Path(tmp) / "block-1")
            second, = fast.fresh_copies([side], Path(tmp) / "block-2")
            lab = [Path(s["bin"]) / "release" / "examples" / "perf_lab" for s in (first, second)]
            self.assertEqual([x.read_bytes() for x in lab], [b"\x7fELF perf lab"] * 2)
            self.assertFalse(lab[0].samefile(built / "release" / "examples" / "perf_lab"))
            self.assertFalse(lab[0].samefile(lab[1]))
            self.assertEqual((first["built"], (Path(first["bin"]) / ".refs").exists()), (built.as_posix(), False))
        ps = passes()
        del ps[2]["report"]["instance"]
        self.assertIn("timed its side's build in place", " ".join(run_gate(ps)["errors"]))

    def test_a_block_removes_only_the_copies_it_made_also_when_a_copy_fails(self):
        """Sol, 20 Oct 2026: block removed a block-<n>-<pid> directory that was there before it,
        copied outside its cleanup, and took any side name as a path."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "instances"
            other = root / f"block-1-{os.getpid()}"
            other.mkdir(parents=True)
            (other / "keep").write_text("another block's", encoding="utf-8")
            built = Path(tmp) / "bin"
            (built / "release").mkdir(parents=True)
            (built / "release" / "tool").write_text("tool", encoding="utf-8")
            sides = [{"name": "base", "bin": built.as_posix()}, {"name": "cand", "bin": built.as_posix()}]
            with fast.block_copies(sides, root, 1) as timed:
                self.assertTrue(all(Path(s["bin"], "release", "tool").is_file() for s in timed))
            self.assertEqual(sorted(p.name for p in root.iterdir()), [other.name])
            with self.assertRaises(OSError), fast.block_copies([*sides, {"name": "main", "bin": f"{tmp}/none"}], root, 1):
                pass
            self.assertEqual(sorted(p.name for p in root.iterdir()), [other.name])
            self.assertTrue((other / "keep").is_file())
        with self.assertRaises(SystemExit):
            fast.parse_side("../escape:/bin:rev")

    def test_each_square_of_blocks_gives_every_side_every_place_once_drawn_from_the_seed(self):
        """tf2-gate-1 (20 Oct 2026): a block's first run cost 15.8 % on one series. With the mirror
        alternating per pair of blocks the middle side (main; A' in an A-B-B-A) never opened."""
        firsts = set()
        for seed in [None, *map(str, range(20))]:
            for square in range(3):
                orders = [[s for s, run in fast.palindrome(list(SIDES), 3 * square + j, seed) if run == 1]
                          for j in (1, 2, 3)]
                for place in range(3):
                    self.assertEqual(sorted(o[place] for o in orders), sorted(SIDES), (seed, square, orders))
            firsts.add(fast.palindrome(list(SIDES), 1, seed)[0][0])
            self.assertEqual(fast.palindrome(list(SIDES), 3, seed), fast.palindrome(list(SIDES), 3, seed))
        self.assertEqual(firsts, set(SIDES))
        self.assertEqual([fast.palindrome(["a", "b"], b)[0][0] for b in (1, 2, 3)], ["a", "b", "a"])

    def test_a_loss_is_judged_against_the_host_s_a_a_floor(self):
        """An A-A' gate (every side one revision, each block its own copies) gives the floor; a
        round whose candidate is 3.5 % slower in every block, by its instances' luck as in
        lks-abba-11, is no loss against it, while a 15 % loss still is."""
        import math
        import random

        def offset(side, n):  # each block's copy of a side: its own offset
            return math.exp(random.Random(f"{side}{(n - 1) // 6}").gauss(0, 0.04))

        def copies(side, n):
            return [record(samples={"q06": [v * offset(side, n) for v in (4.8, 4.85, 4.8)]})]
        aa = passes(copies, blocks=8, sides=("base", "cand"))  # main, the same build, timed once as base
        for p in aa:
            for key in ("binaries", "binaries_finished"):
                p["report"][key]["revision"] = REVS["base"]
        summary = gate_module.gate(aa, {s: REVS["base"] for s in ("cand", "base", "main")}, "cand",
                                   ["base", "main"], HARNESS, context=CONTEXT)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "phuoc-yu.toml"
            path.write_text(gate_module.write_floor(gate_module.noise_floor(summary), "aa-test"), encoding="utf-8")
            floor = gate_module.read_floor(path)
        self.assertGreater(floor["cases"]["lab-olympics"]["time/q06"], 0.03)

        def flagged(factor, floor_):
            s = run_gate(passes(slower(factor), blocks=4), floor=floor_)
            return {f["side"] for f in s["flagged"] if f["dimension"] == "time/q06"}
        self.assertEqual(flagged(1.035, None), {"base", "main"})
        self.assertEqual(flagged(1.035, floor), set())
        self.assertEqual(flagged(1.15, floor), {"base", "main"})
        with self.assertRaises(ValueError):  # a gate of two revisions is no A-A' run
            gate_module.noise_floor(run_gate(passes()))


class MeasurementSides(unittest.TestCase):
    """A side is built and timed from its revision, or from its measurement commit: a commit on
    that revision alone that changes nothing but its perf lab (PROTOCOL.md §5). T0, 28 Oct 2026:
    until then the gate patched main's and v1.5's perf lab at build time, code no commit held."""

    PERF_LAB = "crates/nrese-store/examples/perf_lab.rs"
    ENGINE = "crates/nrese-store/src/lib.rs"

    def setUp(self):
        self.repo = tempfile.mkdtemp(prefix="measurement-sides-")
        self.addCleanup(shutil.rmtree, self.repo, True)
        self.git("init", "-q")
        self.side = self.commit({self.ENGINE: "pub fn engine() {}\n", self.PERF_LAB: "fn main() { spawn(); }\n",
                                 "Cargo.lock": "# the side's\n"}, "the side")
        self.later = self.commit({"README.md": "later\n"}, "a later revision")

    def git(self, *args: str) -> str:
        return subprocess.run(["git", "-C", self.repo, *args], capture_output=True, text=True, check=True).stdout.strip()

    def commit(self, changes: dict, message: str, on: str | None = None) -> str:
        """A commit of `changes` (path: text, or None to delete) on `on` (else the current HEAD)."""
        if on:
            self.git("checkout", "-q", "--detach", on)
        for path, text in changes.items():
            file = Path(self.repo, path)
            if text is None:
                file.unlink()
            else:
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_text(text, encoding="utf-8")
        self.git("add", "-A")
        self.git("-c", "user.name=t", "-c", "user.email=t@example.org", "commit", "-q", "-m", message)
        return self.git("rev-parse", "HEAD")

    def test_only_a_commit_on_the_side_that_changes_just_its_perf_lab_is_built_for_it(self):
        loop = {self.PERF_LAB: "fn main() { arm(); }\n"}
        outside, parent = "changes more than its perf lab", "isn't a commit on"
        cases = {  # name: (its changes, the revision it is on, None if built for the side, else why not)
            "its perf lab": (loop, self.side, None),
            "the engine as well": ({**loop, self.ENGINE: "pub fn engine() { faster() }\n"}, self.side, outside),
            "its lockfile": ({**loop, "Cargo.lock": "# another\n"}, self.side, outside),
            "the engine moved into the perf lab": ({self.ENGINE: None,
                                                    "crates/nrese-store/examples/lib.rs": "pub fn engine() {}\n"},
                                                   self.side, outside),
            "on another revision": (loop, self.later, parent),
        }
        wrong = []
        for name, (changes, on, why) in cases.items():
            commit = self.commit(changes, name, on)
            try:
                got = "built" if gate_module.measured_at(self.repo, self.side, {self.side: commit}) == commit else "?"
            except SystemExit as e:
                got = str(e)
            if (got == "built") != (why is None) or (why and why not in got):
                wrong.append(f"{name}: {got}")
        self.assertEqual(wrong, [], "SPEC measurement side: its only parent is the side, and it changes "
                                    "nothing outside the perf lab's sources")

    def test_a_change_outside_the_perf_lab_is_seen_whatever_the_diff_configuration(self):
        """Sol, PR #54 r1 R1: under a user's diff.ignoreSubmodules=all, `git diff` left a gitlink
        added outside the perf lab out of its paths, and the commit was built for the side."""
        self.git("config", "diff.ignoreSubmodules", "all")
        self.git("checkout", "-q", "--detach", self.side)
        self.git("update-index", "--add", "--cacheinfo", f"160000,{self.side},outside-submodule")
        self.git("-c", "user.name=t", "-c", "user.email=t@example.org", "commit", "-q", "-m", "a gitlink outside")
        gitlink = self.git("rev-parse", "HEAD")
        with self.assertRaisesRegex(SystemExit, "changes more than its perf lab .*outside-submodule"):
            gate_module.measured_at(self.repo, self.side, {self.side: gitlink})
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        self.assertEqual(gate_module.measured_at(self.repo, self.side, {self.side: loop}), loop,
                         "SPEC measurement side: its perf lab alone is still built for it")

    def test_a_replace_ref_changes_nothing_the_check_reads(self):
        """Sol, PR #54 R1 fix review D1 and D2: a replace ref stands other objects in for the
        stored ones; standing the side's tree in for an engine change hid the change (D1). The
        check reads the objects as stored (STORED_OBJECTS): the change is refused, and a valid
        commit is admitted whatever stands in for its tree, which the gate's checkout doesn't read
        either (test_the_build_compiles_the_commit_checked)."""
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        engine = self.commit({self.ENGINE: "pub fn engine() { faster() }\n"}, "the engine", self.side)
        tree = lambda commit: self.git("rev-parse", f"{commit}^{{tree}}")
        self.git("replace", tree(engine), tree(self.side))
        with self.assertRaisesRegex(SystemExit, "changes more than its perf lab .*crates/nrese-store/src/lib.rs"):
            gate_module.measured_at(self.repo, self.side, {self.side: engine})
        self.git("replace", "-d", tree(engine))
        self.git("replace", tree(loop), tree(engine))
        self.assertEqual(gate_module.measured_at(self.repo, self.side, {self.side: loop}), loop)

    def gate_beside(self, loop: str) -> Path:
        """The gate's scripts beside the side's commits, in a commit of their own on HEAD, its side
        measured at LOOP: the commit the gate is launched from, whose checkout its code runs from
        (release-gate.sh); a scratch directory for its runs."""
        for rel in ("scripts/release-gate.sh", "benches/fast/release_gate.py", ".gitignore"):
            Path(self.repo, rel).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(ROOT / rel, Path(self.repo, rel))
        shutil.copytree(ROOT / "scripts/lib", Path(self.repo, "scripts/lib"), ignore=shutil.ignore_patterns("__pycache__"))
        shutil.copytree(ROOT / "benches/suite/suitekit", Path(self.repo, "benches/suite/suitekit"),
                        ignore=shutil.ignore_patterns("__pycache__"))
        Path(self.repo, "benches/fast/measurement-sides.toml").write_text(
            f'[side.{self.side}]\ncommit = "{loop}"\nref = "refs/measure/t-{self.side[:8]}"\n', encoding="utf-8")
        self.git("add", ".gitignore", "scripts", "benches")
        self.git("-c", "user.name=t", "-c", "user.email=t@example.org", "commit", "-q", "-m", "the gate")
        tmp = Path(tempfile.mkdtemp(prefix="measurement-build-"))
        self.addCleanup(shutil.rmtree, tmp, True)
        return tmp

    def test_the_build_compiles_the_revisions_bytes(self):
        """Sol, PR #54 fix check 1 S1: worktree() took HEAD and a clean status for the revision's
        source, and both trust the index and the attributes. A smudge filter (info/attributes) made
        the checkout other bytes, and a reused checkout changed under core.checkStat=minimal with its
        size and mtime kept stayed clean: the build went on with either. Now it refuses both, and a
        checkout as stored still builds."""
        bash = shutil.which("bash")
        if bash is None or shutil.which("flock") is None:
            self.skipTest("the gate builds on Linux hosts (bash, flock)")
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        tmp = self.gate_beside(loop)
        git_dir = Path(self.git("rev-parse", "--absolute-git-dir"))

        def build(gate: str) -> str:
            env = {**os.environ, "NRESE_GATE_DIR": (tmp / gate / "gate").as_posix(),
                   "GATE_BIN": (tmp / gate / "bin").as_posix(), "FLEET_OUT": (tmp / gate / "out").as_posix()}
            done = subprocess.run([bash, "scripts/release-gate.sh", "build", self.side], cwd=self.repo, env=env,
                                  capture_output=True, text=True)
            return done.stdout + done.stderr

        went = f"build {self.side} at {loop}"
        # A filter pair rewrites the engine at checkout and back for status: HEAD and a clean
        # status agree with the rewritten bytes.
        self.git("config", "filter.other.smudge", "sed s/engine/rival/")
        self.git("config", "filter.other.clean", "sed s/rival/engine/")
        (git_dir / "info").mkdir(exist_ok=True)
        (git_dir / "info" / "attributes").write_text("*.rs filter=other\n", encoding="utf-8")
        smudged = build("smudged")
        (git_dir / "info" / "attributes").unlink()
        self.git("config", "--remove-section", "filter.other")
        # A checkout as stored builds. Its engine then changed, size and mtime kept, under a stat
        # cache that compares only those, its entry older than the index (not racily clean).
        stored = build("stat")
        src = tmp / "stat" / "gate" / "build" / f"src-{loop[:8]}"
        engine = src / self.ENGINE
        old = os.stat(engine).st_mtime_ns - 10**10
        os.utime(engine, ns=(old, old))
        self.git("-C", src.as_posix(), "update-index", "-q", "--refresh")
        engine.write_text(engine.read_text(encoding="utf-8").replace("engine", "engin3"), encoding="utf-8")
        os.utime(engine, ns=(old, old))
        self.git("config", "core.checkStat", "minimal")
        self.git("config", "core.trustctime", "false")
        changed = build("stat")
        self.git("config", "--unset", "core.checkStat")
        self.git("config", "--unset", "core.trustctime")
        # A checkout as stored, plus a file the repository ignores, which status doesn't show and
        # a build may read (nrese-server's build.rs embeds apps/nrese-console/dist/; Sol, S1 r1):
        # it builds without it (Sol, S1 r2: the gate's own leftovers go the same way).
        clean = build("ignored")
        (git_dir / "info" / "exclude").write_text("dist/\n", encoding="utf-8")
        extra = tmp / "ignored" / "gate" / "build" / f"src-{loop[:8]}" / "dist" / "index.html"
        extra.parent.mkdir(parents=True)
        extra.write_text("<p>not the revision's</p>\n", encoding="utf-8")
        ignored = build("ignored")
        self.assertEqual((went in smudged, went in stored, went in changed, went in clean, went in ignored,
                          extra.exists()), (False, True, False, True, True, False),
                         f"SPEC the gate builds the revision's stored bytes and nothing else\n--- smudged\n"
                         f"{smudged}\n--- changed\n{changed}\n--- ignored\n{ignored}")
        self.assertIn("doesn't hold", smudged + changed)

    def test_the_gates_own_checkout_may_hold_its_scratch_a_built_one_not(self):
        """Sol, S1 r2: the gate writes ignored scratch into its own harness checkout (fast.py's
        tmp/fast/queries, the watcher's __pycache__, cargo-guarded's tmp logs) and checks that
        checkout again before each block: only a checkout to be built must hold nothing else."""
        self.git("checkout", "-q", "--detach", self.side)
        scratch = Path(self.repo, "tmp", "fast", "queries", "q.rq")
        scratch.parent.mkdir(parents=True)
        scratch.write_text("SELECT * {}\n", encoding="utf-8")
        self.assertEqual(gate_module.checkout_differs(self.repo, self.side), [],
                         "SPEC the harness's checkout: its stored bytes, the gate's scratch beside them")
        self.assertIn("tmp/fast/queries/q.rq (not in the revision)",
                      gate_module.checkout_differs(self.repo, self.side, nothing_else=True),
                      "SPEC a checkout to be built: nothing else")
        # Sol, S1 r3: code the harness would import isn't scratch: a sourceless benches/suite/csv.pyc
        # (ignored as *.py[cod]) loads before the standard library's csv.
        planted = Path(self.repo, "benches", "suite", "csv.pyc")
        planted.parent.mkdir(parents=True, exist_ok=True)
        planted.write_bytes(b"\x00not the revision's\n")
        self.assertEqual(gate_module.checkout_differs(self.repo, self.side),
                         ["benches/suite/csv.pyc (not in the revision)"],
                         "SPEC the harness's checkout: no code but the revision's")

    def test_the_gates_own_checkout_holds_nothing_else_outside_its_scratch(self):
        """Sol, PR #60 r1 R1: the harness's checkout refused only names that looked like code, by a
        case-sensitive suffix. An ignored benches/cases/private.toml put an unreviewed case in the
        plan (the catalogue reads every *.toml), and Windows imports benches/suite/csv.PYC as
        csv.pyc. A scratch root that was a link was left out before links were looked at."""
        self.git("checkout", "-q", "--detach", self.side)
        named = lambda: [d.split(" (")[0] for d in gate_module.checkout_differs(self.repo, self.side)]
        for planted in ("benches/cases/private.toml", "benches/suite/csv.PYC"):
            path = Path(self.repo, planted)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"not the revision's\n")
            with self.subTest(planted=planted):
                self.assertEqual(named(), [planted], "SPEC the harness's checkout: its revision and its scratch")
            path.unlink()
        elsewhere = tempfile.mkdtemp(prefix="linked-scratch-")
        self.addCleanup(shutil.rmtree, elsewhere, True)
        try:
            os.symlink(elsewhere, Path(self.repo, "tmp"), target_is_directory=True)
        except OSError as error:
            self.skipTest(f"no directory link here: {error}")
        self.assertEqual(named(), ["tmp"], "SPEC the scratch is the checkout's own directory")

    def test_no_bytecode_from_before_the_run_is_executed(self):
        """Sol, PR #60 r1 R2: the gate kept Python's caches in $GATE/pycache, where they outlive a
        run and no check reads them: an unchecked-hash .pyc planted there for suitekit/cases.py ran
        in place of the source the gate had checked. No cache older than a run runs in it: an
        earlier run's go with it, and one planted where they were kept, or beside the source, isn't
        read."""
        bash = shutil.which("bash")
        if bash is None or shutil.which("flock") is None:
            self.skipTest("the gate builds on Linux hosts (bash, flock)")
        import py_compile
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        tmp = self.gate_beside(loop)
        gate = tmp / "planted"
        build = lambda: self.run_gate(tmp, "planted", "build", self.side)
        went = f"build {self.side} at {loop}"
        self.assertIn(went, build(), "an earlier run")
        source = str(Path(self.repo, "benches/suite/suitekit/cases.py").resolve())
        body = tmp / "planted.py"
        body.write_text('raise RuntimeError("PR60 planted bytecode executed")\n', encoding="utf-8")
        self.assertEqual(list((gate / "gate" / "run").iterdir()), [], "SPEC an earlier run's caches go with it")
        planted = []
        for prefix in ((gate / "gate" / "pycache").as_posix(), None):
            held, sys.pycache_prefix = sys.pycache_prefix, prefix
            try:
                planted.append(importlib.util.cache_from_source(source))
            finally:
                sys.pycache_prefix = held
        for cached in planted:
            py_compile.compile(str(body), cfile=str(cached), dfile=source, doraise=True,
                               invalidation_mode=py_compile.PycInvalidationMode.UNCHECKED_HASH)
        output = build()
        self.assertNotIn("PR60 planted bytecode executed", output, "SPEC the gate runs the source it checked")
        self.assertIn(went, output)

    def test_nothing_the_launching_checkout_holds_beside_its_commit_runs(self):
        """Sol, PR #60 fix checks 1 and 2: files in the checkout the gate was launched from, beside
        its commit's, ran in the validator before anything was checked: a sourceless
        suitekit/cases/__init__.pyc (git clean -fd keeps it), an untracked suitekit/cases/__init__.py
        or benches/suite/csv.py (no rule of what may lie there tells one from a commit hook's new
        file). The gate's code runs from a checked checkout of the launching commit: nothing else
        in the launching checkout runs, nor stops the gate."""
        if shutil.which("bash") is None or shutil.which("flock") is None:
            self.skipTest("the gate builds on Linux hosts (bash, flock)")
        import py_compile
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        tmp = self.gate_beside(loop)
        went = f"build {self.side} at {loop}"
        ran = "PR60 A LAUNCHING CHECKOUT FILE RAN"
        body = tmp / "planted.py"
        body.write_text(f'raise RuntimeError("{ran}")\n', encoding="utf-8")
        planted = []
        for name in ("benches/suite/suitekit/cases/__init__.py", "benches/suite/csv.py",
                     "benches/suite/target/__init__.pyc", "benches/suite/__pycache__/__init__.pyc"):
            path = Path(self.repo, name)
            path.parent.mkdir(parents=True, exist_ok=True)
            if name.endswith(".pyc"):
                py_compile.compile(str(body), cfile=str(path), doraise=True)
            else:
                shutil.copy(body, path)
            planted.append(path)
        output = self.run_gate(tmp, "sources", "build", self.side)
        self.assertNotIn(ran, output, "SPEC nothing beside the launching commit's files runs")
        self.assertIn(went, output, "SPEC nor stops the gate")
        # Sol, fix check 1: the sourceless package, also in a directory no walk can list (0111).
        planted[0].unlink()
        py_compile.compile(str(body), cfile=str(planted[0].with_suffix(".pyc")), doraise=True)
        output = self.run_gate(tmp, "sourceless", "build", self.side)
        self.assertNotIn(ran, output, "SPEC nothing beside the launching commit's files runs")
        self.assertIn(went, output, "SPEC nor stops the gate")
        if os.name == "nt" or os.geteuid() == 0:
            self.skipTest("a directory only searchable needs a non-root POSIX user")
        os.chmod(planted[0].parent, 0o111)
        self.addCleanup(os.chmod, planted[0].parent, 0o755)
        output = self.run_gate(tmp, "unlisted", "build", self.side)
        self.assertNotIn(ran, output, "SPEC nothing beside the launching commit's files runs")
        self.assertIn(went, output, "SPEC nor stops the gate")

    def without_flock(self, tmp: Path) -> str:
        """A PATH of this one's commands but flock, as on a host without it (Git Bash): ns.sh then
        locks with suitekit/lock.py."""
        if shutil.which("flock") is None:
            return os.environ.get("PATH", "")
        farm = tmp / "no-flock"
        farm.mkdir()
        for entry in os.environ.get("PATH", "").split(os.pathsep):
            for name in os.listdir(entry) if os.path.isdir(entry) else ():
                target = Path(entry, name)
                if name != "flock" and not os.path.lexists(farm / name) and target.is_file() \
                        and os.access(target, os.X_OK):
                    (farm / name).symlink_to(target)
        return farm.as_posix()

    def test_no_python_of_the_launching_checkout_runs_in_the_libraries(self):
        """Sol, PR #60 fix 3's sol-loop S1: scripts/lib/ns.sh, sourced from the launching checkout,
        named that checkout's suite for the Python it starts: where there is no flock it locks with
        suitekit/lock.py, and a changed one there ran in `clean --patch-keyed --remove`. The
        libraries' Python is the launching commit's: the checked checkout's, or for `clean`, which
        makes none, its lock helper's bytes alone."""
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("needs bash")
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        tmp = self.gate_beside(loop)
        ran = "PR60 A LAUNCHING CHECKOUT LOCK RAN"
        Path(self.repo, "benches/suite/suitekit/lock.py").write_text(f'raise RuntimeError("{ran}")\n',
                                                                     encoding="utf-8")
        path = self.without_flock(tmp)
        # BIN NAME IDENTITY-LINE AGE: a build there, its files and references AGE old.
        make = r'''b=$1/$2; mkdir -p "$b/release/examples" "$b/.refs"; echo tool > "$b/release/examples/perf_lab"
printf 'revision x\n%s\nlayout 1\n' "$3" > "$b/IDENTITY"; echo built > "$b/BUILD.txt"
(cd "$b" && sha256sum release/examples/perf_lab > SHA256SUMS); touch "$b/.refs/gate-$2"
find "$b" -maxdepth 2 -exec touch -d "$4" {} +'''
        subprocess.run([bash, "-c", make, "make", (tmp / "keyed" / "bin").as_posix(), "bin-unused",
                        "patch perf_lab_watchdog.py 0123456789abcdef", "3 days ago"], check=True)
        output = self.run_gate(tmp, "keyed", "clean", "--patch-keyed", "--remove", path=path)
        self.assertNotIn(ran, output, "SPEC no Python of the launching checkout runs")
        self.assertIn("released-now\tbin-unused", output, "SPEC the build goes, under the launching commit's lock")
        subprocess.run([bash, "-c", make, "make", (tmp / "clean" / "bin").as_posix(), "bin-old",
                        "profile release --locked", "30 days ago"], check=True)
        output = self.run_gate(tmp, "clean", "clean", path=path)
        self.assertNotIn(ran, output, "SPEC no Python of the launching checkout runs")
        self.assertIn("unused builds removed: bin-old", output, "SPEC clean locks with the launching commit's Python")
        self.assertEqual(list((tmp / "clean" / "gate" / "run").iterdir()), [], "SPEC and leaves nothing of its own")

    def test_a_callers_paths_are_the_launching_checkouts(self):
        """Sol, PR #60 fix 3's sol-loop S2: the gate runs its code from a checkout of its own, and
        `jobs HEAD --out jobs` wrote its jobs into that checkout, named it their repository, and
        left it unclean, so the gate's next start refused it. A caller's paths, and the jobs'
        repository, are the launching checkout's."""
        if shutil.which("bash") is None:
            self.skipTest("needs bash")
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        # The jobs place their parts by the host registry, which the gate's commit holds.
        Path(self.repo, "benches").mkdir(exist_ok=True)
        shutil.copy(ROOT / "benches/hosts.toml", Path(self.repo, "benches/hosts.toml"))
        tmp = self.gate_beside(loop)
        for start in ("first", "next"):
            output = self.run_gate(tmp, "jobs", "jobs", "HEAD", "--base", "HEAD", "--main", "HEAD", "--no-checks",
                                   "--out", "jobs")
            written = sorted(Path(self.repo, "jobs").glob("*.toml"))
            self.assertTrue(written, f"SPEC the {start} start writes the jobs where the caller said:\n{output}")
        for file in written:
            repositories = re.findall(r'git = "([^"]+)"', file.read_text(encoding="utf-8"))
            self.assertTrue(repositories, file.name)
            for repository in repositories:
                self.assertEqual(Path(repository).resolve(), Path(self.repo).resolve(),
                                 f"SPEC the jobs' repository is the launching checkout: {file.name}")

    def test_callers_at_once_share_no_code_checkout(self):
        """Sol, PR #60 fix check 3 (R3): the code checkout, shared under the launching commit's name,
        had no owner: of two `jobs` started at once on one gate directory, one met the other's
        checkout half made (a directory, not yet a checkout) and both failed. Each invocation makes,
        checks and removes its own: one paused while making it stops no other, neither leaves
        anything behind, and a clean and a restart after them run."""
        if shutil.which("bash") is None:
            self.skipTest("needs bash")
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        # The jobs place their parts by the host registry, which the gate's commit holds.
        Path(self.repo, "benches").mkdir(exist_ok=True)
        shutil.copy(ROOT / "benches/hosts.toml", Path(self.repo, "benches/hosts.toml"))
        tmp = self.gate_beside(loop)
        # git, but the first `worktree add` makes its directory and waits there until released.
        signals = tmp / "signals"
        signals.mkdir()
        hold = tmp / "hold" / "git"
        hold.parent.mkdir()
        hold.write_text(f'''#!/usr/bin/env bash
case " $* " in
  *" worktree add "*)
    if mkdir "{signals.as_posix()}/claimed" 2>/dev/null; then
      mkdir -p "${{@: -2:1}}" && touch "{signals.as_posix()}/made"
      while [ ! -e "{signals.as_posix()}/go" ]; do sleep 0.05; done
    fi ;;
esac
exec "{Path(shutil.which("git")).as_posix()}" "$@"
''', encoding="utf-8", newline="\n")
        hold.chmod(0o755)
        path = hold.parent.as_posix() + os.pathsep + os.environ.get("PATH", "")
        jobs = lambda out: ("jobs", "HEAD", "--base", "HEAD", "--main", "HEAD", "--no-checks", "--out", out)
        start = lambda out: subprocess.Popen(
            [shutil.which("bash"), "scripts/release-gate.sh", *jobs(out)], cwd=self.repo,
            env=self.gate_env(tmp, "at-once", path), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        first = start("jobs-a")
        self.addCleanup(lambda: first.poll() is None and first.kill())
        try:
            deadline = time.monotonic() + 120
            while not (signals / "made").exists():
                self.assertIsNone(first.poll(), "the first caller ended before making its checkout")
                self.assertLess(time.monotonic(), deadline, "the first caller made no checkout")
                time.sleep(0.05)
            second = start("jobs-b")
            self.addCleanup(lambda: second.poll() is None and second.kill())
            output = second.communicate(timeout=300)[0]
            self.assertEqual(second.returncode, 0, f"SPEC a caller beside a half-made checkout runs:\n{output}")
        finally:
            (signals / "go").touch()
        output = first.communicate(timeout=300)[0]
        self.assertEqual(first.returncode, 0, f"SPEC so does the one paused while making it:\n{output}")
        for out in ("jobs-a", "jobs-b"):
            self.assertTrue(list(Path(self.repo, out).glob("*.toml")), f"{out}: no jobs written")
        self.assertEqual(list((tmp / "at-once" / "gate" / "run").iterdir()), [], "SPEC each removes its own")
        self.assertIn("MiB reclaimed", self.run_gate(tmp, "at-once", "clean"), "SPEC a clean after them runs")
        output = self.run_gate(tmp, "at-once", *jobs("jobs-c"))
        self.assertTrue(list(Path(self.repo, "jobs-c").glob("*.toml")), f"SPEC and a restart:\n{output}")
        listed = self.git("worktree", "list", "--porcelain")
        self.assertEqual(listed.count("worktree "), 1, f"SPEC no checkout stays registered:\n{listed}")

    def test_what_a_dead_run_left_stops_neither_clean_nor_build(self):
        """Sol, PR #60 fix check 3 (R3): a code checkout left half made (a directory, not a checkout)
        stopped `build` and `clean` alike, clean behind the bootstrap that refused it. `clean` makes
        no code checkout: it removes what dead invocations left, keeps a live one's, and the build
        runs after it."""
        bash = shutil.which("bash")
        if bash is None or shutil.which("flock") is None:
            self.skipTest("the gate builds on Linux hosts (bash, flock)")
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        tmp = self.gate_beside(loop)
        gate = tmp / "stale" / "gate"
        head = self.git("rev-parse", "--short=8", "HEAD")
        # A dead invocation's pid, and a live one's, as the gate's shell numbers them.
        dead = subprocess.run([bash, "-c", "echo $$"], capture_output=True, text=True, check=True).stdout.strip()
        live = subprocess.Popen([bash, "-c", "echo $$; exec sleep 300"], stdout=subprocess.PIPE, text=True)
        self.addCleanup(live.wait)
        self.addCleanup(live.kill)
        alive = live.stdout.readline().strip()
        for pid in (dead, alive):
            (gate / "run" / f"{pid}-abcdef" / f"src-{head}").mkdir(parents=True)
        (gate / f"src-{head}").mkdir()  # 47e5cee7's shared code checkout, half made (Sol's case)
        output = self.run_gate(tmp, "stale", "clean")
        self.assertIn("MiB reclaimed", output, f"SPEC clean runs:\n{output}")
        self.assertFalse((gate / "run" / f"{dead}-abcdef").exists(), "SPEC what a dead invocation left goes")
        self.assertTrue((gate / "run" / f"{alive}-abcdef").exists(), "SPEC a live one's stays")
        output = self.run_gate(tmp, "stale", "build", self.side)
        self.assertIn(f"build {self.side} at {loop}", output, f"SPEC and the build runs:\n{output}")

    def gate_env(self, tmp: Path, name: str, path: str | None = None) -> dict[str, str]:
        """A release-gate.sh run's environment: its gate, binaries and results under TMP/NAME, its
        python3 this Python (bytecode planted for it carries its tag), then PATH (this process's by
        default)."""
        shim = tmp / "shim" / "python3"
        if not shim.exists():
            shim.parent.mkdir()
            shim.write_text(f'#!/usr/bin/env bash\nexec "{Path(sys.executable).as_posix()}" "$@"\n',
                            encoding="utf-8", newline="\n")
            shim.chmod(0o755)
        gate = tmp / name
        return {**os.environ, "NRESE_GATE_DIR": (gate / "gate").as_posix(), "GATE_BIN": (gate / "bin").as_posix(),
                "FLEET_OUT": (gate / "out").as_posix(),
                "PATH": shim.parent.as_posix() + os.pathsep + (path or os.environ.get("PATH", ""))}

    def run_gate(self, tmp: Path, name: str, *command: str, path: str | None = None) -> str:
        """release-gate.sh COMMAND from the test's checkout in gate_env's environment; the output."""
        done = subprocess.run([shutil.which("bash"), "scripts/release-gate.sh", *command], cwd=self.repo,
                              env=self.gate_env(tmp, name, path), capture_output=True, text=True)
        return done.stdout + done.stderr

    def test_the_build_compiles_the_commit_checked(self):
        """PR #54 fix check 1 (D2's other half): a replace ref made after the check returned stood
        another engine in for the checked commit's in the checkout the build compiles, and HEAD
        and a clean status didn't show it. release-gate.sh build, from the check to the checkout,
        reads the objects as stored: with no replace ref (the control) and with one made right
        after the check, its worktree holds the commit checked."""
        bash = shutil.which("bash")
        if bash is None or shutil.which("flock") is None:
            self.skipTest("the gate builds on Linux hosts (bash, flock)")
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        engine = self.commit({self.ENGINE: "pub fn engine() { faster() }\n"}, "another engine", self.side)
        tmp = self.gate_beside(loop)
        # The check as it is; right after it (INJECT), a replace ref standing another engine in.
        (tmp / "shim").mkdir()
        (tmp / "shim" / "python3").write_text(f"""#!/usr/bin/env bash
out=$("{Path(sys.executable).as_posix()}" "$@"); code=$?
case " $* " in *" benches/fast/release_gate.py measured-at "*) [ -z "${{INJECT:-}}" ] || git replace $INJECT ;; esac
printf '%s\\n' "$out"; exit $code
""", encoding="utf-8", newline="\n")
        (tmp / "shim" / "python3").chmod(0o755)
        tree = lambda commit: self.git("rev-parse", f"{commit}^{{tree}}")
        got = {}
        for name, inject in (("no replace ref", ""), ("one made after the check", f"{tree(loop)} {tree(engine)}")):
            gate = tmp / name.replace(" ", "-")
            env = {**os.environ, "NRESE_GATE_DIR": (gate / "gate").as_posix(), "GATE_BIN": (gate / "bin").as_posix(),
                   "FLEET_OUT": (gate / "out").as_posix(), "INJECT": inject,
                   "PATH": (tmp / "shim").as_posix() + os.pathsep + os.environ.get("PATH", "")}
            done = subprocess.run([bash, "scripts/release-gate.sh", "build", self.side], cwd=self.repo, env=env,
                                  capture_output=True, text=True)
            src = gate / "gate" / "build" / f"src-{loop[:8]}"
            got[name] = tuple((src / p).read_text(encoding="utf-8") if (src / p).exists() else done.stdout + done.stderr
                              for p in (self.ENGINE, self.PERF_LAB))
            if inject:
                self.git("replace", "-d", tree(loop))
        self.assertEqual(got, {name: ("pub fn engine() {}\n", "fn main() { arm(); }\n") for name in got},
                         "SPEC measurement side: the gate compiles the commit it checked")

    def test_a_message_in_another_encoding_is_no_error(self):
        """Sol, PR #54 R1 fix review r3 D4: the commit object was decoded in the locale's encoding, so
        a measurement commit whose message is ISO-8859-1 stopped the check with UnicodeDecodeError."""
        message = Path(tempfile.mkdtemp(prefix="measurement-message-"), "message")
        self.addCleanup(shutil.rmtree, message.parent, True)
        message.write_bytes(b"its perf lab, caf\xe9\n")
        self.git("checkout", "-q", "--detach", self.side)
        Path(self.repo, self.PERF_LAB).write_text("fn main() { arm(); }\n", encoding="utf-8")
        self.git("add", self.PERF_LAB)
        self.git("-c", "user.name=t", "-c", "user.email=t@example.org", "-c", "i18n.commitEncoding=ISO-8859-1",
                 "commit", "-q", "-F", str(message))
        commit = self.git("rev-parse", "HEAD")
        self.assertEqual(gate_module.measured_at(self.repo, self.side, {self.side: commit}), commit)

    def test_a_graft_or_a_shallow_boundary_moves_no_parent(self):
        """Sol, PR #54 R1 fix review D3: the parent query walked history, so a graft or a shallow
        boundary refused a valid measurement commit; its parent is the one its object names."""
        commit = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        git_dir = Path(self.git("rev-parse", "--absolute-git-dir"))
        for name, text in (("info/grafts", f"{commit} {self.later}\n"), ("shallow", f"{commit}\n")):
            with self.subTest(name):
                (git_dir / name).parent.mkdir(exist_ok=True)
                (git_dir / name).write_text(text, encoding="utf-8")
                try:
                    self.assertEqual(gate_module.measured_at(self.repo, self.side, {self.side: commit}), commit)
                finally:
                    (git_dir / name).unlink()

    def test_a_side_without_a_measurement_commit_is_built_from_itself(self):
        self.assertEqual(gate_module.measured_at(self.repo, self.later, {}), self.later)

    def test_a_measurement_commit_is_built_only_as_its_side(self):
        """Sol r1 S1: built as a side of its own, it would share its side's build directory under
        another REVISION, and the second of the two builds would be refused."""
        commit = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        with self.assertRaisesRegex(SystemExit, f"is the measurement commit of {self.side[:12]}"):
            gate_module.measured_at(self.repo, commit, {self.side: commit})

    def test_a_job_that_builds_a_side_takes_its_measurement_commit_as_an_input(self):
        commit = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        inputs = gate_module.measurement_inputs(self.repo, [self.side, self.later, self.side],
                                                {self.side: commit})
        self.assertEqual(inputs, [{"name": f"measure-{self.side[:8]}", "source": {"git": self.repo, "rev": commit}}])

    def test_main_and_v1_5_are_measured_at_their_commits(self):
        table = gate_module.tomllib.loads(gate_module.MEASUREMENT_SIDES.read_text(encoding="utf-8"))["side"]
        self.assertTrue({gate_module.MAIN, gate_module.BASE} <= set(table))
        for rev, side in table.items():
            with self.subTest(side=rev):
                self.assertRegex(rev, r"^[0-9a-f]{40}$")
                self.assertRegex(side["commit"], r"^[0-9a-f]{40}$")
                self.assertTrue(re.fullmatch(rf"refs/measure/[a-z0-9]+-{rev[:8]}", side["ref"]), side["ref"])


class PatchKeyedBuilds(unittest.TestCase):
    """The builds of a perf lab patched at build time (before T0) can't be reused after it: their
    identity names the patch. `clean --patch-keyed` lists them, and with --remove their binaries go
    unless a gate used them within GATE_LIVE_HOURS; their IDENTITY, BUILD.txt and SHA256SUMS stay."""

    def test_only_an_unused_patch_keyed_build_loses_its_binaries_and_only_when_asked(self):
        bash = shutil.which("bash")
        if not bash:
            self.skipTest("needs bash")
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp).as_posix()
            setup = f'''
make() {{  # NAME IDENTITY-LINE REF-AGE
  b={t}/bin/$1; mkdir -p "$b/release/examples" "$b/.refs"; echo tool > "$b/release/examples/perf_lab"
  printf 'revision x\\n%s\\nlayout 1\\n' "$2" > "$b/IDENTITY"; echo built > "$b/BUILD.txt"
  (cd "$b" && sha256sum release/examples/perf_lab > SHA256SUMS); touch -d "$3" "$b/.refs/gate-$1"
}}
make bin-unused 'patch perf_lab_watchdog.py 0123456789abcdef' '3 days ago'
make bin-live 'patch perf_lab_watchdog.py 0123456789abcdef' 'now'
make bin-after 'profile release --locked' '3 days ago'
'''
            env = {**os.environ, "NRESE_GATE_DIR": f"{t}/gate", "GATE_BIN": f"{t}/bin", "FLEET_OUT": f"{t}/out"}
            subprocess.run([bash, "-c", setup], env=env, check=True, capture_output=True)

            def clean(*args: str) -> dict[str, str]:
                done = subprocess.run([bash, "scripts/release-gate.sh", "clean", "--patch-keyed", *args], cwd=ROOT,
                                      env=env, capture_output=True, text=True, encoding="utf-8")
                self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
                return {line.split("\t")[1]: line.split("\t")[0] for line in done.stdout.splitlines()}

            def binaries() -> set[str]:
                return {d.name for d in Path(tmp, "bin").iterdir() if (d / "release").is_dir()}

            self.assertEqual(clean(), {"bin-unused": "would-release", "bin-live": "kept-live"})
            self.assertEqual(binaries(), {"bin-unused", "bin-live", "bin-after"}, "SPEC: a dry run removes nothing")
            self.assertEqual(clean("--remove"), {"bin-unused": "released-now", "bin-live": "kept-live"})
            self.assertEqual(binaries(), {"bin-live", "bin-after"})
            self.assertEqual({p.name for p in Path(tmp, "bin", "bin-unused").iterdir() if p.is_file()},
                             {"IDENTITY", "BUILD.txt", "SHA256SUMS"}, "SPEC: the record stays")
            self.assertEqual(clean(), {"bin-unused": "released", "bin-live": "kept-live"})
            self.assertTrue(Path(tmp, "out", "patch-keyed-builds.tsv").is_file())
        # The gate's own checkout (of this checkout's commit) was under the scratch directory.
        subprocess.run(["git", "worktree", "prune"], cwd=ROOT, capture_output=True)

    def test_a_patch_keyed_build_is_never_a_verified_build(self):
        """Sol r1 S2: a gate's pointer or an archive from before T0 could hand a block a build of
        patched source; build_verified, which block, reuse and pack go through, refuses it."""
        bash = shutil.which("bash")
        if not bash:
            self.skipTest("needs bash")
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp).as_posix()
            script = f'''
. scripts/lib/gate-publish.sh
for kind in patched plain; do
  b={t}/bin-$kind; mkdir -p "$b/release"; echo tool > "$b/release/tool"; echo r > "$b/REVISION"
  (cd "$b" && sha256sum release/tool > SHA256SUMS)
  [ $kind = patched ] && echo "patch perf_lab_watchdog.py 0123456789abcdef" > "$b/IDENTITY" || echo "layout 1" > "$b/IDENTITY"
  build_verified "$b" r && echo "$kind verified" || echo "$kind refused"
done
'''
            done = subprocess.run([bash, "-c", script], cwd=ROOT, capture_output=True, text=True, encoding="utf-8")
        self.assertEqual(done.stdout.split("\n")[:2], ["patched refused", "plain verified"], done.stderr)


if __name__ == "__main__":
    unittest.main()
