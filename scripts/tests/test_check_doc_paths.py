"""scripts/check-doc-paths.py: a doc that links or names a path that doesn't exist fails the check.

    python3 -I scripts/tests/test_check_doc_paths.py

Spec: the checker's docstring. The documents below are review PR #92 r1's fixtures and witnesses
(Harness notes review-pr92-r1: fixtures/*.md, P09, P18), each beside a control whose target exists.
Stdlib only; every document is written to a temporary directory, nothing in the checkout changes.
"""
import contextlib
import importlib.util
import io
import os
import shutil
import sys
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True  # importing the checker leaves no __pycache__ in the checkout
SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), os.pardir, "check-doc-paths.py")
spec = importlib.util.spec_from_file_location("check_doc_paths", SCRIPT)
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


class CheckDocPaths(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp(prefix="check-doc-paths-")
        self.addCleanup(shutil.rmtree, self.dir)
        os.makedirs(os.path.join(self.dir, "sub dir"))
        self.touch("guide.md", "")
        self.touch(os.path.join("sub dir", "guide.md"), "")
        self.allow = self.touch("doc-paths.allow", "")
        patcher = mock.patch.object(checker, "ALLOW", self.allow)
        patcher.start()
        self.addCleanup(patcher.stop)

    def touch(self, name, text):
        path = os.path.join(self.dir, name)
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(text)
        return path

    def check(self, text, name="doc.md"):
        """The checker's exit status for one document with this text."""
        doc = self.touch(name, text)
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            return checker.main([doc])

    # Inline links resolve from their document (dbe29b41), never from the repository root.
    def test_an_inline_link_to_a_missing_file_fails(self):
        self.assertEqual(self.check("[missing](definitely-missing-pr92.md)\n"), 1)  # fixtures/missing.md
        self.assertEqual(self.check("[exists](guide.md)\n"), 0)

    def test_a_link_resolves_from_its_document_not_the_root(self):
        self.assertEqual(self.check("[status](docs/STATUS.md)\n"), 1)

    # B1: an angle-bracket destination with a space is a link like any other.
    def test_an_angle_bracket_link_with_a_space_is_checked(self):
        self.assertEqual(self.check("[missing](<missing directory/guide.md>)\n"), 1)  # fixtures/space-link.md
        self.assertEqual(self.check("[exists](<sub dir/guide.md>)\n"), 0)

    # B1: a reference-style link's destination is in its definition.
    def test_a_reference_definition_is_checked(self):
        self.assertEqual(self.check("[missing][guide]\n\n[guide]: definitely-missing-pr92.md\n"), 1)
        self.assertEqual(self.check("[exists][guide]\n\n[guide]: guide.md\n"), 0)
        self.assertEqual(self.check("[exists][g]\n\n  [g]: <sub dir/guide.md> \"Title\"\n"), 0)

    # Fix-round sol-loop F1: a fence's end can't hide the links after it (GFM 4.5). Code isn't
    # told apart at all (fail closed, after r2's F5/F6), so link-shaped text inside it is checked too.
    def test_a_link_after_a_closed_fence_is_checked(self):
        fenced = "````\n```\n````\n"
        self.assertEqual(self.check(fenced + "[x](missing.md)\n"), 1)
        self.assertEqual(self.check(fenced + "[x](guide.md)\n"), 0)
        self.assertEqual(self.check("```\n[x](missing.md)\n```\n"), 1)
        self.assertEqual(self.check("```\n[x](guide.md)\n```\n"), 0)

    # F2: a reference definition inside a blockquote is a definition (GFM 4.7).
    def test_a_reference_definition_in_a_blockquote_is_checked(self):
        self.assertEqual(self.check("> [x][guide]\n>\n> [guide]: definitely-missing-pr92.md\n"), 1)
        self.assertEqual(self.check("> [x][guide]\n>\n> [guide]: guide.md\n"), 0)

    # F3: backticks inside a destination belong to it; link-shaped text in a code span is checked.
    def test_a_destination_with_backticks_is_checked(self):
        self.assertEqual(self.check("[x](`missing.md`)\n"), 1)
        self.assertEqual(self.check("`[x](missing.md)` is code\n"), 1)
        self.assertEqual(self.check("`[x](guide.md)` is code\n"), 0)

    # Fix-round sol-loop r2 F5: a fence inside a blockquote ends with the quote (GFM example 98).
    def test_a_link_after_a_quoted_fence_is_checked(self):
        self.assertEqual(self.check("> ```\n> sample\n\n[x](missing.md)\n"), 1)
        self.assertEqual(self.check("> ```\n> sample\n\n[x](guide.md)\n"), 0)

    # F6: a backtick in a title doesn't open a code span that hides the next link (GFM 6.6).
    def test_a_backtick_in_a_title_hides_no_link(self):
        self.assertEqual(self.check('[a](guide.md "a ` title") [b](missing.md) tail `\n'), 1)
        self.assertEqual(self.check('[a](guide.md "a ` title") [b](guide.md) tail `\n'), 0)

    # F4: a title may hold a parenthesis.
    def test_a_title_with_a_parenthesis_is_read(self):
        self.assertEqual(self.check("[x](guide.md (title))\n"), 0)
        self.assertEqual(self.check('[x](guide.md "a) title")\n'), 0)
        self.assertEqual(self.check('[x](missing.md "a) title")\n'), 1)

    # B1: link syntax the checker can't read fails instead of passing unread.
    def test_an_unreadable_link_destination_fails(self):
        self.assertEqual(self.check("[odd](<unterminated\n"), 1)

    # B3: the allow list exempts mentions, never a clickable link.
    def test_an_allowed_target_exempts_a_mention_but_not_a_link(self):
        doc = os.path.join(self.dir, "doc.md")
        key = os.path.relpath(doc, checker.ROOT).replace(os.sep, "/")  # the doc's name in the allow list
        with open(self.allow, "w", encoding="utf-8") as fh:
            fh.write(f"{key} src/tests/v1_scenarios.rs  # named as pending\n")
        self.assertEqual(self.check("Pending: `src/tests/v1_scenarios.rs`.\n"), 0)
        self.assertEqual(self.check("[Current test](src/tests/v1_scenarios.rs)\n"), 1)  # P18

    # B2: the default scan reads every tracked doc; an explicit list may name deleted ones.
    def test_the_default_scan_fails_on_a_tracked_doc_it_cannot_read(self):
        absent = os.path.join(self.dir, "review-pr92-absent-tracked.md")
        with mock.patch.object(checker, "tracked_docs", lambda: [absent]), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(checker.main([]), 1)
            self.assertEqual(checker.main([absent]), 0)


if __name__ == "__main__":
    unittest.main()
