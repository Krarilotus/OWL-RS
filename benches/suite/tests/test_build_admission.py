"""Build admission (scripts/cargo-guarded.sh): a cargo run waits while the machine is busy and
until its declared memory peak fits above the floor, never building anyway, and says when the
main PC's guard stopped it.

Run: python -m unittest discover -s benches/suite/tests -p test_build_admission.py
"""

import os
from pathlib import Path
import shutil
import subprocess
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
                    "scripts/lib/prune-stale.py"):
            (self.repo / rel).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(ROOT / rel, self.repo / rel)
        self.stubs = self.root / "stubs"
        self.stubs.mkdir()
        # cargo: says when it started and with how many jobs, runs a while, and ends with $CODE.
        self.stub("cargo", 'echo "$(date +%s%N) $CARGO_BUILD_JOBS" > "$OUT/$RUN.started"\n'
                           'sleep "${SLEEP:-0}"\n'
                           '[ -z "${KILL:-}" ] || echo "$(date +%s) KILLED-BY-GUARD 4242 rustc.exe" >> "$GUARD/killed.log"\n'
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

    def start(self, run, **env):
        base = {k: v for k, v in os.environ.items() if not k.startswith(("GIT_", "NRESE_", "CARGO_", "FLEET_"))}
        env = {**base, "HOME": str(self.root), "PATH": f"{self.stubs}{os.pathsep}{os.environ['PATH']}",
               "OUT": str(self.out), "RUN": run, "GUARD": str(self.guard),
               "NRESE_BUILD_SLOT_DIR": str(self.root / "slots"), "NRESE_GUARD_BUSY": str(self.guard / "BUSY"),
               "NRESE_GUARD_KILLED": str(self.guard / "killed.log"), "NRESE_MEMINFO": str(self.root / "meminfo"),
               "NRESE_BUILD_FLOOR_GB": "16", "NRESE_MEMORY_CAP_GB": "0", "NRESE_MIN_FREE_GB": "0",
               "NRESE_TARGET_BUDGET_GB": "1000", "CARGO_TARGET_DIR": str(self.root / "target"),
               "NRESE_QUIET_DIR": str(self.root / "no-quiet-slot"), "NRESE_BUILD_SLOT_WAIT_S": "0", **env}
        err = subprocess.PIPE if run == "said" else subprocess.DEVNULL
        build = subprocess.Popen([BASH, "scripts/cargo-guarded.sh", "build"], cwd=self.repo, env=env,
                                 stdout=subprocess.DEVNULL, stderr=err, text=True)
        self.addCleanup(lambda: build.poll() is not None or (build.kill(), build.wait()))
        return build

    def started(self, run, within):
        deadline = time.time() + within
        while time.time() < deadline:
            if (self.out / f"{run}.started").exists():
                return (self.out / f"{run}.started").read_text().split()
            time.sleep(0.2)
        return None

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
        a_ended = int((self.out / "a.ended").read_text())
        self.assertGreater(int(self.started("b", within=1)[0]), a_ended, "b ran beside a's declared peak")
        # Room for fewer than 2 jobs: no build, however long it waits.
        self.free(18)
        third = self.start("c")
        self.assertIsNone(self.started("c", within=8))

    def test_a_run_the_guard_stopped_says_so(self):
        build = self.start("said", KILL="1", CODE="127")
        _, err = build.communicate(timeout=120)
        self.assertEqual(build.returncode, 75, err)
        self.assertIn("stopped by the PC guard, low memory (it stopped 4242 rustc.exe", err)


if __name__ == "__main__":
    unittest.main()
