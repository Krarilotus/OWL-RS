"""A load case's `ready` series (fast.py READY, D16-606): the load plus the planner statistics
build the load command runs before it returns, where the side reports one; the load alone where
the side has no such step; compared against main only, never against a side whose perf lab leaves
the build out (the orchestrator's decision, 14 Nov 2026).

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
import release_gate as rg  # noqa: E402

CATALOG = fast.load_cases()
CASES = {c["name"]: c for c in CATALOG}


def series(case: str, result: dict) -> dict:
    """The samples a process of `case` reports, as the gate reads them."""
    return fast.invocation_observation(CASES[case], result, "", {"sweep": 0, "repeat": 0, "cache": "warm"})["samples"]


def side(case: str, results: list[dict]) -> dict:
    """One side's evidence of `case`: a pass per result, each its own block."""
    passes = []
    for n, result in enumerate(results, 1):
        obs = fast.invocation_observation(CASES[case], result, "", {"sweep": 0, "repeat": 0, "cache": "warm"})
        record = {"case": case, "status": "ok", "expected_outcome": "ok", "checks": [], "routes": [], "notes": [],
                  "samples": obs["samples"], "observations": [obs], "counters": {}}
        passes.append({"n": n, "report": {"block": n, "records": [record]}})
    return rg.evidence(passes, case)


def loads(seconds: float, statistics_ms: float | None, n: int = 6) -> list[dict]:
    """n processes' perf-lab results, each a little different (blocks whose contrasts differ)."""
    return [{"load_s": seconds * (1 + 0.01 * (k % 3)), "peak_mib": 100,
             **({"statistics_ms": statistics_ms} if statistics_ms is not None else {})} for k in range(n)]


class Ready(unittest.TestCase):
    def test_ready_is_the_load_plus_the_build_where_the_side_reports_one(self):
        # Spec (decision, 14 Nov): ready = load + the statistics build; load_s unchanged.
        self.assertEqual(series("load-entities", {"load_s": 2.5, "statistics_ms": 400.0}),
                         {"_": [2500.0], "ready": [2900.0]})

    def test_ready_is_the_load_where_the_side_has_no_such_step(self):
        # Spec: main's perf lab reports no statistics build; its ready is its load.
        self.assertEqual(series("load-entities", {"load_s": 2.5}), {"_": [2500.0], "ready": [2500.0]})

    def test_every_process_and_every_sweep_entry_has_its_own_ready(self):
        # Spec: fresh processes (`repeat`) each give one value; a sweep's entry names its series.
        repeats = {"_repeats": [{"load_s": 1.0, "statistics_ms": 100.0}, {"load_s": 1.2, "statistics_ms": 50.0}]}
        self.assertEqual(series("mem-load-2g", repeats)["ready"], [1100.0, 1250.0])
        swept = {"_repeats": [{"_sweep": "--threads 1", "load_s": 3.0, "statistics_ms": 200.0},
                              {"_sweep": "--threads 2", "load_s": 2.0, "statistics_ms": 200.0}]}
        got = series("load-threads", swept)
        self.assertEqual({k: v for k, v in got.items() if k.startswith("ready")},
                         {"ready --threads 1": [3200.0], "ready --threads 2": [2200.0]})
        self.assertEqual(got["--threads 1"], [3000.0])

    def test_each_sweep_process_declares_its_own_ready(self):
        # Spec (review r1 F1): a sweep's process reports its load (`_`) and its entry's ready, and
        # its invocation declares exactly those, not every entry's.
        case = CASES["load-threads"]
        plan, declared = fast.invocation_plan(case), fast.case_manifest([case])["invocation_series"]["load-threads"]
        by_entry = {p["sweep"]: d for p, d in zip(plan, declared)}
        self.assertEqual(by_entry[0], ["_", "ready --threads 1"])
        self.assertEqual(by_entry[4], ["_", "ready --threads 16"])
        process = series("load-threads", {"_sweep": "--threads 1", "load_s": 3.0, "statistics_ms": 200.0})
        self.assertEqual(sorted(process), by_entry[0])

    def test_a_case_not_timed_by_its_load_has_no_ready(self):
        # Spec: ready is a load case's; a query case's statistics build is outside its timing.
        query = next(c for c in CATALOG if c.get("metric") == "sum_p50_ms" and c.get("tool", "perf_lab") == "perf_lab")
        self.assertNotIn("ready", fast.ready_series(query, {"load_s": 1.0, "statistics_ms": 5.0}))
        self.assertNotIn("ready", fast.case_manifest([query])["series"][query["name"]])


class Family(unittest.TestCase):
    def test_ready_is_declared_against_main_and_never_against_base(self):
        # Spec (decision): gated against main only; against base `load_s` stays the load's series.
        family = rg.family_of(CATALOG, ["load-entities", "load-threads"])
        self.assertIn("main|load-entities|time/ready", family)
        self.assertIn("main|load-threads|time/ready --threads 1", family)
        self.assertIn("base|load-entities|time/_", family)
        self.assertIn("base|load-threads|time/--threads 1", family)
        self.assertEqual([h for h in family if h.startswith("base|") and "|time/ready" in h], [])

    def test_against_base_ready_is_not_compared_and_turns_into_no_loss(self):
        # Spec: "No row against base is to turn into a loss from a step only one side's harness
        # runs": the candidate's build (a third of its load) against base's perf lab, which has none.
        cand, base = side("load-entities", loads(2.0, 1000.0)), side("load-entities", loads(2.0, None))
        row = rg.compare_case(cand, base, "base", set())
        self.assertNotIn("time/ready", row["dimensions"])
        self.assertIn("not comparable against base", row["not_compared"]["time/ready"])
        self.assertNotEqual(row["dimensions"]["time/_"]["status"], "loss shown")

    def test_against_main_ready_is_compared(self):
        # Spec: against main, whose server has no build, ready is the load the user waits for.
        cand, main = side("load-entities", loads(2.0, 1000.0)), side("load-entities", loads(2.0, None))
        row = rg.compare_case(cand, main, "main", set())
        self.assertNotIn("time/ready", row["not_compared"])
        self.assertGreater(row["dimensions"]["time/ready"]["ratio"], 1.4)


if __name__ == "__main__":
    unittest.main()
