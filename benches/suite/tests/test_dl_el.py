"""The DL kit's EL track converts a task whose N-Triples are missing through the reference
image (by its pinned ID, the runner's `ntriples` task), classifies the converted file, and
writes the canonical taxonomy's hash: equal to the reference's when the taxonomies are equal."""
import hashlib
import importlib.util
import os
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

DL = Path(__file__).resolve().parents[2] / "reasoning" / "dl"
sys.path.insert(0, str(DL))
sys.path.insert(0, str(DL.parents[1] / "suite"))
import isolation  # noqa: E402

READS = ("benches/reasoning/dl/canonical.py", "benches/reasoning/dl/nrese.py", "benches/reasoning/dl/reference.py",
         "benches/suite/suitekit/__init__.py", "benches/suite/suitekit/hosts.py", "benches/suite/suitekit/lock.py",
         "benches/suite/suitekit/ns.py")
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
        # Spec: nrese.py el converts a missing N-Triples input and writes a classified row with status 0.
        image = "sha256:" + "ab" * 32  # the reference image's ID, pinned at the start (ns.pin_images)
        with tempfile.TemporaryDirectory(dir=isolation.OWN) as tmp:
            cases = Path(tmp)
            (cases / "tax").mkdir()
            (cases / "tax" / f"{TASK}.hermit.tax").write_text(REFERENCE, encoding="utf-8")
            (cases / "results.tsv").write_text(f"{TASK}\thermit\tclassify\tclassified\t9\t\n", encoding="utf-8")
            commands = []

            def run(command, **_):
                if command[1:3] == ["image", "inspect"]:
                    return SimpleNamespace(returncode=0, stdout=image, stderr="")
                commands.append(command)
                if "batch" in command:  # the runner: the N-Triples, then its `converted` row (Runner.java)
                    (cases / "tax" / f"{TASK}.nt").write_text("<urn:a> <urn:p> <urn:b> .\n", encoding="utf-8")
                    (cases / "results.nt.tsv").write_text(f"{TASK}\tconvert\tntriples\tconverted\t5\t{TASK}.nt\n",
                                                          encoding="utf-8")
                else:  # the EL classifier: its closure
                    Path(command[command.index("--out") + 1]).write_text("urn:a\turn:b\n", encoding="utf-8")
                return SimpleNamespace(returncode=0, stdout="", stderr="")

            argv = ["nrese.py", "el", str(cases), "--binary", "classify", "--timeout", "60"]
            env = isolation.inherit("NRESE_", "BENCH_INPUTS_", "FLEET_", NRESE_LOCK_DIR=str(cases / "locks"))
            with patch.dict(os.environ, env, clear=True), patch.object(sys, "argv", argv), \
                    patch.object(nrese.subprocess, "run", side_effect=run):
                try:
                    nrese.main()
                except SystemExit as done:
                    status = done.code
                except AttributeError as error:
                    status = str(error)
            self.assertEqual(status, 0)
            self.assertTrue((cases / "tax" / f"{TASK}.nt").is_file())
            rows = [l.split("\t") for l in (cases / "results.tsv").read_text(encoding="utf-8").splitlines()]
        row = next(r for r in rows if r[1] == "nrese-el")
        self.assertEqual(row[3], "classified")
        self.assertEqual(row[5].split(" ")[0], hashlib.sha256(REFERENCE.encode()).hexdigest())
        self.assertEqual(len(commands), 2)
        self.assertEqual(commands[0][commands[0].index("batch") - 1], image)
        self.assertEqual(commands[1][-1], str(cases / "tax" / f"{TASK}.nt"))


if __name__ == "__main__":
    unittest.main()
