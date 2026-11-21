"""The standards table of docs/what-it-covers.md says the counts the release gate requires
(benches/fast/w3c-expected.toml): each suite's expected passes or checks, and its known skips,
disputes and listed failures, stand in the one row that names the suite (in its first or its tests
column) and links its kit's runner.
A count changed on one side only, or moved to another suite's row, fails here.

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

PAGE = ROOT / "docs/what-it-covers.md"


def standards_rows(page: str) -> list[str]:
    """The table rows of the page's `## Standards` section."""
    section = page.split("\n## Standards\n", 1)[1].split("\n## ", 1)[0]
    return [line for line in section.splitlines() if line.startswith("|")]


def text(markdown: str) -> str:
    """What a reader sees of `markdown`: links as their text."""
    return re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", markdown)


def numbers(row: str) -> set[int]:
    """The numbers a reader sees in `row` (`1,234` is 1234)."""
    return {int(n.replace(",", "")) for n in re.findall(r"\d[\d,]*", text(row))}


def names(row: str, suite: str) -> bool:
    """Whether `row` names `suite` in its first column (the standard) or its last (the tests)."""
    cells = row.split("|")[1:-1]
    return suite in text(cells[0]) or suite in text(cells[-1])


def stands_before(row: str, count: int, words: set[str], stops: set[int]) -> bool:
    """Whether `count` stands in `row` before one of `words`, in the phrase that follows it up to
    the next of `stops` (`98 Core and 22 …`, `403 ontologies in the OWL 2 RDF/XML round trip, 295 …`)."""
    tokens = text(row).split()
    number = lambda token: int(token.replace(",", "")) if re.fullmatch(r"\d[\d,]*", token) else None  # noqa: E731
    for i, token in enumerate(tokens):
        if number(token) != count:
            continue
        phrase = []
        for word in tokens[i + 1:]:
            if number(word) in stops:
                break
            phrase.append(word.strip(",.;:()"))
        if words & set(phrase):
            return True
    return False


# The gate's counts a row states: its passes or checks, and its known skips, disputes and listed failures.
KEYS = ("passed", "checked", "skipped", "disputed", "known")


def row_errors(row: str, suite: str, suites: tuple[str, ...], expected: dict) -> list[str]:
    """What `row` gets wrong of `suite`'s counts (`expected`, the gate's, by suite). A row that names
    others of its kit's `suites` too says each count before a word only its own suite's name has."""
    others = [s for s in suites if s != suite and names(row, s)]
    words = set(suite.split()) - {w for s in others for w in s.split()}
    stops = {expected.get(s, {}).get(key) for s in others for key in KEYS} - {None, 0}
    want = expected.get(suite, {})
    stated = [key for key in KEYS if want.get(key)]
    errors = [] if stated else [f"{suite}: the gate states no count for its row ({', '.join(KEYS)})"]
    for key in stated:
        count = want[key]
        if others and not stands_before(row, count, words, stops):
            errors.append(f"{suite}: the gate requires {key} = {count}, before one of {sorted(words)}")
        elif not others and count not in numbers(row):
            errors.append(f"{suite}: the gate requires {key} = {count}; its row says {sorted(numbers(row))}")
    return errors


def runner_link(kit) -> str:
    """The page's link to a kit's runner: `../crates/…/<crate>/tests/<test>`."""
    found = sorted((ROOT / "crates").glob(f"**/{kit.crate}/tests/{kit.test}"))
    assert len(found) == 1, f"{kit.name}: runner {found}"
    return "../" + found[0].relative_to(ROOT).as_posix()


class StandardsTable(unittest.TestCase):
    def test_each_suite_count_the_gate_requires_stands_in_its_runners_row(self):
        expected, kits, errors = gate.load_w3c_expected(
            (ROOT / "benches/fast/w3c-expected.toml").read_text(encoding="utf-8"))
        self.assertEqual(errors, [])
        rows = standards_rows(PAGE.read_text(encoding="utf-8"))
        checked = 0
        for kit in kits:
            if not kit.suites:
                continue
            link = runner_link(kit)
            for suite in kit.suites:
                own = [row for row in rows if f"]({link})" in row and names(row, suite)]
                self.assertEqual(len(own), 1, f"{suite}: rows naming it and linking {link}: {own}")
                self.assertEqual(row_errors(own[0], suite, kit.suites, expected), [])
                checked += 1
        self.assertGreater(checked, 0)

    def test_a_row_reads_as_its_reader_sees_it(self):
        row = "| [SPARQL 1.2](https://www.w3.org/TR/sparql12-query/) drafts | x | 1,271 tests ([runner](../a/b2)) |"
        self.assertEqual(numbers(row), {1, 2, 1271})
        self.assertTrue(names(row, "SPARQL 1.2"))
        self.assertFalse(names(row, "SPARQL 1.1"))

    def test_a_shared_row_binds_each_count_to_its_suite(self):
        expected = {"SHACL Core": {"passed": 98}, "SHACL-SPARQL": {"passed": 22}}
        row = "| SHACL Core and SHACL-SPARQL | x | 98 Core and 22 SHACL-SPARQL W3C tests |"
        swapped = "| SHACL Core and SHACL-SPARQL | x | 22 Core and 98 SHACL-SPARQL W3C tests |"
        for suite in expected:
            self.assertEqual(row_errors(row, suite, tuple(expected), expected), [])
            self.assertEqual(len(row_errors(swapped, suite, tuple(expected), expected)), 1, suite)
        # A suite's name after its count may hold a number (`OWL 2`, `RDF 1.1`): its phrase runs to
        # the next count the gate states for the row.
        expected = {"RDF 1.1 syntax": {"passed": 993}, "RDF 1.2 syntax": {"passed": 351}}
        row = "| Turtle | x | 993 RDF 1.1 syntax and 351 RDF 1.2 syntax tests |"
        swapped = "| Turtle | x | 351 RDF 1.1 syntax and 993 RDF 1.2 syntax tests |"
        for suite in expected:
            self.assertEqual(row_errors(row, suite, tuple(expected), expected), [])
            self.assertEqual(len(row_errors(swapped, suite, tuple(expected), expected)), 1, suite)

    def test_a_suite_with_no_stated_count_is_no_check(self):
        row = "| SPARQL 1.1 | x | 531 tests |"
        self.assertEqual(row_errors(row, "SPARQL 1.1", ("SPARQL 1.1",), {"SPARQL 1.1": {"passed": 531}}), [])
        self.assertEqual(len(row_errors(row, "SPARQL 1.1", ("SPARQL 1.1",), {"SPARQL 1.1": {}})), 1)


if __name__ == "__main__":
    unittest.main()
