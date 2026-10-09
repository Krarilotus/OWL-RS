"""Fast report counters accept structured metadata without changing numeric work rules."""
import importlib.util
import json
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    "fast_counter_contract", Path(__file__).resolve().parents[2] / "fast" / "fast.py"
)
fast = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fast)


class FastCounters(unittest.TestCase):
    def test_unchanged_nested_metadata(self):
        values = {"q.a.rewrites": [[{"rules": ["r1"]}]] * 3}
        self.assertEqual(fast.counter_changes(values, values), (False, []))

    def test_changed_nested_metadata_is_preserved(self):
        before, after = [{"rules": ["r1"]}], [{"rules": ["r1", "r2"]}]
        grew, notes = fast.counter_changes({"rewrite": [before] * 3}, {"rewrite": [after] * 3})
        self.assertFalse(grew)
        self.assertEqual(notes, [f"rewrite: {json.dumps(before, sort_keys=True)} -> "
                                 f"{json.dumps(after, sort_keys=True)} (metadata changed)"])

    def test_variable_repetitions_are_not_judged(self):
        for b, n in [([[1], [2]], [[3], [3]]), ([[1], [1]], [[2], [3]]),
                     ([100, 101], [150, 150])]:
            with self.subTest(b=b, n=n):
                self.assertEqual(fast.counter_changes({"work": b}, {"work": n}), (False, []))

    def test_numeric_thresholds_are_unchanged(self):
        for before, after, expected in [
            (100, 100, (False, [])),
            (100, 102, (False, ["work: 100 -> 102 (+2.0%)"])),
            (100, 103, (True, ["work: 100 -> 103 (+3.0%)  MORE WORK"])),
            (100, 98, (False, ["work: 100 -> 98 (-2.0%)"])),
            (100, 97, (False, ["work: 100 -> 97 (-3.0%)  less work"])),
            (0, 1, (True, ["work: 0 -> 1 (+inf%)  MORE WORK"])),
        ]:
            with self.subTest(before=before, after=after):
                self.assertEqual(fast.counter_changes({"work": [before] * 3},
                                                     {"work": [after] * 3}), expected)

    def test_scalar_metadata_and_type_changes(self):
        for before, after in [("a", "b"), (False, True), (None, "x"), ([1], 2)]:
            with self.subTest(before=before, after=after):
                grew, notes = fast.counter_changes({"metadata": [before]}, {"metadata": [after]})
                self.assertFalse(grew)
                self.assertEqual(len(notes), 1)
                self.assertIn("metadata changed", notes[0])

    def test_metadata_cannot_hide_numeric_growth(self):
        grew, notes = fast.counter_changes({"metadata": [[1]], "work": [100]},
                                           {"metadata": [[2]], "work": [103]})
        self.assertTrue(grew)
        self.assertEqual(len(notes), 2)
        self.assertIn("MORE WORK", notes[1])


if __name__ == "__main__":
    unittest.main()
