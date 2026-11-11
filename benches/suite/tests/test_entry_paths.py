"""The suite's entry scripts, run by path (`python benches/suite/suite.py`, `abba.py`), leave the
standard library's modules first: no module of theirs may shadow one, `queue` least of all, which
every thread pool needs (Virtuoso's loader runs one)."""
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SUITE = Path(__file__).resolve().parents[1]
# An entry as Python runs a script by path (its directory first on sys.path, run as __main__),
# then `queue` imported as any later module imports it; prints where it came from.
PROBE = """
import runpy, sys
from pathlib import Path
entry = sys.argv[1]
sys.path.insert(0, str(Path(entry).parent))
sys.argv = [entry, *sys.argv[2:]]
try:
    runpy.run_path(entry, run_name="__main__")
except SystemExit:
    pass
import queue
print(Path(queue.__file__).resolve())
"""


class EntryPaths(unittest.TestCase):
    def queue_after(self, entry, *args):
        with tempfile.TemporaryDirectory() as cwd:
            run = subprocess.run([sys.executable, "-B", "-c", PROBE, str(SUITE / entry), *args], cwd=cwd,
                                 capture_output=True, text=True, timeout=120)
        self.assertEqual(run.returncode, 0, run.stderr[-2000:])
        return Path(run.stdout.strip().splitlines()[-1])

    def test_the_suite_run_by_path_imports_the_standard_queue(self):
        self.assertNotEqual(self.queue_after("suite.py", "--check").parent, SUITE)

    def test_abba_run_by_path_imports_the_standard_queue(self):
        self.assertNotEqual(self.queue_after("abba.py", "--help").parent, SUITE)

    def test_no_bench_or_script_module_has_a_standard_module_name(self):
        # The kits and scripts put their directories first on sys.path (about 45 places), so a
        # module named like a standard one shadows it in every process that does.
        root = SUITE.parents[1]
        clashes = sorted(str(p.relative_to(root)) for d in ("benches", "scripts") for p in (root / d).rglob("*.py")
                         if p.stem in sys.stdlib_module_names
                         and not any(part.startswith(".") or part == "site-packages" for part in p.parts))
        self.assertEqual(clashes, [])


if __name__ == "__main__":
    unittest.main()
