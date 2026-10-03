"""The evaluation contracts (suitekit/summary.py) on the synthetic cases of the independent
review of 3 October 2026 (B1-B5): lost work stays visible, an intermittent timeout makes an
item incomplete, wrong answers never enter a speed verdict, report and compare estimate the
same way, and the newest record of a day is the latest.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import contextlib
import csv
import io
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from suitekit import compare, report, status  # noqa: E402
from suitekit.schema import Result  # noqa: E402
from suitekit.summary import summarise  # noqa: E402

FIELDS = list(Result.__dataclass_fields__)


def row(**fields) -> dict:
    base = dict(date="2026-10-03T09:00", host="h", runtime="docker", system="nrese", version="-", publish="free",
                workload="w", tier="t", regime="none", run=1, task="query", item="", repeat="", status="ok", ms="",
                peak_mib="", rows="", bytes="", note="", cache="-", order="-")
    base.update({k: str(v) for k, v in fields.items()})
    return base


def load(run=1, status="ok", system="nrese") -> dict:
    return row(task="load", run=run, status=status, ms=10, rows=5, system=system)


def query(item, ms, run=1, repeat=1, status="ok", rows=1, system="nrese") -> dict:
    return row(task="query", item=item, run=run, repeat=repeat, status=status, ms=ms, rows=rows, system=system)


def write(directory: Path, rows: list[dict]) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / "results.csv"
    with open(path, "w", newline="", encoding="utf-8") as f:
        writer = csv.DictWriter(f, fieldnames=FIELDS)
        writer.writeheader()
        writer.writerows(rows)
    return path


def output(main, argv) -> str:
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        main(argv)
    return out.getvalue()


class Contracts(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())

    def test_b1_a_failed_load_and_lost_queries_stay_visible(self):
        base = write(self.dir / "base", [load(), query("q1", 10), query("q2", 20)])
        failed = write(self.dir / "failed", [load(status="failed")])
        text = output(compare.main, [str(base), str(failed)])
        self.assertIn("1 of 1 loads not ok", text)
        self.assertIn("lost queries", text)
        fewer = write(self.dir / "fewer", [load(), query("q1", 10)])
        text = output(compare.main, [str(base), str(fewer)])
        self.assertIn("lost queries:** q2", text)
        gone = write(self.dir / "gone", [load(system="other")])
        text = output(compare.main, [str(base), str(gone)])
        self.assertIn("Lost: pairs the baseline ran", text)

    def test_b2_an_intermittent_timeout_makes_the_item_incomplete(self):
        rows = [load(1), load(2), query("q1", 100, run=1), query("q1", 300000, run=2, status="timeout", rows="")]
        pair = summarise(rows)[("w", "t", "nrese")]
        outcome = pair.outcome()
        self.assertEqual(outcome["outcome"], "partial")
        self.assertEqual(outcome["items"], "0/1")
        self.assertIn("1 timeout of 2", outcome["note"])
        text = output(report.main, [str(write(self.dir / "r", rows))])
        self.assertIn("| 0/1 |", text)
        self.assertIn("| q1 |", text)

    def test_b3_wrong_answers_never_enter_a_speed_verdict(self):
        base = write(self.dir / "base", [load(), query("q1", 100)])
        new = write(self.dir / "new", [load(), query("q1", 1, status="wrong", rows=2)])
        text = output(compare.main, [str(base), str(new)])
        self.assertIn("wrong answer", text)
        self.assertIn("0 faster", text)
        self.assertIn("1 not comparable", text)
        text = output(report.main, [str(new)])
        self.assertNotIn("| 1.0 |", text)

    def test_b4_report_and_compare_estimate_alike(self):
        rows = [load(1), load(2), load(3)]
        for run, values in ((1, [1, 1, 1000]), (2, [2, 2, 1000]), (3, [100, 1000, 1000])):
            rows += [query("q1", v, run=run, repeat=i + 1) for i, v in enumerate(values)]
        pair = summarise(rows)[("w", "t", "nrese")]
        self.assertEqual(pair.items["-"]["q1"].estimate()[0], 2)
        text = output(report.main, [str(write(self.dir / "r", rows))])
        self.assertIn("| 2.0 |", text)

    def test_b5_the_newest_of_a_day_is_the_latest(self):
        early = {"id": "early", "started": "2026-10-03T09:00",
                 "pairs": [{"workload": "w", "tier": "t", "system": "nrese", "outcome": "ok"}]}
        later = {"id": "later", "started": "2026-10-03T18:00",
                 "pairs": [{"workload": "w", "tier": "t", "system": "nrese", "outcome": "failed"}]}
        for records in ([early, later], [later, early]):
            self.assertEqual(status.newest(records)[("w", "t", "nrese")][1]["outcome"], "failed")

    def test_answers_that_differ_between_systems_are_disputed(self):
        rows = [load(system="a"), load(system="b"), query("q1", 5, rows=1, system="a"),
                query("q1", 5, rows=2, system="b")]
        pairs = summarise(rows)
        for system in ("a", "b"):
            self.assertEqual(pairs[("w", "t", system)].outcome()["outcome"], "disputed")


    def test_an_adjudication_settles_a_dispute(self):
        rows = [load(system="a"), load(system="b"), query("q1", 5, rows=1, system="a"),
                query("q1", 5, rows=2, system="b")]
        pairs = summarise(rows, settled={("w", "t", "q1"): "1"})
        self.assertEqual(pairs[("w", "t", "a")].outcome()["outcome"], "ok")
        self.assertEqual(pairs[("w", "t", "b")].outcome()["outcome"], "wrong")
        self.assertIsNone(pairs[("w", "t", "b")].items["-"]["q1"].estimate())

    def test_a_workload_without_a_load_counts_its_runs_from_its_steps(self):
        rows = [row(task="update", run=r, ms=100) for r in (1, 2, 3)]
        outcome = summarise(rows)[("w", "t", "nrese")].outcome()
        self.assertEqual((outcome["outcome"], outcome["runs"]), ("ok", 3))


if __name__ == "__main__":
    unittest.main()
