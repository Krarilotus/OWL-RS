"""The suite's tests run outside the repository their caller's git names.

A commit hook runs the suite with GIT_DIR, GIT_INDEX_FILE and the rest of git's
repository-locating variables set; they take precedence over `git -C DIR` and a working
directory, so a test that runs git, or a script that does (release-gate.sh's worktrees), then
acts on the commit being made instead of its own repository (20 Oct 2026: two commits emptied
and a branch moved by tests under the hook). Importing this module removes them from the
environment of the tests and every process they start, once, for the whole run:

    pytest:    tests/conftest.py imports it
    unittest:  python benches/suite/tests/isolation.py discover -s benches/suite/tests
               (scripts/check.sh's suite step; it is `python -m unittest` with the scrub first)
"""
from __future__ import annotations

import os
import subprocess
import unittest

# git rev-parse --local-env-vars (git 2.42): the variables git clears itself before it works in
# another repository (a submodule). The installed git's list is added where it names more.
REPOSITORY_VARS = ("GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_CONFIG", "GIT_CONFIG_PARAMETERS", "GIT_CONFIG_COUNT",
                   "GIT_OBJECT_DIRECTORY", "GIT_DIR", "GIT_WORK_TREE", "GIT_IMPLICIT_WORK_TREE", "GIT_GRAFT_FILE",
                   "GIT_INDEX_FILE", "GIT_NO_REPLACE_OBJECTS", "GIT_REPLACE_REF_BASE", "GIT_PREFIX",
                   "GIT_SHALLOW_FILE", "GIT_COMMON_DIR")


def repository_vars() -> set[str]:
    try:
        listed = subprocess.run(["git", "rev-parse", "--local-env-vars"], capture_output=True, text=True,
                                timeout=30).stdout.split()
    except (OSError, subprocess.SubprocessError):
        listed = []
    return set(REPOSITORY_VARS) | set(listed)


def scrub(environ=os.environ) -> list[str]:
    """Removes the repository-locating variables; the names it removed."""
    removed = sorted(name for name in repository_vars() if name in environ)
    for name in removed:
        del environ[name]
    return removed


scrub()

if __name__ == "__main__":
    unittest.main(module=None)
