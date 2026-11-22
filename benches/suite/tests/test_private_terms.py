"""The private-terms check (scripts/lib/private_terms.py, called by scripts/check.sh private_terms): with the
private bench overlay configured, a commit or push that would publish one of its host names is refused, and the
output says where, never what. Every term here is a made-up one in a fake overlay.

Run: python benches/suite/tests/isolation.py discover -s benches/suite/tests -p test_private_terms.py
"""
import importlib.util
import io
import os
from pathlib import Path
import runpy
import shutil
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

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
TERMS = (*DERIVED.values(), "zzlineonezz", "zzlisted", "zzdirzz", "zzlinezz", "zz42zz", "zzfield", "zznumberfield",
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
        (self.overlay / "publish-deny.txt").write_text("\nzzlisted[0-9]+\n\nzzdirzz/\n^zzlinezz$\n", newline="\n")
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

    # Spec (the master, 22 Nov 03:11 3., and the push guard it would replace, which matches its list by `grep -Eiq -f`):
    # a list pattern means what an extended regular expression means to grep, constructs Python reads otherwise
    # included: a backslash in brackets is a backslash, and a POSIX class is a class.
    def test_the_list_is_read_as_grep_reads_it(self):
        cases = (("a backslash in brackets", "^[ ]*[\\t]ry:$", "    try:"),
                 ("a POSIX class", "zz[[:digit:]]+zz", "see zz42zz"),
                 ("a CR inside the line it matches", "zzlisted", "zzlisted\rzzlinezz"))
        for name, pattern, line in cases:
            with self.subTest(name):
                (self.overlay / "publish-deny.txt").write_text(pattern + "\n", newline="\n")
                self.git("reset", "-q", "--hard", self.base)
                done = self.staged("b.txt", line + "\n")
                self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
                self.assertIn("added lines match in a file under .\n", done.stdout)

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

    def branch(self, name, start, files):
        """A commit on a new branch NAME from START, writing FILES; the work tree back on main."""
        self.git("checkout", "-q", "-b", name, start)
        for rel, content in files.items():
            self.write(rel, content)
        tip = self.commit(name)
        self.git("checkout", "-q", "main")
        return tip

    def merge(self, *tips, files=None):
        """A merge of TIPS into main, with FILES written into it before it is committed (an evil merge's own)."""
        self.git("merge", "-q", "--no-ff", "--no-commit", *tips)
        for rel, content in (files or {}).items():
            self.write(rel, content)
        return self.commit("merge")

    # Spec (the master, 22 Nov 12:06): a push is refused exactly when a line or path it publishes, one the base
    # doesn't hold already, holds a term. A merge publishes what none of its parents holds; what it takes from a
    # parent is read at that parent's commits in the range, and is published already when the base holds it.
    def test_a_merge_of_the_published_line_publishes_none_of_its_lines(self):
        # The line's term in a file of its own, in a path, and in a file both sides changed (so the merge's
        # combined diff shows that line, as taken from the line).
        lines = "".join(f"{n}\n" for n in range(1, 7))
        start = self.branch("start", self.base, {"m.txt": lines})
        self.git("merge", "-q", "--ff-only", start)
        line = self.branch("line", start, {"benches/hosts.txt": "zznamezz\n", "zznamezz-dir/x.txt": "x\n",
                                           "m.txt": lines + "zznamezz\n"})
        self.write("m.txt", "0\n" + lines)
        self.commit("topic work")
        merge = self.merge(line)
        self.assertIn("zznamezz", self.git("diff-tree", "-c", "-p", merge), "the row's own premise")
        done = self.check("push", line, "HEAD", "refs/heads/topic")
        self.assertEqual(done.returncode, 0,
                         f"SPEC merge-of-the-base: the line's own lines were read as the push's\n{done.stdout}")
        self.assertIn("; 2 commits", done.stdout)
        self.git("reset", "-q", "--hard", f"{merge}^1")
        evil = self.merge(line, files={"a.txt": "base\nabout zzsshzz\n"})
        done = self.check("push", line, "HEAD", "refs/heads/topic")
        self.assertEqual(done.returncode, 1, f"SPEC merge-own-lines: a merge's own line passed\n{done.stdout}")
        self.assertIn(f"commit {evil[:9]}: added lines match in a file under .\n", done.stdout)
        self.assertNotIn("under benches", done.stdout)
        self.assertNotIn("path matches", done.stdout)

    def test_a_merges_own_binary_content_is_read(self):
        # Spec (as above): a file the merge adds is its own, a binary one too (git's combined diff can leave a
        # binary file's lines out).
        line = self.branch("line", self.base, {"l.txt": "line\n"})
        merge = self.merge(line, files={"bin.dat": "\x00\x01zznamezz\n"})
        done = self.check("push", line, "HEAD", "refs/heads/topic")
        self.assertEqual(done.returncode, 1, f"SPEC merge-own-binary: a merge's own binary line passed\n{done.stdout}")
        self.assertIn(f"commit {merge[:9]}: added lines match in a file under .\n", done.stdout)

    def test_a_merged_side_branch_nobody_has_is_read_at_its_own_commits(self):
        side = self.branch("side", self.base, {"s.txt": "zznamezz\n"})
        merge = self.merge(side)
        done = self.check("push", self.base, "HEAD", "refs/heads/topic")
        self.assertEqual(done.returncode, 1, f"SPEC merge-side: an unpublished side branch's term passed\n{done.stdout}")
        self.assertIn(f"commit {side[:9]}: added lines match in a file under .\n", done.stdout)
        self.assertNotIn(f"commit {merge[:9]}", done.stdout)

    def test_an_octopus_merge_publishes_only_its_own_lines(self):
        line = self.branch("line", self.base, {"benches/hosts.txt": "zznamezz\n"})
        one = self.branch("one", self.base, {"one.txt": "one\n"})
        two = self.branch("two", self.base, {"two.txt": "two\n"})
        octopus = self.merge(line, one, two)
        done = self.check("push", line, "HEAD", "refs/heads/topic")
        self.assertEqual(done.returncode, 0, f"SPEC merge-octopus: a parent's published line was read\n{done.stdout}")
        self.git("reset", "-q", "--hard", f"{octopus}^1")
        evil = self.merge(line, one, two, files={"two.txt": "two\nabout zzsshzz\n"})
        done = self.check("push", line, "HEAD", "refs/heads/topic")
        self.assertEqual(done.returncode, 1, f"SPEC merge-octopus: an octopus merge's own line passed\n{done.stdout}")
        self.assertIn(f"commit {evil[:9]}: added lines match in a file under .\n", done.stdout)

    def test_a_merges_own_line_is_read_whatever_blank_lines_look_like(self):
        # Spec (as above; git-config's diff.suppressBlankEmpty prints a blank context line without its space): a
        # merge's own line after a blank line, taken from one parent and new to the other, is read.
        self.git("config", "diff.suppressBlankEmpty", "true")
        start = self.branch("start", self.base, {"f.txt": "x\ny\n"})
        self.git("merge", "-q", "--ff-only", start)
        line = self.branch("line", start, {"f.txt": "x\n\ny\n"})
        merge = self.merge(line, files={"f.txt": "x\n\ny\nabout zzsshzz\n"})
        done = self.check("push", line, "HEAD", "refs/heads/topic")
        self.assertEqual(done.returncode, 1, f"SPEC merge-own-blank: a merge's own line passed\n{done.stdout}")
        self.assertIn(f"commit {merge[:9]}: added lines match in a file under .\n", done.stdout)

    def test_a_merged_path_that_isnt_utf8_is_refused(self):
        # Spec (the master, 22 Nov 03:08 (b): what can't be read is refused, exit 2): a merge's own file whose name
        # isn't UTF-8 can't be named back to git, so it is refused, never passed unread.
        def plumbing(*args, data):
            return subprocess.run(["git", "-C", str(self.repo), *args], input=data, check=True, capture_output=True,
                                  env=self.env).stdout
        line = self.branch("line", self.base, {"l.txt": "line\n"})
        self.git("merge", "-q", "--no-ff", "--no-commit", line)
        blob = plumbing("hash-object", "-w", "--stdin", data=b"about zzsshzz\n").strip()
        plumbing("update-index", "--index-info", data=b"100644 " + blob + b"\t\xff.txt\n")
        self.git("commit", "-q", "-m", "merge")
        done = self.check("push", line, "HEAD", "refs/heads/topic")
        self.assertEqual(done.returncode, 2, f"SPEC merge-path-bytes: a path git can't be given back passed\n{done.stdout}")


def module():
    """The check as a module of its own, fresh: STOP starts empty."""
    spec = importlib.util.spec_from_file_location("private_terms_under_test", SCRIPT)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


class TheCheckOwnEnds(unittest.TestCase):
    """How the check reads grep's answer, and how it ends with its pattern file (review r6, 22 Nov 2026)."""

    # Spec (GNU grep's manual, Exit Status: 0 a line selected, 1 none; -n prints NUMBER: before each selected
    # line, numbered from 1): an answer that isn't one of those -> a refusal, never "none found".
    def test_greps_answer_counts_only_as_grep_gives_it(self):
        terms = module()
        lies = {"a match with nothing printed": (0, b""), "a record not ended": (0, b"1:zz"),
                "line 0": (0, b"0:zz\n"), "a line past the input": (0, b"3:zz\n"),
                "a number past Python's int limit": (0, b"9" * 5000 + b":zz\n"), "no match yet printed": (1, b"1:zz\n")}

        def answering(code, out):
            return mock.patch.object(terms.subprocess, "run",
                                     return_value=subprocess.CompletedProcess([], code, out, b""))
        for name, (code, out) in lies.items():
            with self.subTest(name), answering(code, out):
                with self.assertRaises(terms.Unreadable, msg="SPEC grep-answer: taken as an answer"):
                    terms.grep("list", ["one", "two"])
        with answering(0, b"2:two\n"):
            self.assertEqual(terms.grep("list", ["one", "two"]), [1], "the one well-formed answer")

    # Spec (never-printed; reviews r8 and r9, 22 Nov 2026): an error the check doesn't expect -> a refusal
    # (exit 2) and no traceback, which can show a path or a line that holds a term; with a stdout that can't be
    # written to (closed) as well.
    def test_an_unexpected_error_is_a_refusal_with_no_traceback(self):
        secret = "c:/users/zzhomezz/private"
        saved = signal.getsignal(signal.SIGTERM)
        self.addCleanup(signal.signal, signal.SIGTERM, saved)
        for closed in (False, True):
            with self.subTest(closed_stdout=closed), \
                    tempfile.TemporaryDirectory(prefix="nrese-private-terms-") as overlay, \
                    mock.patch.dict(os.environ, {"NRESE_BENCH_PRIVATE": overlay}), \
                    mock.patch.object(sys, "argv", [str(SCRIPT), "commit"]), \
                    mock.patch("tomllib.loads", side_effect=RuntimeError(secret)), \
                    mock.patch("sys.stdout", new_callable=io.StringIO) as out, \
                    mock.patch("sys.stderr", new_callable=io.StringIO) as err:
                Path(overlay, "hosts.toml").write_text("", encoding="utf-8")
                said = ""
                if closed:
                    out.close()
                try:
                    runpy.run_path(str(SCRIPT), run_name="__main__")
                    ended = None
                except SystemExit as stop:
                    ended = stop.code
                except Exception as escaped:  # what would print as a traceback
                    ended = f"escaped: {escaped}"
                if not closed:
                    said = out.getvalue()
                self.assertEqual(ended, 2, "SPEC never-printed: an unexpected error escaped the check")
                self.assertNotIn(secret, said + err.getvalue(), "SPEC never-printed")

    # Spec (the master, 22 Nov 05:24: the signal path is where fail-opens sat): a stop while the pattern file
    # exists ends the check only once the file is removed, with the stop's exit (143, a refusal to check.sh).
    def test_a_stop_never_leaves_the_pattern_file(self):
        terms = module()
        held = terms.Terms(None, ["zzlisted"])
        path = Path(held.listed)
        self.addCleanup(lambda: path.unlink(missing_ok=True))
        try:
            terms.stop()
            ended = False
        except SystemExit:
            ended = True
        self.assertEqual((ended, path.exists()), (False, True), "SPEC stop: it ended with its pattern file there")
        with self.assertRaises(SystemExit) as ended:
            held.close()
        self.assertEqual((ended.exception.code, path.exists()), (143, False), "SPEC stop")

    # Spec (as above; never-printed): a pattern file that can't be removed -> a refusal that says so and names no
    # path (the temp folder's names the user), a stop waiting for it or not.
    def test_a_pattern_file_that_cant_be_removed_is_a_refusal_naming_no_path(self):
        for stopped in (False, True):
            with self.subTest(stopped=stopped):
                terms = module()
                held = terms.Terms(None, ["zzlisted"])
                path = held.listed
                self.addCleanup(lambda p=path: Path(p).unlink(missing_ok=True))
                if stopped:
                    terms.stop()
                with mock.patch.object(terms.os, "remove", side_effect=PermissionError(13, "denied", path)), \
                        mock.patch("sys.stdout", new_callable=io.StringIO) as out, \
                        self.assertRaises(SystemExit if stopped else terms.Unreadable, msg="SPEC cleanup") as refused:
                    held.close()
                said = ("" if stopped else str(refused.exception)) + out.getvalue()
                self.assertIn("removing its own pattern file", said, "SPEC cleanup: it doesn't say what failed")
                self.assertNotIn(path, said, "SPEC never-printed: the path")


if __name__ == "__main__":
    unittest.main()
