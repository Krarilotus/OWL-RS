"""The DL kit's NRESE rows carry the driver's own outcome: an ontology `dl_classify` finds
inconsistent is `inconsistent`, as the references' rows say it, and one it doesn't decide
because it is outside OWL 2 DL is `unsupported`, never `classified` or `timeout`."""
import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

DL = Path(__file__).resolve().parents[2] / "reasoning" / "dl"
sys.path.insert(0, str(DL))
# `reference` resolves its image through Docker on import; `dl` doesn't use it.
sys.modules.setdefault("reference", SimpleNamespace(IMAGE="unused"))
spec = importlib.util.spec_from_file_location("nrese_outcomes_contract", DL / "nrese.py")
nrese = importlib.util.module_from_spec(spec)
spec.loader.exec_module(nrese)

TASK = "ore_ont_1-dl-classification"
# dl_classify on `Ontology(ClassAssertion(owl:Nothing <urn:w:a>))` (fleet job
# ore13912-lab-contract-4a7a5a3a-r1): exit 0, a taxonomy written, the verdict on stderr.
INCONSISTENT = SimpleNamespace(
    returncode=0, stdout="",
    stderr="0 triples; 0 classes, 0 subsumptions, 0 unsatisfiable, inconsistent\n"
           "profile read=0.1 owl=0.3 path=context-core total=1.1\n")
# An anonymous individual as an ObjectHasValue filler (Structural Specification §11.2), as
# dl_classify reported ore_ont_7320 in the vendor comparison's run.
UNSUPPORTED = SimpleNamespace(
    returncode=0,
    stdout="incomplete: consistency not decided (unsupported): 1 reader diagnostics (not OWL 2 DL, left out)\n"
           "incomplete: 1 classes not decided (unsupported: 1 reader diagnostics (not OWL 2 DL, left out))\n",
    stderr="0 triples; 1 classes, 0 subsumptions, 0 unsatisfiable, unknown\n"
           "profile read=0.1 owl=0.2 path=tableau total=0.4\n")


class DriverOutcomes(unittest.TestCase):
    def row(self, run):
        with tempfile.TemporaryDirectory() as tmp:
            cases = Path(tmp)
            (cases / "files").mkdir()
            (cases / "tax").mkdir()
            (cases / "files" / "ore_ont_1.owl").write_text("Ontology()\n", encoding="utf-8")
            (cases / "results.tsv").write_text(f"{TASK}\thermit\tclassify\tinconsistent\t16\t\n",
                                               encoding="utf-8")

            def driver(command, **_):
                Path(command[command.index("--tax") + 1]).write_text("= a a\n", encoding="utf-8")
                return run

            args = SimpleNamespace(cases=str(cases), binary="dl_classify", only=None, threads=1,
                                   timeout=300, again=False, extra=[])
            with patch.object(nrese.subprocess, "run", side_effect=driver):
                nrese.dl(args)
            rows = [l.split("\t") for l in (cases / "results.tsv").read_text(encoding="utf-8").splitlines()]
            return next(r for r in rows if r[1] == "nrese-dl")

    def test_an_inconsistent_ontology_is_reported_inconsistent(self):
        self.assertEqual(self.row(INCONSISTENT)[3], "inconsistent")

    def test_an_input_outside_owl2_dl_is_reported_unsupported(self):
        row = self.row(UNSUPPORTED)
        self.assertEqual(row[3], "unsupported")
        self.assertIn("not OWL 2 DL", row[5])


if __name__ == "__main__":
    unittest.main()
