"""The DL kit's EL track converts a task whose N-Triples are missing through the reference
image (`reference.IMAGE`, the runner's `ntriples` task), classifies the converted file, and
writes the canonical taxonomy's hash: equal to the reference's when the taxonomies are equal."""
import hashlib
import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

DL = Path(__file__).resolve().parents[2] / "reasoning" / "dl"
sys.path.insert(0, str(DL))
# `reference` resolves its image through Docker on import; a stand-in names it here.
sys.modules.setdefault("reference", SimpleNamespace(IMAGE="unused"))
spec = importlib.util.spec_from_file_location("nrese_el_contract", DL / "nrese.py")
nrese = importlib.util.module_from_spec(spec)
spec.loader.exec_module(nrese)

TASK = "ore_ont_1-el-classification"
THING = "http://www.w3.org/2002/07/owl#Thing"
NOTHING = "http://www.w3.org/2002/07/owl#Nothing"
# A reference's canonical taxonomy of A ⊑ B (docs/design/owl2-dl.md §11), written by hand.
REFERENCE = (f"< urn:a urn:b\n< urn:b {THING}\n= {NOTHING} {NOTHING}\n= {THING} {THING}\n"
             "= urn:a urn:a\n= urn:b urn:b\n")


class ElTrack(unittest.TestCase):
    def test_a_missing_conversion_runs_through_the_reference_image(self):
        image = sys.modules["reference"].IMAGE
        with tempfile.TemporaryDirectory() as tmp:
            cases = Path(tmp)
            (cases / "tax").mkdir()
            (cases / "tax" / f"{TASK}.hermit.tax").write_text(REFERENCE, encoding="utf-8")
            (cases / "results.tsv").write_text(f"{TASK}\thermit\tclassify\tclassified\t9\t\n", encoding="utf-8")
            commands = []

            def run(command, **_):
                commands.append(command)
                if image in command:  # the runner: the N-Triples and its result file
                    (cases / "tax" / f"{TASK}.nt").write_text("<urn:a> <urn:p> <urn:b> .\n", encoding="utf-8")
                    (cases / "results.nt.tsv").write_text("", encoding="utf-8")
                else:  # the EL classifier: its closure
                    Path(command[command.index("--out") + 1]).write_text("urn:a\turn:b\n", encoding="utf-8")
                return SimpleNamespace(returncode=0, stdout="", stderr="")

            args = SimpleNamespace(cases=str(cases), binary="classify", threads=1, timeout=60)
            with patch.object(nrese.subprocess, "run", side_effect=run):
                nrese.el(args)
            rows = [l.split("\t") for l in (cases / "results.tsv").read_text(encoding="utf-8").splitlines()]
        self.assertEqual(len(commands), 2)
        self.assertEqual(commands[0][commands[0].index(image) + 1], "batch")
        self.assertEqual(commands[1][-1], str(cases / "tax" / f"{TASK}.nt"))
        row = next(r for r in rows if r[1] == "nrese-el")
        self.assertEqual(row[3], "classified")
        self.assertEqual(row[5].split(" ")[0], hashlib.sha256(REFERENCE.encode()).hexdigest())


if __name__ == "__main__":
    unittest.main()
