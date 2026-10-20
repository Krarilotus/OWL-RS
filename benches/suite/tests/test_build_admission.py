"""Build admission (scripts/cargo-guarded.sh): a cargo run waits while the machine is busy, for a
slot and until its declared memory peak fits above the floor, never building anyway; a request
that can never fit is refused at once; a dead holder is reclaimed by its identity; and a run says
it was stopped by the main PC's guard only when the guard stopped a process of its own build.
Sol's round-1 probes (Harness/notes/review-build-admission-r1-20261020/probe.py) are here with
the behaviour they asked for.

Run: python -m unittest discover -s benches/suite/tests -p test_build_admission.py
"""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[3]
BASH = shutil.which("bash")
GB = 1024 * 1024  # kB


class Admission(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="nrese-admission-test-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.repo = self.root / "repo"
        for rel in ("scripts/cargo-guarded.sh", "scripts/lib/disk.sh", "scripts/lib/target-cpu.sh",
                    "scripts/lib/prune-stale.py", "scripts/lib/memory-cap.ps1"):
            (self.repo / rel).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(ROOT / rel, self.repo / rel)
        self.stubs = self.root / "stubs"
        self.stubs.mkdir()
        # cargo: says when it started and with how many jobs, runs a while, and ends with $CODE.
        # NOTICE=late: the machine turns busy and the guard's notice for this build's job comes a
        # second after cargo ended; NOTICE=other: a notice for another build's job.
        self.stub("cargo", 'echo "$(date +%s%N) $CARGO_BUILD_JOBS" > "$OUT/$RUN.started"\n'
                           'sleep "${SLEEP:-0}"\n'
                           'note() { echo "$(date +%s) KILLED-BY-GUARD 4242 rustc.exe 4242@1 jobs=$1" >> "$GUARD/killed.log"; }\n'
                           'case "${NOTICE:-}" in\n'
                           '  late) echo busy > "$GUARD/BUSY"; (sleep 1; note "$NRESE_BUILD_JOB") > /dev/null 2>&1 & ;;\n'
                           '  other) note nrese-build-another ;;\n'
                           'esac\n'
                           'echo "$(date +%s%N)" > "$OUT/$RUN.ended"\nexit "${CODE:-0}"')
        self.stub("nproc", "echo 32")  # 12 compiler jobs by default
        self.guard = self.root / "guard"
        self.guard.mkdir()
        self.out = self.root / "out"
        self.out.mkdir()
        self.free(24)

    def stub(self, name, body):
        (self.stubs / name).write_text("#!/bin/sh\n" + body + "\n")
        (self.stubs / name).chmod(0o755)

    def free(self, gb):
        (self.root / "meminfo").write_text(f"MemTotal: {64 * GB} kB\nMemFree: {gb * GB} kB\n")

    def env(self, run, **env):
        base = {k: v for k, v in os.environ.items() if not k.startswith(("GIT_", "NRESE_", "CARGO_", "FLEET_"))}
        return {**base, "HOME": str(self.root), "PATH": f"{self.stubs}{os.pathsep}{os.environ['PATH']}",
                "OUT": str(self.out), "RUN": run, "GUARD": str(self.guard),
                "NRESE_BUILD_SLOT_DIR": str(self.root / "slots"), "NRESE_GUARD_BUSY": str(self.guard / "BUSY"),
                "NRESE_GUARD_KILLED": str(self.guard / "killed.log"), "NRESE_MEMINFO": str(self.root / "meminfo"),
                "NRESE_BUILD_FLOOR_GB": "16", "NRESE_MEMORY_CAP_GB": "0", "NRESE_MIN_FREE_GB": "0",
                "NRESE_TARGET_BUDGET_GB": "1000", "CARGO_TARGET_DIR": str(self.root / "target"),
                "NRESE_QUIET_DIR": str(self.root / "no-quiet-slot"), "NRESE_BUILD_SLOT_WAIT_S": "0", **env}

    def start(self, run, stderr=subprocess.DEVNULL, **env):
        build = subprocess.Popen([BASH, "scripts/cargo-guarded.sh", "build"], cwd=self.repo,
                                 env=self.env(run, **env), stdout=subprocess.DEVNULL, stderr=stderr, text=True)
        self.addCleanup(lambda: build.poll() is not None or (build.kill(), build.wait()))
        return build

    def started(self, run, within):
        deadline = time.time() + within
        while time.time() < deadline:
            if (self.out / f"{run}.started").exists():
                return (self.out / f"{run}.started").read_text().split()
            time.sleep(0.2)
        return None

    def ended(self, run):
        return int((self.out / f"{run}.ended").read_text())

    def test_a_busy_machine_holds_every_build_however_long_and_frees_them_when_it_clears(self):
        (self.guard / "BUSY").write_text("5.4 GB RAM, 394 GB disk free")
        build = self.start("a")
        self.assertIsNone(self.started("a", within=8), "built while the machine was busy")
        (self.guard / "BUSY").unlink()
        self.assertIsNotNone(self.started("a", within=60))
        self.assertEqual(build.wait(timeout=120), 0)

    def test_a_build_runs_with_the_jobs_memory_allows_and_the_next_waits_for_its_peak(self):
        # 24 GB free, a 16 GB floor: 8 GB of room; 12 jobs would peak at 9.2 GB, 10 at 8.0 GB.
        first = self.start("a", SLEEP="6")
        self.assertEqual(self.started("a", within=60)[1], "10")
        second = self.start("b")
        self.assertEqual(first.wait(timeout=120), 0)
        self.assertEqual(second.wait(timeout=120), 0)
        self.assertGreater(int(self.started("b", within=1)[0]), self.ended("a"), "b ran beside a's declared peak")
        # Room for fewer than 2 jobs: no build, however long it waits.
        self.free(18)
        self.start("c")
        self.assertIsNone(self.started("c", within=8))

    def test_no_more_runs_than_slots_however_much_memory_is_free(self):
        self.free(64)
        first = self.start("a", SLEEP="4", NRESE_BUILD_SLOTS="1", NRESE_BUILD_PEAK_GB="1")
        self.assertIsNotNone(self.started("a", within=60))
        second = self.start("b", NRESE_BUILD_SLOTS="1", NRESE_BUILD_PEAK_GB="1")
        self.assertEqual(first.wait(timeout=120), 0)
        self.assertEqual(second.wait(timeout=120), 0)
        self.assertGreater(int(self.started("b", within=1)[0]), self.ended("a"))

    def test_a_request_that_can_never_fit_is_refused_at_once_and_holds_no_one_up(self):
        never = self.start("a", stderr=subprocess.PIPE, NRESE_BUILD_PEAK_GB="100")
        _, err = never.communicate(timeout=60)
        self.assertEqual(never.returncode, 2)
        self.assertIn("more than this machine can ever give it", err)
        follower = self.start("b", NRESE_BUILD_PEAK_GB="1")
        self.assertIsNotNone(self.started("b", within=60))
        self.assertEqual(follower.wait(timeout=120), 0)

    def test_a_holder_that_is_gone_is_reclaimed_even_when_its_pid_lives_on(self):
        # A live process whose pid a crashed holder had: its identity (pid and start time) differs.
        sleeper = subprocess.Popen([BASH, "-c", "sleep 120 & echo $!; wait"], stdout=subprocess.PIPE, text=True)
        self.addCleanup(lambda: (sleeper.kill(), sleeper.wait()))
        pid = sleeper.stdout.readline().strip()
        self.addCleanup(lambda: subprocess.run([BASH, "-c", f"kill {pid} 2> /dev/null"]))
        for holder in (f"{pid}@1", "99999999@1"):
            stale = self.root / "slots" / "run" / holder
            stale.mkdir(parents=True)
            (stale / "peak").write_text("999999")
        build = self.start("a")
        self.assertIsNotNone(self.started("a", within=60))
        self.assertEqual(build.wait(timeout=120), 0)
        self.assertEqual(list((self.root / "slots" / "run").iterdir()), [])

    def test_only_a_stop_of_its_own_build_counts_and_a_late_notice_is_waited_for(self):
        for notice, code in (("late", 75), ("other", 127)):
            (self.guard / "BUSY").unlink(missing_ok=True)
            build = self.start(f"said-{notice}", stderr=subprocess.PIPE, NOTICE=notice, CODE="127")
            _, err = build.communicate(timeout=120)
            self.assertEqual(build.returncode, code, (notice, err))
            self.assertEqual("stopped by the PC guard" in err, code == 75, (notice, err))

    @unittest.skipUnless(sys.platform.startswith("linux"), "a pseudo-terminal (pty)")
    def test_only_a_terminal_gives_up_waiting(self):
        import pty
        (self.guard / "BUSY").write_text("busy")
        primary, secondary = pty.openpty()
        self.addCleanup(os.close, primary)
        self.addCleanup(os.close, secondary)
        build = self.start("a", stderr=secondary)
        self.assertEqual(build.wait(timeout=60), 75)
        self.assertFalse((self.out / "a.started").exists())


if __name__ == "__main__":
    unittest.main()
