"""The suite's runner keeps a test that runs git out of the repository, the configuration and the
hooks a calling git names (isolation.py). 20 Oct 2026: under the commit hook, tests that ran git
emptied two commits and moved a branch of the repository being committed to; a caller's
configuration (GIT_CONFIG_GLOBAL, GIT_CONFIG_SYSTEM, GIT_CONFIG_COUNT) set core.hooksPath and ran
an outside hook in a test's own repository (Sol)."""
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
    def test_commits_in_its_own_repository_with_the_suite_s_configuration(self):
        with tempfile.TemporaryDirectory() as tmp:
            git = lambda *a: subprocess.run(["git", "-C", tmp, *a], capture_output=True, text=True,
                                            check=True).stdout.strip()
            git("init", "-q")
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "inner")
            git("checkout", "-q", "-b", "elsewhere")
            self.assertIn("inner", git("log", "--oneline"))
            self.assertEqual(git("config", "user.name"), "suite test")
            self.assertTrue(git("config", "core.hooksPath").endswith("/hooks"))
'''
INNER_FINDS_NO_REPOSITORY = '''
import os, subprocess, tempfile, unittest

class Inner(unittest.TestCase):
    def test_finds_no_repository(self):
        for where in (tempfile.mkdtemp(), os.getcwd()):
            found = subprocess.run(["git", "rev-parse", "--show-toplevel"], cwd=where, capture_output=True, text=True)
            self.assertNotEqual(found.returncode, 0, (where, found.stdout))
'''


class Isolation(unittest.TestCase):
    def test_a_test_running_git_under_a_hook_touches_neither_the_outer_repository_nor_its_hooks(self):
        clean = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            outer, inner, hooks = tmp / "outer", tmp / "inner", tmp / "outside-hooks"
            for d in (outer, inner, hooks):
                d.mkdir()
            git = lambda *a: subprocess.run(["git", "-C", str(outer), "-c", "user.name=t", "-c", "user.email=t@t",  # noqa: E731
                                             "-c", "core.hooksPath=", *a], capture_output=True, text=True, env=clean,
                                            check=True).stdout
            git("init", "-q")
            git("commit", "-q", "--allow-empty", "-m", "outer")
            state = lambda: (git("for-each-ref"), git("rev-parse", "HEAD"), git("symbolic-ref", "HEAD"),  # noqa: E731
                             (outer / ".git" / "index").read_bytes() if (outer / ".git" / "index").exists() else b"")
            before = state()
            # An outside hook a caller's configuration names: it marks where it ran.
            marker = tmp / "hook-ran"
            (hooks / "pre-commit").write_text(f"#!/bin/sh\necho ran > '{marker.as_posix()}'\n", encoding="utf-8",
                                              newline="\n")
            (hooks / "pre-commit").chmod(0o755)
            config = tmp / "caller.gitconfig"
            config.write_text(f"[core]\n\thooksPath = {hooks.as_posix()}\n", encoding="utf-8")
            (inner / "test_inner.py").write_text(INNER_TEST, encoding="utf-8")
            # What a commit hook's environment can name: the outer repository, its index and tree,
            # a namespace, and configuration of every level.
            hook = {**clean, "GIT_DIR": str(outer / ".git"), "GIT_INDEX_FILE": str(outer / ".git" / "index"),
                    "GIT_WORK_TREE": str(outer), "GIT_NAMESPACE": "caller", "GIT_CONFIG_GLOBAL": str(config),
                    "GIT_CONFIG_SYSTEM": str(config), "GIT_CONFIG_COUNT": "1", "GIT_CONFIG_KEY_0": "core.hooksPath",
                    "GIT_CONFIG_VALUE_0": hooks.as_posix()}
            done = subprocess.run([sys.executable, str(ISOLATION), "discover", "-s", str(inner)], env=hook,
                                  capture_output=True, text=True, timeout=120)
            self.assertFalse(marker.exists(), "an outside hook ran in a test's repository")
            self.assertEqual(state(), before)
            self.assertEqual(done.returncode, 0, done.stderr[-800:])

    def test_a_test_reaches_no_repository_it_did_not_name_or_make(self):
        """27 Oct 2026: a test read the history of the checkout it ran in and failed on hosts
        that had pruned it. Run from a checkout whose temporary directory lies inside it (a job
        directory's), a test's own directory and the one it runs in find no repository."""
        clean = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        with tempfile.TemporaryDirectory() as tmp:
            outer, inner = Path(tmp) / "outer", Path(tmp) / "inner"
            for d in (outer / "tmp", inner):
                d.mkdir(parents=True)
            for args in (["init", "-q"], ["commit", "-q", "--allow-empty", "-m", "outer"]):
                subprocess.run(["git", "-C", str(outer), "-c", "user.name=t", "-c", "user.email=t@t",
                                "-c", "core.hooksPath=", *args], env=clean, check=True, capture_output=True, timeout=60)
            (inner / "test_inner.py").write_text(INNER_FINDS_NO_REPOSITORY, encoding="utf-8")
            done = subprocess.run([sys.executable, str(ISOLATION), "discover", "-s", str(inner)], cwd=outer,
                                  env={**clean, "TMPDIR": str(outer / "tmp"), "TEMP": str(outer / "tmp"),
                                       "TMP": str(outer / "tmp")}, capture_output=True, text=True, timeout=120)
            self.assertEqual(done.returncode, 0, done.stderr[-800:])


if __name__ == "__main__":
    unittest.main()
