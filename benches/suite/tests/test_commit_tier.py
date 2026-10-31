"""The commit tier (.githooks/pre-commit, scripts/check.sh commit): checks of a few seconds on this
machine, with no build, lint or test and no build host (process review 20 Oct 2026 §3A, the owner's
yes: "branches may hold unchecked commits; only checked trees land"). The compile, the lints and the
tests run on the fleet, by the tree that is pushed (test_push_gate.py). What the tier keeps still
refuses a commit: rustfmt, a stale lock file, a Docker name outside the job namespace.

Run: python benches/suite/tests/isolation.py discover -s benches/suite/tests -p test_commit_tier.py
An assertion tagged `SPEC <tag>` states the behaviour its test is for.
"""

import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
BASH = shutil.which("bash")
# Run from a git hook or inside a gate job, the caller's variables (git's own, the gate's, the slot's)
# would make the tier these tests run behave as their caller's does.
LEAKS = ("GIT_", "NRESE_", "FLEET_", "CARGO_BUILD_", "CARGO_TARGET_DIR")
CLEAN = {k: v for k, v in os.environ.items() if not k.startswith(LEAKS)}
# The tier's own scripts, copied into each scratch repository.
COPIED = (".githooks/pre-commit", "scripts/check.sh", "scripts/check-locks.sh", "scripts/check-names.sh")
# Stand-ins, each call one line in $FAKE/calls. cargo: fmt fails for a manifest listed in
# $FAKE/unformatted (`workspace` for the root's), metadata finds a stale lock while $FAKE/stale-lock
# exists; anything else succeeds, as an installed cargo-nextest answers `nextest --version`.
CARGO = """#!/bin/sh
echo "cargo $*" >> "$FAKE/calls"
case "$1" in
  fmt)
    manifest=workspace prev=
    for arg in "$@"; do [ "$prev" = --manifest-path ] && manifest=$arg; prev=$arg; done
    ! grep -qxF "$manifest" "$FAKE/unformatted" 2> /dev/null ;;
  metadata)
    [ ! -e "$FAKE/stale-lock" ] || { echo "error: the lock file needs to be updated" >&2; exit 101; } ;;
esac
"""
# The build admission (scripts/cargo-guarded.sh), the build host (scripts/lib/remote.sh) and ssh:
# called at all, they fail, so the commit is refused.
STUBS = {
    "scripts/cargo-guarded.sh": '#!/bin/sh\necho "guarded $*" >> "$FAKE/calls"\nexit 1\n',
    "scripts/lib/remote.sh": '#!/bin/sh\necho "remote $*" >> "$FAKE/calls"\n[ "$1" = tree ] && echo t\nexit 2\n',
}
SSH = '#!/bin/sh\necho "ssh $*" >> "$FAKE/calls"\nexit 255\n'
WORKSPACE = '[workspace]\nmembers = ["crates/foo"]\nresolver = "2"\n'
CRATE = '[package]\nname = "foo"\nversion = "0.1.0"\nedition = "2021"\n'
HARNESS = '[package]\nname = "nrese-bench-harness"\nversion = "0.1.0"\nedition = "2021"\n\n[workspace]\n'


