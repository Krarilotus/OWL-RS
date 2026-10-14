"""The perf bisect (scripts/bisect-perf.py; the owner, 14 Oct 2026, after a bisect that read one
revision's numbers into its neighbour's row and named the wrong merge): every revision measured
itself, bit-identical runs of two revisions refused, the ends checked to separate, and both ends
of the last step measured again before a first bad commit is named.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location("bisect_perf", ROOT / "scripts/bisect-perf.py")
bp = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bp)

REVS = [f"r{i}" for i in range(1, 9)]  # r5 is the first slow one


class Fake:
    """A revision's runs: fast before r5, slow from it; each call a little different, as runs are."""

    def __init__(self, first_bad="r5", fixed=None):
        self.first_bad, self.fixed, self.calls = first_bad, fixed or {}, []

    def __call__(self, rev):
        self.calls.append(rev)
        if rev in self.fixed:
            return list(self.fixed[rev])
        slow = rev != "good" and int(rev[1:]) >= int(self.first_bad[1:])
        jitter = len(self.calls) * 0.001
        return [(18.0 if slow else 4.7) + jitter + k * 0.01 for k in range(3)]


class Search(unittest.TestCase):
    def test_it_finds_the_first_bad_commit_and_measures_both_ends_again(self):
        fake = Fake()
        found = bp.Bisect(REVS, "good", fake, say=lambda _: None).run()
        self.assertEqual((found["first_bad"], found["last_good"]), ("r5", "r4"))
        # The ends of the last step were each measured twice: in the search and again at the end.
        self.assertGreaterEqual(fake.calls.count("r5"), 2)
        self.assertGreaterEqual(fake.calls.count("r4"), 2)
        self.assertEqual(fake.calls[-2:], ["r4", "r5"])

    def test_bit_identical_runs_of_two_revisions_are_refused(self):
        # 14 Oct: e0f84185's row carried c9506f37's 780 MiB and 2,253.35 ms.
        same = [780.0, 780.0, 780.0]
        fake = Fake(fixed={"r4": same, "r2": same})
        with self.assertRaisesRegex(bp.Refused, "bit-identical"):
            bp.Bisect(REVS, "good", fake, say=lambda _: None).run()

    def test_ends_that_dont_separate_are_refused(self):
        with self.assertRaisesRegex(bp.Refused, "overlap"):
            bp.threshold([4.0, 5.0, 9.0], [8.0, 9.5, 10.0])
        cut, higher = bp.threshold([4.6, 4.7, 4.8], [17.9, 18.0, 18.2])
        self.assertTrue(higher)
        self.assertTrue(4.8 < cut < 17.9)

    def test_a_last_step_that_doesnt_hold_is_refused(self):
        class Flaky(Fake):
            def __call__(self, rev):
                values = super().__call__(rev)
                # r4 slow when measured again: noise, not a verdict.
                return [v + 20 for v in values] if rev == "r4" and self.calls.count("r4") > 1 else values
        with self.assertRaisesRegex(bp.Refused, "didn't hold"):
            bp.Bisect(REVS, "good", Flaky(), say=lambda _: None).run()

    def test_the_metric_comes_from_the_runs_own_report(self):
        report = {"sum_p50_ms": 50.1, "peak_mib": 598,
                  "queries": [{"name": "q06-text-filter", "p50_ms": 18.12}, {"name": "q01", "p50_ms": 0.01}]}
        self.assertEqual(bp.metric(report, "queries.q06-text-filter.p50_ms"), 18.12)
        self.assertEqual(bp.metric(report, "sum_p50_ms"), 50.1)
        self.assertEqual(bp.metric(report, "scope_peak_mib", 3420), 3420.0)
        with self.assertRaises(bp.Refused):
            bp.metric(report, "queries.q99.p50_ms")
        with self.assertRaises(bp.Refused):
            bp.metric(report, "scope_peak_mib")


if __name__ == "__main__":
    unittest.main()
