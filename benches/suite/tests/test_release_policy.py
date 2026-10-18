"""The release gate's noise policy (suitekit/compare.py `judge`, benches/PROTOCOL.md §5).

The owner's rule (18 Oct 2026): no loss is tolerated, only an approved trade-off. Any
statistically clear loss in time or memory blocks (its interval entirely on the worse side),
whatever its size above the measurement's resolution; a case too noisy to decide is suspect,
with the passes it needs, never a pass. (Defect 2 of the 18 Oct audit, before: identical
timings were `inconclusive` and a neutral change could never pass.)
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


def steady(value, n=4, wobble=0.002):
    """n passes of a series that repeats within `wobble`."""
    return [[value * (1 + wobble * ((i % 3) - 1)), value, value * (1 + wobble * (((i + 1) % 3) - 1))]
            for i in range(n)]


class Neutral(unittest.TestCase):
    def test_identical_timings_are_neutral_and_pass(self):
        same = passes([10.0, 10.1, 9.9], [10.0, 10.2, 9.8], [10.1, 10.0, 9.9], [10.0, 10.0, 10.1])
        decision = compare.judge(same, same, unit="ms")
        self.assertEqual(decision["verdict"], "neutral")
        self.assertFalse(decision["blocks"])
        self.assertAlmostEqual(decision["ratio"], 1.0)

    def test_a_change_inside_the_noise_and_the_interval_is_neutral(self):
        base = passes([10.0], [10.4], [9.8], [10.2])
        new = passes([10.1], [10.3], [9.9], [10.2])
        decision = compare.judge(base, new, unit="ms")
        self.assertLess(decision["ci"][0], 1)
        self.assertEqual(decision["verdict"], "neutral")


class Losses(unittest.TestCase):
    """No relative tolerance: a repeatable loss blocks whatever its size."""

    def test_repeatable_slowdowns_of_8_3_and_1_percent_block(self):
        for factor in (1.08, 1.03, 1.01):
            decision = compare.judge(steady(10.0), steady(10.0 * factor), unit="ms")
            self.assertEqual(decision["verdict"], "regression", factor)
            self.assertTrue(decision["blocks"])

    def test_a_large_slowdown_blocks(self):
        base = passes([4.8, 4.9, 5.0], [4.7, 4.8, 4.9], [4.8, 4.8, 5.1], [4.9, 5.0, 4.8])
        new = passes([18.0, 18.2, 18.1], [18.3, 18.1, 18.0], [17.9, 18.0, 18.2], [18.1, 18.4, 18.0])
        self.assertEqual(compare.judge(base, new, unit="ms")["verdict"], "regression")

    def test_more_memory_blocks_without_a_mib_tolerance(self):
        base = passes([500.0], [501.0], [500.0], [501.0])
        decision = compare.judge(base, passes([520.0], [521.0], [520.0], [521.0]), unit="MiB")
        self.assertEqual(decision["verdict"], "regression")
        self.assertFalse(compare.judge(base, passes([501.0], [502.0], [501.0], [502.0]), unit="MiB")["blocks"])

    def test_lower_throughput_is_the_loss_when_higher_is_better(self):
        base = passes([1000.0], [1010.0], [990.0], [1005.0])
        new = passes([600.0], [610.0], [590.0], [605.0])
        self.assertEqual(compare.judge(base, new, unit="qps", higher_is_better=True)["verdict"], "regression")
        better = compare.judge(new, base, unit="qps", higher_is_better=True)
        self.assertEqual(better["verdict"], "better")
        self.assertFalse(better["blocks"])


class Resolution(unittest.TestCase):
    """Only the measurement's resolution: 5 µs (the timer, from the A/A passes), 2 MiB."""

    def test_differences_under_the_timer_resolution_are_no_verdict(self):
        base = passes([0.003], [0.003], [0.003], [0.003])
        decision = compare.judge(base, passes([0.004], [0.004], [0.004], [0.004]), unit="ms")
        self.assertEqual(decision["verdict"], "neutral")
        self.assertTrue(decision["below_resolution"])

    def test_a_clear_loss_above_the_resolution_blocks_however_small_the_time(self):
        base = passes([0.007], [0.007], [0.008], [0.007])
        new = passes([0.026], [0.025], [0.027], [0.026])  # 19 µs
        self.assertEqual(compare.judge(base, new, unit="ms")["verdict"], "regression")


class Undecided(unittest.TestCase):
    def test_a_case_too_noisy_to_decide_is_suspect_with_the_passes_it_needs(self):
        base = passes([10.0], [10.6], [9.5], [10.3])
        new = passes([10.6], [11.6], [9.9], [11.3])  # +8 %, the interval reaching 1 and past 2 sd
        decision = compare.judge(base, new, unit="ms")
        self.assertEqual(decision["verdict"], "suspect")
        self.assertTrue(decision["blocks"])
        self.assertGreater(decision["needs_passes"], 4)

    def test_a_bimodal_case_is_never_passed(self):
        base = passes([2.0], [2.1], [2.0], [2.1])
        new = passes([3.3], [7.5], [3.4], [7.4])
        self.assertIn(compare.judge(base, new, unit="ms")["verdict"], ("regression", "suspect"))

    def test_the_level_is_a_parameter(self):
        base = passes([10.0], [10.1], [9.9], [10.0])
        new = passes([10.2], [10.25], [10.05], [10.15])
        loose = compare.judge(base, new, unit="ms", confidence=0.8)
        strict = compare.judge(base, new, unit="ms", confidence=0.999)
        self.assertEqual(loose["verdict"], "regression")
        self.assertNotEqual(strict["verdict"], "regression")
        self.assertLess(strict["ci"][0], loose["ci"][0])


class Evidence(unittest.TestCase):
    def test_missing_or_invalid_evidence_is_incomplete_and_blocks(self):
        good = passes([1.0], [1.0], [1.0], [1.0])
        for bad in ([], passes([1.0]), passes([1.0], []), passes([1.0], [float("nan")]), passes([1.0], [-1.0])):
            decision = compare.judge(good, bad, unit="ms")
            self.assertEqual(decision["verdict"], "incomplete", bad)
            self.assertTrue(decision["blocks"])

    def test_every_decision_reports_its_interval_noise_and_level(self):
        decision = compare.judge(passes([1.0], [1.2], [1.0], [1.1]), passes([1.0], [1.1], [1.0], [1.2]), unit="ms")
        for key in ("base", "new", "ratio", "ci", "noise", "spread", "confidence", "needs_passes", "verdict",
                    "blocks", "passes"):
            self.assertIn(key, decision)
        self.assertEqual(decision["passes"], [4, 4])
        self.assertEqual(decision["confidence"], 0.95)


if __name__ == "__main__":
    unittest.main()
