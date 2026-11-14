"""The case registry (benches/cases/, suitekit/cases.py): the repository's cases validate, and
each kind of mistake a group's author can make is caught.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

import isolation  # noqa: E402
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

    def test_the_catalogue_table_is_the_generators_output(self):
        # CATALOG.md's case table is generated from the registry (fast.py table --write): a case
        # added or changed without regenerating it leaves the one catalogue wrong. The repository's
        # table: no caller's NRESE_FAMILY (cases.load would add that family's cases).
        made = subprocess.run([sys.executable, "-B", str(SUITE.parent / "fast" / "fast.py"), "table"],
                              capture_output=True, encoding="utf-8", check=True, timeout=120,
                              env=isolation.environment(PYTHONIOENCODING="utf-8")).stdout
        catalog = (SUITE.parent / "CATALOG.md").read_text(encoding="utf-8")
        start, end = "<!-- fast-cases:start (fast.py table --write) -->", "<!-- fast-cases:end -->"
        committed = catalog[catalog.index(start) + len(start):catalog.index(end)]
        self.assertEqual(committed.strip(), made.strip(), "run: python benches/fast/fast.py table --write")


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
        self.assertEqual(self.problems(absent={"main": "supported inference"}), [])
        self.assertEqual(self.problems(tool="classify-el", phases=["read", "normalise", "wall"]), [])
        self.assertEqual(self.problems(tool="classify-horn", phases=["read", "owl", "write"]), [])

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
            # The master's decision of 11 Nov on IB1347, 1a: a baseline (base or main) and the capability it lacks.
            ({"absent": {"cand": "supported inference"}}, "absent: a baseline (base, main) and the capability"),
            ({"absent": {"main": ""}}, "absent: a baseline (base, main) and the capability"),
            # A case declares only phases its classifier tool prints (README.md, what the tool can resolve).
            ({"phases": ["read"]}, "phases: only a classifier tool's case"),
            ({"tool": "classify-el", "phases": ["read", "parse"]}, "phases: a non-empty list of what classify-el prints"),
            ({"tool": "classify-el", "phases": []}, "phases: a non-empty list of what classify-el prints"),
            # A phase left out still counts in the whole process (master, 14 Nov 2026 08:12: nothing
            # unobserved after the drop).
            ({"tool": "classify-el", "phases": ["read", "normalise", "saturate", "assemble", "write"]},
             "phases: leaving out prepare, it declares wall"),
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
