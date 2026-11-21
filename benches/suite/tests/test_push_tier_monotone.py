"""Files that reach no crate only add steps to the push tier (scripts/check.sh push, as a build host
runs it over the gate's file list; check.sh reaches_no_crate). scripts/lib/gate-key.sh gate_covering
relies on it: a pass of a commit covers a narrower push of that commit when the pass's other files
reach no crate (21 Nov 2026). Were a run over more crates the same check of each, more would do;
it isn't: cargo resolves features across the crates one run selects (review r1 of 21 Nov 2026).

So the contract is exact where it must be: for two file lists, one holding the other with only
files that reach no crate besides, the tier's workspace cargo runs are the same runs, in the same
order with the same arguments, and every other check the smaller list's tier makes (a step of its
summary, a Python file whose names are checked), the larger list's makes too. A step that ran less
for more such files, or a workspace run that changed with them, fails here before a pass can cover
checks that never ran.

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
# The build admission fails `check` of the crate ALONE_FAILS names: one that doesn't compile alone.
STUBS = {
    "scripts/cargo-guarded.sh": '#!/bin/sh\necho "guarded ${NRESE_FUZZ_SEED:+seed=$NRESE_FUZZ_SEED }$*" >> "$CALLS"\n'
                                '[ "$1 ${4:-}" != "check ${ALONE_FAILS:-}" ] || exit 101\nexit 0\n',
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
# git on a host whose repository holds another object sharing the commit's first six hex digits:
# `rev-parse --short=6 HEAD` gives seven; anything else, git itself (REAL_GIT, filled in by the test).
AMBIGUOUS_GIT = """#!/bin/sh
if [ "$*" = "rev-parse --short=6 HEAD" ]; then
  "REAL_GIT" rev-parse HEAD | cut -c1-7
  exit
