"""Each W3C runner's own lists agree with the counts the release gate states (benches/fast/w3c-expected.toml),
read without running a suite: a list's entries, read by the rule its runner reads them with, are the sum its
suites state for that list's count. A change to a list without the toml, or the other way round, fails here at the
landing, where the gate itself would only fail at the release. Every list file beside a declared runner is counted
here or named with why it is no count of the gate's, and a counted list that is missing fails.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import importlib.util
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location("release_gate", ROOT / "benches/fast/release_gate.py")
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)

# How a runner reads a list's entries (each rule is the runner's own, cited):
RULES = {
    # the text before the first `#`, trimmed, not empty (w3c_owl2_ql, w3c_owl2_rl, w3c_syntax, geosparql_compliance)
    "name": lambda line: bool(line.split("#", 1)[0].strip()),
    # a line neither blank nor beginning with `#` (tableau_w3c `listed`, w3c_n3)
    "line": lambda line: bool(line.strip()) and not line.startswith("#"),
    # a line beginning with an IRI in angle brackets (w3c_sparql11, w3c_shacl, w3c_rdf)
    "iri": lambda line: bool(re.match(r"<[^>]*>", line)),
}
# (kit, list file beside its runner) -> (its rule, the count it is, the suites whose stated counts sum to its entries)
COUNTED = {
    ("nrese-sparql:w3c_sparql11", "expected-failures.txt"): ("iri", "failed", ("SPARQL 1.1",)),
    ("nrese-sparql:w3c_sparql11", "expected-failures-12.txt"): ("iri", "failed", ("SPARQL 1.2",)),
    ("nrese-sparql:geosparql_compliance", "disputed.txt"): ("name", "disputed", ("GeoSPARQL",)),
    ("nrese-shacl:w3c_shacl", "expected-failures.txt"): ("iri", "failed", ("SHACL Core",)),
    ("nrese-shacl:w3c_shacl", "expected-failures-sparql.txt"): ("iri", "failed", ("SHACL-SPARQL",)),
    ("nrese-store:w3c_owl2_rl", "expected-failures.txt"): ("name", "known", ("OWL 2 RL",)),
    ("nrese-dl:tableau_w3c", "expected-wrong.txt"): ("line", "failed", ("OWL 2 DL",)),
    ("nrese-dl:tableau_w3c", "disputed.txt"): ("line", "disputed", ("OWL 2 DL",)),
    ("nrese-dl:tableau_w3c", "undecided.txt"): ("line", "open", ("OWL 2 DL",)),
    ("nrese-store:w3c_owl2_ql", "expected-failures.txt"): ("name", "known", ("OWL 2 QL",)),
    ("nrese-rdf-io:w3c_rdf", "expected-failures.txt"): ("iri", "known", ("RDF 1.1 syntax", "RDF 1.2 syntax")),
    ("nrese-rdf-io:w3c_n3", "expected-failures.txt"): ("line", "known", ("N3 parser",)),
    ("nrese-sparql-syntax:w3c_syntax", "expected-failures.txt"): ("name", "known", ("SPARQL syntax",)),
}
# Lists beside a runner that are no count of the gate's, and why.
NOT_COUNTS = {
    ("nrese-dl:tableau_w3c", "over-budget.txt"): "decided on a small budget, so among the passes",
    ("nrese-dl:tableau_w3c", "uncertified.txt"): "the certificate check's, a test function of its own",
    ("nrese-dl:tableau_w3c", "full-wrong.txt"): "species-Full tests, a test function of its own, not the DL table",
    ("nrese-owl:w3c_owl2", "expected-failures.txt"): "among the round trip's checked count, which counts them",
    ("nrese-owl:w3c_owl2", "functional-differences.txt"): "among the functional syntax's checked count",
    ("nrese-owl:w3c_owl2", "profile-disagreements.txt"): "among the profiles' checked count",
}


def entries(text: str, rule: str) -> int:
    """A list's entries, read by its runner's rule."""
    return sum(1 for line in text.splitlines() if RULES[rule](line))


def runner_dir(kit) -> Path:
    found = sorted((ROOT / "crates").glob(f"**/{kit.crate}/tests/{kit.test}"))
    assert len(found) == 1, f"{kit.name}: runner {found}"
    return found[0]


def read_file(path: Path) -> str | None:
    return path.read_text(encoding="utf-8") if path.is_file() else None


def list_errors(expected: dict, kits: list, read=read_file, files=lambda folder: folder.glob("*.txt")) -> list[str]:
    """Where the stated counts and the runners' lists disagree, a counted list is missing, or a list is neither
    counted nor named."""
    errors = []
    for kit in kits:
        folder = runner_dir(kit)
        names = {path.name for path in files(folder)} | {name for k, name in COUNTED if k == kit.name}
        for name in sorted(names):
            key = (kit.name, name)
            if key in NOT_COUNTS:
                continue
            if key not in COUNTED:
                errors.append(f"{kit.name}: {name} is neither a count of w3c-expected.toml's nor named as none")
                continue
            rule, count, suites = COUNTED[key]
            text = read(folder / name)
            if text is None:
                errors.append(f"{kit.name}: {name}, which w3c-expected.toml's {count} counts, is missing")
                continue
            stated = sum(expected[suite].get(count, 0) for suite in suites)
            listed = entries(text, rule)
            if stated != listed:
                errors.append(f"{kit.name}: {name} lists {listed}, w3c-expected.toml states {count} = {stated} "
                              f"({', '.join(suites)}): change them together")
    return errors


def declared():
    expected, kits, bad = gate.load_w3c_expected((ROOT / "benches/fast/w3c-expected.toml").read_text(encoding="utf-8"))
    assert not bad, bad
    return expected, kits


class ListCounts(unittest.TestCase):
    def test_every_runner_list_agrees_with_its_stated_count(self):
        expected, kits = declared()
        self.assertEqual(list_errors(expected, kits), [])
        named = {kit for kit, _ in (*COUNTED, *NOT_COUNTS)}
        self.assertEqual(named - {kit.name for kit in kits}, set(), "a list of a kit the gate doesn't declare")

    def test_a_list_and_its_count_must_change_together(self):
        # Spec, on fixed lists (not today's): the line's state on 21 Nov, two undecided tests and no `open`
        # stated; a QL list grown by one with the toml unchanged; a counted list deleted; a new unnamed list.
        expected, kits = declared()
        expected = {**expected, "OWL 2 DL": {"passed": 408, "disputed": 1, "failed": 0},
                    "OWL 2 QL": {"passed": 51, "known": 2}}
        fixed = {("tableau_w3c", "undecided.txt"): "# head\nA (ConsistencyTest)\twhy\nB (ConsistencyTest)\twhy\n",
                 ("tableau_w3c", "disputed.txt"): "D (InconsistencyTest)\tmodel\n",
                 ("tableau_w3c", "expected-wrong.txt"): "# none\n",
                 ("w3c_owl2_ql", "expected-failures.txt"): "One # why\nTwo # why\nThree # why\n"}
        # Every counted list of the two runners is a fixture here; only the others are read from the tree.
        self.assertEqual({(k.split(":")[1], name) for k, name in COUNTED if k.split(":")[1] in ("tableau_w3c",
                                                                                               "w3c_owl2_ql")}, set(fixed))

        def read(path):
            return fixed.get((path.parent.name, path.name), read_file(path))

        dl_ql = [e for e in list_errors(expected, kits, read) if "tableau_w3c" in e or "w3c_owl2_ql" in e]
        self.assertEqual(dl_ql, [
            "nrese-dl:tableau_w3c: undecided.txt lists 2, w3c-expected.toml states open = 0 (OWL 2 DL): "
            "change them together",
            "nrese-store:w3c_owl2_ql: expected-failures.txt lists 3, w3c-expected.toml states known = 2 (OWL 2 QL): "
            "change them together"])
        real = declared()[0]
        gone = list_errors(real, kits, lambda path: None if path.parent.name == "w3c_n3" else read_file(path))
        self.assertIn("nrese-rdf-io:w3c_n3: expected-failures.txt, which w3c-expected.toml's known counts, is missing",
                      gone)
        added = list_errors(real, kits, files=lambda folder: [*folder.glob("*.txt"), folder / "new-list.txt"])
        self.assertTrue(any("new-list.txt is neither a count" in e for e in added), added)

    def test_entries_are_read_as_each_runner_reads_them(self):
        text = "# head\n\n  # indented comment\nA (ConsistencyTest)\twhy\n<http://x/y> # reason\nname # why\n"
        self.assertEqual(entries(text, "name"), 3)  # the indented comment is no name
        self.assertEqual(entries(text, "line"), 4)  # tableau_w3c and w3c_n3 count any line not starting with `#`
        self.assertEqual(entries(text, "iri"), 1)
        self.assertEqual(entries("None.\n", "iri"), 0)


if __name__ == "__main__":
    unittest.main()
