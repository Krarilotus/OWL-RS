"""The DL kit's consistency track: NRESE's hypertableau (`tableau_consistency`) answers first,
and an input it refuses as outside OWL 2 DL goes to `nrese_dl::beyond` (`dl_beyond --decide`),
whose verdict is the row's, with the path named in the detail column. One budget covers both:
no decision after it counts as solved, and either path running out of it is a `timeout`."""
import importlib.util
import itertools
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

DL = Path(__file__).resolve().parents[2] / "reasoning" / "dl"
sys.path.insert(0, str(DL))
# `reference` resolves its image through Docker on import; the consistency track doesn't use it.
sys.modules.setdefault("reference", SimpleNamespace(IMAGE="unused"))
spec = importlib.util.spec_from_file_location("nrese_consistency_contract", DL / "nrese.py")
nrese = importlib.util.module_from_spec(spec)
spec.loader.exec_module(nrese)

TASK = "ore_ont_14766-dl-consistency"
# tableau_consistency's answer and reason on ore_ont_14766 in the vendor comparison's run
# (its telemetry column left out): anonymous individuals as ObjectHasValue fillers.
REFUSED = SimpleNamespace(
    returncode=0, stderr="",
    stdout="ore_ont_14766.owl\tunsupported\t-\t13 reader diagnostics (not OWL 2 DL, left out)\n")
# dl_beyond --decide's line on the same file (fleet job ore13912-beyond-4a7a5a3a-r1).
DECIDED = SimpleNamespace(
    returncode=0, stderr="",
    stdout="ore_ont_14766.owl\tconsistent\t1\t0\t0\t7206\tcertified model of 1208 elements\n")
# dl_beyond --decide out of time: its own reason (crates/nrese-dl/src/beyond/mod.rs), the counts made up.
OUT_OF_TIME = SimpleNamespace(
    returncode=0, stderr="",
    stdout="ore_ont_14766.owl\tundecided\t2\t0\t0\t300000\tthe time budget ran out\n")


class ConsistencyTrack(unittest.TestCase):
    def row(self, answers, clock=None):
        """The kit's row for TASK with the drivers answering `answers` in turn, and the commands
        they got; `clock` the monotonic readings, if the test sets the time."""
        with tempfile.TemporaryDirectory() as tmp:
            cases = Path(tmp)
            (cases / "files").mkdir()
            (cases / "files" / "ore_ont_14766.owl").write_text("Ontology()\n", encoding="utf-8")
            (cases / "results.tsv").write_text(f"{TASK}\thermit\tconsistency\tconsistent\t204\t\n",
                                               encoding="utf-8")
            commands, replies = [], iter(answers)

            def run(command, **_):
                commands.append(command)
                return next(replies)

            args = SimpleNamespace(cases=str(cases), binary="tableau_consistency", beyond="dl_beyond",
                                   only=None, timeout=300, again=False)
            with patch.object(nrese.subprocess, "run", side_effect=run), \
                    patch.object(nrese.time, "monotonic", side_effect=clock or itertools.count()):
                nrese.consistency(args)
            rows = [l.split("\t") for l in (cases / "results.tsv").read_text(encoding="utf-8").splitlines()]
        return next(r for r in rows if r[1] == "nrese-dl"), commands

    def test_an_input_the_tableau_refuses_is_decided_by_the_beyond_path(self):
        row, commands = self.row([REFUSED, DECIDED])
        self.assertEqual(row[:4], [TASK, "nrese-dl", "consistency", "consistent"])
        self.assertIn("beyond: certified model of 1208 elements", row[5])
        self.assertEqual([c[0] for c in commands], ["tableau_consistency", "dl_beyond"])
        self.assertIn("--decide", commands[1])
        self.assertEqual(commands[1][-1], commands[0][-1])

    def test_the_beyond_path_running_out_of_time_is_a_timeout(self):
        self.assertEqual(self.row([REFUSED, OUT_OF_TIME])[0][3], "timeout")

    def test_a_refusal_after_the_budget_is_a_timeout_without_the_beyond_path(self):
        # The tableau refuses 310.75 s into a 300 s budget (reading and normalising come first).
        row, commands = self.row([REFUSED, DECIDED], clock=[0.0, 310.75, 310.8])
        self.assertEqual(row[3], "timeout")
        self.assertEqual([c[0] for c in commands], ["tableau_consistency"])

    def test_a_decision_after_the_budget_is_a_timeout(self):
        # The tableau decides 300.5 s into a 300 s budget; the references' runner counts that a timeout too.
        consistent = SimpleNamespace(returncode=0, stderr="", stdout="ore_ont_14766.owl\tconsistent\t-\t\n")
        self.assertEqual(self.row([consistent], clock=[0.0, 300.5])[0][3], "timeout")

    def test_the_beyond_path_gets_only_the_whole_seconds_left(self):
        _, commands = self.row([REFUSED, DECIDED], clock=[0.0, 120.6, 200.0])
        self.assertEqual(commands[1][commands[1].index("--timeout") + 1], "179")


if __name__ == "__main__":
    unittest.main()
