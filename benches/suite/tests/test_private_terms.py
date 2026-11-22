"""The private-terms check (scripts/lib/private_terms.py, called by scripts/check.sh private_terms): with the
private bench overlay configured, a commit or push that would publish one of its host names is refused, and the
output says where, never what. Every term here is a made-up one in a fake overlay.

Run: python benches/suite/tests/isolation.py discover -s benches/suite/tests -p test_private_terms.py
"""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

import isolation

ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "scripts/lib/private_terms.py"
HOSTS = """\
[host.zzkeyzz]
hostname = "zznamezz"
ssh = "zzuserzz@zzsshzz:22"
assets = true
has = ["docker"]
roles = ["test"]
queue = { dir = "/home/zzhomezz/q", repo = 'c:\\users\\zzwinzz\\repo', chunk_min = 30 }
config = [{ since = "2026-01-01", toolchain = "rustc 1.98.1", note = "kept in /home/zznotezz/x" }]
zzfield = "zzunknownzz"
zznumberfield = 424242
zzshortfield = "ab"
zzmultifield = \"\"\"zzlineonezz
zzlinetwozz\"\"\"

[host.q]
hostname = "zzotherzz"
ssh = "zzuser6zz@[fd00::5eed]:22"
"""
# Each derived term, by the field it comes from.
DERIVED = {"the host's key": "zzkeyzz", "hostname": "zznamezz", "ssh user": "zzuserzz", "ssh host": "zzsshzz",
           "a path's home directory": "zzhomezz", "a Windows home in lower case": "zzwinzz",
           "a note's home directory": "zznotezz", "a field the check doesn't know": "zzunknownzz",
           "a number in a field the check doesn't know": "424242", "another host's hostname": "zzotherzz",
           "an ssh user beside an IPv6 host": "zzuser6zz", "an IPv6 ssh host": "fd00::5eed",
           "a line of a value over several lines": "zzlinetwozz"}
# Never printed: the terms, the list's, and the names of the fields the check doesn't know.
TERMS = (*DERIVED.values(), "zzlineonezz", "zzlisted", "zzdirzz", "zzlinezz", "zzfield", "zznumberfield",
         "zzshortfield", "zzmultifield")


