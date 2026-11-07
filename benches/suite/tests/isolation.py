"""The suite's tests run outside the repository, the configuration and the hooks of their
caller's git.

A commit hook runs the suite with GIT_DIR, GIT_INDEX_FILE and the rest of git's repository-
locating variables set; they take precedence over `git -C DIR`, so a test that runs git, or a
script that does (release-gate.sh's worktrees), acted on the commit being made (20 Oct 2026: two
commits emptied and a branch moved by tests under the hook). A caller's configuration reaches a
test repository too (GIT_CONFIG_GLOBAL, GIT_CONFIG_SYSTEM, GIT_CONFIG_COUNT: a core.hooksPath ran
an outside hook in a test's own repository; Sol, 20 Oct 2026). And a test that read this
repository's history passed where the history was kept and failed where it wasn't (27 Oct 2026:
commit ids from before a rewrite, on fleet hosts that had pruned them). So importing this module,
once for the whole run, for the tests and every process they start:

- removes every GIT_* variable but ALLOWED (none: a test sets what it needs itself), and the
  scope settings a calling suite run passes its workers (SUITE_SCOPE_*: runtime.scope_argv
  reads them by default, so a caller's SUITE_SCOPE_STOP_S=bad failed every scoped runtime test);
- gives git a configuration of the suite's own: no system file (GIT_CONFIG_NOSYSTEM), an empty
  global file this module owns (GIT_CONFIG_GLOBAL), and through GIT_CONFIG_COUNT a test identity
  and core.hooksPath on an empty directory it owns, so no test repository runs our hooks or the
  user's. Both go when the run ends;
- stops git's search for a repository at the temporary directory and at this repository
  (GIT_CEILING_DIRECTORIES): git started in a test's temporary directory, or below the
  repository's top, finds none above it.

The ceilings are a guard on git's discovery, not a sandbox: a test that names this repository
(`git -C ROOT`, `cwd=ROOT`) still reaches it, and git started at its top finds it. What keeps a
test out is that it names its own: it runs in a scratch directory of the module's own (SCRATCH),
not in the checkout the run started in; it makes the commits it needs in a repository of its
own (repo()), and copies the scripts it runs into a checkout of its own (checkout()). A process
a test starts takes its environment from inherit() or environment(), the suite's one git policy
(GIT) included.

    pytest:    tests/conftest.py imports it
    unittest:  python benches/suite/tests/isolation.py discover -s benches/suite/tests
               (scripts/check.sh's suite step; it is `python -m unittest` with the isolation first)
"""
from __future__ import annotations

import atexit
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ALLOWED: frozenset[str] = frozenset()
CONFIG = (("user.name", "suite test"), ("user.email", "suite-test@localhost"))
ROOT = Path(__file__).resolve().parents[3]
START = Path.cwd()


def isolate(environ=os.environ) -> Path:
    """The caller's GIT_* and SUITE_SCOPE_* variables out, the suite's git configuration and ceilings in; the
    directory holding that configuration (its global file and its empty hooks directory) and
    the scratch directory the tests run in."""
    for name in [k for k in environ if k.startswith(("GIT_", "SUITE_SCOPE_")) and k not in ALLOWED]:
        del environ[name]
    own = Path(tempfile.mkdtemp(prefix="suite-git-"))
    (own / "gitconfig").write_text("", encoding="utf-8")
    (own / "hooks").mkdir()
    (own / "cwd").mkdir()
    config = (*CONFIG, ("core.hooksPath", (own / "hooks").as_posix()))
    ceilings = (Path(os.path.realpath(tempfile.gettempdir())), ROOT)
    environ.update({"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": (own / "gitconfig").as_posix(),
                    "GIT_CONFIG_COUNT": str(len(config)),
                    "GIT_CEILING_DIRECTORIES": os.pathsep.join(c.as_posix() for c in ceilings)})
    for i, (key, value) in enumerate(config):
        environ[f"GIT_CONFIG_KEY_{i}"], environ[f"GIT_CONFIG_VALUE_{i}"] = key, value
    return own


LAUNCH = ("PATH", "PATHEXT", "SYSTEMROOT", "SYSTEMDRIVE", "WINDIR", "COMSPEC", "HOME", "USERPROFILE", "TEMP", "TMP",
          "TMPDIR", "LANG", "LC_ALL")


def inherit(*drop: str, git: dict[str, str] | None = None, **values: str) -> dict[str, str]:
    """A process environment from the run's own (os.environ, isolated on import): without any GIT_*
    a test has set in it since, without the variables whose names start with `drop` (NRESE_,
    FLEET_, CARGO_: a caller's gate, job or build settings), with the suite's git policy (GIT), and
    `values` last. A scenario with git configuration of its own (its global file, hooks it runs)
    passes `git`, which replaces the policy whole: GIT_CONFIG_COUNT outranks every configuration
    file, so the suite's core.hooksPath and identity would win over its own; `git={}` is a
    caller's environment with no git settings at all. HOME and the rest stay the caller's."""
    base = {k: v for k, v in os.environ.items() if not k.startswith(("GIT_", *drop))}
    return {**base, **(GIT if git is None else git), **values}


def environment(**values: str) -> dict[str, str]:
    """A process environment of the test's own: what the OS needs to launch a program (LAUNCH),
    the suite's git policy (GIT), and `values`; nothing else of the caller's (an inherited
    NRESE_DOCKER took a cleanup test past its fake Docker). LAUNCH keeps the caller's HOME and
    USERPROFILE: a program reading its home's files reads the caller's, unless `values` names
    another."""
    return {**{k: v for k, v in os.environ.items() if k in LAUNCH}, **GIT, **values}


def checkout(dest: Path, *paths: str) -> Path:
    """A private checkout at `dest` holding copies of this repository's `paths`: a script that
    finds its repository from its own location acts on this one, not on the checkout the run
    started in (bench-cleanup.sh removes files there)."""
    for rel in paths:
        (dest / rel).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / rel, dest / rel)
    return dest


def repo(path: Path, commits: int) -> list[str]:
    """A repository of the test's own at `path` with `commits` commits in a line, the first holding
    the files already at `path` (a checkout() of this one's scripts), the others empty: their full
    ids, oldest first. A test that needs commits makes them here, never reads this repository's
    history (a fleet checkout may be shallow or pruned)."""
    def git(*args: str) -> str:
        return subprocess.run(["git", "-C", str(path), *args], capture_output=True, text=True, check=True,
                              timeout=60).stdout.strip()
    path.mkdir(parents=True, exist_ok=True)
    git("init", "-q")
    git("add", "-A")
    for n in range(commits):
        git("commit", "-q", "--allow-empty", "-m", f"commit {n + 1}")
    return git("rev-list", "--reverse", "HEAD").split()


if __name__ == "__main__":
    sys.modules["isolation"] = sys.modules["__main__"]  # one isolation, however a test imports it
OWN = isolate()
GIT = {k: v for k, v in os.environ.items() if k.startswith("GIT_")}  # the policy isolate() set, all of it
SCRATCH = OWN / "cwd"


@atexit.register
def _remove() -> None:
    os.chdir(START)
    shutil.rmtree(OWN, True)


class Program(unittest.TestProgram):
    """unittest's main: discovery reads its paths where the run started, the tests run in SCRATCH."""

    def runTests(self):
        os.chdir(SCRATCH)
        super().runTests()


if __name__ == "__main__":
    Program(module=None)
