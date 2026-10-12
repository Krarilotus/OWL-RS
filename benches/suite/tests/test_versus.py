"""NRESE against a comparator (suitekit/versus.py) and the standings (suitekit/standings.py):
answers before time, compare's rule for the verdict, licensed systems kept out.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from suitekit import standings, versus  # noqa: E402
from suitekit.schema import Result, Writer  # noqa: E402


def run(directory: Path, times: dict[str, dict[str, float]], wrong: tuple = (), publish=None):
    """A suite run of tier `c~plain`: per system and query a latency (three runs of five)."""
    writer = Writer(directory / "results.csv")
    for system, queries in times.items():
        for n in (1, 2, 3):
            base = dict(date="d", host="h", runtime="docker", system=system, version="-",
                        publish=(publish or {}).get(system, "free"), workload="fast", tier="c~plain",
                        regime="none", run=n, cache="off" if system == "nrese" else "-")
            writer.write(Result(**base, task="load", ms=1000.0 if system == "nrese" else 2000.0))
            for query, ms in queries.items():
                for repeat in range(1, 6):
                    writer.write(Result(**base, task="query", item=query, repeat=repeat, ms=ms * (1 + repeat / 100),
                                        rows=1))
    verdicts = [{"workload": "fast", "tier": "c~plain", "regime": "none", "query": f"{q}.rq", "system": s,
                 "verdict": "wrong" if (q, s) in wrong else "ok", "note": ""}
                for s, queries in times.items() for q in queries]
    (directory / "answers.json").write_text(json.dumps({"verdicts": verdicts}), encoding="utf-8")


class Versus(unittest.TestCase):
    def entries(self, times, **options):
        with tempfile.TemporaryDirectory() as tmp:
            run(Path(tmp), times, **options)
            return {e["system"]: e for e in versus.versus(Path(tmp), {"c": "qlever"})}

    def test_faster_beyond_the_threshold_is_a_win_slower_a_loss_close_a_tie(self):
        found = self.entries({"nrese": {"q1": 10, "q2": 20}, "qlever": {"q1": 40, "q2": 80},
                              "jena": {"q1": 5, "q2": 10}, "oxigraph": {"q1": 10.5, "q2": 20.5}})
        self.assertEqual(found["qlever"]["verdict"], "win")
        self.assertAlmostEqual(found["qlever"]["time"]["ratio"], 0.25, places=2)
        self.assertEqual(found["jena"]["verdict"], "loss")
        self.assertEqual(found["oxigraph"]["verdict"], "tie")
        self.assertEqual(found["qlever"]["home_turf"], "qlever")
        self.assertEqual(found["qlever"]["load"], 0.5)

    def test_a_wrong_answer_is_never_timed(self):
        found = self.entries({"nrese": {"q1": 10, "q2": 20}, "qlever": {"q1": 40, "q2": 1}},
                             wrong=(("q2", "qlever"),))
        self.assertIn("q2.rq wrong (qlever)".replace(".rq", ""), found["qlever"]["answers"].replace(".rq", ""))
        self.assertEqual(found["qlever"]["timed"], "1/2")
        self.assertEqual(found["qlever"]["verdict"], "win")


class Standings(unittest.TestCase):
    def test_the_newest_comparison_shows_and_a_licensed_system_only_that_it_ran(self):
        entry = {"case": "c", "semantics": "plain", "system": "qlever", "home_turf": "qlever", "answers": "ok",
                 "timed": "2/2", "verdict": "win", "time": {"ratio": 0.25, "ci": [0.2, 0.3]}}
        records = [
            {"id": "old", "started": "2026-10-01", "host": "office-pc", "commit": "a", "versus": [{**entry, "verdict": "loss"}]},
            {"id": "new", "started": "2026-10-10", "host": "office-pc", "commit": "b", "versus": [entry],
             "pairs": [{"system": "graphdb", "outcome": "restricted"}]},
        ]
        systems = {"nrese": {}, "qlever": {"publish": "free"}, "graphdb": {"publish": "permission"}, "jena": {}}
        text = standings.render(records, systems, Path("."))
        self.assertIn("| qlever | 1 / 0 / 0 / 0 | 1 / 0 / 0 / 0 | 0 |", text)
        self.assertIn("c ★", text)
        self.assertNotIn("loss", text.split("## qlever")[1])
        self.assertIn("| graphdb | restricted | restricted | - | ran in 1 runs; results stay local |", text)
        self.assertIn("| jena | - | - | - | not measured yet |", text)


class Refused(unittest.TestCase):
    def test_a_refused_system_shows_its_published_numbers_apart_never_as_measured(self):
        systems = {"nrese": {}, "rdfox": {"publish": "permission", "refused": "no evaluation licence"}}
        entry = {"system": "rdfox", "benchmark": "claros", "tier": "l", "metric": "materialisation", "value": 1.0,
                 "unit": "s", "hardware": "the paper's machine", "source": "a paper", "date": "2021"}
        from unittest.mock import patch
        from suitekit import reported
        with patch.object(reported, "load", lambda: [entry]):
            text = standings.render([], systems, Path("."))
        self.assertIn("| rdfox | refused | refused | - | no evaluation licence; 1 published numbers below |", text)
        self.assertIn("## rdfox: reported, not measured by us", text)
        self.assertIn("the paper's machine", text)
        self.assertEqual(reported.problems(systems, [{**entry, "hardware": ""}])[0][:22], "reported.toml entry 1:")


if __name__ == "__main__":
    unittest.main()
