"""The standards table of docs/what-it-covers.md says the counts the release gate requires
(benches/fast/w3c-expected.toml): each suite's expected passes, and its known skips and
disputes, stand in a row that links its kit's runner. A count changed on one side only fails here.

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


def numbers(row: str) -> set[int]:
    """The numbers a reader sees in `row`: its text without link targets (`1,234` is 1234)."""
    text = re.sub(r"\]\([^)]*\)", "]", row)
    return {int(n.replace(",", "")) for n in re.findall(r"\d[\d,]*", text)}


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
            linked = [row for row in rows if f"]({link})" in row]
            self.assertTrue(linked, f"{kit.name}: no row of the standards table links {link}")
            shown = set().union(*(numbers(row) for row in linked))
            for suite in kit.suites:
                for key in ("passed", "skipped", "disputed"):
                    if want := expected[suite].get(key):
                        checked += 1
                        self.assertIn(want, shown, f"{suite}: the gate requires {key} = {want}; "
                                      f"the rows linking {link} say {sorted(shown)}")
        self.assertGreater(checked, 0)

    def test_a_row_reads_its_numbers_without_its_links(self):
        row = "| [SPARQL 1.2](https://www.w3.org/TR/sparql12-query/) | x | 1,271 tests ([runner](../a/b2)) |"
        self.assertEqual(numbers(row), {1, 2, 1271})


if __name__ == "__main__":
    unittest.main()
