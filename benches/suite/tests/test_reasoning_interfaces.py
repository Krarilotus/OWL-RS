"""The reasoning kits in the framework: the `driver` plan kind, regime `custom`, relation
counts (`count:REL`) checked before the materialisation is timed, reported rows in the
standings, and a timed step only inside the quiet slot.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches/suite"))

from suitekit import quiet, reported, versus, workloads  # noqa: E402
from suitekit.driver import Suite  # noqa: E402
from suitekit.schema import Result, Writer, read  # noqa: E402


def row(system, run, task, **fields):
    return Result(date="d", host="h", runtime="docker", system=system, version="-", publish="free",
                  workload="claros", tier="l", regime="custom", run=run, task=task, **fields)


class DriverKind(unittest.TestCase):
    def test_the_kits_rows_join_the_run_and_a_silent_kit_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp)
            writes = ("import csv, pathlib, sys; from suitekit.schema import Result, Writer; "
                      "Writer(pathlib.Path(sys.argv[1]) / 'results.csv').write(Result(date='d', host='h', "
                      "runtime='docker', system='souffle', version='-', publish='free', workload='claros', tier='l', "
                      "regime='custom', run=1, task='reason', ms=5.0))")
            plan = workloads.Plan("claros", "l", kind="driver",
                                  driver=[[sys.executable, "-c", f"import sys; sys.path.insert(0, 'benches/suite'); {writes}",
                                           "{results}/timed"]])
            suite = SimpleNamespace(results=results, stamp="s", date="d", host="h", say=lambda *a: None, written=[],
                                    args=SimpleNamespace(dry_run=False, runs=3), root=ROOT, publish=lambda k: "free")
            suite.writer = Writer(results / "results.csv")
            suite.host_command = lambda command, log=None: Suite.host_command(suite, command, log)
            suite.emit = lambda base, **fields: Suite.emit(suite, base, **fields)
            (results / "driver/claros-l/timed").mkdir(parents=True)
            Suite.driver(suite, plan, ["souffle"])
            rows = read(results / "results.csv")
            self.assertEqual([(r["system"], r["task"], r["regime"]) for r in rows], [("souffle", "reason", "custom")])
            # A kit that writes nothing leaves a failed row per system, not silence.
            plan.driver = [[sys.executable, "-c", "pass"]]
            plan.tier = "le"
            Suite.driver(suite, plan, ["souffle", "nrese"])
            failed = [r for r in read(results / "results.csv") if r["status"] == "failed"]
            self.assertEqual(sorted(r["system"] for r in failed), ["nrese", "souffle"])

    def test_the_reasoning_workloads_have_driver_plans(self):
        plans = {name: workloads.plans(ROOT, name, None, {}) for name in ("claros", "datalog-graphs", "commit-latency")}
        for name, found in plans.items():
            with self.subTest(name=name):
                self.assertTrue(found and all(p.kind == "driver" and p.driver for p in found))
        claros = plans["claros"][0]
        # Qualify, untimed, before the timed step reads the qualified totals.
        self.assertIn("--qualify", claros.driver[1])
        self.assertIn("{results}/qualify/qualified.json", claros.driver[2])
        # lubm keeps its suite cycle; RDFox's programs on it go to the kit.
        self.assertEqual(workloads.plans(ROOT, "lubm", ["1"], {})[0].kind, "cycle")
        self.assertEqual(workloads.plans(ROOT, "lubm", ["rdfox-l-125"], {})[0].kind, "driver")


class ReasonVerdict(unittest.TestCase):
    def entries(self, nrese_counts="ok"):
        with tempfile.TemporaryDirectory() as tmp:
            writer = Writer(Path(tmp) / "results.csv")
            for system, ms in (("nrese", 100.0), ("souffle", 400.0)):
                for run in (1, 2, 3):
                    writer.write(row(system, run, "reason", ms=ms * (1 + run / 100), rows=89))
                writer.write(row(system, 1, "count", item="count:sameAs", rows=5,
                                 status=nrese_counts if system == "nrese" else "ok"))
            return {e["system"]: e for e in versus.versus(Path(tmp))}

    def test_the_materialisation_is_timed_when_the_counts_passed(self):
        found = self.entries()
        self.assertEqual(found["souffle"]["verdict"], "win")
        self.assertEqual(found["souffle"]["timed"], "1/1")

    def test_a_failed_count_leaves_the_materialisation_untimed(self):
        found = self.entries("wrong")
        self.assertEqual(found["souffle"]["verdict"], "no-verdict")
        self.assertIn("count:sameAs wrong (nrese)", found["souffle"]["answers"])


class ReportedRows(unittest.TestCase):
    def test_a_kits_reported_rows_are_entries_with_their_hardware_and_source(self):
        entries = [e for e in reported.load() if e["system"] == "rdfox"]
        self.assertTrue(entries)
        for e in entries:
            self.assertTrue(e["hardware"] and e["source"] and e["date"])
        self.assertIn("timeout", {e["value"] for e in entries})


class QuietSlot(unittest.TestCase):
    def test_a_timed_step_runs_only_inside_the_slot(self):
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("NRESE_QUIET_TOKEN", None)
            with self.assertRaises(RuntimeError):
                with quiet.step("timed", timed=True):
                    pass
            with quiet.step("untimed", timed=False):
                pass
            os.environ["NRESE_QUIET_TOKEN"] = "t"
            with quiet.step("timed", timed=True):
                pass

    def test_sizes(self):
        self.assertEqual(quiet.gib("28g"), 28.0)
        self.assertEqual(quiet.gib("512m"), 0.5)
        self.assertEqual(quiet.gib("0"), 0.0)


if __name__ == "__main__":
    unittest.main()
