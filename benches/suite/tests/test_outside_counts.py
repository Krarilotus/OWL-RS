"""The outside check's counts in the nightly tier (scripts/lib/outside-counts.py): each run says how many
cases reached the final comparison and why the others were dropped, so a quiet run is never an empty one,
and it gives the check's verdict.

Run: python benches/suite/tests/isolation.py discover -s benches/suite/tests -p test_outside_counts.py
An assertion tagged `SPEC <tag>` states the behaviour its test is for.
"""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
READS = ("scripts/lib/outside-counts.py",)


def summary(directory, rows, code):
    """A summary.tsv of outside.sh's shape (seed, shape, tags, name, class, outcome, detail), and its exit
    CODE as scripts/check.sh keeps it (None: no exit kept)."""
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "summary.tsv").write_text("".join(f"{seed}\tshape\ttags\t{name}\tclass\t{outcome}\tdetail\n"
                                                   for seed, name, outcome in rows), encoding="utf-8")
    if code is not None:
        (directory / "exit").write_text(f"{code}\n", encoding="utf-8")


class OutsideCounts(unittest.TestCase):
    def counts(self, fixed, fresh, drawn=4, first=100, fresh_drawn=2, fixed_exit=0, fresh_exit=0):
        with tempfile.TemporaryDirectory(prefix="nrese-outside-counts-") as tmp:
            summary(Path(tmp) / "fixed", fixed, fixed_exit)
            summary(Path(tmp) / "fresh", fresh, fresh_exit)
            done = subprocess.run([sys.executable, str(ROOT / "scripts/lib/outside-counts.py"), tmp, str(drawn),
                                   str(first), str(fresh_drawn)], capture_output=True, text=True, timeout=60)
            return done.returncode, json.loads((Path(tmp) / "outside.json").read_text()), done.stdout

    def test_each_case_is_compared_or_dropped_by_its_reason(self):
        # Spec (scripts/lib/outside-counts.py, its docs; worked by hand): a case is compared once a row of it reached
        # pass, pass-one, FAIL or FAIL-one; any other is dropped, by its reason, so that compared plus
        # dropped is the drawn count. Seed 0 compares (one row passes beside a declined one); seed 1 is a
        # reference gap; seed 2 has only the references' split; seed 3 never reached the summary.
        code, out, said = self.counts([(0, "q", "pass"), (0, "write", "declined-consistent"),
                                       (1, "case", "reference-gap"), (2, "q", "set-aside"), (2, "r", "reference-undecided")],
                                      [(100, "q", "pass"), (101, "q", "pass")], drawn=4)
        self.assertEqual((out["fixed"]["compared"], out["fixed"]["dropped"]),
                         (1, {"reference gap": 1, "references split": 1, "missing": 1}), "SPEC outside-counted")
        self.assertIn("outside: seeds 0..3, drawn 4, compared 1, dropped: missing 1, reference gap 1, "
                      "references split 1; disagreements 0; EMPTY RUN", said, "SPEC outside-log")
        self.assertEqual(code, 1, "SPEC outside-floor: 1 of 4 compared is below half")

    def test_a_run_below_the_floor_is_empty_and_at_it_is_not(self):
        # Spec: fewer than half of the fixed block's drawn cases compared is an empty run (exit 1); half
        # passes. The fresh block's counts never decide it.
        half = [(0, "q", "pass"), (1, "q", "pass-one"), (2, "q", "reference-undecided"), (3, "w", "declined-x")]
        code, out, _ = self.counts(half, [])
        self.assertEqual((code, out["fixed"]["empty"], out["fresh"]["dropped"]), (0, False, {"missing": 2}),
                         "SPEC outside-floor: half compared, the fresh block empty")
        code, out, _ = self.counts(half[1:], [(100, "q", "pass"), (101, "q", "pass")])
        self.assertEqual((code, out["fixed"]["empty"]), (1, True), "SPEC outside-floor: 1 of 4")

    def test_a_split_key_control_drops_its_case_from_both_verdicts(self):
        # Spec (the outside check's bar: where the references split they are no oracle, so the case is
        # neither agreement nor disagreement): a case with a control-split row is dropped whole, as
        # "references split", its FAIL rows with it, in the fixed block as in the fresh one; outside.sh's
        # exit 101 for it is a clean end. A FAIL in a case without a split is a fixed disagreement: red.
        split = [(0, "named-two", "control-split"), (0, "exists", "FAIL"), (1, "q", "pass"), (2, "q", "pass")]
        code, out, said = self.counts(split, [(100, "named-two", "control-split"), (100, "q", "FAIL"),
                                              (101, "q", "pass")], drawn=3, fixed_exit=101, fresh_exit=101)
        self.assertEqual((code, out["fixed"]["dropped"], out["fixed"]["disagreements"], out["new_disagreements"]),
                         (0, {"references split": 1}, [], []), f"SPEC outside-split: {said}")
        self.assertEqual(out["fresh"]["dropped"], {"references split": 1}, "SPEC outside-split: the fresh block")
        code, out, said = self.counts(split + [(1, "type", "FAIL")], [], drawn=3, fixed_exit=101)
        self.assertEqual((code, out["fixed"]["disagreements"]), (1, [{"seed": 1, "query": "type", "verdict": "FAIL"}]),
                         "SPEC outside-disagreement: a fixed seed's difference is red")
        self.assertIn("disagreements 1", said, "SPEC outside-log")

    def test_fresh_disagreements_are_listed_and_never_red(self):
        # Spec (scripts/lib/outside-counts.py, its docs): the fresh seeds' rows both references hold against the
        # store (FAIL, FAIL-one) are listed as new_disagreements with seed, query and verdict, and counted
        # on the discovery line; they never make the run red, outside.sh's exit 101 for them included.
        fixed = [(s, "q", "pass") for s in range(4)]
        code, out, said = self.counts(fixed, [(100, "exists", "FAIL"), (100, "type", "pass"), (101, "q", "FAIL-one")],
                                      fresh_exit=101)
        self.assertEqual((code, out["new_disagreements"]), (0, [
            {"seed": 100, "query": "exists", "verdict": "FAIL"},
            {"seed": 101, "query": "q", "verdict": "FAIL-one"}]), "SPEC discovery-listed")
        self.assertIn("outside discovery: seeds 100..101, compared 2, dropped: none; new disagreements 2", said,
                      "SPEC discovery-log")

    def test_a_block_whose_run_did_not_end_cleanly_is_red_the_fresh_one_too(self):
        # Spec (only a fresh seed's difference never gates): a block whose outside.sh run ended otherwise
        # than 0, or 101 with a failing row to show for it, is an error and red, in the fresh block as in
        # the fixed one: exit 7 (a seed missing, a control not established), 5 (a planted fault missed),
        # 101 with no failing row (a crash in the test), 124 (over its time), or no exit kept at all.
        fixed = [(s, "q", "pass") for s in range(4)]
        fresh = [(100, "q", "pass"), (101, "q", "pass")]
        for code_kept in (7, 5, 101, 124, None):
            code, out, said = self.counts(fixed, fresh, fresh_exit=code_kept)
            self.assertEqual((code, out["fresh"]["error"]), (1, f"outside.sh exit {code_kept or 'none'}"),
                             f"SPEC outside-error: the fresh block, exit {code_kept}")
            self.assertIn(f"; ERROR: outside.sh exit {code_kept or 'none'}", said.splitlines()[1], "SPEC outside-log")
        code, out, _ = self.counts(fixed, fresh, fixed_exit=5)
        self.assertEqual((code, out["fixed"]["error"]), (1, "outside.sh exit 5"), "SPEC outside-error: the fixed block")


if __name__ == "__main__":
    unittest.main()
