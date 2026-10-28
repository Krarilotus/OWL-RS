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


def report(probe, count, members: int, zero: str = "") -> dict:
    """A support-stress perf lab report as fast.py checks it: the probe's and the count's
    answers, and the tallies of the reader's updates (examples/perf_lab/support.rs), pending and
    applied, with one marker per member of ex:D (`members`) of each kind an update has written
    by then: u1 `marked`, u2 also `seen`. `zero` (UPDATE.WHEN.FIELD) is answered 0."""
    tallies = {"u1": {"d": members, "marked": members, "seen": 0},
               "u2": {"d": members, "marked": members, "seen": members}}
    updates = {u: {when: dict(tally) for when in ("pending", "applied")} for u, tally in tallies.items()}
    if zero:
        update, when, field = zero.split(".")
        updates[update][when][field] = 0
    return {"queries": [{"name": "probe", "value": probe}, {"name": "count", "value": count}],
            "support": {"updates": updates}}


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


class Series(unittest.TestCase):
    """Spec: abba.py and the release gate need samples of every series the catalogue plans for
    a case (fast.case_manifest, declared before any result); the case runner keeps a perf lab
    run's samples by fast.samples (O4's timing: support-copies-17 had none of `_`)."""

    maxDiff = None

    def test_every_planned_series_is_filled_warm_and_cold_or_warm_only(self):
        catalog = cases.load()
        planned = fast.case_manifest(catalog)["series"]
        phases = ("sets", "view", "first_read", "commit_1", "read_1", "commit_2", "read_2",
                  "pending_1", "update_1", "pending_2", "update_2")
        updates = {u: {"pending_ms": 1.0, "update_ms": 1.0, "read_ms": 1.0} for u in ("u1", "u2")}
        empty = []
        for case in (c for c in catalog if c.get("workload") == "support-stress"):
            sweep = case.get("sweep", [[]])
            for modes in (["warm", "cold"], ["warm"]):
                # Each invocation's perf lab report as measure() keeps it: the last, with every
                # invocation's in `_repeats` where there are several.
                runs = [{"support": {"first": {"read_ms": 1.0}, "updates": updates},
                         "phase_samples": {p: [1.0] for p in phases},
                         "_sweep": " ".join(sweep[i["sweep"]]), "_page_cache": i["cache"]}
                        for i in fast.invocation_plan(case, modes)]
                result = {**runs[-1], **({"_repeats": runs} if len(runs) > 1 else {})}
                got = fast.samples(case, result)
                empty += [f"{case['name']} {'+'.join(modes)}: {s}" for s in planned[case["name"]] if not got.get(s)]
        self.assertEqual(empty, [])


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
        checked = []
        for case in suite:
            for entry in case.get("sweep", [[]]):
                argv = [*case["args"], *entry]
                cap = argv[max(k for k, word in enumerate(argv) if word == "--support-sets") + 1]
                if cap != "off" and int(cap) < sets[case["data"]]:
                    continue
                right = oracle[case["data"]]
                verdicts = lambda result: [fast.check(p, w, result, {}, {})["ok"] for p, w in case["check"].items()]  # noqa: E731
                self.assertTrue(all(verdicts(report("true", right, right))), f"{case['name']} {entry}")
                for probe, count in (("false", right), ("true", right - 1), ("false", 0)):
                    self.assertIn(False, verdicts(report(probe, count, right)), f"{case['name']} {entry}: {probe}, {count}")
                checked.append(cap)
        self.assertIn("16", checked, "an arm capped at 16 on at most 16 sets is checked")
        self.assertIn("off", checked)


class Updates(unittest.TestCase):
    """Spec: a case timing the reader's updates checks what its timed step did (Sol's review r1
    of the D6 harness, H1 and H2): the markers the update leaves, one per member of ex:D, and
    for the pending read its own answer, where u2's `seen` markers exist only if its operations
    ran in order. An update that matches nothing, or a pending read that ignores its operations
    or answers 0, fails its case's check."""

    def test_an_update_case_fails_where_its_step_does_nothing(self):
        suite = {c["name"]: c for c in cases.load() if c.get("metric", "").startswith("support.updates.")}
        nothing = {"support-update-committed": ["u1.applied.marked"],
                   "support-update-pending": ["u2.applied.seen"],
                   "support-pending-read": ["u2.pending.seen", "u2.pending.d"]}
        self.assertEqual(sorted(suite), sorted(nothing))
        for name, case in suite.items():
            right = case["check"]["q.count.value"]
            verdicts = lambda result: [fast.check(p, w, result, {}, {})["ok"] for p, w in case["check"].items()]  # noqa: E731
            self.assertTrue(all(verdicts(report("true", right, right))), name)
            for zero in nothing[name]:
                self.assertIn(False, verdicts(report("true", right, right, zero)), f"{name}: {zero} answered 0")