fi
exec "REAL_GIT" "$@"
"""
# The crate part of a list, fixed within a comparison: none, a crate others depend on, the crate
# depending on it, a workspace-wide file (every crate).
CRATE_PARTS = (frozenset(), frozenset({"crates/alpha/src/lib.rs"}), frozenset({"crates/beta/src/lib.rs"}),
               frozenset({"Cargo.toml"}))
# Files that reach no crate, one for each kind of step they select: the gate's own Python (the suite
# contracts, its names, the Docker names), the bench harness and its lock file (the lock and feature
# checks), the console, a document (nothing).
SIDE = ("scripts/gate.py", "benches/nrese-bench-harness/Cargo.lock",
        "benches/nrese-bench-harness/src/main.rs", "apps/nrese-console/src/App.tsx", "docs/notes.md")
# Every crate part with every set of those files.
TABLE = [part | frozenset(side) for part in CRATE_PARTS for n in range(len(SIDE) + 1)
         for side in combinations(SIDE, n)]


def workspace_runs(calls):
    """The tier's workspace cargo runs, in order, as called: cargo itself (but a separate project's,
    named by --manifest-path) and the build admission (scripts/cargo-guarded.sh), which runs every
    build, lint and test."""
    return [line for line in calls
            if line.split()[0] in ("cargo", "guarded") and "--manifest-path" not in line.split()]


def side_checks(stdout, calls):
    """The tier's other checks, as a set: each step of its summary, each Python file whose names are
    checked."""
    done = {("step", m) for m in re.findall(r"(?m)^ +\d+  (.+) \(\d+ s\)$", stdout)}
    for line in calls:
        words = line.split()
        if words[0] == "python-names":
            done |= {("python names", f) for f in words[1:]}
    return frozenset(done)


def facts(stdout, calls):
    """What one run of the tier did, as a set: each step of its summary; each crate linted; each crate
    tested, fully or fast (fully includes fast); each crate whose random tests ran; each crate compiled
    alone; each Python file whose names were checked."""
    done = {("step", m) for m in re.findall(r"(?m)^ +\d+  (.+) \(\d+ s\)$", stdout)}
    for line in calls:
        words = line.split()
        crates = [words[i + 1] for i, w in enumerate(words[:-1]) if w == "-p"]
        if words[0] == "python-names":
            done |= {("python names", f) for f in words[1:]}
        elif words[0] != "guarded" or "--manifest-path" in words:
            continue
        elif words[1].startswith("seed="):
            done |= {("seeds", c) for c in crates}
        elif words[1] == "clippy":
            done |= {("clippy", c) for c in crates}
        elif words[1] == "test":
            fully = "--tests" in words
            done |= {("test", c, "fast") for c in crates} | {("test", c, "full") for c in crates if fully}
        elif words[1] == "check":
            done |= {("alone", c) for c in crates}
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
    def tier(cls, n, files, first=None, alone_fails=""):
        """The tier's run over FILES, with FIRST's tools first on PATH and ALONE_FAILS's crate failing to
        compile alone: (exit code, its standard output, its calls, all its output)."""
        own = cls.root / f"run-{n}"
        own.mkdir()
        (own / "files").write_text("".join(f"{f}\n" for f in sorted(files)), newline="\n")
        calls = own / "calls"
        env = {**cls.env, "CALLS": calls.as_posix(), "NRESE_FILES_FROM": (own / "files").as_posix(),
               "ALONE_FAILS": alone_fails}
        if first:
            env["PATH"] = first.as_posix() + os.pathsep + env["PATH"]
        done = subprocess.run([BASH, "scripts/check.sh", "push"], cwd=cls.repo, capture_output=True, text=True,
                              env=env, timeout=120, stdin=subprocess.DEVNULL)
        lines = calls.read_text().splitlines() if calls.exists() else []
        return done.returncode, done.stdout, lines, done.stdout + done.stderr

    def test_every_list_runs_through(self):
        for files, (code, _, _, out) in self.runs.items():
            with self.subTest(files=sorted(files)):
                self.assertEqual(code, 0, out)

    def test_one_crates_file_runs_its_checks_and_its_dependents_fast_tests(self):
        # Worked by hand from the push tier's description (scripts/check.sh's header): alpha changed,
        # beta depends on it. The fixture's reading of a run, checked once.
        _, stdout, calls, out = self.runs[frozenset({"crates/alpha/src/lib.rs"})]
        steps = {"fmt", "clippy", "W3C data", "test", "seeds", "locks", "features", "crates alone"}
        expected = {("step", s) for s in steps} | {("clippy", "alpha"), ("test", "alpha", "full"),
                                                   ("test", "alpha", "fast"), ("test", "beta", "fast"),
                                                   ("seeds", "alpha"), ("alone", "alpha"), ("alone", "beta")}
        self.assertEqual(facts(stdout, calls), expected, f"SPEC push-tier-selection: the run as read\n{out}")

    def test_a_crate_that_fails_to_compile_alone_fails_the_tier(self):
        """Its tests run in one nextest run with the other crates', whose features it gets there: checked
        alone, with its own features, it fails, and the tier with it. Here alpha changed and beta,
        which depends on it, doesn't compile alone."""
        code, stdout, _, out = self.tier(len(TABLE) + 1, {"crates/alpha/src/lib.rs"}, alone_fails="beta")
        self.assertEqual(code, 1, f"SPEC crates-alone: a dependent that doesn't compile alone passed the tier\n{out}")
        self.assertRegex(stdout, r"(?m)^ +101  crates alone \(\d+ s\)$", out)

    def test_the_seeds_come_from_the_full_commit_id(self):
        """Review r2 (21 Nov 2026): the random tests' seeds are drawn from the commit, its id's first six
        hex digits, the same on every host. A host whose repository holds another object sharing those
        six digits abbreviates the id longer (`git rev-parse --short=6` gives seven); the seeds don't
        change with it. Here git stands in for such a host: it answers that abbreviation with seven
        digits and runs everything else as git does."""
        ambiguous = self.root / "ambiguous"
        ambiguous.mkdir(exist_ok=True)
        (ambiguous / "git").write_text(AMBIGUOUS_GIT.replace("REAL_GIT", shutil.which("git").replace("\\", "/")),
                                       newline="\n")
        (ambiguous / "git").chmod(0o755)
        head = subprocess.run(["git", "-C", str(self.repo), "rev-parse", "HEAD"], capture_output=True, text=True,
                              env=self.env, check=True).stdout.strip()
        base = int(head[:6], 16) % 100000 + 1000
        code, _, calls, out = self.tier(len(TABLE), {"crates/alpha/src/lib.rs"}, first=ambiguous)
        self.assertEqual(code, 0, out)
        seeds = [w.split("=", 1)[1] for line in calls for w in line.split()[1:2] if w.startswith("seed=")]
        self.assertEqual(seeds, [str(base), str(base + 1), str(base + 2)],
                         f"SPEC seeds-from-the-commit: seeds not drawn from the id's first six digits\n{out}")

    def test_files_reaching_no_crate_only_add_checks(self):
        for smaller, larger in ((s, t) for s in TABLE for t in TABLE if s < t):
            if larger - smaller <= frozenset(SIDE):  # the same crate part, more files reaching none
                with self.subTest(smaller=sorted(smaller), larger=sorted(larger)):
                    _, small_out, small_calls, _ = self.runs[smaller]
                    _, large_out, large_calls, out = self.runs[larger]
                    self.assertEqual(workspace_runs(large_calls), workspace_runs(small_calls),
                                     f"SPEC covering-same-runs: files reaching no crate changed the workspace "
                                     f"runs\n{out}")
                    lost = side_checks(small_out, small_calls) - side_checks(large_out, large_calls)
                    self.assertEqual(lost, frozenset(),
                                     f"SPEC covering-only-adds: checks the smaller list made and the larger "
                                     f"didn't\n{out}")


if __name__ == "__main__":
    unittest.main()
