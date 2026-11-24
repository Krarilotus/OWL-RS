"""The gate's scripts answer for a text of the size at which Git Bash never returns from a here-string.

In Git Bash, `cmd <<< "$text"` with a text of 65,536 to about 65,600 bytes blocks for ever: the shell
writes it into a pipe before anything reads it. Below and above that window it answers; on Linux it
always does. A script that feeds such a text through a here-string hangs a commit or a push hook on a
Windows machine, and a hung hook can't be stopped without committing unhooked. These rows give each
such script a text of 65,560 bytes, as `$(...)` holds it, and a time limit:
- scripts/check-features.sh, in the commit hook: the server's dependency tree (56,768 bytes in Nov 2026);
- scripts/lib/gate-key.sh gate_covering, in pre-push: a kept file list's files beyond the push's.

Run: python benches/suite/tests/isolation.py discover -s benches/suite/tests -p test_here_string_size.py
An assertion tagged `SPEC <tag>` states the behaviour its test is for.
"""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

import isolation  # the suite's git environment policy (inherit())

READS = ("scripts/check-features.sh", "scripts/lib/gate-key.sh")

ROOT = Path(__file__).resolve().parents[3]
BASH = shutil.which("bash")
SIZE = 65560  # the text's bytes, its last newline cut as $(...) cuts it: inside the window
WIDTH = 52  # 65,561 bytes = 1,237 lines of 52 characters and a newline


def lines(count: int, prefix: str) -> list[str]:
    """COUNT distinct lines of WIDTH characters, each starting with PREFIX."""
    return [f"{prefix}{n:06d}".ljust(WIDTH, "x") for n in range(count)]


@unittest.skipUnless(BASH, "the gate's scripts are bash scripts")
class HereStringSize(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="nrese-here-string-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)

    def limited(self, script: str, *args: str, cwd: Path, env: dict) -> subprocess.CompletedProcess:
        """SCRIPT under GNU timeout, which ends its whole process group on expiry (exit 124), so a hung
        here-string leaves nothing running."""
        return subprocess.run([BASH, "-c", 'exec timeout -k 5 30 bash -c "$@"', "limited", script, "script", *args],
                              cwd=cwd, env=env, capture_output=True, text=True, timeout=60)

    # Spec (scripts/check-features.sh's header): a test-only feature of nrese-engine in the server's
    # normal dependency tree -> exit 1, naming it; none -> exit 0. Here for a tree of the window's size.
    def test_check_features_answers_for_a_tree_of_the_size(self):
        checkout = self.root / "checkout"
        (checkout / "scripts").mkdir(parents=True)
        shutil.copy(ROOT / "scripts/check-features.sh", checkout / "scripts/check-features.sh")
        tools = self.root / "bin"
        tools.mkdir()
        (tools / "cargo").write_text('#!/bin/sh\ncat "$TREE_TEXT"\n', newline="\n")
        (tools / "cargo").chmod(0o755)
        clean = lines(SIZE // (WIDTH + 1) + 1, "dependency v1.0.0 ")
        found = clean[:-1] + ['nrese-engine feature "witness"'.ljust(WIDTH, " ")]
        for name, tree, code, said in (("clean", clean, 0, "no test-only feature"),
                                       ("a test-only feature", found, 1, 'nrese-engine feature "witness"')):
            with self.subTest(tree=name):
                text = self.root / f"{name}.txt"
                text.write_text("\n".join(tree) + "\n", newline="\n")
                self.assertEqual(len(text.read_bytes()) - 1, SIZE, "the row's own premise: the tree's size")
                env = isolation.inherit("NRESE_", PATH=tools.as_posix() + os.pathsep + os.environ["PATH"],
                                        TREE_TEXT=text.as_posix())
                done = self.limited("bash scripts/check-features.sh", cwd=checkout, env=env)
                self.assertEqual(done.returncode, code,
                                 f"SPEC here-string-size: check-features didn't answer (124: it hung)\n"
                                 f"{done.stdout}{done.stderr}")
                self.assertIn(said, done.stdout + done.stderr)

    # Spec (scripts/lib/gate-key.sh gate_covering): a recorded pass of the commit whose kept file list
    # holds every file of the push's list covers the push when ACCEPTS takes its files beyond that list,
    # each one an argument. Here for files beyond of the window's size.
    def test_gate_covering_answers_for_files_beyond_of_the_size(self):
        repo = self.root / "repo"
        (repo / "scripts/lib").mkdir(parents=True)
        shutil.copy(ROOT / "scripts/lib/gate-key.sh", repo / "scripts/lib/gate-key.sh")
        env = isolation.inherit("NRESE_", git={"GIT_CONFIG_NOSYSTEM": "1",
                                               "GIT_CONFIG_GLOBAL": (self.root / "gitconfig").as_posix()})
        (self.root / "gitconfig").write_text("[user]\n\tname = t\n\temail = t@t\n[core]\n\tautocrlf = false\n",
                                             newline="\n")

        def git(*args, data=None):
            return subprocess.run(["git", "-C", str(repo), *args], input=data, env=env, check=True,
                                  capture_output=True, text=True).stdout.strip()

        git("init", "-q")
        git("add", "-A")
        git("commit", "-q", "-m", "base")
        commit, tree = git("rev-parse", "HEAD"), git("rev-parse", "HEAD^{tree}")
        pushed = ["crates/alpha/src/lib.rs"]
        beyond = lines(SIZE // (WIDTH + 1) + 1, "docs/")
        self.assertEqual(len("\n".join(beyond).encode()), SIZE, "the row's own premise: the files beyond")
        kept = git("hash-object", "-w", "--stdin", data="".join(f"{f}\n" for f in pushed + beyond))
        listed = self.root / "list"
        listed.write_text("".join(f"{f}\n" for f in pushed), newline="\n")
        common = Path(git("rev-parse", "--absolute-git-dir"))
        (common / "nrese-gate-passed").write_text(f"push {tree} {kept} IDENTITY fleet | {commit} job host\n",
                                                  newline="\n")
        accepts = self.root / "accepts"
        accepts.write_text('#!/bin/sh\necho "accepts $#" >&2\n', newline="\n")
        accepts.chmod(0o755)
        done = self.limited('. scripts/lib/gate-key.sh && gate_covering "$@"', "push", tree, listed.as_posix(),
                            "IDENTITY", commit, accepts.as_posix(), cwd=repo, env=env)
        self.assertEqual((done.returncode, done.stdout.strip()), (0, f"{commit} job host"),
                         f"SPEC here-string-size: gate_covering didn't answer (124: it hung)\n{done.stderr}")
        self.assertIn(f"accepts {len(beyond)}", done.stderr, "SPEC covering-beyond: each file beyond, one argument")


if __name__ == "__main__":
    unittest.main()
