"""The push tier given more files never runs fewer checks (scripts/check.sh push, as a build host
runs it over the gate's file list). scripts/lib/gate-key.sh gate_covering relies on it: a pass for a
tree on a file list covers a push of that tree whose files the list holds (21 Nov 2026). A step that
ran less for more files would make such a pass cover checks that never ran; this contract fails
first.

"Fewer checks" is read on what the tier does: the steps of its summary; the crates linted; the
crates tested, each fully or with its fast tests only (fully includes fast); the crates whose random
tests run; the Python files whose names are checked. For every two file lists of the table, one
holding the other, everything the smaller list's tier does, the larger list's does too.

Run: python benches/suite/tests/isolation.py discover -s benches/suite/tests -p test_push_tier_monotone.py
An assertion tagged `SPEC <tag>` states the behaviour its test is for.
"""

from concurrent.futures import ThreadPoolExecutor
from itertools import combinations
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

import isolation  # the suite's git environment policy (inherit())

ROOT = Path(__file__).resolve().parents[3]
BASH = shutil.which("bash")
# A caller's gate, fleet or cargo variables would make the tier behave as its caller's does.
LEAKS = ("NRESE_", "FLEET_", "NEXTEST_", "CARGO_BUILD_", "CARGO_TARGET_DIR", "CARGO_ENCODED_RUSTFLAGS",
         "RUSTFLAGS", "RUST_TEST_THREADS")
# Stand-ins, each call one line in $CALLS. A workspace of two crates, beta depending on alpha;
# no cargo-nextest (the tier runs cargo test crate by crate, so each crate's tests are one call).
CARGO = """#!/bin/sh
echo "cargo $*" >> "$CALLS"
case "$1" in
  metadata) echo '{"packages":[{"name":"alpha"},{"name":"beta"}]}' ;;
  nextest) exit 1 ;;
  tree) case "$3" in alpha) printf 'alpha v0.1.0\\nbeta v0.1.0\\n' ;; beta) printf 'beta v0.1.0\\n' ;; esac ;;
esac
exit 0
"""
RECORD = '#!/bin/sh\necho "{name} $*" >> "$CALLS"\nexit 0\n'
STUBS = {
    "scripts/cargo-guarded.sh": '#!/bin/sh\necho "guarded ${NRESE_FUZZ_SEED:+seed }$*" >> "$CALLS"\nexit 0\n',
    "scripts/fetch-w3c-tests.sh": RECORD.format(name="w3c"),
    "scripts/check-locks.sh": RECORD.format(name="locks"),
    "scripts/check-features.sh": RECORD.format(name="features"),
    "scripts/check-names.sh": RECORD.format(name="names"),
    "scripts/miri.sh": RECORD.format(name="miri"),
    "scripts/check-python-names.py": 'import os, sys\nopen(os.environ["CALLS"], "a")'
                                     '.write("python-names " + " ".join(sys.argv[1:]) + "\\n")\n',
    "scripts/lib/test-targets.py": 'import sys\nprint("-p " + sys.argv[2] + " --lib")\n',
    "scripts/lib/fuzz-targets.txt": "alpha:lib:random\nbeta:lib:random\n",
    "benches/suite/tests/isolation.py": 'import os\nopen(os.environ["CALLS"], "a").write("suite\\n")\n',
    "crates/alpha/Cargo.toml": '[package]\nname = "alpha"\n',
    "crates/beta/Cargo.toml": '[package]\nname = "beta"\n',
    "apps/nrese-console/package.json": "{}\n",
}
TOOLS = {"cargo": CARGO, "npm": RECORD.format(name="npm"), "npx": RECORD.format(name="npx")}
# One changed file for each kind of selection the tier makes: a crate others depend on, the crate
# depending on it, a workspace-wide file (every crate, the lock and feature checks), the gate's own
# Python (the suite contracts, its names, the Docker names), the bench harness, the console.
UNIVERSE = ("crates/alpha/src/lib.rs", "crates/beta/src/lib.rs", "Cargo.toml", "scripts/gate.py",
            "benches/nrese-bench-harness/src/main.rs", "apps/nrese-console/src/App.tsx")
# The lists compared: none, each file alone, each two of them, all of them.
TABLE = [frozenset(c) for n in (0, 1, 2, len(UNIVERSE)) for c in combinations(UNIVERSE, n)]


def facts(stdout, calls):
    """What one run of the tier did, as a set: each step of its summary; each crate linted; each crate
    tested, fully or fast (fully includes fast); each crate whose random tests ran; each Python file
    whose names were checked."""
    done = {("step", m) for m in re.findall(r"(?m)^ +\d+  (.+) \(\d+ s\)$", stdout)}
    for line in calls:
        words = line.split()
        crates = [words[i + 1] for i, w in enumerate(words[:-1]) if w == "-p"]
        if words[0] == "python-names":
            done |= {("python names", f) for f in words[1:]}
        elif words[0] != "guarded" or "--manifest-path" in words:
            continue
        elif words[1] == "seed":
            done |= {("seeds", c) for c in crates}
        elif words[1] == "clippy":
            done |= {("clippy", c) for c in crates}
        elif words[1] == "test":
            fully = "--tests" in words
            done |= {("test", c, "fast") for c in crates} | {("test", c, "full") for c in crates if fully}
    return frozenset(done)


