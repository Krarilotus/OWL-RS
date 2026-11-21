"""Each W3C runner's own lists agree with the counts the release gate states (benches/fast/w3c-expected.toml),
read without running a suite: a list's entries (its lines neither blank nor a `#` comment, as the runners read
them) are the sum its suites state for that list's count. A change to a list without the toml, or the other way
round, fails here at the landing, where the gate itself would only fail at the release. Every list file beside a
declared runner is counted here or named with why it is no count of the gate's.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location("release_gate", ROOT / "benches/fast/release_gate.py")
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)

# (kit, list file beside its runner) -> (the count it is, the suites whose stated counts sum to its entries)
COUNTED = {
    ("nrese-sparql:w3c_sparql11", "expected-failures.txt"): ("failed", ("SPARQL 1.1",)),
    ("nrese-sparql:w3c_sparql11", "expected-failures-12.txt"): ("failed", ("SPARQL 1.2",)),
    ("nrese-sparql:geosparql_compliance", "disputed.txt"): ("disputed", ("GeoSPARQL",)),
    ("nrese-shacl:w3c_shacl", "expected-failures.txt"): ("failed", ("SHACL Core",)),
    ("nrese-shacl:w3c_shacl", "expected-failures-sparql.txt"): ("failed", ("SHACL-SPARQL",)),
    ("nrese-store:w3c_owl2_rl", "expected-failures.txt"): ("known", ("OWL 2 RL",)),
    ("nrese-dl:tableau_w3c", "expected-wrong.txt"): ("failed", ("OWL 2 DL",)),
    ("nrese-dl:tableau_w3c", "disputed.txt"): ("disputed", ("OWL 2 DL",)),
    ("nrese-dl:tableau_w3c", "undecided.txt"): ("open", ("OWL 2 DL",)),
    ("nrese-store:w3c_owl2_ql", "expected-failures.txt"): ("known", ("OWL 2 QL",)),
    ("nrese-rdf-io:w3c_rdf", "expected-failures.txt"): ("known", ("RDF 1.1 syntax", "RDF 1.2 syntax")),
    ("nrese-rdf-io:w3c_n3", "expected-failures.txt"): ("known", ("N3 parser",)),
    ("nrese-sparql-syntax:w3c_syntax", "expected-failures.txt"): ("known", ("SPARQL syntax",)),
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


def entries(text: str) -> int:
    """A list's entries as the runners read them: lines neither blank nor a `#` comment."""
    return sum(1 for line in text.splitlines() if line.strip() and not line.startswith("#"))


def runner_dir(kit) -> Path:
    found = sorted((ROOT / "crates").glob(f"**/{kit.crate}/tests/{kit.test}"))
    assert len(found) == 1, f"{kit.name}: runner {found}"
    return found[0]


def list_errors(expected: dict, kits: list, read=lambda path: path.read_text(encoding="utf-8")) -> list[str]:
    """Where the stated counts and the runners' lists disagree, or a list is neither counted nor named."""
    errors = []
    for kit in kits:
        folder = runner_dir(kit)
        for path in sorted(folder.glob("*.txt")):
            key = (kit.name, path.name)
            if key in NOT_COUNTS:
                continue
            if key not in COUNTED:
                errors.append(f"{kit.name}: {path.name} is neither a count of w3c-expected.toml's nor named as none")
                continue
            count, suites = COUNTED[key]
            stated = sum(expected[suite].get(count, 0) for suite in suites)
            listed = entries(read(path))
            if stated != listed:
                errors.append(f"{kit.name}: {path.name} lists {listed}, w3c-expected.toml states {count} = {stated} "
                              f"({', '.join(suites)}): change them together")
    return errors


class ListCounts(unittest.TestCase):
    def test_every_runner_list_agrees_with_its_stated_count(self):
        expected, kits, bad = gate.load_w3c_expected(
            (ROOT / "benches/fast/w3c-expected.toml").read_text(encoding="utf-8"))
        self.assertEqual(bad, [])
        self.assertEqual(list_errors(expected, kits), [])
        named = {kit for kit, _ in (*COUNTED, *NOT_COUNTS)}
        self.assertEqual(named - {kit.name for kit in kits}, set(), "a list of a kit the gate doesn't declare")

    def test_a_list_changed_without_its_count_fails(self):
        # Spec: the line's state on 21 Nov: undecided.txt has two entries, the toml states no `open`.
        expected, kits, _ = gate.load_w3c_expected(
            (ROOT / "benches/fast/w3c-expected.toml").read_text(encoding="utf-8"))
        stale = {**expected, "OWL 2 DL": {k: v for k, v in expected["OWL 2 DL"].items() if k != "open"}}
        self.assertEqual(list_errors(stale, kits), [
            "nrese-dl:tableau_w3c: undecided.txt lists 2, w3c-expected.toml states open = 0 (OWL 2 DL): "
            "change them together"])
        # The other way: one more QL failure listed, the toml unchanged.
        real = lambda path: path.read_text(encoding="utf-8")  # noqa: E731
        grown = lambda path: real(path) + ("New-Case # why\n" if path.parent.name == "w3c_owl2_ql" else "")  # noqa: E731
        self.assertEqual(list_errors(expected, kits, grown), [
            "nrese-store:w3c_owl2_ql: expected-failures.txt lists 10, w3c-expected.toml states known = 9 (OWL 2 QL): "
            "change them together"])

    def test_entries_are_what_the_runners_read(self):
        self.assertEqual(entries("# head\n\nA (ConsistencyTest)\twhy\n<iri> # reason\n  \n"), 2)


if __name__ == "__main__":
    unittest.main()
