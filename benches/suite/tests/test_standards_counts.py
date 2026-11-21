"""The standards table of docs/what-it-covers.md says the counts the release gate requires
(benches/fast/w3c-expected.toml): each suite's expected passes, and its known skips and
disputes, stand in the row that names the suite in its first column and links its kit's runner.
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
    """Whether `row`'s first column names `suite`."""
    return suite in text(row.split("|")[1])


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
                for key in ("passed", "skipped", "disputed"):
                    if want := expected[suite].get(key):
                        checked += 1
                        self.assertIn(want, numbers(own[0]), f"{suite}: the gate requires {key} = "
                                      f"{want}; its row says {sorted(numbers(own[0]))}")
        self.assertGreater(checked, 0)

    def test_a_row_reads_as_its_reader_sees_it(self):
        row = "| [SPARQL 1.2](https://www.w3.org/TR/sparql12-query/) drafts | x | 1,271 tests ([runner](../a/b2)) |"
        self.assertEqual(numbers(row), {1, 2, 1271})
        self.assertTrue(names(row, "SPARQL 1.2"))
        self.assertFalse(names(row, "SPARQL 1.1"))


if __name__ == "__main__":
    unittest.main()
