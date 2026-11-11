"""The DL kit's EL track converts a task whose N-Triples are missing through the reference
image (`reference.IMAGE`, the runner's `ntriples` task) before it classifies."""
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


class ElTrack(unittest.TestCase):
    def test_a_missing_conversion_runs_through_the_reference_image(self):
        image = sys.modules["reference"].IMAGE
        with tempfile.TemporaryDirectory() as tmp:
            cases = Path(tmp)
            (cases / "tax").mkdir()
            (cases / "results.tsv").write_text("ore_ont_1-el-classification\thermit\tclassify\tclassified\t9\t\n",
                                               encoding="utf-8")
            commands = []

            def run(command, **_):
                commands.append(command)
                (cases / "results.nt.tsv").write_text("", encoding="utf-8")
                return SimpleNamespace(returncode=0, stdout="", stderr="")

            args = SimpleNamespace(cases=str(cases), binary="classify", threads=1, timeout=60)
            with patch.object(nrese.subprocess, "run", side_effect=run):
                nrese.el(args)
        self.assertEqual(len(commands), 1)
        self.assertIn(image, commands[0])
        self.assertEqual(commands[0][commands[0].index(image) + 1], "batch")


if __name__ == "__main__":
    unittest.main()
