"""The tests hand git no fixed revision id: they take revisions from a repository of their own
(isolation.repo) or by name (HEAD), never as a literal id. T0, 28 Oct 2026: test_abba's dcb3b0e2
and 0251a486, ids from before the 26 Oct rewrite, failed every push gate on the hosts whose job
repositories no longer held them.

The check reads the tests' code and never asks git: an id is refused whether or not its commit
is in the clone the check runs in. F-16 (tests audit, 28 Oct 2026): the check before this one
refused only the ids this clone held as commits, so the same stale literal passed where a host
had pruned it, the incident's own environment.

What it reads as git's operands, each with what follows a revision (^{commit}, ~N, ^N, @{...},
:path, A..B, A...B, ^A, --option=value) and the constant parts of an f-string:
- the elements after "git" of a list or tuple written out, or after *NAME where NAME is such a
  list; the word after -C or -c is a directory or a setting, not an operand;
- the arguments of a call to a function or method named git;
- for an operand that is a NAME, every string the module binds NAME to (NAME = "...", a tuple
  unpacked, for NAME in ("...", ...)): the incident's form was
  `abba.git("rev-parse", rev) for rev in ("dcb3b0e2", "0251a486")`;
- in any other string of code (not a docstring), each `git SUBCOMMAND ...` up to the end of its
  shell command (a newline, ;, &, |, `, or a closing parenthesis).
A literal of 7 to 40 lower-case hex digits there is refused. A hex string used as data (a
record's revision, "a" * 40) is no operand. What it can't see: an id that reaches git through a
parameter, an attribute, another module, a file or a TOML family (abba's sides: a family names
them and load_family resolves them), a $variable of a shell script, or Python source inside a
string.

    python -m unittest discover -s benches/suite/tests -p test_fixed_revisions.py
"""
from __future__ import annotations

import ast
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

import isolation

HERE = Path(__file__).resolve().parent
HEX = re.compile(r"[0-9a-f]{7,40}")
SHELL = re.compile(r"(?<![\w./-])git\s+((?:-[Cc]\s+\S+\s+)*[a-z][a-z-]*\b[^\n;&|`)]*)")
VALUED = ("-C", "-c")  # git's options whose next word is no operand


def ids(operand: str) -> list[str]:
    """The fixed ids an operand names."""
    if operand.startswith("-"):
        operand = operand.partition("=")[2]
    out = []
    for side in re.split(r"\.{2,3}", operand.split(":", 1)[0]):
        rev = re.split(r"[\^~@{]", side.lstrip("^"), maxsplit=1)[0]
        if HEX.fullmatch(rev):
            out.append(rev)
    return out


def texts(node: ast.AST) -> list[str]:
    """A string's text, or an f-string's constant parts."""
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return [node.value]
    if isinstance(node, ast.JoinedStr):
        return [v.value for v in node.values if isinstance(v, ast.Constant)]
    return []


def operands(words: list) -> list[ast.AST]:
    """The words handed to git that are operands: each after -C or -c left out."""
    out, skip = [], False
    for word in words:
        if not skip:
            out.append(word)
        skip = not skip and isinstance(word, ast.Constant) and word.value in VALUED
    return out


def fixed_ids(source: str) -> list[tuple[int, str]]:
    """Every fixed id `source`'s code hands git, with its line."""
    tree = ast.parse(source)

    def is_git(node: ast.AST, names: set[str]) -> bool:
        first = node.elts[0] if isinstance(node, (ast.List, ast.Tuple)) and node.elts else None
        return (isinstance(first, ast.Constant) and first.value == "git") or (
            isinstance(first, ast.Starred) and isinstance(first.value, ast.Name) and first.value.id in names)

    lists = {t.id for node in ast.walk(tree) if isinstance(node, ast.Assign) and is_git(node.value, set())
             for t in node.targets if isinstance(t, ast.Name)}
    # The strings a name is bound to anywhere in the module: NAME = "...", A, B = "...", "...", and
    # for NAME in ("...", ...) (the incident's own form: abba.git("rev-parse", rev) for rev in (...)).
    bound: dict[str, list[str]] = {}
    for node in ast.walk(tree):
        pairs = []
        if isinstance(node, ast.Assign):
            for t in node.targets:
                both = isinstance(t, (ast.Tuple, ast.List)) and isinstance(node.value, (ast.Tuple, ast.List))
                pairs += zip(t.elts, node.value.elts) if both else [(t, node.value)]
        elif isinstance(node, (ast.For, ast.comprehension)) and isinstance(node.iter, (ast.Tuple, ast.List)):
            pairs += [(node.target, v) for v in node.iter.elts]
        for target, value in pairs:
            if isinstance(target, ast.Name):
                bound.setdefault(target.id, []).extend(texts(value))
    docs = {id(node.body[0].value) for node in ast.walk(tree)
            if isinstance(node, (ast.Module, ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef))
            and node.body and isinstance(node.body[0], ast.Expr) and isinstance(node.body[0].value, ast.Constant)}
    found = []
    for node in ast.walk(tree):
        words = []
        if isinstance(node, (ast.List, ast.Tuple)) and is_git(node, lists):
            words = node.elts[1:]
        elif isinstance(node, ast.Call) and getattr(node.func, "id", getattr(node.func, "attr", None)) == "git":
            words = node.args
        found += [(word.lineno, rev) for word in operands(words)
                  for text in texts(word) + (bound.get(word.id, []) if isinstance(word, ast.Name) else [])
                  for rev in ids(text)]
        if isinstance(node, ast.Constant) and isinstance(node.value, str) and id(node) not in docs:
            for command in SHELL.finditer(node.value):
                tokens = command.group(1).split()
                found += [(node.lineno, rev) for i, token in enumerate(tokens)
                          if not (i and tokens[i - 1] in VALUED) for rev in ids(token.strip("\"'"))]
    return sorted(set(found))


