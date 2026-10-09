"""Fast report counters accept structured metadata without changing numeric work rules."""
import importlib.util
import contextlib
import copy
import io
import json
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "fast_counter_contract", Path(__file__).resolve().parents[2] / "fast" / "fast.py"
)
fast = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fast)


class FastCounters(unittest.TestCase):
    def test_unchanged_nested_metadata(self):
        values = {"q.a.rewrites": [[{"rules": ["r1"]}]] * 3}
        self.assertEqual(fast.counter_changes(values, values), (False, []))

    def test_changed_nested_metadata_is_preserved(self):
        before, after = [{"rules": ["r1"]}], [{"rules": ["r1", "r2"]}]
        grew, notes = fast.counter_changes({"rewrite": [before] * 3}, {"rewrite": [after] * 3})
        self.assertFalse(grew)
        self.assertEqual(notes, [f"rewrite: {json.dumps(before, sort_keys=True)} -> "
                                 f"{json.dumps(after, sort_keys=True)} (metadata changed)"])

    def test_variable_repetitions_are_not_judged(self):
        for b, n in [([[1], [2]], [[3], [3]]), ([[1], [1]], [[2], [3]]),
                     ([100, 101], [150, 150])]:
            with self.subTest(b=b, n=n):
                self.assertEqual(fast.counter_changes({"work": b}, {"work": n}), (False, []))

    def test_numeric_thresholds_are_unchanged(self):
        for before, after, expected in [
            (100, 100, (False, [])),
            (100, 102, (False, ["work: 100 -> 102 (+2.0%)"])),
            (100, 103, (True, ["work: 100 -> 103 (+3.0%)  MORE WORK"])),
            (100, 98, (False, ["work: 100 -> 98 (-2.0%)"])),
            (100, 97, (False, ["work: 100 -> 97 (-3.0%)  less work"])),
            (0, 1, (True, ["work: 0 -> 1 (+inf%)  MORE WORK"])),
        ]:
            with self.subTest(before=before, after=after):
                self.assertEqual(fast.counter_changes({"work": [before] * 3},
                                                     {"work": [after] * 3}), expected)

    def test_scalar_metadata_and_type_changes(self):
        for before, after in [("a", "b"), (False, True), (None, "x"), ([1], 2)]:
            with self.subTest(before=before, after=after):
                grew, notes = fast.counter_changes({"metadata": [before]}, {"metadata": [after]})
                self.assertFalse(grew)
                self.assertEqual(len(notes), 1)
                self.assertIn("metadata changed", notes[0])

    def test_metadata_cannot_hide_numeric_growth(self):
        grew, notes = fast.counter_changes({"metadata": [[1]], "work": [100]},
                                           {"metadata": [[2]], "work": [103]})
        self.assertTrue(grew)
        self.assertEqual(len(notes), 2)
        self.assertIn("MORE WORK", notes[1])


def report(ms=1.0, status="ok", qps=None):
    records = []
    for rep in range(1, 4):
        records.append({"case": "micro", "rep": rep, "status": status, "metric": ms,
                        "samples": {"q": [ms] * 4}, "checks": [], "routes": [],
                        **({"throughput": [{"clients": n, "qps": v} for n, v in qps.items()]}
                           if qps is not None else {})})
    return {"reps": 3, "records": records}


def comparison(base, new, **options):
    out = io.StringIO()
    with patch.object(fast, "load_report", side_effect=[base, new]), contextlib.redirect_stdout(out):
        code = fast.compare(SimpleNamespace(base="base", new="new", **options))
    return code, out.getvalue()


