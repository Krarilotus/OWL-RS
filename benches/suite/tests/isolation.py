"""The suite's tests run outside the repository, the configuration and the hooks of their
caller's git.

A commit hook runs the suite with GIT_DIR, GIT_INDEX_FILE and the rest of git's repository-
locating variables set; they take precedence over `git -C DIR`, so a test that runs git, or a
script that does (release-gate.sh's worktrees), acted on the commit being made (20 Oct 2026: two
commits emptied and a branch moved by tests under the hook). A caller's configuration reaches a
test repository too (GIT_CONFIG_GLOBAL, GIT_CONFIG_SYSTEM, GIT_CONFIG_COUNT: a core.hooksPath ran
an outside hook in a test's own repository; Sol, 20 Oct 2026). So importing this module, once
for the whole run, for the tests and every process they start:

- removes every GIT_* variable but ALLOWED (none: a test sets what it needs itself);
- gives git a configuration of the suite's own: no system file (GIT_CONFIG_NOSYSTEM), an empty
  global file this module owns (GIT_CONFIG_GLOBAL), and through GIT_CONFIG_COUNT a test identity
  and core.hooksPath on an empty directory it owns, so no test repository runs our hooks or the
  user's. Both go when the run ends.

    pytest:    tests/conftest.py imports it
    unittest:  python benches/suite/tests/isolation.py discover -s benches/suite/tests
               (scripts/check.sh's suite step; it is `python -m unittest` with the isolation first)
"""
from __future__ import annotations

import atexit
import os
import shutil
import tempfile
import unittest
from pathlib import Path

ALLOWED: frozenset[str] = frozenset()
CONFIG = (("user.name", "suite test"), ("user.email", "suite-test@localhost"))


def isolate(environ=os.environ) -> Path:
    """The caller's GIT_* variables out, the suite's git configuration in; the directory holding
    that configuration (its global file and its empty hooks directory)."""
    for name in [k for k in environ if k.startswith("GIT_") and k not in ALLOWED]:
        del environ[name]
    own = Path(tempfile.mkdtemp(prefix="suite-git-"))
    (own / "gitconfig").write_text("", encoding="utf-8")
    (own / "hooks").mkdir()
    config = (*CONFIG, ("core.hooksPath", (own / "hooks").as_posix()))
    environ.update({"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": (own / "gitconfig").as_posix(),
                    "GIT_CONFIG_COUNT": str(len(config))})
    for i, (key, value) in enumerate(config):
        environ[f"GIT_CONFIG_KEY_{i}"], environ[f"GIT_CONFIG_VALUE_{i}"] = key, value
    return own


atexit.register(shutil.rmtree, isolate(), True)

if __name__ == "__main__":
    unittest.main(module=None)
