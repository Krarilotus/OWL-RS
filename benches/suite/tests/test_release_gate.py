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
        self.assertEqual([s[3] for s in blocks_], ["1", "2"])
        self.assertEqual(blocks_[0][2:], ["block", "1", "c" * 40, "1", "-", f"base={gate_module.BASE}",
                                          f"main={gate_module.MAIN}", f"cand={'c' * 40}"])
        self.assertEqual(timed["env"]["GATE_MIN_BLOCKS"], "2")
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
        self.assertEqual(done.returncode, 2, done.stdout + done.stderr)
        self.assertIn("is not Linux", done.stdout)

    def test_a_block_of_two_sides_is_complete_with_its_four_reports(self):
        """20 Oct 2026: release-gate.sh block counted six reports a block whatever the sides, so an
        A-B-B-A of two (the agents' confirmation rounds) never completed."""
        import shutil
        import subprocess
        bash, python = shutil.which("bash"), shutil.which("python3")
        if bash is None or python is None:
            self.skipTest("no bash or python3")
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
                                   f'exec bash scripts/release-gate.sh block 2 {head} 1 - base={head} cand={head}'],
                                  cwd=ROOT, env=env, capture_output=True, text=True, encoding="utf-8")
            subprocess.run(["git", "worktree", "remove", "--force", f"{t}/gate/src-{head[:8]}"], cwd=ROOT,
                           capture_output=True)
        subprocess.run(["git", "worktree", "prune"], cwd=ROOT, capture_output=True)
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn("block 2: done", done.stdout)

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
                                   "--measure", str(Path(tmp) / "measure.toml"), "--out", tmp],
                                  capture_output=True, text=True, encoding="utf-8")
            self.assertEqual(done.returncode, 0, done.stderr[-500:])
            third = json.loads((Path(tmp) / "plan-block-3.json").read_text(encoding="utf-8"))
        self.assertEqual(third["run"], plan["run"][:1])

    def test_an_empty_case_selection_is_refused_never_every_case(self):
        from types import SimpleNamespace
        with self.assertRaises(SystemExit):
            fast.select([{"name": "a", "area": "query"}], SimpleNamespace(cases="", areas=None))

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
        return [run_gate(passes(records_of), tradeoffs=list(tradeoffs), context={**CONTEXT, "round": k})
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
        self.assertEqual(gate_module.blocks(["a", "b", "c"], measure), [["a", "b", "c"], ["a", "b", "c"], ["b", "c"], ["b"]])
        timed = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 2, None, measure=measure)["rg-test-timed"]
        self.assertEqual(timed["steps"][0][2:], ["plan-blocks", "c" * 40])
        self.assertEqual([s[6] for s in timed["steps"] if s[2] == "block"],
                         ["plan-block-1.json", "plan-block-2.json", "plan-block-3.json", "plan-block-4.json"])

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

    def test_each_pair_of_blocks_has_both_orientations_which_first_drawn_from_the_seed(self):
        firsts = set()
        for seed in map(str, range(20)):
            for b in (1, 3, 5, 7):
                pair = [fast.palindrome(list(SIDES), x, seed)[0][0] for x in (b, b + 1)]
                self.assertEqual(sorted(pair), ["base", "cand"], (seed, b))
            firsts.add(fast.palindrome(list(SIDES), 1, seed)[0][0])
            self.assertEqual(fast.palindrome(list(SIDES), 3, seed), fast.palindrome(list(SIDES), 3, seed))
        self.assertEqual(firsts, {"base", "cand"})

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
        aa = passes(copies, blocks=8)
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


if __name__ == "__main__":
    unittest.main()