class PrivateTerms(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="nrese-private-terms-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.overlay = self.root / "overlay"
        self.overlay.mkdir()
        (self.overlay / "hosts.toml").write_text(HOSTS, newline="\n")
        gitconfig = self.root / "gitconfig"
        gitconfig.write_text("[user]\n\tname = t\n\temail = t@t\n[core]\n\tautocrlf = false\n", newline="\n")
        self.env = isolation.inherit("NRESE_", git={"GIT_CONFIG_NOSYSTEM": "1",
                                                    "GIT_CONFIG_GLOBAL": gitconfig.as_posix()},
                                     NRESE_BENCH_PRIVATE=self.overlay.as_posix())
        self.repo = self.root / "repo"
        self.git("init", "-q", "-b", "main", str(self.repo), where=self.root)
        self.write("a.txt", "base\n")
        self.base = self.commit("base")

    def git(self, *args, where=None):
        return subprocess.run(["git", "-C", str(where or self.repo), *args], check=True, capture_output=True,
                              text=True, env=self.env).stdout.strip()

    def write(self, rel, text):
        (self.repo / rel).parent.mkdir(parents=True, exist_ok=True)
        (self.repo / rel).write_text(text, encoding="utf-8", newline="\n")

    def commit(self, message):
        self.git("add", "-A")
        self.git("commit", "-q", "-m", message)
        return self.git("rev-parse", "HEAD")

    def check(self, *args, env=None):
        done = subprocess.run([sys.executable, str(SCRIPT), *args], cwd=self.repo, capture_output=True,
                              text=True, env=env or self.env, timeout=120)
        for term in TERMS:
            self.assertNotIn(term, (done.stdout + done.stderr).lower(), "SPEC never-printed: no term in the output")
        return done

    def staged(self, rel, text, env=None):
        self.write(rel, text)
        self.git("add", "-A")
        return self.check("commit", env=env)

    # Spec (the master, 22 Nov 03:08 (b), (e)): no overlay configured -> nothing to check, said so, nothing read.
    def test_no_overlay_is_nothing_to_check(self):
        env = {k: v for k, v in self.env.items() if k != "NRESE_BENCH_PRIVATE"}
        done = subprocess.run([sys.executable, str(SCRIPT), "commit"], cwd=self.root, capture_output=True,
                              text=True, env=env, timeout=60)  # outside any repository: no git read made
        self.assertEqual((done.returncode, done.stdout.strip()),
                         (0, "private terms: no overlay configured (NRESE_BENCH_PRIVATE), nothing to check"))

    # Spec ((b)): an overlay configured that can't be read, or a list that can't be used -> a refusal (exit 2).
    def test_an_overlay_that_cant_be_read_is_a_refusal(self):
        cases = {
            "no such folder": lambda: shutil.rmtree(self.overlay),
            "hosts.toml unparsable": lambda: (self.overlay / "hosts.toml").write_text("[host\n"),
            "a list line that is no pattern": lambda: (self.overlay / "publish-deny.txt").write_text("zz(\n"),
            "a list line Python reads otherwise": lambda: (self.overlay / "publish-deny.txt").write_text("[[:alpha:]]\n"),
            "an equivalence class": lambda: (self.overlay / "publish-deny.txt").write_text("[[=h=]]ostname\n"),
            "an equivalence class among others": lambda: (self.overlay / "publish-deny.txt").write_text(
                "[a[=h=]]ostname\n"),
            "a collating element among others": lambda: (self.overlay / "publish-deny.txt").write_text(
                "[a[.h.]]ostname\n"),
            "a list that links to nothing": lambda: os.symlink(self.root / "gone", self.overlay / "publish-deny.txt"),
            "an identifying field that isn't text": lambda: (self.overlay / "hosts.toml").write_text(
                "[host.zzkeyzz]\nssh = 42\n"),
            "an identifying field that is a flag": lambda: (self.overlay / "hosts.toml").write_text(
                "[host.zzkeyzz]\nssh = false\n"),
        }
        for name, spoil in cases.items():
            with self.subTest(name):
                self.setUp()
                try:
                    spoil()
                except OSError as error:
                    if getattr(error, "winerror", None) != 1314:  # only Windows without the right to make a link
                        raise
                    self.skipTest(f"{name}: {error}")
                done = self.staged("b.txt", "ordinary\n")
                self.assertEqual(done.returncode, 2, done.stdout + done.stderr)
                self.assertIn("could not check", done.stdout)

    # Spec ((b)): a git read that fails -> a refusal, never clean.
    def test_a_failed_git_read_is_a_refusal(self):
        done = self.check("push", "f" * 40, "HEAD")
        self.assertEqual(done.returncode, 2, done.stdout + done.stderr)
        self.assertIn("could not check", done.stdout)

    # Spec (the master, 03:11 1.): every identifying field of the host table gives a term, a field the check
    # doesn't know included; a derived term matches as a whole word, ignoring case.
    def test_every_identifying_field_is_a_term(self):
        for field, term in DERIVED.items():
            with self.subTest(field):
                self.git("reset", "-q", "--hard", self.base)
                done = self.staged("b.txt", f"see {term.upper()} here\n")
                self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
                self.assertIn("added lines match in a file under .\n", done.stdout)

    # Spec (03:11 1.): a field whose verdict is "not identifying" gives no term, and a term inside a longer word
    # is not that term.
    def test_what_is_not_identifying_is_not_refused(self):
        done = self.staged("b.txt", "docker test rustc 1.98.1 2026-01-01 true 30 zznamezzlonger\n")
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)

    # Spec (03:11 2.): a derived term too short to match as a word is never matched alone, and never skipped
    # silently: the run counts it and names its field, not the term.
    def test_a_term_too_short_is_counted_by_field(self):
        done = self.staged("b.txt", "a q in a line\n")
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn("14 derived terms, 0 patterns from publish-deny.txt; 2 derived terms too short to match as a "
                      "word, not checked alone (fields: a field the check doesn't know, the host's key)", done.stdout)

    # Spec (03:11 3.): the overlay's optional list adds patterns, matched ignoring case and each line on its own
    # (^ and $ are a line's ends); blank lines are none. A match that reaches into a directory withholds it.
    def test_the_overlays_list_adds_patterns(self):
        (self.overlay / "publish-deny.txt").write_text("\nzzlisted[0-9]+\n\n(?<=zzdirzz)/\n^zzlinezz$\n", newline="\n")
        done = self.staged("b.txt", "ZZLISTED42\n")
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("3 patterns from publish-deny.txt", done.stdout)
        self.git("reset", "-q", "--hard", self.base)
        done = self.staged("c.txt", "before\nzzlinezz\nafter\n")
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("added lines match in a file under .\n", done.stdout)
        self.git("reset", "-q", "--hard", self.base)
        done = self.staged("zzdirzz/plan.txt", "ordinary\n")
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("path matches: a file under a directory whose name matches (not shown)", done.stdout)

    # Spec ((c), the push guard's rows of 22 Nov): the location never shows a term: a directory whose name holds
    # one is withheld; a directory named like an option, a name outside ASCII, an inherited pathspec mode and a
    # path a shell runtime would rewrite all still name and read their file.
    def test_locations_and_paths(self):
        cases = {
            "a term in a directory name": ("docs/zznamezz-notes/plan.txt", "ordinary\n", None,
                                           "path matches: a file under a directory whose name matches (not shown)"),
            "a directory named like an option": ("--zznamezz/plan.txt", "ordinary\n", None,
                                                 "path matches: a file under a directory whose name matches"),
            "a name outside ASCII": ("plan-é.txt", "zznamezz\n", None, "added lines match in a file under .\n"),
            "an inherited pathspec mode": ("b.txt", "zznamezz\n", {"GIT_LITERAL_PATHSPECS": "1"},
                                           "added lines match in a file under .\n"),
            "a path a runtime would rewrite": ("docs=/plan.txt", "zznamezz\n", None,
                                               "added lines match in a file under docs=\n"),
            "a form feed before the term": ("b.txt", "x\fzznamezz\n", None, "added lines match in a file under .\n"),
        }
        for name, (rel, text, extra, expected) in cases.items():
            with self.subTest(name):
                self.git("reset", "-q", "--hard", self.base)
                self.git("clean", "-q", "-fd")
                done = self.staged(rel, text, env={**self.env, **(extra or {})})
                self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
                self.assertIn(expected, done.stdout)
                self.assertEqual(done.stderr, "")

    # Spec (a patch's own shape): a removed line `-- x` before an added `++ <term>` is content, not a header.
    def test_a_line_that_looks_like_a_header_is_content(self):
        self.write("b.txt", "-- x\n")
        self.commit("b")
        done = self.staged("b.txt", "++ zznamezz\n")
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)

    # Spec (the page, 3.): at push, each published commit's message and lines and the branch name are read; a
    # term added and removed again is still published in history. Commits the remote's branch holds (the hook's
    # BASE) are not this push's; a local remote-tracking ref, which may be stale, excuses nothing.
    def test_a_push_reads_what_it_publishes(self):
        self.write("b.txt", "zznamezz\n")
        added = self.commit("add")
        self.write("b.txt", "ordinary\n")
        self.commit("remove")
        self.git("update-ref", "refs/remotes/origin/other", "HEAD")
        done = self.check("push", self.base, "HEAD", "refs/heads/topic")
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn(f"commit {added[:9]}: added lines match in a file under .\n", done.stdout)
        self.assertIn("; 2 commits", done.stdout)

        remote = self.git("rev-parse", "HEAD")  # now the remote's branch is here
        clean = self.commit_message("ordinary message")
        done = self.check("push", remote, "HEAD", "refs/heads/topic")
        self.assertEqual((done.returncode, "; 1 commits" in done.stdout), (0, True), done.stdout)

        flagged = self.commit_message("about zzsshzz")
        done = self.check("push", remote, "HEAD", "refs/heads/zzkeyzz-work")
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn(f"commit {flagged[:9]}: message matches", done.stdout)
        self.assertNotIn(clean[:9], done.stdout)
        self.assertIn("the branch name matches", done.stdout)

    def commit_message(self, message):
        self.write("c.txt", message + "\n")
        return self.commit(message)


if __name__ == "__main__":
    unittest.main()
