"""docs/README.md "Where is": a row about timing sends its reader to benches/suite/abba.py only.

    python3 -I scripts/tests/test_docs_map.py

Spec: CONTRIBUTING.md and benches/abba/README.md, "the one way to time a change against its
base", on the fleet's timing hosts. PR #92 review r1 B4: the map sent a slowdown to
scripts/bisect-perf.py, which builds and times each revision itself. Stdlib only.
"""
import os
import re
import sys
import unittest

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), os.pardir, os.pardir))
TIMING = re.compile(r"\b(tim(e|es|ed|ing)|slowdown|bisect\w*)\b", re.I)
DIRECT_TIMERS = ("scripts/bisect-perf.py", "scripts/quiet-slot.sh")


def map_rows(text):
    """The rows of the "Where is" table: (what, where)."""
    section = text.split("## Where is", 1)[1].split("\n## ", 1)[0]
    return [tuple(cell.strip() for cell in line.strip().strip("|").split("|", 1))
            for line in section.splitlines()
            if line.startswith("|") and not line.startswith(("|---", "| What"))]


def timing_problems(text):
    """Rows about timing that don't point to abba.py, or name a script that times directly."""
    problems = []
    for what, where in map_rows(text):
        if TIMING.search(what) and "benches/suite/abba.py" not in where:
            problems.append(f"{what}: not abba.py")
        problems += [f"{what}: {timer}" for timer in DIRECT_TIMERS if timer in where]
    return problems


class DocsMap(unittest.TestCase):
    def test_timing_rows_point_to_abba_only(self):
        with open(os.path.join(ROOT, "docs", "README.md"), encoding="utf-8") as fh:
            self.assertEqual(timing_problems(fh.read()), [])


if __name__ == "__main__":
    sys.dont_write_bytecode = True
    unittest.main()
