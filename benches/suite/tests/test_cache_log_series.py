"""A replayed query log (`queries = "@cache-log"`) is timed as one series: the planned series
of every case and the samples a perf lab run gives it must be the same.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches/suite"))
sys.path.insert(0, str(ROOT / "benches/fast"))

import fast  # noqa: E402
from suitekit import cases  # noqa: E402


class ReplayedLog(unittest.TestCase):
    """Spec: abba.py (`N samples of SERIES, F required`) and the release gate's family
    (release_gate.family_of) need samples of every series fast.case_manifest plans for a case,
    declared before any result. A replayed log's case (cache-replay-cached: "the same query log
    with the result cache on", metric `sum_p50_ms`) is one timing, the log's sum of medians per
    process; its generated queries, one execution each, are no series of their own. The
    huge-pages family of 11 Nov 2026 read "0 samples of _" in each of its 18 passes."""

    def test_the_planned_series_gets_the_replay_s_sum_per_process(self):
        catalog = cases.load()
        replays = [c for c in catalog if c.get("queries") == "@cache-log" and not c.get("sweep")]
        self.assertIn("cache-replay-cached", [c["name"] for c in replays])
        planned = fast.case_manifest(catalog)["series"]
        # A perf lab report of a three-query log, one execution each (`--runs 1`), as the
        # family's pass files hold it: each query's own sample, and the report's sum of p50s.
        result = {"queries": [{"name": f"0000{i}", "p50_ms": v, "samples_ms": [v]}
                              for i, v in enumerate((0.25, 20.5, 0.25))],
                  "sum_p50_ms": 21.0}
        for case in replays:
            got = fast.samples(case, result)
            with self.subTest(case=case["name"]):
                self.assertEqual({s: got.get(s) for s in planned[case["name"]]}, {"_": [21.0]})


if __name__ == "__main__":
    unittest.main()
