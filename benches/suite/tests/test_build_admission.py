"""Build admission (scripts/cargo-guarded.sh): a cargo run waits for a slot and until its declared
memory peak fits above the floor, never building anyway, and for no earlier run that can't start
itself; its wait is bounded, terminal or not; a request that can never fit is refused at once; a
dead holder is reclaimed by its identity; and a run says
it was stopped by the main PC's guard only when the guard stopped a process of its own build.
Sol's round-1 probes (Harness/notes/review-build-admission-r1-20261020/probe.py) are here with
the behaviour they asked for.

Run: python -m unittest discover -s benches/suite/tests -p test_build_admission.py
"""

import hashlib
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
        # The guard's notices (an intent, then STOPPED or STOP-FAILED), by NOTICE: late, the machine
        # turns busy and a completed stop of this build's process comes a second after cargo ended;
        # other, a completed stop of another build's; failed, an intent for this build whose stop
        # fails two seconds later; intent, an intent that never completes.
        self.stub("cargo", 'echo "$(date +%s%N) $CARGO_BUILD_JOBS" > "$OUT/$RUN.started"\n'
                           'sleep "${SLEEP:-0}"\n'
                           'log() { echo "$(date +%s) $*" >> "$GUARD/killed.log"; }\n'
                           'intent() { log "KILLED-BY-GUARD 4242 rustc.exe 4242@1 jobs=$1"; }\n'
                           'case "${NOTICE:-}" in\n'
                           '  late) echo busy > "$GUARD/BUSY"\n'
                           '    (sleep 1; intent "$NRESE_BUILD_JOB"; log GUARD-STOPPED 4242@1) > /dev/null 2>&1 & ;;\n'
                           '  other) intent nrese-build-another; log GUARD-STOPPED 4242@1 ;;\n'
                           '  failed) intent "$NRESE_BUILD_JOB"; (sleep 2; log GUARD-STOP-FAILED 4242@1) > /dev/null 2>&1 & ;;\n'
                           '  intent) intent "$NRESE_BUILD_JOB" ;;\n'
                           '  after) intent "$NRESE_BUILD_JOB"; (/usr/bin/sleep 3; log GUARD-STOPPED 4242@1) > /dev/null 2>&1 & ;;\n'
                           'esac\n'
                           'echo "$(date +%s%N)" > "$OUT/$RUN.ended"\nexit "${CODE:-0}"')
        self.stub("nproc", "echo 32")  # 12 compiler jobs by default
        self.guard = self.root / "guard"
        self.guard.mkdir()
        self.out = self.root / "out"
        self.out.mkdir()
        self.free(24)
        self.disk(400)

    def stub(self, name, body):
        (self.stubs / name).write_text("#!/bin/sh\n" + body + "\n")
        (self.stubs / name).chmod(0o755)

    def disk(self, gb):
        (self.root / "diskfree").write_text(f"{gb}\n")

    def free(self, gb):
        (self.root / "meminfo").write_text(f"MemTotal: {64 * GB} kB\nMemFree: {gb * GB} kB\n")

    def env(self, run, **env):
        base = {k: v for k, v in os.environ.items() if not k.startswith(("GIT_", "NRESE_", "CARGO_", "FLEET_"))}
        merged = {**base, "HOME": str(self.root), "PATH": f"{self.stubs}{os.pathsep}{os.environ['PATH']}",
                "OUT": str(self.out), "RUN": run, "GUARD": str(self.guard),
                "NRESE_BUILD_SLOT_DIR": str(self.root / "slots"), "NRESE_GUARD_BUSY": str(self.guard / "BUSY"),
                "NRESE_GUARD_KILLED": str(self.guard / "killed.log"), "NRESE_MEMINFO": str(self.root / "meminfo"),
                "NRESE_BUILD_FLOOR_GB": "16", "NRESE_MEMORY_CAP_GB": "0", "NRESE_MIN_FREE_GB": "0",
                "NRESE_TARGET_BUDGET_GB": "1000", "CARGO_TARGET_DIR": str(self.root / "target"),
                "NRESE_QUIET_DIR": str(self.root / "no-quiet-slot"), "NRESE_BUILD_SLOT_WAIT_S": "600",
                "NRESE_BUILD_SLOT_POLL_S": "1",
                "NRESE_GUARD_DISKFREE": str(self.root / "diskfree"), **env}
        # None: not set at all (the script's own default).
        return {k: v for k, v in merged.items() if v is not None}

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

    def late_writer_done(self):
        # The stub's late writer (NOTICE=after) ends before the test does: Windows can't remove a
        # directory a process still writes to.
        deadline = time.time() + 15
        while "GUARD-STOPPED" not in (self.guard / "killed.log").read_text() and time.time() < deadline:
            time.sleep(0.2)

    def ended(self, run):
        return int((self.out / f"{run}.ended").read_text())

    def test_a_build_that_fits_above_the_floor_runs_though_the_machine_is_busy(self):
        # The orchestrator, 20 Oct 2026: BUSY stayed set for hours under the agents' own load. 12 GB
        # free, the default 8 GB floor: room for a 3 GB peak, not for a 5 GB one.
        (self.guard / "BUSY").write_text("12.0 GB RAM, 394 GB disk free")
        self.free(12)
        floor = {"NRESE_BUILD_FLOOR_GB": None, "NRESE_GUARD_SCRIPT": str(self.root / "no-guard.ps1")}
        fits = self.start("a", NRESE_BUILD_PEAK_GB="3", **floor)
        self.assertIsNotNone(self.started("a", within=60), "held by BUSY though it fits")
        self.assertEqual(fits.wait(timeout=120), 0)
        self.start("b", NRESE_BUILD_PEAK_GB="5", **floor)
        self.assertIsNone(self.started("b", within=8))

    def test_no_run_starts_below_the_guards_disk_line_busy_or_not(self):
        # Sol, review-build-admission-r5 (r5-1): ignoring BUSY ignored the guard's disk kill line (100 GB
        # on C:). Memory fits; C: has 90 GB free: no build; at 400 GB the same run builds.
        (self.guard / "BUSY").write_text("24.0 GB RAM, 90 GB disk free")
        self.disk(90)
        floor = {"NRESE_BUILD_FLOOR_GB": None, "NRESE_GUARD_SCRIPT": str(self.root / "no-guard.ps1")}
        low = self.start("low", stderr=subprocess.PIPE, NRESE_BUILD_PEAK_GB="1", NRESE_BUILD_SLOT_WAIT_S="3", **floor)
        _, err = low.communicate(timeout=60)
        self.assertEqual(low.returncode, 75, err)
        self.assertIn("below its kill line of 100 GB", err)
        self.assertFalse((self.out / "low.started").exists())
        self.disk(400)
        fits = self.start("fits", NRESE_BUILD_PEAK_GB="1", **floor)
        self.assertEqual(fits.wait(timeout=120), 0)
        self.assertIsNotNone(self.started("fits", within=1))

    def test_the_guards_line_is_read_as_powershell_reads_it_or_nothing_builds(self):
        # Sol, r5-2: other casing or spacing, or a last line without a newline, read as no line: an
        # 8 GB floor under a guard that kills at 12 GB. 11 GB free: none of them builds.
        self.free(11)
        script = self.root / "guard.ps1"
        for n, text in enumerate(["$Hard = ($RAM -lt 12 ) -or ( $Disk -lt 100)\n",
                                  "$soft = ($ram -lt 16)\n$hard = ($ram -lt 12) -or ($disk -lt 100)"]):
            script.write_text(text)
            run = self.start(f"v{n}", stderr=subprocess.PIPE, NRESE_BUILD_FLOOR_GB=None, NRESE_GUARD_SCRIPT=str(script),
                             NRESE_BUILD_PEAK_GB="1", NRESE_BUILD_SLOT_WAIT_S="2")
            _, err = run.communicate(timeout=60)
            self.assertEqual(run.returncode, 75, (text, err))
            self.assertIn("above the 16 GB floor", err)
            self.assertFalse((self.out / f"v{n}.started").exists(), text)
        # A line it can't read: no build at all, never a guessed lower line.
        script.write_text("$hard = $ram -lt 12\n")
        run = self.start("unread", stderr=subprocess.PIPE, NRESE_BUILD_FLOOR_GB=None, NRESE_GUARD_SCRIPT=str(script),
                         NRESE_BUILD_PEAK_GB="1")
        _, err = run.communicate(timeout=60)
        self.assertEqual(run.returncode, 2, err)
        self.assertIn("kill line can't be read", err)
        self.assertFalse((self.out / "unread.started").exists())

    def test_room_that_appears_after_the_deadline_admits_nothing(self):
        # Sol, r5 follow-up: a sleep running past the deadline, then room: the run must end, not build.
        self.free(10)
        run = self.start("late", stderr=subprocess.PIPE, NRESE_BUILD_PEAK_GB="2", NRESE_BUILD_SLOT_WAIT_S="2",
                         NRESE_BUILD_SLOT_POLL_S="5")
        # Its wait starts with its ticket (a loaded machine takes seconds to get there); the room
        # comes a second after its 2 s deadline, before its 5 s sleep would have ended.
        deadline = time.time() + 60
        while not list((self.root / "slots" / "queue").glob("*")) and time.time() < deadline:
            time.sleep(0.1)
        time.sleep(3)
        self.free(64)
        _, err = run.communicate(timeout=60)
        self.assertEqual(run.returncode, 75, err)
        self.assertFalse((self.out / "late.started").exists())

    def test_a_run_that_cannot_start_holds_up_no_one_behind_it_who_can(self):
        # 20 Oct 2026 (16:52, 18:11): a run that couldn't start headed the line and stalled every
        # hook behind it. 24 GB free, a 16 GB floor: 8 GB of room; the first asks 12 GB, the next 2.
        self.start("big", NRESE_BUILD_PEAK_GB="12")
        deadline = time.time() + 30
        while not list((self.root / "slots" / "queue").glob("*")) and time.time() < deadline:
            time.sleep(0.2)
        small = self.start("small", NRESE_BUILD_PEAK_GB="2")
        self.assertIsNotNone(self.started("small", within=60), "held up by a run that can't start")
        self.assertEqual(small.wait(timeout=120), 0)
        self.assertFalse((self.out / "big.started").exists())

    def test_the_default_floor_is_the_guards_kill_line_and_a_margin(self):
        # 20 Oct 2026: the soft line (16 GB) as the floor kept every hook on a merged base waiting.
        # The guard's script says its kill line; plus 4 GB. Here 6: a 10 GB floor, 13 GB free.
        script = self.root / "guard.ps1"
        script.write_text("$soft = ($ram -lt 16) -or ($disk -lt 150)\n$hard = ($ram -lt 6) -or ($disk -lt 100)\n")
        self.free(13)
        floor = {"NRESE_BUILD_FLOOR_GB": None, "NRESE_GUARD_SCRIPT": str(script)}
        fits = self.start("a", NRESE_BUILD_PEAK_GB="3", **floor)
        self.assertIsNotNone(self.started("a", within=60), "3 GB above a 10 GB floor with 13 GB free")
        self.assertEqual(fits.wait(timeout=120), 0)
        self.start("b", NRESE_BUILD_PEAK_GB="4", **floor)
        self.assertIsNone(self.started("b", within=8), "4 GB above a 10 GB floor with 13 GB free")

    def test_without_the_guards_script_the_floor_is_8_gb(self):
        self.free(12)
        floor = {"NRESE_BUILD_FLOOR_GB": None, "NRESE_GUARD_SCRIPT": str(self.root / "no-guard.ps1")}
        fits = self.start("a", NRESE_BUILD_PEAK_GB="4", **floor)
        self.assertIsNotNone(self.started("a", within=60))
        self.assertEqual(fits.wait(timeout=120), 0)
        self.start("b", NRESE_BUILD_PEAK_GB="5", **floor)
        self.assertIsNone(self.started("b", within=8))

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
        # The holder named by its pid alone (before identities: kill -0 says it lives) and by its pid
        # with another start time; and one whose pid is gone.
        for holder in (pid, f"{pid}@1", "99999999@1"):
            stale = self.root / "slots" / "run" / holder
            stale.mkdir(parents=True)
            (stale / "peak").write_text("999999")
        build = self.start("a")
        self.assertIsNotNone(self.started("a", within=60))
        self.assertEqual(build.wait(timeout=120), 0)
        self.assertEqual(list((self.root / "slots" / "run").iterdir()), [])

    def test_only_a_completed_stop_of_its_own_build_counts_and_a_late_one_is_waited_for(self):
        for notice, code in (("late", 75), ("other", 127), ("failed", 127), ("intent", 127)):
            (self.guard / "BUSY").unlink(missing_ok=True)
            (self.guard / "killed.log").unlink(missing_ok=True)
            build = self.start(f"said-{notice}", stderr=subprocess.PIPE, NOTICE=notice, CODE="127",
                               # 10 s: the late stop comes after 1 s, but a loaded Git Bash took
                               # over 4 s to show it (20 Oct 2026, full suite, 2 of 3 runs).
                               NRESE_GUARD_NOTICE_WAIT_S="10")
            _, err = build.communicate(timeout=120)
            self.assertEqual(build.returncode, code, (notice, err))
            self.assertEqual("stopped by the PC guard" in err, code == 75, (notice, err))

    def test_a_completion_seen_after_the_deadline_does_not_count(self):
        # Sol, round 3: a turn of the wait that runs late (sleep 1 taking 4 s) must not stretch a
        # 2 s deadline to take in a completion published at 3 s.
        self.stub("sleep", '[ "$1" = 1 ] && exec /usr/bin/sleep 4; exec /usr/bin/sleep "$@"')
        build = self.start("said-after", stderr=subprocess.PIPE, NOTICE="after", CODE="127",
                           NRESE_GUARD_NOTICE_WAIT_S="2")
        _, err = build.communicate(timeout=120)
        self.assertEqual(build.returncode, 127, err)
        self.assertNotIn("stopped by the PC guard", err)
        self.late_writer_done()

    def test_even_the_first_look_at_the_notices_counts_only_before_the_deadline(self):
        # Sol, round 4: the first scan of the guard's notices taking 4 s (an awk that is slow only
        # for them) must not take in a completion published at 3 s against a 2 s deadline.
        self.stub("awk", 'case "$*" in *"-v s="*) /bin/sleep 4 ;; esac\nexec /usr/bin/awk "$@"')
        build = self.start("said-after", stderr=subprocess.PIPE, NOTICE="after", CODE="127",
                           NRESE_GUARD_NOTICE_WAIT_S="2")
        _, err = build.communicate(timeout=120)
        self.assertEqual(build.returncode, 127, err)
        self.assertNotIn("stopped by the PC guard", err)
        self.late_writer_done()

    @unittest.skipUnless(shutil.which("powershell"), "the memory cap's helper is Windows'")
    def test_a_broken_cap_helper_is_built_again_and_a_cap_never_silently_dropped(self):
        script = self.repo / "scripts/lib/memory-cap.ps1"
        # A copy of the script of its own (a comment of its own): its own cache entry, never the
        # one every build here uses.
        script.write_text(script.read_text() + f"\n# test {self.root.name}\n")
        digest = hashlib.sha256(script.read_bytes()).hexdigest()[:12]
        cached = Path(tempfile.gettempdir()) / "nrese-memory-cap" / f"memory-cap-{digest}.exe"
        cached.parent.mkdir(exist_ok=True)
        self.addCleanup(lambda: cached.unlink(missing_ok=True))
        cached.write_bytes(b"not a program")
        build = self.start("corrupt", stderr=subprocess.PIPE, NRESE_MEMORY_CAP_GB="32")
        _, err = build.communicate(timeout=300)
        self.assertEqual(build.returncode, 0, err)
        self.assertIsNotNone(self.started("corrupt", within=1), "no build after the helper was rebuilt")
        self.assertNotEqual(cached.read_bytes(), b"not a program", "the broken helper stayed")
        # A helper that can't be built: with a cap asked for, no build.
        script.write_text(script.read_text() + "\n} this script does not parse {\n")
        build = self.start("uncapped", stderr=subprocess.PIPE, NRESE_MEMORY_CAP_GB="32")
        _, err = build.communicate(timeout=300)
        self.assertEqual(build.returncode, 1, err)
        self.assertIn("memory cap could not be applied (scripts/lib/memory-cap.ps1): not building", err)
        self.assertIsNone(self.started("uncapped", within=1))

    def test_a_wait_without_a_terminal_ends_failing_with_its_reason_and_nothing_built(self):
        # A hook has no terminal and no one to interrupt it: it fails, it never hangs or builds.
        build = self.start("a", stderr=subprocess.PIPE, NRESE_BUILD_PEAK_GB="12", NRESE_BUILD_SLOT_WAIT_S="3")
        _, err = build.communicate(timeout=60)
        self.assertEqual(build.returncode, 75, err)
        self.assertIn("not built: no room to build after", err)
        self.assertIn("it may peak at 12 GB", err)
        self.assertFalse((self.out / "a.started").exists())
        self.assertEqual(list((self.root / "slots" / "queue").glob("*")), [])


if __name__ == "__main__":
    unittest.main()