@unittest.skipUnless(BASH, "the push tier is a bash script")
class PushTierMonotone(unittest.TestCase):
    """A scratch repository with check.sh and its steps stood in for, where origin's
    refactor/engine-v2 is HEAD (nothing of nrese-exec changed: Miri's step, chosen by the range and
    not the list, is out of the comparison), the tier run once for each list of TABLE."""

    @classmethod
    def setUpClass(cls):
        temp = tempfile.TemporaryDirectory(prefix="nrese-push-monotone-")
        cls.addClassCleanup(temp.cleanup)
        cls.root = Path(temp.name)
        tools = cls.root / "bin"
        tools.mkdir()
        for name, text in TOOLS.items():
            (tools / name).write_text(text, newline="\n")
            (tools / name).chmod(0o755)
        gitconfig = cls.root / "gitconfig"
        gitconfig.write_text("[user]\n\tname = t\n\temail = t@t\n[core]\n\tautocrlf = false\n"
                             "[init]\n\tdefaultBranch = main\n")
        cls.env = isolation.inherit(
            *LEAKS, git={"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": gitconfig.as_posix()},
            PATH=tools.as_posix() + os.pathsep + os.environ.get("PATH", ""), NRESE_ON_BUILD_HOST="1")
        cls.repo = cls.root / "repo"
        subprocess.run(["git", "init", "-q", str(cls.repo)], check=True, capture_output=True, env=cls.env)
        (cls.repo / "scripts").mkdir()
        shutil.copy(ROOT / "scripts/check.sh", cls.repo / "scripts/check.sh")
        for rel, text in STUBS.items():
            (cls.repo / rel).parent.mkdir(parents=True, exist_ok=True)
            (cls.repo / rel).write_text(text, newline="\n")
        (cls.repo / "scripts/cargo-guarded.sh").chmod(0o755)  # check.sh runs it, as the tree's is
        for args in (["add", "-A"], ["commit", "-q", "-m", "base"],
                     ["update-ref", "refs/remotes/origin/refactor/engine-v2", "HEAD"]):
            subprocess.run(["git", "-C", str(cls.repo), *args], check=True, capture_output=True, env=cls.env)
        with ThreadPoolExecutor(max_workers=4) as pool:
            cls.runs = dict(zip(TABLE, pool.map(cls.tier, range(len(TABLE)), TABLE)))

    @classmethod
    def tier(cls, n, files):
        """The tier's run over FILES: (exit code, its facts, its output)."""
        own = cls.root / f"run-{n}"
        own.mkdir()
        (own / "files").write_text("".join(f"{f}\n" for f in sorted(files)), newline="\n")
        calls = own / "calls"
        env = {**cls.env, "CALLS": calls.as_posix(), "NRESE_FILES_FROM": (own / "files").as_posix()}
        done = subprocess.run([BASH, "scripts/check.sh", "push"], cwd=cls.repo, capture_output=True, text=True,
                              env=env, timeout=120, stdin=subprocess.DEVNULL)
        lines = calls.read_text().splitlines() if calls.exists() else []
        return done.returncode, facts(done.stdout, lines), done.stdout + done.stderr

    def test_every_list_runs_through(self):
        for files, (code, _, out) in self.runs.items():
            with self.subTest(files=sorted(files)):
                self.assertEqual(code, 0, out)

    def test_one_crates_file_runs_its_checks_and_its_dependents_fast_tests(self):
        # Worked by hand from the push tier's description (scripts/check.sh's header): alpha changed,
        # beta depends on it.
        _, done, out = self.runs[frozenset({"crates/alpha/src/lib.rs"})]
        steps = {"fmt", "clippy", "W3C data", "test", "seeds", "locks", "features"}
        expected = {("step", s) for s in steps} | {("clippy", "alpha"), ("test", "alpha", "full"),
                                                   ("test", "alpha", "fast"), ("test", "beta", "fast"),
                                                   ("seeds", "alpha")}
        self.assertEqual(done, expected, f"SPEC push-tier-selection: the facts are read from the run\n{out}")

    def test_more_files_never_mean_fewer_checks(self):
        for smaller, larger in ((s, t) for s in TABLE for t in TABLE if s < t):
            with self.subTest(smaller=sorted(smaller), larger=sorted(larger)):
                lost = self.runs[smaller][1] - self.runs[larger][1]
                self.assertEqual(lost, frozenset(),
                                 f"SPEC push-tier-monotone: checks the smaller list ran and the larger didn't\n"
                                 f"{self.runs[larger][2]}")


if __name__ == "__main__":
    unittest.main()
