"""The case registry (benches/cases/, suitekit/cases.py): the repository's cases validate, and
each kind of mistake a group's author can make is caught.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

from suitekit import cases  # noqa: E402

SUITE = HERE.parent


def registry(name):
    with open(SUITE / name, "rb") as f:
        return tomllib.load(f)


class Repository(unittest.TestCase):
    def test_every_case_of_every_group_validates(self):
        frozen = {d["file"] for d in registry("../datasets.toml")["dataset"]}
        found = cases.load()
        self.assertGreater(len(found), 90)
        self.assertEqual(cases.problems(found, registry("systems.toml"), registry("workloads.toml"), frozen), [])
        self.assertEqual({c["group"] for c in found} >= {"core"}, True)


class Mistakes(unittest.TestCase):
    SYSTEMS = {"nrese": {}, "jena": {}, "oxigraph": {}, "virtuoso": {}}
    GOOD = {"name": "c", "area": "query", "what": "w", "semantics": "none", "data": "volume:x.nt",
            "metric": "sum_p50_ms", "cap_gb": 4, "systems": ["jena:plain", "oxigraph:plain"],
            "expect": {"answers": ["jena", "oxigraph"]}, "home_turf": "jena", "levels": ["full"]}

    def problems(self, **changes):
        case = {**self.GOOD, **changes}
        return cases.problems([{k: v for k, v in case.items() if v is not None}], self.SYSTEMS,
                              {"bsbm": {}}, {"x.nt"})

    def test_a_good_case_has_no_problem(self):
        self.assertEqual(self.problems(), [])

    def test_each_mistake_is_named(self):
        for changes, words in [
            ({"cap_gb": None}, "missing cap_gb"),
            ({"colour": "red"}, "unknown key 'colour'"),
            ({"area": "misc"}, "area 'misc'"),
            ({"systems": ["stardust:plain"]}, "comparator 'stardust'"),
            ({"systems": ["jena:magic"]}, "semantics 'magic'"),
            ({"expect": {"answers": ["nrese"]}}, "never NRESE"),
            ({"expect": {"answers": ["virtuoso"]}}, "one of the case's comparators"),
            ({"data": "volume:y.nt"}, "frozen dataset"),
            ({"outcome": "memory-limit"}, "names its issue in `known`"),
            ({"workload": "watdiv"}, "workload 'watdiv'"),
            ({"home_turf": "rdfox"}, "home_turf 'rdfox'"),
            ({"levels": ["nightly"]}, "levels"),
        ]:
            with self.subTest(changes=changes):
                found = self.problems(**changes)
                self.assertTrue(any(words in p for p in found), found)

    def test_names_are_unique_across_the_group_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            for group in ("a", "b"):
                (Path(tmp) / f"{group}.toml").write_text('[[case]]\nname = "same"\n', encoding="utf-8")
            with self.assertRaises(ValueError):
                cases.load(Path(tmp))


if __name__ == "__main__":
    unittest.main()
