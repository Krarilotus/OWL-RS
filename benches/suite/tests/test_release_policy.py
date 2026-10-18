"""The release gate's noise policy (suitekit/compare.py `judge`, benches/PROTOCOL.md §5).

Defect 2 of the 18 Oct 2026 audit: identical timings were `inconclusive` and the gate
demanded a `win` on every row, so a neutral change could never pass.
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from suitekit import compare  # noqa: E402


def passes(*values):
    """One pass per argument, each a list of samples."""
    return [list(v) for v in values]


class Neutral(unittest.TestCase):
    def test_identical_timings_are_neutral_and_pass(self):
        same = passes([10.0, 10.1, 9.9], [10.0, 10.2, 9.8], [10.1, 10.0, 9.9], [10.0, 10.0, 10.1])
        decision = compare.judge(same, same, unit="ms")
        self.assertEqual(decision["verdict"], "neutral")
        self.assertFalse(decision["blocks"])
        self.assertAlmostEqual(decision["ratio"], 1.0)

    def test_a_change_within_the_margin_is_neutral_even_when_significant(self):
        base = passes([10.0] * 5, [10.1] * 5, [10.0] * 5, [10.1] * 5)
        new = passes([10.6] * 5, [10.7] * 5, [10.6] * 5, [10.7] * 5)  # +6 %, CI excludes 1
        decision = compare.judge(base, new, unit="ms")
        self.assertGreater(decision["ci"][0], 1.0)
        self.assertEqual(decision["verdict"], "neutral")
        self.assertFalse(decision["blocks"])


class Losses(unittest.TestCase):
    def test_a_slowdown_beyond_margin_and_noise_is_a_regression(self):
        base = passes([4.8, 4.9, 5.0], [4.7, 4.8, 4.9], [4.8, 4.8, 5.1], [4.9, 5.0, 4.8])
        new = passes([18.0, 18.2, 18.1], [18.3, 18.1, 18.0], [17.9, 18.0, 18.2], [18.1, 18.4, 18.0])
        decision = compare.judge(base, new, unit="ms")
        self.assertEqual(decision["verdict"], "regression")
        self.assertTrue(decision["blocks"])

    def test_a_wide_slowdown_that_the_interval_cant_settle_blocks_as_suspect(self):
        base = passes([2.0], [2.1], [2.0], [2.1])
        new = passes([3.3], [7.5], [3.4], [7.4])  # bimodal: its interval reaches 1
        decision = compare.judge(base, new, unit="ms")
        self.assertIn(decision["verdict"], ("regression", "suspect"))
        self.assertTrue(decision["blocks"])

    def test_the_margin_widens_to_the_measured_spread(self):
        base = passes([10.0], [13.0], [10.0], [13.0])   # 30 % between passes of one side
        new = passes([12.0], [14.0], [12.0], [14.0])    # +13 %: inside that noise
        decision = compare.judge(base, new, unit="ms")
        self.assertGreaterEqual(decision["margin"], 0.25)
        self.assertEqual(decision["verdict"], "neutral")

    def test_below_the_floor_only_strong_evidence_blocks(self):
        quantised = compare.judge(passes([0.003]), passes([0.004]) , unit="ms")  # one microsecond
        self.assertEqual(quantised["verdict"], "incomplete")  # one pass per side is no evidence
        base = passes([0.003], [0.003], [0.003], [0.003])
        new = passes([0.004], [0.004], [0.004], [0.004])
        self.assertFalse(compare.judge(base, new, unit="ms")["blocks"])  # timer resolution
        base = passes([0.007], [0.007], [0.008], [0.007])
        new = passes([0.026], [0.025], [0.027], [0.026])  # 3.7x, 19 µs, tight interval
        decision = compare.judge(base, new, unit="ms")
        self.assertEqual(decision["verdict"], "regression")
        self.assertTrue(decision["below_floor"])
        base = passes([0.30], [0.35], [0.30], [0.35])
        new = passes([0.36], [0.45], [0.36], [0.44])  # +22 %, 0.1 ms, interval reaches the margin
        self.assertFalse(compare.judge(base, new, unit="ms")["blocks"])

    def test_lower_throughput_is_the_loss_when_higher_is_better(self):
        base = passes([1000.0], [1010.0], [990.0], [1005.0])
        new = passes([600.0], [610.0], [590.0], [605.0])
        decision = compare.judge(base, new, unit="qps", higher_is_better=True)
        self.assertEqual(decision["verdict"], "regression")
        better = compare.judge(new, base, unit="qps", higher_is_better=True)
        self.assertEqual(better["verdict"], "better")
        self.assertFalse(better["blocks"])

    def test_memory_needs_64_mib_beyond_the_margin(self):
        base = passes([500.0], [505.0], [498.0], [502.0])
        self.assertFalse(compare.judge(base, passes([540.0], [545.0], [538.0], [542.0]), unit="MiB")["blocks"])
        decision = compare.judge(base, passes([800.0], [805.0], [798.0], [802.0]), unit="MiB")
        self.assertEqual(decision["verdict"], "regression")


class Evidence(unittest.TestCase):
    def test_missing_or_invalid_evidence_is_incomplete_and_blocks(self):
        good = passes([1.0], [1.0], [1.0], [1.0])
        for bad in ([], passes([1.0]), passes([1.0], []), passes([1.0], [float("nan")]), passes([1.0], [-1.0])):
            decision = compare.judge(good, bad, unit="ms")
            self.assertEqual(decision["verdict"], "incomplete", bad)
            self.assertTrue(decision["blocks"])

    def test_every_decision_reports_its_spread_and_interval(self):
        decision = compare.judge(passes([1.0], [1.2], [1.0], [1.1]), passes([1.0], [1.1], [1.0], [1.2]), unit="ms")
        for key in ("base", "new", "ratio", "ci", "spread", "margin", "verdict", "blocks", "passes"):
            self.assertIn(key, decision)
        self.assertEqual(decision["passes"], [4, 4])


if __name__ == "__main__":
    unittest.main()