class CommitTier(unittest.TestCase):
    """A scratch repository with one crate and the bench harness, configured as the main checkout is
    (`nrese.buildHost fleet`), the tier's scripts copied in, cargo, the build admission, the build
    host and ssh stood in for, and none of the commit slot's environment."""

    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="nrese-commit-tier-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.fake = self.root / "fake"
        (self.fake / "bin").mkdir(parents=True)
        for name, text in (("cargo", CARGO), ("ssh", SSH)):
            (self.fake / "bin" / name).write_text(text, newline="\n")
            (self.fake / "bin" / name).chmod(0o755)
        gitconfig = self.root / "gitconfig"
        gitconfig.write_text("[user]\n\tname = t\n\temail = t@t\n[core]\n\tautocrlf = false\n"
                             "[init]\n\tdefaultBranch = main\n")
        self.env = {**CLEAN, "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": gitconfig.as_posix(),
                    "PATH": (self.fake / "bin").as_posix() + os.pathsep + CLEAN.get("PATH", ""),
                    "FAKE": self.fake.as_posix()}
        self.repo = self.root / "repo"
        self.git(self.root, "init", "-q", str(self.repo))
        for rel in COPIED:
            (self.repo / rel).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(ROOT / rel, self.repo / rel)
        for rel, text in {**STUBS, "Cargo.toml": WORKSPACE, "crates/foo/Cargo.toml": CRATE,
                          "crates/foo/src/lib.rs": "pub fn f() {}\n",
                          "benches/nrese-bench-harness/Cargo.toml": HARNESS,
                          "benches/nrese-bench-harness/src/lib.rs": "pub fn g() {}\n"}.items():
            self.write(rel, text)
        self.git(self.repo, "add", "-A")
        self.git(self.repo, "commit", "-q", "-m", "base")
        self.git(self.repo, "config", "nrese.buildHost", "fleet")

    def git(self, where, *args):
        return subprocess.run(["git", "-C", str(where), *args], check=True, capture_output=True, text=True,
                              env=self.env).stdout.strip()

    def write(self, rel, text):
        (self.repo / rel).parent.mkdir(parents=True, exist_ok=True)
        (self.repo / rel).write_text(text, newline="\n")

    def hook(self):
        """The pre-commit hook as git runs it for a commit of the changes, staged (a new file is in
        the index, so the checks that read what git tracks see it)."""
        self.git(self.repo, "add", "-A")
        return subprocess.run([BASH, ".githooks/pre-commit"], cwd=self.repo, capture_output=True, text=True,
                              env=self.env, timeout=120, stdin=subprocess.DEVNULL)

    def calls(self):
        path = self.fake / "calls"
        return path.read_text().splitlines() if path.exists() else []

    def test_a_commit_builds_nothing_here_or_elsewhere(self):
        # A change to a crate's code: the old tier linted and compiled it and its dependents, ran its
        # tests, or sent all that to the build host.
        self.write("crates/foo/src/lib.rs", "pub fn f() -> u8 {\n    1\n}\n")
        self.write("crates/foo/tests/t.rs", "#[test]\nfn t() {}\n")
        done = self.hook()
        calls = self.calls()
        self.assertEqual(done.returncode, 0, f"SPEC commit-tier-here: the commit is accepted, checked here "
                                             f"(calls {calls}):\n{done.stdout}{done.stderr}")
        self.assertEqual([c for c in calls if not c.startswith("cargo ")], [],
                         "SPEC commit-tier-no-build: no build admission, build host or ssh")
        self.assertEqual({c.split()[1] for c in calls} - {"fmt", "metadata"}, set(),
                         "SPEC commit-tier-no-build: cargo only formats and reads metadata: no build, "
                         "lint, test or dependency walk")

    def test_its_checks_still_refuse_a_commit(self):
        cases = {
            "fmt": lambda: (self.write("crates/foo/src/lib.rs", "pub fn f(){}\n"),
                            (self.fake / "unformatted").write_text("workspace\n")),
            "fmt (bench harness)": lambda: (self.write("benches/nrese-bench-harness/src/lib.rs", "pub fn g(){}\n"),
                                            (self.fake / "unformatted").write_text(
                                                "benches/nrese-bench-harness/Cargo.toml\n")),
            "locks": lambda: (self.write("crates/foo/Cargo.toml", CRATE + '\n[dependencies]\nserde = "1"\n'),
                              (self.fake / "stale-lock").write_text("")),
            # A hard-coded container name, which every job on a host would share (scripts/check-names.sh).
            "names": lambda: self.write("scripts/run.sh", "#!/bin/sh\ndocker run --name nrese-target-x img\n"),
        }
        for step, change in cases.items():
            with self.subTest(step):
                self.git(self.repo, "reset", "-q", "--hard")
                self.git(self.repo, "clean", "-q", "-fd")
                for name in ("unformatted", "stale-lock", "calls"):
                    (self.fake / name).unlink(missing_ok=True)
                change()
                done = self.hook()
                self.assertNotEqual(done.returncode, 0, f"SPEC commit-tier-keeps: {step} refuses the commit:\n"
                                                        f"{done.stdout}{done.stderr}")
                self.assertRegex(done.stdout, rf"(?m)^=== {re.escape(step)}: exit [1-9]",
                                 f"SPEC commit-tier-keeps: the refusal is {step}'s")


if __name__ == "__main__":
    unittest.main()
