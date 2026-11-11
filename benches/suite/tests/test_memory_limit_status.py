"""A run the engine stops at its memory budget is a memory limit, whatever form the limit's
error takes: the process limit (`ProcessMemoryLimit`) or the shared query budget's refusal
(`MemoryLimit(BudgetExceeded …)`).

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches/suite"))
sys.path.insert(0, str(ROOT / "benches/fast"))

import abba  # noqa: E402
import fast  # noqa: E402
import release_gate as rg  # noqa: E402
from suitekit import cases  # noqa: E402

# support-ladder-256 on 62b9e1854 (the release smoke's record, office-pc, 11 Nov 2026): the
# shared budget refused the first restricted read; the process exited 1.
STDERR = ("Error: SparqlEvaluation(MemoryLimit(BudgetExceeded { limit: 4831838208, requested: 134217792, "
          "used: 4716745931, shared: true }))\n")
RESULT = {"error": "SPARQL evaluation error: the server's query memory is in use: 4498 MiB in use, "
                   "128 MiB more requested, limit 4608 MiB"}


def record(case: str, expected: str) -> dict:
    """The run's record as fast.py writes it: no answers (the read was refused), its status from
    classify_failure()."""
    checks = [{"path": "q.probe.value", "expected": "true", "got": None, "ok": False},
              {"path": "q.count.value", "expected": 5030, "got": None, "ok": False}]
    return {"case": case, "status": fast.classify_failure(1, STDERR, RESULT), "expected_outcome": expected,
            "notes": [STDERR.strip()], "checks": checks, "routes": [], "samples": {},
            "observations": [{"identity": {"sweep": 0, "repeat": 0, "cache": "warm"}, "samples": {},
                              "exit": 1, "checks": checks, "routes": []}]}


class BudgetRefusal(unittest.TestCase):
    """Spec: a case declaring `outcome = "memory-limit"` (support-ladder-256, benches/cases/core.toml)
    is checked, not timed: a run that ends at a memory limit is that outcome, a known issue to the
    release gate (release_gate.state(): `known`), not a failure; the same end on a case declaring no
    outcome is a failure (fast.not_as_expected, rg.state, abba.record_check)."""

    def test_the_budget_s_refusal_is_the_declared_memory_limit(self):
        catalog = cases.load()
        self.assertEqual(next(c for c in catalog if c["name"] == "support-ladder-256").get("outcome"), "memory-limit")
        run = record("support-ladder-256", "memory-limit")
        self.assertEqual(run["status"], "memory-limit")
        self.assertEqual(fast.not_as_expected([run]), [])
        self.assertEqual(rg.state(rg.evidence([{"n": 1, "report": {"records": [run]}}], run["case"])), "known")
        self.assertEqual(abba.record_check(catalog, run), [])

    def test_the_same_end_without_a_declared_outcome_fails(self):
        catalog = cases.load()
        undeclared = next(c for c in catalog if c["name"] == "support-ladder-256-capped")
        self.assertEqual(undeclared.get("outcome", "ok"), "ok")
        run = record("support-ladder-256-capped", "ok")
        self.assertEqual(fast.not_as_expected([run]), [run])
        self.assertEqual(rg.state(rg.evidence([{"n": 1, "report": {"records": [run]}}], run["case"])), "failed")
        self.assertNotEqual(abba.record_check(catalog, run), [])

    def test_the_process_limit_stays_a_memory_limit(self):
        # mem-reason-lubm100-2g's known end (12 Oct 2026), and a kill (exit 137).
        self.assertEqual(fast.classify_failure(1, "Error: ProcessMemoryLimit { limit: 1610612736 }", {}), "memory-limit")
        self.assertEqual(fast.classify_failure(137, "", {}), "memory-limit")
        self.assertEqual(fast.classify_failure(1, "Error: parse error at line 3", {}), "failed")


if __name__ == "__main__":
    unittest.main()
