"""The suite `support-stress` (workloads.toml; its cases in benches/cases/core.toml): it runs
from the catalogue as any workload does, systems without its capabilities are reported as
unable, its records are the suite's rows, and an arm whose cap keeps every support set is
qualified by the exact answers.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches/suite"))
sys.path.insert(0, str(ROOT / "benches/fast"))

import fast  # noqa: E402
from suitekit import cases  # noqa: E402
from suitekit.schema import problems  # noqa: E402


class FromTheCatalogue(unittest.TestCase):
    """Spec: the owner's bench intent (run at any level, pruned by capability, the systems
    that can't express it reported, never dropped); review r1 R1."""

    def test_the_suite_runs_its_cases_and_reports_who_cant(self):
        with tempfile.TemporaryDirectory() as tmp:
            done = subprocess.run(
                [sys.executable, "-B", str(ROOT / "benches/suite/suite.py"), "run", "--dry-run",
                 "--runtime", "process", "--data", tmp, "--systems", "nrese,oxigraph",
                 "--workloads", "support-stress", "--tier", "support-stress=copies-8"],
                capture_output=True, text=True, cwd=ROOT)
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn("fast.py run --cases support-copies-8,support-copies-8-capped", done.stdout)
        # The rows echoed as written (a dry run's results file goes with its scratch).
        self.assertIn("oxigraph support-stress copies-8 run 0 load skipped "
                      "(lacks reason-owl2-rl, graph-access-inference)", done.stdout)


class Rows(unittest.TestCase):
    """Spec: suitekit/schema.py, one row per measured step; a wrong answer is `wrong`, a run
    that ended without its report is `failed` with its known issue."""

    def test_each_invocation_is_a_row_and_a_killed_one_says_why(self):
        case = {"name": "c", "workload": "support-stress", "tier": "copies-8", "semantics": "owl2-rl",
                "sweep": [["--support-sets", "4"], ["--support-sets", "off"]]}
        wrong = {"identity": {"sweep": 0, "repeat": 0, "cache": "warm"}, "exit": 0, "metric": 12.5,
                 "peak_mib": 60, "checks": [{"path": "q.count.value", "expected": 5125, "got": "0", "ok": False}]}
        report = {"started": "2026-10-27T01:02:03", "runtime": "native", "machine": {"host": "h"},
                  "git": {"commit": "0123456789abcdef"},
                  "records": [{"case": "c", "rep": 1, "status": "memory-limit", "page_cache": ["warm"],
                               "notes": ["Killed"], "verdict": {"verdict": "incomplete", "issue": "C-310"},
                               "observations": [wrong]}]}
        rows = fast.suite_rows(report, [case])
        self.assertEqual([problems(r) for r in rows], [[], []])
        self.assertEqual([(r.item, r.status, r.ms, r.peak_mib, r.run, r.regime) for r in rows],
                         [("c --support-sets 4", "wrong", 12.5, 60, 1, "owl2-rl"),
                          ("c --support-sets off", "failed", "", "", 1, "owl2-rl")])
        self.assertIn("q.count.value 0 (expected 5125)", rows[0].note)
        self.assertIn("C-310", rows[1].note)
        self.assertEqual({(r.system, r.workload, r.tier, r.task, r.date) for r in rows},
                         {("nrese", "support-stress", "copies-8", "query", "2026-10-27T01:02")})


class Qualification(unittest.TestCase):
    """Spec: StoreConfig::support_sets keeps that many sets per statement, so at a cap of at
    least an input's sets per target nothing is hidden and a reader's answers are the oracle's
    exactly (review r1 R2); below it the cap only hides."""

    def test_an_arm_whose_cap_keeps_every_set_rejects_a_wrong_answer(self):
        sets = {}
        for entry in tomllib.loads((ROOT / "benches/datasets.toml").read_text(encoding="utf-8"))["dataset"]:
            if entry.get("workload") == "support-stress" and entry["file"].endswith(".nq"):
                p = dict(word.split("=") for word in entry["made_by"].split()[4:])
                sets[f"volume:{entry['file']}"] = {"copies": lambda: int(p["sets"]),
                                                   "product": lambda: int(p["a"]) * int(p["b"]),
                                                   "ladder": lambda: int(p["width"]) ** int(p["length"])}[p["shape"]]()
        suite = [c for c in cases.load() if c.get("workload") == "support-stress"]
        oracle = {c["data"]: c["check"]["q.count.value"] for c in suite if isinstance(c["check"].get("q.count.value"), int)}
        answer = lambda probe, count: {"queries": [{"name": "probe", "value": probe},  # noqa: E731
                                                   {"name": "count", "value": count}]}
        checked = []
        for case in suite:
            for entry in case.get("sweep", [[]]):
                argv = [*case["args"], *entry]
                cap = argv[max(k for k, word in enumerate(argv) if word == "--support-sets") + 1]
                if cap != "off" and int(cap) < sets[case["data"]]:
                    continue
                right = oracle[case["data"]]
                verdicts = lambda result: [fast.check(p, w, result, {}, {})["ok"] for p, w in case["check"].items()]  # noqa: E731
                self.assertTrue(all(verdicts(answer("true", right))), f"{case['name']} {entry}")
                for probe, count in (("false", right), ("true", right - 1), ("false", 0)):
                    self.assertIn(False, verdicts(answer(probe, count)), f"{case['name']} {entry}: {probe}, {count}")
                checked.append(cap)
        self.assertIn("16", checked, "an arm capped at 16 on at most 16 sets is checked")
        self.assertIn("off", checked)
