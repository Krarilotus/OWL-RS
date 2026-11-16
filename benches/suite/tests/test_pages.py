"""The generated pages, STATUS.md and STANDINGS.md: each says the day it was made, and the
committed page is its generator's output as of that day, so an unchanged tree checks the same on
any later day.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import datetime
import os
import re
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches/suite"))

import suite  # noqa: E402
from suitekit import standings, status  # noqa: E402
from suitekit.ledger import load_records  # noqa: E402

GENERATED = re.compile(r"^Generated (\d{4}-\d{2}-\d{2}) ", re.MULTILINE)


def epoch(day: str) -> str:
    """SOURCE_DATE_EPOCH for noon UTC of `day` (YYYY-MM-DD)."""
    noon = datetime.datetime.fromisoformat(day).replace(hour=12, tzinfo=datetime.timezone.utc)
    return str(int(noon.timestamp()))


def made_on(day: str, make) -> str:
    """make()'s page, with SOURCE_DATE_EPOCH set to `day` while it runs."""
    before = os.environ.get("SOURCE_DATE_EPOCH")
    os.environ["SOURCE_DATE_EPOCH"] = epoch(day)
    try:
        return make()
    finally:
        if before is None:
            os.environ.pop("SOURCE_DATE_EPOCH", None)
        else:
            os.environ["SOURCE_DATE_EPOCH"] = before


SYSTEMS, WORKLOADS = suite.load()
PAGES = {
    "STATUS.md": lambda: status.render(ROOT, SYSTEMS, WORKLOADS),
    "STANDINGS.md": lambda: standings.render(load_records(ROOT / "benches/runs"), SYSTEMS, ROOT),
}


class SourceDate(unittest.TestCase):
    """Spec: SOURCE_DATE_EPOCH (reproducible-builds.org, "SOURCE_DATE_EPOCH" specification): a
    tool that embeds a date uses the variable's time, in UTC, in place of the current one. So a page
    made with it set to 2 Nov 2026 says "Generated 2026-11-02", whatever day it is made on."""

    def test_each_page_says_the_source_date_not_today(self):
        for page, make in PAGES.items():
            with self.subTest(page=page):
                found = GENERATED.search(made_on("2026-11-02", make))
                self.assertIsNotNone(found, f"{page}: no 'Generated YYYY-MM-DD' line")
                self.assertEqual(found.group(1), "2026-11-02")


class CommittedPages(unittest.TestCase):
    """Spec: each page's own header ("Don't edit it by hand: change those and regenerate"): the
    committed page is exactly what its generator makes from the committed registries and records, as
    of the day the page says it was made; a record, registry or generator changed without
    regenerating it leaves the page wrong."""

    def test_each_committed_page_is_its_generators_output(self):
        for page, make in PAGES.items():
            with self.subTest(page=page):
                committed = (ROOT / "benches" / page).read_text(encoding="utf-8")
                day = GENERATED.search(committed).group(1)
                command = "status" if page == "STATUS.md" else "standings"
                self.assertEqual(committed, made_on(day, make),
                                 f"run: python benches/suite/suite.py {command} --write")


if __name__ == "__main__":
    unittest.main()
