"""The problem-finders' chunk tools (13 Oct 2026, Khaleesi's staged load test): a command under
a memory cap and a time budget (scripts/capped-chunk.py), a seed sweep's order and resumption
(scripts/seed-sweep.py).

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import importlib.util
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


sweep = load("seed_sweep", ROOT / "scripts/seed-sweep.py")


class CappedChunk(unittest.TestCase):
    def run_chunk(self, budget_min, code):
        return subprocess.run([sys.executable, str(ROOT / "scripts/capped-chunk.py"), "--memory-mib", "512",
                               "--budget-min", str(budget_min), "--", sys.executable, "-c", code],
                              capture_output=True, text=True, timeout=60)

    def test_the_commands_exit_status_and_the_budget_ending_a_chunk(self):
        done = self.run_chunk(1, "raise SystemExit(3)")
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        started = time.monotonic()
        stopped = self.run_chunk(0.05, "import time; time.sleep(30)")
        self.assertEqual(stopped.returncode, 0, stopped.stdout + stopped.stderr)
        self.assertIn("the next chunk resumes", stopped.stdout)
        self.assertLess(time.monotonic() - started, 25)


class SeedSweep(unittest.TestCase):
    def test_first_seeds_then_the_range_each_once(self):
        self.assertEqual(sweep.seeds("1-5", "52580,3"), [52580, 3, 1, 2, 4, 5])

    def test_every_target_names_a_crate_and_its_seeded_tests(self):
        targets = sweep.targets(ROOT / "benches/seed-sweeps.toml")
        self.assertGreaterEqual(len(targets), 13)
        for t in targets:
            with self.subTest(target=sweep.name(t)):
                self.assertTrue(t.get("lib") or t.get("test"))
                self.assertTrue(t["filters"])
                self.assertIn("--release", sweep.cargo(t, []))

    def test_each_target_runs_under_its_own_cap(self):
        # 14 Oct 2026: tableau_fuzz's seed 600026 needs 6 GB, every answer right.
        targets = {sweep.name(t): t for t in sweep.targets(ROOT / "benches/seed-sweeps.toml")}
        fuzz = sweep.capped(targets["nrese-dl:tableau_fuzz"], ["--", "x"], 4000, 20)
        self.assertEqual(fuzz[fuzz.index("--memory-mib") + 1], "8192")
        self.assertEqual(fuzz[fuzz.index("--") + 1:][:2], ["cargo", "test"])
        plain = sweep.capped(targets["nrese-dl:islands"], [], 4000, 20)
        self.assertEqual(plain[plain.index("--memory-mib") + 1], "4000")

    def test_a_run_at_its_cap_or_without_a_build_is_not_a_finding(self):
        self.assertEqual(sweep.status_of(0, ""), "ok")
        self.assertEqual(sweep.status_of(sweep.CAP_REACHED, "memory allocation of 3489660944 bytes failed"),
                         "CAP-REACHED")
        self.assertEqual(sweep.status_of(101, "error: could not compile `nrese-server`"), "BUILD-FAILED")
        self.assertEqual(sweep.status_of(101, "only 47 of 240 decided incrementally"), "FAILED(101)")


if __name__ == "__main__":
    unittest.main()
