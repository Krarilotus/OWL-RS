"""The suite's runner keeps a test that runs git out of the repository a calling git names
(isolation.py). 20 Oct 2026: under the commit hook, tests that ran git emptied two commits and
moved a branch of the repository being committed to."""
from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ISOLATION = Path(__file__).with_name("isolation.py")
INNER_TEST = '''
import subprocess, tempfile, unittest

class Inner(unittest.TestCase):
    def test_commits_in_its_own_repository(self):
        with tempfile.TemporaryDirectory() as tmp:
            git = lambda *a: subprocess.run(["git", "-C", tmp, "-c", "user.name=t", "-c", "user.email=t@t", *a],
                                            capture_output=True, text=True, check=True).stdout
            git("init", "-q")
            git("commit", "-q", "--allow-empty", "-m", "inner")
            git("checkout", "-q", "-b", "elsewhere")
            self.assertIn("inner", git("log", "--oneline"))
'''


class Isolation(unittest.TestCase):
    def test_a_test_running_git_under_a_hook_leaves_the_outer_repository_alone(self):
        clean = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        with tempfile.TemporaryDirectory() as tmp:
            outer, inner = Path(tmp) / "outer", Path(tmp) / "inner"
            outer.mkdir()
            inner.mkdir()
            git = lambda *a: subprocess.run(["git", "-C", str(outer), "-c", "user.name=t", "-c", "user.email=t@t",  # noqa: E731
                                             *a], capture_output=True, text=True, env=clean, check=True).stdout
            git("init", "-q")
            git("commit", "-q", "--allow-empty", "-m", "outer")
            state = lambda: (git("for-each-ref"), git("rev-parse", "HEAD"), git("symbolic-ref", "HEAD"),  # noqa: E731
                             (outer / ".git" / "index").read_bytes() if (outer / ".git" / "index").exists() else b"")
            before = state()
            (inner / "test_inner.py").write_text(INNER_TEST, encoding="utf-8")
            # What a commit hook's environment names: the outer repository, its index, its tree.
            hook = {**clean, "GIT_DIR": str(outer / ".git"), "GIT_INDEX_FILE": str(outer / ".git" / "index"),
                    "GIT_WORK_TREE": str(outer)}
            done = subprocess.run([sys.executable, str(ISOLATION), "discover", "-s", str(inner)], env=hook,
                                  capture_output=True, text=True, timeout=120)
            self.assertEqual(done.returncode, 0, done.stderr[-800:])
            self.assertEqual(state(), before)


if __name__ == "__main__":
    unittest.main()
