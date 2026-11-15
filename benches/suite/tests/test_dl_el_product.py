"""The DL kit's `dl` driver answers every classification task with the product's classifier
(`dl_classify`, what `/dataset/classification` serves), the EL track's included, so that ORE's
classification tasks are counted with one classifier. The EL classifier's `nrese-el` rows stay
as the oracle beside them. Consistency tasks stay the `consistency` driver's."""
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
spec = importlib.util.spec_from_file_location("nrese_el_product_contract", DL / "nrese.py")
nrese = importlib.util.module_from_spec(spec)
spec.loader.exec_module(nrese)

CLASSIFIED = SimpleNamespace(
    returncode=0, stdout="",
    stderr="0 triples; 1 classes, 0 subsumptions, 0 unsatisfiable, consistent\n"
           "profile read=0.1 owl=0.2 path=context-core total=0.4\n")
# The references' rows of one task of each track (ore.py manifest's task ids).
REFERENCES = [
    ("ore_ont_1-el-classification", "classify"),
    ("ore_ont_2-dl-classification", "classify"),
    ("ore_ont_3-pure_dl-classification", "classify"),
    ("ore_ont_4-dl-consistency", "consistency"),
]


class ProductClassifierOnEveryClassificationTask(unittest.TestCase):
    def rows(self):
        with tempfile.TemporaryDirectory() as tmp:
            cases = Path(tmp)
            (cases / "files").mkdir()
            (cases / "tax").mkdir()
            for tid, _ in REFERENCES:
                (cases / "files" / f"{tid.split('-')[0]}.owl").write_text("Ontology()\n", encoding="utf-8")
            (cases / "results.tsv").write_text(
                "".join(f"{tid}\thermit\t{kind}\tclassified\t9\t\n" for tid, kind in REFERENCES)
                + "ore_ont_1-el-classification\tnrese-el\tclassify\tclassified\t3\tabc\n",
                encoding="utf-8")

            def driver(command, **_):
                Path(command[command.index("--tax") + 1]).write_text("= a a\n", encoding="utf-8")
                return CLASSIFIED

            args = SimpleNamespace(cases=str(cases), binary="dl_classify", only=None, threads=1,
                                   timeout=300, again=False, extra=[])
            with patch.object(nrese.subprocess, "run", side_effect=driver):
                nrese.dl(args)
            return [l.split("\t") for l in (cases / "results.tsv").read_text(encoding="utf-8").splitlines()]

    def test_the_el_tracks_classification_tasks_are_answered_by_the_product_classifier(self):
        product = {r[0] for r in self.rows() if r[1] == "nrese-dl"}
        self.assertEqual(product, {"ore_ont_1-el-classification", "ore_ont_2-dl-classification",
                                   "ore_ont_3-pure_dl-classification"})

    def test_the_el_classifiers_row_stays_beside_it_as_the_oracle(self):
        rows = self.rows()
        on_el = {r[1] for r in rows if r[0] == "ore_ont_1-el-classification"}
        self.assertEqual(on_el, {"hermit", "nrese-el", "nrese-dl"})


if __name__ == "__main__":
    unittest.main()
