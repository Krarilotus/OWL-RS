"""A case that ends at its memory cap on every side declares `outcome = "memory-limit"`; a pass that
stops there is a known issue to the release gate, not a failure, and still blocks where a baseline
completes (benches/PROTOCOL.md, the declared limit). The kernel ends such a pass with an OOM kill,
but it may postpone the kill past the case's time limit while the scope is held at its cap,
refaulting the binary's own pages (D16-563; sparqloscope-dblp, 15 Nov 2026: the same binary killed
at 46 s once and at 1,237 s once, its scope at 28,668 of 28,672 MiB all along). Such a pass also
stopped at the declared limit. Its record keeps its status (`timeout`); a timeout away from the cap,
or without a reading of where the scope stood when it came, is still a failure.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import subprocess
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches/suite"))
sys.path.insert(0, str(ROOT / "benches/fast"))

import fast  # noqa: E402
import release_gate as rg  # noqa: E402

CASE = "sparqloscope-dblp"
CAP_GB = 28  # the case's cap: its scope's MemoryMax=28672M
# The candidate's timed-out pass in the third cut's timed part on khaleesi-wsl (pass-3-cand.json):
# its scope's anon + kernel peak, 2.8 MiB under the cap.
AT_THE_CAP = 30061797376
BELOW_THE_CAP = 20 << 30


def scoped(outcome, peak=None, last=None) -> tuple[int, str]:
    """fast.process() running the case's command in its scope, the command ending as `outcome`
    (an exit status, or the time limit's TimeoutExpired); the scope's anon + kernel memory
    sampled at `peak` at its largest and at `last` when the command ended."""
    done = types.SimpleNamespace(returncode=outcome, stdout="", stderr="Killed\n") if isinstance(outcome, int) else None
    with tempfile.TemporaryDirectory() as temporary, \
            patch.multiple(fast, native_dirs=lambda: {}, FAST_DATA=Path(temporary),
                           scope_argv=lambda *_: ["scope", "--"], native=lambda command, _: command,
                           end_scope=lambda *_: None, pinned_cpus=lambda: None,
                           AnonKernelSampler=lambda where, **_: types.SimpleNamespace(
                               start=lambda: None, stop=lambda: peak, last=last, thp={})), \
            patch.object(fast.subprocess, "run", return_value=done, side_effect=None if done else outcome):
        code, _, err, _ = fast.process(CASE, CAP_GB, ["perf_lab", "--load", "dblp-kg-20261011.nt"])
    return code, err


def record(code: int, err: str, expected: str = "memory-limit") -> dict:
    """The case's record as fast.py's measure() writes it for an invocation that produced no report:
    its status from classify_failure(), the invocation's readings from its stderr."""
    rec = {"case": CASE, "rep": 1, "status": fast.classify_failure(code, err, {}), "expected_outcome": expected,
           "checks": [], "routes": [], "samples": {}, "observations": [], "counters": {},
           "notes": ["invocation did not produce its JSON report", err.strip().splitlines()[-1]]}
    rec.update(fast.invocation_observation({"name": CASE, "metric": "load_s"}, {}, err, {}))
    rec.pop("identity")
    return rec


def state(rec: dict) -> str:
    return rg.state(rg.evidence([{"n": 1, "report": {"records": [rec]}}], CASE))


TIMED_OUT = subprocess.TimeoutExpired("perf_lab", 900)


class DeclaredMemoryLimit(unittest.TestCase):
    """Spec: D16-563 (the master, 15 Nov 2026): 'a pass killed at the limit and a pass timed out at
    the limit both match; a timeout below the limit, a completed baseline, and a crash do not.' A
    record is never relabelled; a timeout that is not at the limit still fails."""

    def test_a_pass_killed_at_the_cap_matches(self):
        run = record(*scoped(137, peak=AT_THE_CAP))
        self.assertEqual(state(run), "known")
        self.assertEqual(fast.not_as_expected([run]), [])

    def test_a_pass_timed_out_held_at_the_cap_matches_and_stays_a_timeout(self):
        run = record(*scoped(TIMED_OUT, peak=AT_THE_CAP, last=AT_THE_CAP))
        self.assertEqual(state(run), "known")
        self.assertEqual(fast.not_as_expected([run]), [])
        self.assertEqual(run["status"], "timeout")

    def test_a_timeout_below_the_cap_fails(self):
        # Its peak was at the cap, but the scope had come down from it when the time limit came:
        # the pass was slow, not held at its limit.
        run = record(*scoped(TIMED_OUT, peak=AT_THE_CAP, last=BELOW_THE_CAP))
        self.assertEqual(state(run), "failed")
        self.assertEqual(fast.not_as_expected([run]), [run])

    def test_a_timeout_without_a_reading_at_its_end_fails(self):
        # The cut's own record (pass-3-cand.json): its peak at the cap, no reading of the scope
        # when the time limit came. It stays a failure: no record is relabelled.
        run = record(124, f"fast-anon-kernel-peak {AT_THE_CAP}\ntimed out after 900 s")
        self.assertEqual(state(run), "failed")
        self.assertEqual(fast.not_as_expected([run]), [run])

    def test_a_crash_does_not_match(self):
        run = record(*scoped(139, peak=AT_THE_CAP, last=AT_THE_CAP))
        self.assertEqual(state(run), "failed")
        self.assertEqual(fast.not_as_expected([run]), [run])

    def test_the_held_timeout_still_blocks_where_a_baseline_completed(self):
        cand = rg.evidence([{"n": 1, "report": {"records": [
            record(*scoped(TIMED_OUT, peak=AT_THE_CAP, last=AT_THE_CAP))]}}], CASE)
        base = rg.evidence([{"n": 2, "report": {"records": [
            {**record(0, f"fast-anon-kernel-peak {BELOW_THE_CAP}"), "status": "ok"}]}}], CASE)
        row = rg.compare_case(cand, base, "base", set())
        self.assertIn("stops at its declared outcome (timeout) where base completed", row["blocks"])


if __name__ == "__main__":
    unittest.main()