# Ids to refuse, each a different case of "is its commit here?": one of this repository's own
# (engine-v2's merge of PR #60), one no repository holds, and one of a test's own repository.
PRESENT = "14d7b6ff"
ABSENT = "0123456789abcdef0123456789abcdef01234567"
# Each hands git REV one way the check reads.
HANDED = ('subprocess.run(["git", "-C", d, "rev-parse", "--verify", "REV^{commit}"])',
          'run(("git", "-c", "user.name=t", "log", "REV~1..HEAD"))',
          'git = ["git", "-C", d]\nsubprocess.run([*git, "show", "REV:a.txt"])',
          'self.git(repo, "merge-base", "--is-ancestor", "REV", "HEAD")',
          'abba.git(repo, "rev-parse", f"{base}...REV")',
          'git("tag", "--points-at=REV")',
          'subprocess.run(["git", "cat-file", "-e", f"REV^{{tree}}"])',
          'a, = (abba.git("rev-parse", rev) for rev in ("REV",))',
          'BASE = "REV"\ngit("diff", BASE)',
          'SCRIPT = """\nset -e\ncd "$1" && git -C "$d" rev-parse --short=8 REV^ > out\n"""')
# Hex strings that aren't operands, and operands that aren't ids: none is refused.
NOT_HANDED = ('A, B = "a" * 40, "b" * 40\nREVS = {"a": A, "b": "bbbbbbbb"}',
              'family = {"sides": {"a": "REV", "b": "0251a486"}}',
              'BASE = "REV"\nrecord = {"base": BASE}\ngit("diff", "HEAD")',
              'subprocess.run(["git", "-C", "REV", "rev-parse", "HEAD^", "--short=8"])',
              'git("commit", "-m", f"commit {n}")',
              'SCRIPT = "echo bin-REV-identity > $(git rev-parse --short=8 HEAD)"',
              '"""git rev-parse REV: a docstring citing history"""',
              'subprocess.run(["git", "-c", "core.abbrev=REV", "status"])')


class FixedRevisions(unittest.TestCase):
    def test_an_id_handed_to_git_is_refused_whether_or_not_its_commit_exists(self):
        with tempfile.TemporaryDirectory() as tmp:
            [own] = isolation.repo(Path(tmp), 1)
            # Where git would run it, one id names a commit and the other none; the check's
            # verdict is the same for both.
            exists = lambda rev: subprocess.run(["git", "-C", tmp, "cat-file", "-e", rev + "^{commit}"],  # noqa: E731
                                                capture_output=True, timeout=60).returncode == 0
            self.assertEqual((exists(own), exists(ABSENT)), (True, False))
        for rev in (PRESENT, ABSENT, own, own[:7]):
            for source in HANDED:
                with self.subTest(rev=rev, source=source):
                    self.assertEqual([r for _, r in fixed_ids(source.replace("REV", rev))], [rev],
                                     "SPEC fixed revisions: a literal id handed to git is refused")

    def test_hex_data_and_named_revisions_are_not_refused(self):
        for source in NOT_HANDED:
            with self.subTest(source=source):
                self.assertEqual(fixed_ids(source.replace("REV", "deadbeef")), [],
                                 "SPEC fixed revisions: only git's operands are read")

    def test_no_test_hands_git_a_fixed_id(self):
        found = [f"{path.name}:{line} {rev}" for path in sorted(HERE.glob("*.py"))
                 for line, rev in fixed_ids(path.read_text(encoding="utf-8"))]
        self.assertEqual(found, [], "SPEC fixed revisions: a test hands git a revision by name or of its own "
                                    "repository, never a fixed id")


if __name__ == "__main__":
    unittest.main()
