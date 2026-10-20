"""The quiet slot's watcher (scripts/lib/quiet-watch.py): what is the measurement, and what
gets stopped while it runs.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import contextlib
import importlib.util
import os
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


@contextlib.contextmanager
def pidfds(sent: list, fail=None):
    """os.pidfd_open and signal.pidfd_send_signal stood in for: a real descriptor (of os.devnull)
    per opened pid, each signal but 0 recorded as (pid, signal); fail(pid, signal) may raise."""
    fds = {}

    def opened(pid):
        fd = os.open(os.devnull, os.O_RDONLY)
        fds[fd] = pid
        return fd

    def send(fd, sig):
        if fail:
            fail(fds[fd], sig)
        if sig:
            sent.append((fds[fd], sig))
    with patch.object(watch.os, "pidfd_open", opened, create=True), \
            patch.object(watch.signal, "pidfd_send_signal", send, create=True):
        yield


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
        # A slot that knows no owner (no FLEET_JOB, no slice: a manual run) suspends nothing,
        # whatever the uid, nor pauses a container, not even one labelled with the checkout's own
        # namespace (Sol, 20 Oct 2026: it did). Built as the watcher builds it.
        nobody = watch.Owner({})
        self.assertEqual(watch.busy(TABLE, now, 1.0, mine | ancestors, nobody, environ_of, cgroup_of)[0], [])
        self.assertEqual([owner.owns_container(label) for label in ("job-a", "job-b", "")], [True, False, False])
        checkout = watch.job_namespace({})
        self.assertTrue(checkout)
        self.assertEqual([nobody.owns_container(label) for label in (checkout, "")], [False, False])
        self.assertEqual(watch.Owner({"FLEET_JOB": "job-a"}).namespace, "job-a")

    @unittest.skipUnless(hasattr(watch.signal, "SIGCONT"), "POSIX signals (the watcher runs on Linux)")
    def test_a_resume_settles_only_what_it_continued_or_proved_gone_and_the_watch_fails_on_the_rest(self):
        """Sol, 20 Oct 2026: an EPERM on SIGCONT, or a stat that couldn't be read, counted as
        settled: the record of a process left stopped was deleted, and the watch exited 0."""
        with tempfile.TemporaryDirectory() as tmp:
            frozen = Path(tmp) / "frozen"
            frozen.write_text("pid 31 777 boot-1\npid 41 555 boot-1\npid 51 666 boot-1\npid 61 888 boot-0\n")

            def started(pid):
                if pid == 51:
                    raise PermissionError("injected: unreadable stat")
                return {31: "777", 41: None}.get(pid)

            def eperm(pid, sig):
                if sig:
                    raise PermissionError("injected EPERM")
            with patch.object(watch, "started", started), patch.object(watch, "boot_id", lambda: "boot-1"), \
                    pidfds([], fail=eperm):
                left = watch.resume(Path(tmp))
            # 31 couldn't be continued, 51 couldn't be inspected: kept; 41 is gone, 61 of another boot.
            self.assertEqual(left, ["pid 31 777 boot-1", "pid 51 666 boot-1"])
            self.assertEqual(frozen.read_text(), "pid 31 777 boot-1\npid 51 666 boot-1\n")
            with patch.object(watch, "processes", side_effect=[{}, {}]), patch.object(watch.time, "sleep"), \
                    patch.object(watch, "resume", return_value=["container stuck"]), \
                    patch.object(watch, "job_namespace", return_value=None):
                self.assertEqual(watch.watch(Path(tmp), 12345, "token"), 1)

    @unittest.skipUnless(hasattr(watch.signal, "SIGSTOP"), "POSIX signals (the watcher runs on Linux)")
    def test_an_unknown_boot_proves_nothing_gone_and_suspends_nothing(self):
        """Sol, 20 Oct 2026: an unreadable boot id became "?", unequal to the recorded boot, so the
        record of a process still stopped was dropped without looking at it."""
        owner, mine = watch.Owner({"FLEET_JOB": "job-a"}), (lambda pid: {"FLEET_JOB": "job-a"})
        with tempfile.TemporaryDirectory() as tmp:
            frozen = Path(tmp) / "frozen"
            for unknown in (None, "?"):
                frozen.write_text("pid 31 777 boot-1\npid 32 888 ?\n")
                sent = []
                with patch.object(watch, "boot_id", lambda: unknown), patch.object(watch, "started", lambda pid: "777"), \
                        pidfds(sent):
                    self.assertEqual(watch.resume(Path(tmp)), ["pid 31 777 boot-1", "pid 32 888 ?"])
                    self.assertFalse(watch.suspend(41, Path(tmp) / "new", unknown, set(), owner, environ_of=mine))
                self.assertEqual((sent, frozen.read_text(), (Path(tmp) / "new").exists()),
                                 ([], "pid 31 777 boot-1\npid 32 888 ?\n", False))
            # A recorded "?" boot stays unknown under a known one too.
            with patch.object(watch, "boot_id", lambda: "boot-1"), pidfds([]):
                self.assertEqual(watch.resume(Path(tmp)), ["pid 32 888 ?"])

    @unittest.skipUnless(hasattr(watch.signal, "SIGSTOP"), "POSIX signals (the watcher runs on Linux)")
    def test_signals_go_through_a_handle_proven_to_hold_the_recorded_process_never_by_bare_pid(self):
        """Sol, 20 Oct 2026: the start time was read, then kill(pid) sent: a pid reused between
        the two got SIGSTOP or SIGCONT. Every signal now goes through a pidfd whose process was
        alive after its identity and ownership were read; without pidfds, nothing is signalled."""
        owner = watch.Owner({"FLEET_JOB": "job-a"})
        mine = lambda pid: {"FLEET_JOB": "job-a"}  # noqa: E731
        with tempfile.TemporaryDirectory() as tmp:
            frozen = Path(tmp) / "frozen"
            for dies_after_read, expect_stop in ((False, True), (True, False)):
                sent, bare = [], []

                def exits(pid, sig, dies=dies_after_read):
                    if dies and sig == 0:
                        raise ProcessLookupError  # the handle's process exited after its stat was read
                with pidfds(sent, fail=exits), patch.object(watch.os, "kill", lambda *a: bare.append(a)), \
                        patch.object(watch, "boot_id", lambda: "boot-1"), \
                        patch.object(watch, "started", lambda pid: "777"):
                    stopped = watch.suspend(31, frozen, "boot-1", set(), owner, environ_of=mine)
                    frozen.write_text("pid 31 777 boot-1\n")
                    left = watch.resume(Path(tmp))
                self.assertEqual((stopped, [s for _, s in sent], bare, left),
                                 (expect_stop, [watch.signal.SIGSTOP, watch.signal.SIGCONT] if expect_stop else [],
                                  [], []))
            # No pidfds here: nothing is stopped, and a recorded process isn't continued blindly.
            frozen.write_text("pid 31 777 boot-1\n")

            def none(pid):
                raise AttributeError("no pidfd_open")
            with patch.object(watch.os, "pidfd_open", none, create=True), patch.object(watch, "started", lambda pid: "777"), \
                    patch.object(watch, "boot_id", lambda: "boot-1"), patch.object(watch.os, "kill", lambda *a: bare.append(a)):
                self.assertFalse(watch.suspend(31, frozen, "boot-1", set(), owner, environ_of=mine))
                self.assertEqual(watch.resume(Path(tmp)), ["pid 31 777 boot-1"])
            self.assertEqual(bare, [])

    @unittest.skipUnless(hasattr(watch.signal, "SIGCONT"), "POSIX signals (the watcher runs on Linux)")
    def test_a_journal_is_replaced_whole_or_not_at_all_and_its_directory_synced(self):
        """Sol, 20 Oct 2026: the unsettled lines were written over the journal in place, so a
        crash midway left it empty, and the next resume deleted it and reported success."""
        with tempfile.TemporaryDirectory() as tmp:
            frozen = Path(tmp) / "frozen"
            synced = []
            with patch.object(watch, "fsync_dir", lambda d: synced.append(Path(d)), create=True):
                watch.journal(frozen, "pid 51")  # no start time: never settled
                watch.journal(frozen, "container abc")
            self.assertEqual(synced, [Path(tmp)])  # the new journal's entry, once
            whole = frozen.read_text()

            def crash(src, dst):
                raise OSError("injected crash before the rename")
            with patch.object(watch.subprocess, "run", lambda *a, **k: watch.subprocess.CompletedProcess(a, 0, "", "")), \
                    patch.object(watch.os, "replace", crash):
                with self.assertRaises(OSError):
                    watch.resume(Path(tmp))
            self.assertEqual(frozen.read_text(), whole)  # the old journal, whole
            self.assertEqual(sorted(p.name for p in Path(tmp).iterdir()), ["frozen"])
            with patch.object(watch.subprocess, "run", lambda *a, **k: watch.subprocess.CompletedProcess(a, 0, "", "")), \
                    patch.object(watch, "fsync_dir", lambda d: synced.append(Path(d)), create=True):
                self.assertEqual(watch.resume(Path(tmp)), ["pid 51"])
            self.assertEqual((frozen.read_text(), synced[-1]), ("pid 51\n", Path(tmp)))

    @unittest.skipUnless(hasattr(watch.signal, "SIGSTOP"), "POSIX signals (the watcher runs on Linux)")
    def test_the_journal_comes_first_and_a_resume_continues_only_the_process_it_recorded(self):
        """COT-14 (20 Oct 2026): the suspension came before its record, and the record was a bare
        pid: a watcher killed between them stranded the process, and a resume could continue a
        reused pid. A line that can't be settled stays for the next holder."""
        from types import SimpleNamespace
        owner, mine = watch.Owner({"FLEET_JOB": "job-a"}), (lambda pid: {"FLEET_JOB": "job-a"})
        with tempfile.TemporaryDirectory() as tmp:
            frozen = Path(tmp) / "frozen"
            seen = []

            def journalled(pid, sig):  # what the journal holds when the signal is sent
                if sig:
                    seen.append((pid, sig, frozen.read_text()))
            with patch.object(watch, "started", lambda pid: "777"), pidfds([], fail=journalled):
                self.assertTrue(watch.suspend(31, frozen, "boot-1", set(), owner, environ_of=mine))
            self.assertEqual(seen, [(31, watch.signal.SIGSTOP, "pid 31 777 boot-1\n")])
            # 31 is still the process recorded; 41's pid now names another (other start time);
            # 51 has no start time (an older watcher's line); container def won't unpause.
            frozen.write_text("pid 31 777 boot-1\npid 41 555 boot-1\npid 51\ncontainer abc\ncontainer def\n")
            sent, unpaused = [], []

            def docker(args, **_):
                unpaused.append(args[-1])
                return SimpleNamespace(returncode=0 if args[-1] == "abc" else 1, stderr="cannot unpause")
            with patch.object(watch, "started", lambda pid: {31: "777", 41: "999"}.get(pid)), \
                    patch.object(watch, "boot_id", lambda: "boot-1"), pidfds(sent), \
                    patch.object(watch.subprocess, "run", docker):
                left = watch.resume(Path(tmp))
            self.assertEqual(sent, [(31, watch.signal.SIGCONT)])
            self.assertEqual(unpaused, ["abc", "def"])
            self.assertEqual(left, ["pid 51", "container def"])
            self.assertEqual(frozen.read_text(), "pid 51\ncontainer def\n")


if __name__ == "__main__":
    unittest.main()
