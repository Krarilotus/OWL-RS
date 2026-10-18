"""The problem-finders' chunk tools (13 Oct 2026, Khaleesi's staged load test): a command under
a memory cap and a time budget (scripts/capped-chunk.py), a seed sweep's order and resumption
(scripts/seed-sweep.py).

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import importlib.util
import io
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


sweep = load("seed_sweep", ROOT / "scripts/seed-sweep.py")
chunk = load("capped_chunk", ROOT / "scripts/capped-chunk.py")


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
        # Defect 4 (audit, 18 Oct 2026): a budget kill exited 0, which a sweep recorded as ok.
        self.assertEqual(stopped.returncode, chunk.BUDGET_ENDED, stopped.stdout + stopped.stderr)
        self.assertIn("the next chunk resumes", stopped.stdout)
        self.assertLess(time.monotonic() - started, 25)

    def test_a_resumable_chunk_may_end_at_its_budget_with_zero_when_it_says_so(self):
        done = subprocess.run([sys.executable, str(ROOT / "scripts/capped-chunk.py"), "--memory-mib", "512",
                               "--budget-min", "0.05", "--budget-ok", "--", sys.executable, "-c",
                               "import time; time.sleep(30)"], capture_output=True, text=True, timeout=60)
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)

    def test_the_cap_is_the_kernels_oom_kill_not_a_peak_reading(self):
        # Defect 4: on Linux the peak read as 0, so an OOM kill became FAILED(137).
        self.assertEqual(chunk.outcome(137, oom_kills=1, peak_mib=None, cap_mib=4000), chunk.CAP_REACHED)
        self.assertEqual(chunk.outcome(137, oom_kills=0, peak_mib=None, cap_mib=4000), 137)
        self.assertEqual(chunk.outcome(0, oom_kills=1, peak_mib=4000, cap_mib=4000), 0)
        self.assertEqual(chunk.outcome(1, oom_kills=None, peak_mib=3990, cap_mib=4000), chunk.CAP_REACHED)  # Windows job
        self.assertEqual(chunk.outcome(1, oom_kills=None, peak_mib=1000, cap_mib=4000), 1)

    @unittest.skipUnless(shutil.which("systemd-run") and shutil.which("systemctl"), "needs systemd user scopes")
    def test_a_linux_command_past_its_cap_is_cap_reached(self):
        done = subprocess.run([sys.executable, str(ROOT / "scripts/capped-chunk.py"), "--memory-mib", "64",
                               "--budget-min", "1", "--", sys.executable, "-c",
                               "x = [bytearray(2**20) for _ in range(512)]"],
                              capture_output=True, text=True, timeout=120)
        self.assertEqual(done.returncode, chunk.CAP_REACHED, done.stdout + done.stderr)


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
        self.assertEqual(sweep.status_of(chunk.BUDGET_ENDED, ""), "INTERRUPTED")


class SweepResume(unittest.TestCase):
    """Defect 4 (audit, 18 Oct 2026): a timeout kill was recorded as ok, and a resumed sweep
    skipped every seed already in its table, its failures vanishing from the summary."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.out = Path(self.tmp.name)
        targets = self.out / "targets.toml"
        targets.write_text('[[target]]\ncrate = "c"\ntest = "t"\nfilters = ["f"]\nmemory_mib = 8192\n', encoding="utf-8")
        self.args = SimpleNamespace(out=str(self.out), targets=targets, seeds="1-4", first="", budget_min=5,
                                    memory_mib=4000)

    def sweep_with(self, commands):
        """The sweep, each seed's test a capped chunk (budget 1.2 s) of the given command."""
        dispatch = (f"import os; exec({commands!r}.get(int(os.environ['NRESE_FUZZ_SEED']), 'pass'))")

        def capped(t, extra, default_mib, budget_min):
            return [sys.executable, str(ROOT / "scripts/capped-chunk.py"), "--memory-mib", "512",
                    "--budget-min", "0.02", "--", sys.executable, "-c", dispatch]
        with patch.object(sweep, "capped", capped):
            return sweep.run(self.args)

    def rows(self):
        return [line.split("\t") for line in (self.out / "results.tsv").read_text(encoding="utf-8").splitlines()]

    def test_a_budget_kill_is_interrupted_and_runs_again(self):
        self.sweep_with({2: "import time; time.sleep(30)", 3: "raise SystemExit(101)"})
        statuses = {row[0]: row[2] for row in self.rows()}
        self.assertEqual(statuses, {"1": "ok", "2": "INTERRUPTED", "3": "FAILED(101)", "4": "ok"})
        out = io.StringIO()
        with redirect_stdout(out):
            self.sweep_with({})
        latest = {}
        for row in self.rows():
            latest[row[0]] = row[2]
        self.assertEqual(latest["2"], "ok")
        self.assertEqual(sum(row[0] == "1" for row in self.rows()), 1)  # a final seed isn't run again
        self.assertEqual(sum(row[0] == "3" for row in self.rows()), 1)
        # The earlier chunk's failure is still the sweep's.
        self.assertIn("1 failure", out.getvalue())
        self.assertIn("seed 3", out.getvalue())

    def test_a_cap_reached_runs_again_only_under_a_larger_cap(self):
        (self.out / "results.tsv").write_text("1\tc:t\tCAP-REACHED\t3.0\t4000\n2\tc:t\tCAP-REACHED\t3.0\t8192\n"
                                              "3\tc:t\tBUILD-FAILED\t1.0\t8192\n4\tc:t\tok\t1.0\t8192\n",
                                              encoding="utf-8")
        ran = []
        real = sweep.subprocess.run

        def run(argv, env=None, **kwargs):
            ran.append(env["NRESE_FUZZ_SEED"])
            return real([sys.executable, "-c", "pass"], env=env, **kwargs)
        with patch.object(sweep.subprocess, "run", run):
            sweep.run(self.args)
        self.assertEqual(sorted(ran), ["1", "3"])


if __name__ == "__main__":
    unittest.main()
