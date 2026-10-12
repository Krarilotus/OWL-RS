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
# runs perf_lab (21); beside them another agent's build (30, 31), an idle shell (40), a
# colleague's browser (50) and dockerd (60).
TABLE = {10: (1, ME, 0, "python3"), 20: (10, ME, 0, "fast.py"), 21: (20, ME, 0, "perf_lab"),
         30: (1, ME, 0, "cargo"), 31: (30, ME, 0, "rustc"), 40: (1, ME, 0, "bash"),
         50: (1, OTHER, 0, "firefox"), 60: (1, ROOT, 0, "dockerd")}


def later(busy: dict[int, int]) -> dict:
    """The table after one second, `busy` pid -> ticks used meanwhile."""
    return {pid: (ppid, uid, ticks + busy.get(pid, 0), name) for pid, (ppid, uid, ticks, name) in TABLE.items()}


class Watch(unittest.TestCase):
    def test_the_measurement_is_the_root_and_all_it_runs_never_its_ancestors_neighbours(self):
        mine, ancestors = watch.family(TABLE, 20)
        self.assertEqual(mine, {20, 21})
        self.assertEqual(ancestors, {10})

    def test_only_this_users_busy_processes_outside_the_measurement_are_heavy(self):
        ticks = watch.TICKS
        now = later({21: 4 * ticks, 31: 2 * ticks, 40: ticks // 100, 50: 3 * ticks, 60: ticks})
        mine, ancestors = watch.family(now, 20)
        with patch.object(watch.os, "getuid", lambda: ME, create=True):
            heavy, others = watch.heavy(TABLE, now, 1.0, ME, mine | ancestors)
        # The busy build is stopped; the measurement, the idle shell and others' work are not.
        self.assertEqual(heavy, [31])
        self.assertAlmostEqual(others, 3.0, places=1)

    @unittest.skipUnless(hasattr(watch.signal, "SIGCONT"), "POSIX signals (the watcher runs on Linux)")
    def test_a_dead_watchers_stopped_processes_and_containers_are_resumed(self):
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "frozen").write_text("pid 31\ncontainer abc\n")
            sent, unpaused = [], []
            with patch.object(watch.os, "kill", lambda pid, sig: sent.append((pid, sig))), \
                    patch.object(watch.subprocess, "run", lambda args, **_: unpaused.append(args)):
                watch.resume(Path(tmp))
            self.assertEqual(sent, [(31, watch.signal.SIGCONT)])
            self.assertEqual(unpaused, [["docker", "unpause", "abc"]])
            self.assertFalse((Path(tmp) / "frozen").exists())


if __name__ == "__main__":
    unittest.main()