class FastComparison(unittest.TestCase):
    def test_lost_case_fails_but_added_case_is_not_a_regression(self):
        empty = {"reps": 3, "records": []}
        code, text = comparison(report(), empty)
        self.assertEqual(code, 1)
        self.assertIn("LOST case", text)
        code, text = comparison(empty, report())
        self.assertEqual(code, 0)
        self.assertIn("without baseline", text)

    def test_matching_failures_never_acquire_a_timing_verdict(self):
        for status in ("failed", "wrong", "off-route", "timeout", "unsupported"):
            with self.subTest(status=status):
                code, text = comparison(report(100, status), report(1, status))
                self.assertEqual(code, 1)
                self.assertIn("NOT COMPARABLE", text)
                self.assertNotIn("faster", text)

    def test_expected_frontiers_are_recorded_without_performance_credit(self):
        base = report(100, "memory-limit")
        for r in base["records"]:
            r["expected_outcome"] = "memory-limit"
        code, text = comparison(base, copy.deepcopy(base))
        self.assertEqual(code, 0)
        self.assertIn("expected boundary; no timing verdict", text)

    def test_failed_check_or_route_excludes_an_ok_record(self):
        for key in ("checks", "routes"):
            new = report(0.1)
            new["records"][1][key] = [{"ok": False}]
            code, text = comparison(report(100), new)
            self.assertEqual(code, 1)
            self.assertIn("NOT COMPARABLE", text)

    def test_missing_samples_and_series_do_not_get_intersected_away(self):
        for samples in ({}, {"other": [1]}, {"q": []}, {"q": [float("nan")]}, {"q": ["bad"]}):
            new = report()
            new["records"][1]["samples"] = samples
            code, text = comparison(report(), new)
            self.assertEqual(code, 1)
            self.assertIn("missing/invalid timing", text)

    def test_missing_declared_repetition_fails(self):
        new = report()
        new["records"].pop()
        code, text = comparison(report(), new)
        self.assertEqual(code, 1)
        self.assertIn("missing/invalid timing series or repetitions", text)

    def test_missing_expected_frontier_repetition_also_fails(self):
        base = report(status="unsupported")
        for r in base["records"]:
            r["expected_outcome"] = "unsupported"
        new = copy.deepcopy(base)
        new["records"].pop()
        self.assertEqual(comparison(base, new)[0], 1)

    def test_microcase_floor_is_explicit_and_keeps_the_default(self):
        code, text = comparison(report(0.1), report(0.2))
        self.assertEqual(code, 0)
        self.assertIn("slower, below absolute floor", text)
        self.assertIn("latency floor 2 ms", text)
        code, text = comparison(report(0.1), report(0.2), min_ms=0)
        self.assertEqual(code, 1)
        self.assertIn("SLOWER", text)
        self.assertIn("latency floor 0 ms", text)
        for floor in (-1, float("nan"), float("inf")):
            with self.assertRaises(ValueError):
                comparison(report(), report(), min_ms=floor)

    def test_latency_noise_still_prevents_a_microcase_verdict(self):
        new = report(0.2)
        for r, value in zip(new["records"], (0.05, 0.2, 0.5)):
            r["metric"], r["samples"] = value, {"q": [value] * 4}
        self.assertEqual(comparison(report(0.1), new, min_ms=0)[0], 0)

    def test_qps_is_higher_is_better_per_width_independent_of_latency_floor(self):
        base = report(qps={1: 100, 8: 100})
        code, text = comparison(base, report(qps={1: 200, 8: 50}))
        self.assertEqual(code, 1)
        self.assertIn("1 clients QPS: 100 -> 200", text)
        self.assertIn("higher QPS", text)
        self.assertIn("8 clients QPS: 100 -> 50", text)
        self.assertIn("LOWER QPS", text)
        self.assertIn("95% CI", text)
        self.assertEqual(comparison(base, report(qps={1: 200, 8: 200}))[0], 0)

    def test_lost_width_or_missing_repetition_is_not_a_throughput_win(self):
        base = report(qps={1: 100, 8: 100})
        for new in (report(qps={1: 100}), report(qps={1: 100, 8: 100})):
            new["records"][1]["throughput"] = []
            code, text = comparison(base, new)
            self.assertEqual(code, 1)
            self.assertIn("QPS: NOT COMPARABLE", text)

    def test_qps_noise_is_taken_from_qps_not_latency(self):
        new = report(qps={1: 50})
        for r, value in zip(new["records"], (20, 50, 150)):
            r["throughput"][0]["qps"] = value
        code, text = comparison(report(qps={1: 100}), new)
        self.assertEqual(code, 0)
        self.assertIn("within noise", text)


if __name__ == "__main__":
    unittest.main()
