"""Two sides that ran one build compare nothing (D16-593).

Spec (D16-593, 14 Nov 2026): two count jobs compared one binary with itself (every side built in
one source path and one target directory, one sha256 for three sides) and reported "no
difference". The release gate's report refuses to compare two sides whose builds are one (every
binary's sha256 the same) unless they are declared identical, as sides of one revision are (an
A-A' control); a side without its binaries' hashes can't be compared. Not compared is a failure,
never "no difference". The check is per build, not per case: a case one side doesn't run, and a
tool two trees share, change nothing.
"""
from __future__ import annotations

import copy
import unittest

import test_release_gate as fixtures
from suitekit import manifest

gate_module = fixtures.gate_module


def one_build(ps: list[dict], side: str, like: str) -> list[dict]:
    """PS with SIDE's passes carrying LIKE's binaries' hashes (its own revision kept)."""
    sha = next(p["report"]["binaries"]["sha256"] for p in ps if p["side"] == like)
    for p in ps:
        if p["side"] == side:
            for key in ("binaries", "binaries_finished"):
                p["report"][key]["sha256"] = copy.deepcopy(sha)
    return ps


def one_build_errors(summary: dict) -> list[str]:
    return [e for e in summary["errors"] if "ran one build" in e or "no hash of its binaries" in e]


class OneBuild(unittest.TestCase):

    def test_a_candidate_that_ran_its_baseline_s_build_is_not_compared(self):
        summary = fixtures.run_gate(one_build(fixtures.passes(), "cand", "base"))
        self.assertFalse(summary["passed"], "SPEC one build on two sides: not compared, no pass")
        self.assertEqual([("cand" in e, "base" in e) for e in one_build_errors(summary)], [(True, True)])
        # The same round with each side's own build: nothing refused.
        self.assertEqual(one_build_errors(fixtures.run_gate(fixtures.passes())), [])

    def test_sides_of_one_revision_are_declared_identical(self):
        # An A-A' control: candidate and baseline one revision, so one build, compared on purpose.
        ps = one_build(fixtures.passes(sides=("base", "cand")), "cand", "base")
        for p in ps:
            for key in ("binaries", "binaries_finished"):
                p["report"][key]["revision"] = fixtures.REVS["base"]
        sides = {"cand": fixtures.REVS["base"], "base": fixtures.REVS["base"]}
        summary = gate_module.gate(ps, sides, "cand", ["base"], fixtures.HARNESS, context=fixtures.CONTEXT)
        self.assertTrue(summary["passed"], f"SPEC sides of one revision are compared: {summary['errors']}")
        self.assertEqual(summary["errors"], [])

    def test_what_is_one_build(self):
        a = {"release/examples/perf_lab": "1" * 64, "release/examples/classify": "2" * 64}
        for name, hashes, identical, refused in (
                ("every binary the same", {"cand": a, "base": dict(a)}, set(), ["cand and base"]),
                ("declared identical", {"cand": a, "base": dict(a)}, {frozenset(("cand", "base"))}, []),
                ("one tool the trees share", {"cand": a, "base": {**a, "release/examples/classify": "3" * 64}},
                 set(), []),
                ("a binary one side lacks", {"cand": a, "main": {"release/examples/perf_lab": "1" * 64}}, set(), []),
                ("a side without hashes", {"cand": a, "base": {}}, set(), ["base: no hash"])):
            with self.subTest(name):
                errors = manifest.one_binary(hashes, identical)
                self.assertEqual(len(errors), len(refused), errors)
                for error, start in zip(errors, refused):
                    self.assertTrue(error.startswith(start), error)


if __name__ == "__main__":
    unittest.main()
