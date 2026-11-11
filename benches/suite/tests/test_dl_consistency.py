"""The DL kit's consistency track: NRESE's hypertableau (`tableau_consistency`) answers first,
and an input it refuses as outside OWL 2 DL goes to `nrese_dl::beyond` (`dl_beyond --decide`),
whose verdict is the row's, with the path named in the detail column."""
import importlib.util
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


class ConsistencyTrack(unittest.TestCase):
    def test_an_input_the_tableau_refuses_is_decided_by_the_beyond_path(self):
        with tempfile.TemporaryDirectory() as tmp:
            cases = Path(tmp)
            (cases / "files").mkdir()
            (cases / "files" / "ore_ont_14766.owl").write_text("Ontology()\n", encoding="utf-8")
            (cases / "results.tsv").write_text(f"{TASK}\thermit\tconsistency\tconsistent\t204\t\n",
                                               encoding="utf-8")
            commands = []

            def run(command, **_):
                commands.append(command)
                return REFUSED if len(commands) == 1 else DECIDED

            args = SimpleNamespace(cases=str(cases), binary="tableau_consistency", beyond="dl_beyond",
                                   only=None, timeout=300, again=False)
            with patch.object(nrese.subprocess, "run", side_effect=run):
                nrese.consistency(args)
            rows = [l.split("\t") for l in (cases / "results.tsv").read_text(encoding="utf-8").splitlines()]
        row = next(r for r in rows if r[1] == "nrese-dl")
        self.assertEqual(row[:4], [TASK, "nrese-dl", "consistency", "consistent"])
        self.assertIn("beyond: certified model of 1208 elements", row[5])
        self.assertEqual([c[0] for c in commands], ["tableau_consistency", "dl_beyond"])
        self.assertIn("--decide", commands[1])
        self.assertEqual(commands[1][-1], commands[0][-1])


if __name__ == "__main__":
    unittest.main()
