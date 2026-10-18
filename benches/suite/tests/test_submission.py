"""Conformance kits prepare and require pinned data and retain suite counts.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches/suite"))


from suitekit import driver, workloads  # noqa: E402


class W3CRequired(unittest.TestCase):
    def test_preparation_failure_never_runs_the_suite_or_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            suite = SimpleNamespace(date="d", host="h", stamp="s", results=Path(tmp),
                                    publish=lambda key: "free", say=lambda text: None,
                                    host_command=Mock(return_value=1), emit=Mock())
            plan = workloads.plans(ROOT, "w3c-shacl", ["-"], {})[0]
            driver.Suite.kit(suite, plan, "nrese")
            suite.host_command.assert_called_once()
            self.assertEqual(suite.emit.call_args.kwargs["status"], "failed")
            self.assertIn("preparing its data failed", suite.emit.call_args.kwargs["note"])

    def test_required_environment_reaches_logged_and_unlogged_commands(self):
        suite = SimpleNamespace(args=SimpleNamespace(dry_run=False), root=ROOT)
        with tempfile.TemporaryDirectory() as tmp:
            for log in (None, Path(tmp) / "kit.log"):
                with self.subTest(log=log), patch.object(driver.subprocess, "run", return_value=SimpleNamespace(returncode=0)) as run:
                    self.assertEqual(driver.Suite.host_command(suite, ["test-tool"], log,
                                                              env=driver.kit_env({})), 0)
                    self.assertEqual(run.call_args.kwargs["env"]["NRESE_W3C_REQUIRED"], "1")

    def test_the_suites_w3c_kits_fetch_their_data_and_require_it(self):
        for name in ("w3c-sparql11", "w3c-shacl", "w3c-owl2-rl", "geosparql"):
            with self.subTest(workload=name):
                plan = next(p for p in workloads.plans(ROOT, name, ["-" if name != "geosparql" else "compliance"], {}))
                self.assertEqual(plan.prepare, ["bash", "scripts/fetch-w3c-tests.sh"])
                self.assertEqual(plan.command[-2:], ["--", "--nocapture"])
        self.assertEqual(driver.kit_env({}), {"NRESE_W3C_REQUIRED": "1"})
        self.assertEqual(driver.kit_env({"NRESE_W3C_OPTIONAL": "1"}), {})

    def test_a_kit_row_says_what_ran(self):
        log = "\n".join([
            "running 2 tests",
            "W3C SPARQL 1.1 on engine v2: passed / failed / skipped (failures shared with the reference)",
            "  QueryEvaluation: 300 / 0 / 0 (0)",
            "  total: 505 / 0 / 0 (0)",
            "  reference evaluator passed: 480",
            "W3C SHACL Core: 98 of 98 pass",
            "W3C OWL 2 RL test cases (RDF-based semantics): 86 passed, 0 failed (0 expected), 3 skipped",
            "GeoSPARQL compliance benchmark: 194 of 212 queries match the reference, 18 don't (18 disputed)",
            "skipped: W3C rdf-tests not found (run scripts/fetch-w3c-tests.sh)",
            "test w3c_sparql11 ... ok"])
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "kit.log"
            path.write_text(log, encoding="utf-8")
            self.assertEqual(driver.kit_counts(path).split("; "), [
                "SPARQL 1.1: 505 passed, 0 failed, 0 skipped", "W3C SHACL Core: 98 of 98 pass",
                "W3C OWL 2 RL test cases (RDF-based semantics): 86 passed, 0 failed (0 expected), 3 skipped",
                "GeoSPARQL compliance benchmark: 194 of 212 queries match the reference, 18 don't (18 disputed)",
                "skipped: W3C rdf-tests not found (run scripts/fetch-w3c-tests.sh)"])


if __name__ == "__main__":
    unittest.main()
