"""Small Linux lifecycle checks, explicitly enabled in a Fleet correctness job.

NRESE_TEST_NATIVE_CLEANUP=1 python -m unittest discover -s benches/suite/tests -p test_cleanup_native.py
No engine, benchmark input, Docker image or shared path is used.
"""
from __future__ import annotations

import os
import shutil
import sys
import tempfile
import unittest
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from suitekit import runtime  # noqa: E402


@unittest.skipUnless(sys.platform == "linux" and os.environ.get("NRESE_TEST_NATIVE_CLEANUP") == "1",
                     "requires an explicitly enabled Linux Fleet lifecycle check")
class NativeCleanup(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="nrese-cleanup-proof-", dir=os.environ.get("FLEET_OUT")))
        self.runtimes = []

    def runtime(self, scoped=False):
        scratch = self.root / f"scratch-{len(self.runtimes)}"
        rt = (runtime.RootfsRuntime(scratch, 0, self.root) if scoped
              else runtime.ProcessRuntime(scratch, 0))
        self.runtimes.append(rt)
        return rt

    def tearDown(self):
        for rt in self.runtimes:
            rt.cleanup()
        residuals = [rt.residuals() for rt in self.runtimes if any(rt.residuals().values())]
        if residuals:
            self.fail(f"owned resources retained at {self.root}: {residuals}")
        shutil.rmtree(self.root)

    def test_absent_scope_is_not_a_cleanup_failure(self):
        runtime.end_scope(f"nrese-cleanup-absent-{uuid.uuid4().hex}")

    def test_scoped_run_is_reaped_before_its_store_is_removed(self):
        rt = self.runtime(scoped=True)
        store = rt.new_store("store")
        spec = runtime.Spec(f"cleanup-{uuid.uuid4().hex}", None,
                            [sys.executable, "-c", "print('owned scope completed')"], memory="64m")
        measured = rt.run(spec, self.root / "scope.log", 30)
        self.assertTrue(measured.ok, (self.root / "scope.log").read_text())
        rt.cleanup()
        self.assertFalse(rt.cleanup_errors)
        self.assertFalse(any(rt.residuals().values()))
        self.assertFalse(Path(store).exists())

    def test_departed_leader_does_not_leave_a_writing_child(self):
        rt = self.runtime()
        store = Path(rt.new_store("store"))
        marker = store / "child.pid"
        # Synchronize before the leader exits: its child deliberately ignores TERM.
        program = (
            "import os,signal,sys,time\n"
            "r,w=os.pipe()\n"
            "pid=os.fork()\n"
            "if pid==0:\n"
            " os.close(r); signal.signal(signal.SIGTERM,signal.SIG_IGN)\n"
            " os.write(w,b'1'); os.close(w); time.sleep(60)\n"
            "else:\n"
            " os.close(w); os.read(r,1); os.close(r)\n"
            " open(sys.argv[1],'w').write(str(pid))\n"
        )
        measured = rt.run(runtime.Spec("owned-child", None, [sys.executable, "-c", program, str(marker)]),
                          self.root / "process.log", 30)
        self.assertTrue(measured.ok)
        self.assertFalse(rt.cleanup_errors)
        child = Path("/proc") / marker.read_text().strip() / "stat"
        try:
            state = child.read_text().rsplit(")", 1)[1].split()[0]
        except FileNotFoundError:
            state = None
        self.assertIn(state, (None, "Z"), "the child must no longer be able to use the store")
        rt.cleanup()
        self.assertFalse(store.exists())

    def test_symlink_store_cannot_remove_shared_content(self):
        rt = self.runtime()
        store = Path(rt.new_store("store"))
        shared = self.root / "shared"
        shared.mkdir()
        sentinel = shared / "sentinel"
        sentinel.write_text("preserve")
        store.rmdir()
        store.symlink_to(shared, target_is_directory=True)
        try:
            with self.assertRaises(ValueError):
                rt.remove_store(str(store))
            self.assertEqual(sentinel.read_text(), "preserve")
        finally:
            store.unlink()
        rt.remove_store(str(store))


if __name__ == "__main__":
    unittest.main()
