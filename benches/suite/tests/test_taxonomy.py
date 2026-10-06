"""The taxonomy comparison (benches/reasoning/dl/canonical.py `compare`), which the fast
suite's classification checks use: a reference over fewer classes is reported as such, not
as wrong subsumptions. The shape of el-classify on 6 October 2026: a defined class `D` the
reference's reader dropped (undeclared), so `A ⊑ D` looked like an extra direct subsumption
and `A ⊑ B` like a missing one.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "reasoning" / "dl"))

import canonical  # noqa: E402

E = "http://e/"


def reference() -> str:
    """ELK's taxonomy of A ⊑ B over {A, B}, in the runner's canonical format."""
    return canonical.canonical({E + "A", E + "B", canonical.THING, canonical.NOTHING}, [(E + "A", E + "B")])


class Compare(unittest.TestCase):
    def test_the_same_taxonomy(self):
        self.assertEqual(canonical.compare(reference(), [(E + "A", E + "B")]), "true")

    def test_an_extra_subsumption_between_the_references_classes(self):
        self.assertEqual(canonical.compare(reference(), [(E + "A", E + "B"), (E + "B", E + "A")]), "false")

    def test_a_class_the_reference_lacks(self):
        # A ⊑ D ⊑ B, D undeclared and so unknown to the reference: not comparable, rather
        # than "A ⊑ D extra, A ⊑ B missing".
        pairs = [(E + "A", E + "D"), (E + "D", E + "B"), (E + "A", E + "B")]
        self.assertEqual(canonical.compare(reference(), pairs), "signature-differs")


if __name__ == "__main__":
    unittest.main()
