"""Reported numbers (benches/reasoning/reported): a contender's published results beside our
measurements. Every row must say it was reported, where from and on what hardware, and fit
the suite's schema, so that no reader mistakes it for a measurement of ours.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import sys
import tomllib
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))

from suitekit.schema import FIELDS, Result, problems, read  # noqa: E402

REPORTED = HERE.parent / "reasoning" / "reported"
SOURCES = ("motik2014parallel", "nenov2015rdfox", "tsamoura2021triggergraphs", "motik2019maintenance")


class ReportedRows(unittest.TestCase):
    def test_every_row_is_marked_reported_and_cites_its_source(self):
        workloads = tomllib.loads((HERE / "workloads.toml").read_text(encoding="utf-8"))
        files = sorted(REPORTED.glob("*.csv"))
        self.assertTrue(files)
        for path in files:
            rows = read(path)
            self.assertTrue(rows, path)
            for row in rows:
                with self.subTest(path=path.name, item=row["item"]):
                    self.assertEqual(list(row), FIELDS)
                    self.assertEqual(row["runtime"], "reported")
                    self.assertTrue(row["host"], "the paper's hardware")
                    self.assertTrue(row["note"].startswith(SOURCES), row["note"][:60])
                    self.assertIn(row["workload"], workloads)
                    result = Result(**{k: v for k, v in row.items() if k in FIELDS})
                    self.assertEqual(problems(result), [])


if __name__ == "__main__":
    unittest.main()
