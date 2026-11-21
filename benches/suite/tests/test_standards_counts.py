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
    """Whether `count` stands in `row`'s tests column before one of `words`, in the phrase that follows
    it up to the next of `stops` or the column's end (`98 Core and 22 …`, `403 in the OWL 2 RDF/XML
    round trip, 295 …`)."""
    tokens = text(row.split("|")[1:-1][-1]).split()
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


# The gate's counts a row states: its passes or checks, and its known skips, disputes, open cases and listed failures.
KEYS = ("passed", "checked", "skipped", "disputed", "open", "known")

# The runners' lists, by the count each one is (test_w3c_list_counts.py's COUNTED, the one home of that mapping).
_lists = importlib.util.spec_from_file_location("w3c_list_counts", Path(__file__).with_name("test_w3c_list_counts.py"))
list_counts = importlib.util.module_from_spec(_lists)
_lists.loader.exec_module(list_counts)


def list_links(kit, suite: str) -> dict[str, str]:
    """The page's links to the lists of `suite`'s counts that only `suite` states, by count: `{key: ../…/file}`."""
    return {count: f"{runner_link(kit)}/{name}" for (k, name), (_, count, suites) in list_counts.COUNTED.items()
            if k == kit.name and suites == (suite,)}


def link_errors(row: str, suite: str, want: dict, links: dict[str, str]) -> list[str]:
    """A count with a list of its own is the text a link to that list starts with (`[2 not decided](…/undecided.txt)`),
    so no other number in the row (`OWL 2`) can stand for it."""
    errors = []
    for key, path in links.items():
        count = want.get(key)
        if key not in KEYS or not count:
            continue
        found = [int(n.replace(",", "")) for n in re.findall(r"\[(\d[\d,]*)\b[^\]]*\]\(" + re.escape(path) + r"\)", row)]
        if found != [count]:
            errors.append(f"{suite}: the gate requires {key} = {count}, as the start of the link to {path}; "
                          f"the row's links to it start with {found}")
    return errors


def row_errors(row: str, suite: str, suites: tuple[str, ...], expected: dict) -> list[str]:
    """What `row` gets wrong of `suite`'s counts (`expected`, the gate's, by suite). A row that names
    others of its kit's `suites` too says each count before a word only its own suite's name has,
    before any other count the gate states for a suite the row names."""
    others = [s for s in suites if s != suite and names(row, s)]
    words = set(suite.split()) - {w for s in others for w in s.split()}
    stops = {expected[s].get(key) for s in expected if s != suite and names(row, s) for key in KEYS} - {None, 0}
    want = expected.get(suite, {})
    tests = row.split("|")[1:-1][-1]  # the counts stand in the tests column, never in the standard's name
    stated = [key for key in KEYS if want.get(key)]
    errors = [] if stated else [f"{suite}: the gate states no count for its row ({', '.join(KEYS)})"]
    for key in stated:
        count = want[key]
        if others and not stands_before(row, count, words, stops):
            errors.append(f"{suite}: the gate requires {key} = {count}, before one of {sorted(words)}")
        elif not others and count not in numbers(tests):
            errors.append(f"{suite}: the gate requires {key} = {count}; its tests column says {sorted(numbers(tests))}")
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
                self.assertEqual(link_errors(own[0], suite, expected[suite], list_links(kit, suite)), [])
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

    def test_a_phrase_ends_at_any_count_the_row_states(self):
        # Spec: S7's witness: the OWL 2 DL row moved `403` before its own `410`; the phrase after 403 must
        # end at 410 (another kit's stated count), not run on to `RDF/XML`.
        expected = {"OWL 2 DL": {"passed": 410}, "OWL 2 RDF/XML round trip": {"checked": 403},
                    "OWL 2 profiles": {"checked": 295}}
        suites = ("OWL 2 RDF/XML round trip", "OWL 2 profiles")
        row = ("| OWL 2 DL | x | 410 test cases; 403 in the OWL 2 RDF/XML round trip, "
               "295 in the OWL 2 profiles check |")
        moved = ("| OWL 2 DL | x | 403 and 410 test cases; in the OWL 2 RDF/XML round trip, "
                 "295 in the OWL 2 profiles check |")
        self.assertEqual(row_errors(row, "OWL 2 RDF/XML round trip", suites, expected), [])
        self.assertEqual(len(row_errors(moved, "OWL 2 RDF/XML round trip", suites, expected)), 1)

    def test_a_count_is_read_from_the_tests_column_only(self):
        # Spec: S7's second witness: `2 skipped` deleted from the OWL 2 QL row must fail, though the
        # standard's name holds a 2.
        expected = {"OWL 2 QL": {"passed": 51, "known": 9, "skipped": 2}}
        row = "| [OWL 2 QL](x) | Rules | 51 W3C test cases, 9 known failures, and 2 skipped |"
        cut = "| [OWL 2 QL](x) | Rules | 51 W3C test cases, 9 known failures |"
        self.assertEqual(row_errors(row, "OWL 2 QL", ("OWL 2 QL",), expected), [])
        self.assertEqual(len(row_errors(cut, "OWL 2 QL", ("OWL 2 QL",), expected)), 1)

    def test_a_count_with_its_own_list_starts_the_link_to_it(self):
        # Spec: S15's witness: `2 not decided` removed from the OWL 2 DL row must fail, though `OWL 2` holds a 2.
        want = {"passed": 408, "disputed": 1, "open": 2}
        links = {"open": "../t/undecided.txt", "disputed": "../t/disputed.txt"}
        row = ("| OWL 2 DL | x | 408 cases, [2 not decided](../t/undecided.txt) and [1 whose answer is wrong]"
               "(../t/disputed.txt); 403 round trips (OWL 2 RDF/XML round trip) |")
        cut = "| OWL 2 DL | x | 408 cases, and [1 whose answer is wrong](../t/disputed.txt); 403 (OWL 2 RDF/XML) |"
        moved = ("| OWL 2 DL | x | 408 cases, 2 [not decided](../t/undecided.txt) and [1 whose answer is wrong]"
                 "(../t/disputed.txt) |")
        self.assertEqual(row_errors(row, "OWL 2 DL", ("OWL 2 DL",), {"OWL 2 DL": want}), [])
        self.assertEqual(row_errors(cut, "OWL 2 DL", ("OWL 2 DL",), {"OWL 2 DL": want}), [])  # why the link counts
        self.assertEqual(link_errors(row, "OWL 2 DL", want, links), [])
        self.assertEqual(len(link_errors(cut, "OWL 2 DL", want, links)), 1)
        self.assertEqual(len(link_errors(moved, "OWL 2 DL", want, links)), 1)
        # A list's count stated as 0 needs no link.
        self.assertEqual(link_errors(cut, "OWL 2 DL", {**want, "open": 0}, links), [])

    def test_a_suite_with_no_stated_count_is_no_check(self):
        row = "| SPARQL 1.1 | x | 531 tests |"
        self.assertEqual(row_errors(row, "SPARQL 1.1", ("SPARQL 1.1",), {"SPARQL 1.1": {"passed": 531}}), [])
        self.assertEqual(len(row_errors(row, "SPARQL 1.1", ("SPARQL 1.1",), {"SPARQL 1.1": {}})), 1)


if __name__ == "__main__":
    unittest.main()
