"""The perf lab's timed window starts no thread, on every side of a release gate
(benches/fast/patches/perf_lab_watchdog.py; 19 Oct 2026: a canceller thread spawned per run
started inside the timed call, 1-5 µs on microsecond queries)."""
from __future__ import annotations

import importlib.util
import re
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location("perf_lab_watchdog", ROOT / "benches/fast/patches/perf_lab_watchdog.py")
watchdog = importlib.util.module_from_spec(spec)
spec.loader.exec_module(watchdog)
BASELINES = {"main f9b515e8": "f9b515e8:crates/nrese-store/examples/perf_lab.rs",
             "v1.5 0f465633": "0f465633:crates/nrese-store/examples/perf_lab/main.rs"}


def function(text: str, name: str) -> str:
    start = text.index(f"\nfn {name}(")
    return text[start:text.index("\n}\n", start) + 3]


def timed_windows(body: str) -> list[str]:
    """Each stretch from a timer's start to its reading."""
    return re.findall(r"let started = Instant::now\(\);(.*?)started\.elapsed\(\)", body, re.S)


class TimedWindow(unittest.TestCase):
    def assert_no_thread_start(self, text: str, where: str):
        body = function(text, "run_once")
        self.assertNotIn("thread::spawn", body, f"{where}: run_once starts a thread")
        windows = timed_windows(body)
        self.assertTrue(windows, f"{where}: no timed window found in run_once")
        for window in windows:
            self.assertNotIn("spawn", window, f"{where}: a thread starts inside the timed window")
            self.assertNotIn("arm(", window)
        self.assertLess(body.index("perf_lab_watchdog::arm("), body.index("let started = Instant::now();"))
        self.assertGreater(body.index("drop(armed);"), body.index("started.elapsed()"))
        self.assertNotIn("thread::spawn", function(text, "measure"), f"{where}: measure starts a thread")

    def test_the_head_s_perf_lab_starts_no_thread_in_its_timed_window(self):
        path = ROOT / "crates/nrese-store/examples/perf_lab/main.rs"
        text = path.read_text(encoding="utf-8")
        self.assert_no_thread_start(text, "the head")
        self.assertEqual(watchdog.patch(text), (text, "present"))

    def test_the_baselines_perf_labs_are_patched_alike(self):
        for name, spec_ in BASELINES.items():
            shown = subprocess.run(["git", "-C", str(ROOT), "show", spec_], capture_output=True, text=True,
                                   encoding="utf-8")
            if shown.returncode != 0:
                self.skipTest(f"{name} isn't in this clone")
            with self.subTest(baseline=name):
                self.assertIn("thread::spawn", function(shown.stdout, "run_once"))
                patched, outcome = watchdog.patch(shown.stdout)
                self.assertEqual(outcome, "applied")
                self.assert_no_thread_start(patched, name)
                self.assertEqual(watchdog.patch(patched)[1], "present")

    def test_a_loop_other_than_the_known_one_is_not_patched(self):
        text = "fn run_once() {\n    std::thread::spawn(|| {});\n}\n"
        self.assertEqual(watchdog.patch(text), (text, "no match"))


if __name__ == "__main__":
    unittest.main()
