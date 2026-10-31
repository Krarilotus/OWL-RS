"""The tests take the revisions they hand to git from the checkout they run in (HEAD, HEAD^, a
commit they make), never a fixed id: a history rewrite or a pruned host loses it. T0, 28 Oct
2026: test_abba's dcb3b0e2 and 0251a486, ids from before the 26 Oct rewrite, failed every push
gate on the hosts whose job repositories no longer held them. The check finds every string in
a test's code that names a commit of the checkout it runs in; an id whose commit a host has lost
fails its own test there.

    python -m unittest discover -s benches/suite/tests -p test_fixed_revisions.py
"""
from __future__ import annotations

import ast
import re
import subprocess
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
# The repository, named: the suite's git search stops at its top (isolation.py).
ROOT = HERE.parents[2]
HEX = re.compile(r"(?<![0-9a-fA-F])[0-9a-f]{7,40}(?![0-9a-fA-F])")


def strings(path: Path) -> list[tuple[int, str]]:
    """The string constants of a module's code, with their lines; docstrings left out (they cite
    history, which stays)."""
    tree = ast.parse(path.read_text(encoding="utf-8"))
    docs = {id(node.body[0].value) for node in ast.walk(tree)
            if isinstance(node, (ast.Module, ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef))
            and node.body and isinstance(node.body[0], ast.Expr) and isinstance(node.body[0].value, ast.Constant)}
    return [(node.lineno, node.value) for node in ast.walk(tree)
            if isinstance(node, ast.Constant) and isinstance(node.value, str) and id(node) not in docs]


class FixedRevisions(unittest.TestCase):
    def test_no_test_names_a_commit_of_this_repository(self):
        found = [(path.name, line, token) for path in sorted(HERE.glob("*.py"))
                 for line, text in strings(path) for token in HEX.findall(text)]
        tokens = sorted({token for _, _, token in found})
        named = subprocess.run(["git", "-C", str(ROOT), "cat-file", "--batch-check"], capture_output=True, text=True,
                               input="".join(f"{token}^{{commit}}\n" for token in tokens), timeout=60, check=True)
        commits = {token for token, out in zip(tokens, named.stdout.splitlines()) if out.split()[1:2] == ["commit"]}
        self.assertEqual([f"{name}:{line} {token}" for name, line, token in found if token in commits], [],
                         "SPEC fixed revisions: a test hands git a revision of its own checkout, never a fixed id")


if __name__ == "__main__":
    unittest.main()
