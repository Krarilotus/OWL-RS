"""Query-swing distributions keep raw runs and independent process summaries distinct."""
import contextlib
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location("query_swing", ROOT / "benches/probes/query-swing.py")
swing = importlib.util.module_from_spec(spec)
spec.loader.exec_module(swing)


def query(p50, samples=None):
    row = {"name": "q", "min_ms": 0.1, "p50_ms": p50, "max_ms": 1000.0}
    if samples is not None:
        row["samples_ms"] = samples
    return row


class Distribution(unittest.TestCase):
    def test_failed_or_missing_fresh_reports_never_reuse_previous_measurements(self):
        for code, writes_fresh in ((1, False), (0, False), (1, True)):
            with self.subTest(code=code, writes_fresh=writes_fresh), tempfile.TemporaryDirectory() as tmp:
                out = Path(tmp)
                stale = out / "candidate-1.json"
                previous = json.dumps({"queries": [query(999)]})
                stale.write_text(previous)

                def measured(command, **kwargs):
                    report = Path(command[command.index("--json") + 1])
                    self.assertNotEqual(report, stale)
                    self.assertFalse(report.exists())
                    if writes_fresh:
                        report.write_text(json.dumps({"queries": [query(1)]}))
                    return SimpleNamespace(stdout="failed attempt", stderr="diagnostic", returncode=code)

                args = ["query-swing.py", "--bin", "candidate=fake-perf-lab", "--load", "immutable.nt",
                        "--queries", "queries", "--query", "q", "--processes", "1", "--out", str(out)]
                with patch.object(swing.subprocess, "run", side_effect=measured), \
                        patch.object(swing, "scoped", side_effect=lambda unit, cap, command: command), \
                        patch.object(swing.shutil, "which", return_value=None), patch.object(sys, "argv", args):
                    with self.assertRaisesRegex(SystemExit, "candidate process 1: exit"):
                        swing.main()
                self.assertEqual(stale.read_text(), previous)
                self.assertFalse((out / "swing.txt").exists())
                logs = list(out.glob("swing-*/candidate-1.log"))
                self.assertEqual(len(logs), 1)
                self.assertIn("diagnostic", logs[0].read_text())

    def test_complete_raw_samples_are_the_only_timed_sample_distribution(self):
        text = swing.distribution([query(2, [1, 2, 3]), query(8, [7, 8, 9])], 3)
        self.assertIn("timed raw samples n=6 (2 processes)", text)
        self.assertIn("min 1.000", text)
        self.assertIn("p50 5.000", text)
        self.assertIn("max 9.000 ms", text)
        self.assertIn("reported min/max span 0.100-1000.000 ms", text)

    def test_any_missing_empty_or_partial_raw_data_uses_all_process_p50s(self):
        for incomplete in (None, [], [11, 12]):
            with self.subTest(samples=incomplete):
                text = swing.distribution([query(2, [1, 2, 3]), query(12, incomplete)], 3)
                self.assertIn("independent process p50s n=2 (2 processes)", text)
                self.assertIn("min 2.000", text)
                self.assertIn("p50 7.000", text)
                self.assertIn("max 12.000 ms", text)
                self.assertNotIn("timed raw samples", text)

    def test_one_process_and_no_completed_processes_are_reported_honestly(self):
        text = swing.distribution([query(4)], 5)
        self.assertIn("independent process p50s n=1 (1 processes)", text)
        self.assertIn("p5 4.000, p50 4.000, p95 4.000", text)
        self.assertEqual(swing.distribution([], 5), "no completed processes")

    def test_summary_only_probe_reports_30_processes_without_faking_90_samples(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp)
            count = 0

            def measured(command, **kwargs):
                nonlocal count
                count += 1
                report = Path(command[command.index("--json") + 1])
                report.write_text(json.dumps({"queries": [query(count)]}))
                return SimpleNamespace(stdout="", stderr="", returncode=0)

            args = ["query-swing.py", "--bin", "candidate=fake-perf-lab", "--load", "immutable.nt",
                    "--queries", "queries", "--query", "q", "--processes", "30", "--runs", "5",
                    "--out", str(out)]
            with patch.object(swing.subprocess, "run", side_effect=measured) as run, \
                    patch.object(swing, "scoped", side_effect=lambda unit, cap, command: command), \
                    patch.object(swing.shutil, "which", return_value=None), patch.object(sys, "argv", args), \
                    contextlib.redirect_stdout(io.StringIO()):
                swing.main()
            text = (out / "swing.txt").read_text()
            self.assertEqual(run.call_count, 30)
            self.assertIn("independent process p50s n=30 (30 processes)", text)
            self.assertIn("p50 15.500", text)
            self.assertIn("reported min/max span 0.100-1000.000 ms", text)
            self.assertNotIn("n=90", text)
            self.assertNotIn("timed raw samples", text)


if __name__ == "__main__":
    unittest.main()
