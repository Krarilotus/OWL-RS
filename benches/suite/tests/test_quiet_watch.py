"""The quiet slot's watcher (scripts/lib/quiet-watch.py): what is the measurement, and what
gets stopped while it runs.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "quiet_watch", Path(__file__).resolve().parents[3] / "scripts" / "lib" / "quiet-watch.py")
watch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(watch)

ME, OTHER, ROOT = 1000, 1001, 0
# pid: (ppid, uid, cpu ticks, name): the queue runner (10) starts the measurement (20), which
# runs perf_lab (21); beside them this job's own leftover build (30, 31), another job's build
# under the same user (32), a process of ours whose environment can't be read (33), one in the
# job's slice (34), an idle shell (40), a colleague's browser (50) and dockerd (60).
TABLE = {10: (1, ME, 0, "python3"), 20: (10, ME, 0, "fast.py"), 21: (20, ME, 0, "perf_lab"),
         30: (1, ME, 0, "cargo"), 31: (30, ME, 0, "rustc"), 32: (1, ME, 0, "rustc"), 33: (1, ME, 0, "unpack"),
         34: (1, ME, 0, "load"), 40: (1, ME, 0, "bash"), 50: (1, OTHER, 0, "firefox"), 60: (1, ROOT, 0, "dockerd")}
ENVIRON = {31: {"FLEET_JOB": "job-a"}, 32: {"FLEET_JOB": "job-b"}, 34: {}, 40: {}, 50: {}, 60: {}}
CGROUP = {34: "0::/user.slice/user-1000.slice/user@1000.service/fleet.slice/fleet-job-a.slice/step.scope\n"}


def later(busy: dict[int, int]) -> dict:
    """The table after one second, `busy` pid -> ticks used meanwhile."""
    return {pid: (ppid, uid, ticks + busy.get(pid, 0), name) for pid, (ppid, uid, ticks, name) in TABLE.items()}


class Watch(unittest.TestCase):
    def test_the_measurement_is_the_root_and_all_it_runs_never_its_ancestors_neighbours(self):
        mine, ancestors = watch.family(TABLE, 20)
        self.assertEqual(mine, {20, 21})
        self.assertEqual(ancestors, {10})

    def test_only_work_positively_the_jobs_own_is_suspended_never_by_its_user_id(self):
        """COT-13 (20 Oct 2026): every busy process of the user was stopped, another agent's
        build included. Owned is the slot's FLEET_JOB, its slice, its namespace's containers."""
        ticks = watch.TICKS
        now = later({21: 4 * ticks, 31: 2 * ticks, 32: 2 * ticks, 33: 2 * ticks, 34: 2 * ticks,
                     40: ticks // 100, 50: 3 * ticks, 60: ticks})
        mine, ancestors = watch.family(now, 20)
        environ_of, cgroup_of = (lambda pid: ENVIRON.get(pid, {})), (lambda pid: CGROUP.get(pid, ""))
        owner = watch.Owner({"FLEET_JOB": "job-a", "FLEET_SLICE": "fleet-job-a.slice"}, namespace="job-a")
        owned, unowned_cores, unowned = watch.busy(TABLE, now, 1.0, mine | ancestors, owner, environ_of, cgroup_of)
        # The job's own build and the process in its slice; another job's, an unreadable one,
        # a colleague's and root's are left running and count as load.
        self.assertEqual((sorted(owned), sorted(unowned)), ([31, 34], [32, 33, 50, 60]))
        self.assertAlmostEqual(unowned_cores, 8.0, places=1)
        # A slot that knows no owner (no FLEET_JOB, no slice) suspends nothing, whatever the uid.
        nobody = watch.Owner({}, namespace=None)
        self.assertEqual(watch.busy(TABLE, now, 1.0, mine | ancestors, nobody, environ_of, cgroup_of)[0], [])
        self.assertEqual([owner.owns_container(label) for label in ("job-a", "job-b", "")], [True, False, False])
        self.assertFalse(nobody.owns_container(""))

    @unittest.skipUnless(hasattr(watch.signal, "SIGSTOP"), "POSIX signals (the watcher runs on Linux)")
    def test_the_journal_comes_first_and_a_resume_continues_only_the_process_it_recorded(self):
        """COT-14 (20 Oct 2026): the suspension came before its record, and the record was a bare
        pid: a watcher killed between them stranded the process, and a resume could continue a
        reused pid. A line that can't be settled stays for the next holder."""
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as tmp:
            frozen = Path(tmp) / "frozen"
            seen = []
            with patch.object(watch, "started", lambda pid: "777"), \
                    patch.object(watch.os, "kill", lambda pid, sig: seen.append((pid, sig, frozen.read_text()))):
                self.assertTrue(watch.suspend(31, frozen, "boot-1", set()))
            self.assertEqual(seen, [(31, watch.signal.SIGSTOP, "pid 31 777 boot-1\n")])
            # 31 is still the process recorded; 41's pid now names another (other start time);
            # 51 has no start time (an older watcher's line); container def won't unpause.
            frozen.write_text("pid 31 777 boot-1\npid 41 555 boot-1\npid 51\ncontainer abc\ncontainer def\n")
            sent, unpaused = [], []

            def docker(args, **_):
                unpaused.append(args[-1])
                return SimpleNamespace(returncode=0 if args[-1] == "abc" else 1, stderr="cannot unpause")
            with patch.object(watch, "started", lambda pid: {31: "777", 41: "999"}.get(pid)), \
                    patch.object(watch, "boot_id", lambda: "boot-1"), \
                    patch.object(watch.os, "kill", lambda pid, sig: sent.append((pid, sig))), \
                    patch.object(watch.subprocess, "run", docker):
                left = watch.resume(Path(tmp))
            self.assertEqual(sent, [(31, watch.signal.SIGCONT)])
            self.assertEqual(unpaused, ["abc", "def"])
            self.assertEqual(left, ["pid 51", "container def"])
            self.assertEqual(frozen.read_text(), "pid 51\ncontainer def\n")


if __name__ == "__main__":
    unittest.main()
