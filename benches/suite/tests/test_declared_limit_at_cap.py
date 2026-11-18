"""A case that ends at its memory cap on every side declares `outcome = "memory-limit"`; a pass that
stops there is a known issue to the release gate, not a failure, and still blocks where a baseline
completes (benches/PROTOCOL.md, the declared limit). The kernel ends such a pass with an OOM kill,
but it may postpone the kill past the case's time limit while the scope is held at its cap,
refaulting the binary's own pages (D16-563; sparqloscope-dblp, 15 Nov 2026: the same binary killed
at 46 s once and at 1,237 s once, its scope 1 to 3 MiB under 28,672 MiB and all its tasks stalled on
memory 54 to 86 % of the time). Such a pass also stopped at the declared limit. Its record keeps its
status (`timeout`); a timeout that the cap wasn't holding, or with no reading of the scope taken when
it came, is still a failure.

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
from suitekit import compare  # noqa: E402

CASE = "sparqloscope-dblp"
CAP_GB = 28  # the case's cap: its scope's MemoryMax=28672M
CAP = 28672 << 20
# The candidate's timed-out pass in the third cut's timed part on the development machine's WSL VM (pass-3-cand.json):
# its scope's anon + kernel peak, 2.8 MiB under the cap; and the share of time all its tasks stalled
# on memory while held there (the dblp note, cand-1 at 721 s).
AT_THE_CAP, HELD_STALL = 30061797376, 85.0
TIMED_OUT = subprocess.TimeoutExpired("perf_lab", 900)


def scoped(outcome, used=None, stall=None, peak=AT_THE_CAP) -> tuple[int, str]:
    """fast.process() running the case's command in its scope, the command ending as `outcome` (an
    exit status, or the time limit's TimeoutExpired). The scope, as cgroup v2 shows it then: anon +
    kernel memory `used` (memory.stat; none: unreadable), memory.max the cap, and all its tasks
    stalled on memory `stall` % of the last 10 s (memory.pressure; none: unreadable)."""
    with tempfile.TemporaryDirectory() as temporary:
        group = Path(temporary) / "scope"
        group.mkdir()
        (group / "memory.max").write_text(f"{CAP}\n")
        if used is not None:
            (group / "memory.stat").write_text(f"anon {used - (64 << 20)}\nfile 1048576\nkernel {64 << 20}\n")
        if stall is not None:
            (group / "memory.pressure").write_text(f"some avg10={stall:.2f} avg60=0.00 avg300=0.00 total=1\n"
                                                   f"full avg10={stall:.2f} avg60=0.00 avg300=0.00 total=1\n")

        def run(argv, **kw):  # the wrapper's first act in the scope: where it is
            Path(kw["env"]["FAST_SCOPE_DIR"]).write_text(str(group))
            if isinstance(outcome, int):
                return types.SimpleNamespace(returncode=outcome, stdout="", stderr="Killed\n")
            raise outcome

        with patch.multiple(fast, native_dirs=lambda: {}, FAST_DATA=Path(temporary),
                            scope_argv=lambda *_: ["scope", "--"], native=lambda command, _: command,
                            end_scope=lambda *_: None, pinned_cpus=lambda: None,
                            AnonKernelSampler=lambda where, **_: types.SimpleNamespace(
                                start=lambda: None, stop=lambda: peak, thp={})), \
                patch.object(fast.subprocess, "run", side_effect=run):
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


def held() -> dict:
    return record(*scoped(TIMED_OUT, used=AT_THE_CAP, stall=HELD_STALL))


def killed() -> dict:
    return record(*scoped(137))


class DeclaredMemoryLimit(unittest.TestCase):
    """Spec: D16-563 (15 Nov 2026): 'a pass killed at the limit and a pass timed out at
    the limit both match; a timeout below the limit, a completed baseline, and a crash do not.' A
    record is never relabelled; a timeout that is not at the limit still fails."""

    def test_a_pass_killed_at_the_cap_matches(self):
        run = killed()
        self.assertEqual(state(run), "known")
        self.assertEqual(fast.not_as_expected([run]), [])

    def test_a_pass_timed_out_held_at_the_cap_matches_and_stays_a_timeout(self):
        run = held()
        self.assertEqual(state(run), "known")
        self.assertEqual(fast.not_as_expected([run]), [])
        self.assertEqual(run["status"], "timeout")

    def test_a_timeout_below_the_cap_fails_however_stalled(self):
        # Its peak was at the cap, but the scope had come down from it when the time limit came.
        run = record(*scoped(TIMED_OUT, used=20 << 30, stall=HELD_STALL))
        self.assertEqual(state(run), "failed")
        self.assertEqual(fast.not_as_expected([run]), [run])

    def test_a_slow_timeout_near_the_cap_that_the_cap_isn_t_holding_fails(self):
        # 200 MiB under the cap (within 1 % of 28 GiB), its tasks never stalled on
        # memory: a slow run that sits near its cap, not one the cap holds.
        run = record(*scoped(TIMED_OUT, used=CAP - (200 << 20), stall=0.0))
        self.assertEqual(state(run), "failed")
        self.assertEqual(fast.not_as_expected([run]), [run])

    def test_a_timeout_whose_scope_can_t_be_read_when_it_came_fails(self):
        # No reading taken at the time limit is no reading, whatever was seen before.
        for used, stall in ((AT_THE_CAP, None), (None, HELD_STALL)):
            with self.subTest(used=used, stall=stall):
                run = record(*scoped(TIMED_OUT, used=used, stall=stall))
                self.assertEqual(state(run), "failed")
                self.assertEqual(fast.not_as_expected([run]), [run])

    def test_a_timeout_without_a_reading_at_its_end_fails(self):
        # The cut's own record (pass-3-cand.json): its peak at the cap, no reading of the scope
        # when the time limit came. It stays a failure: no record is relabelled.
        run = record(124, f"fast-anon-kernel-peak {AT_THE_CAP}\ntimed out after 900 s")
        self.assertEqual(state(run), "failed")
        self.assertEqual(fast.not_as_expected([run]), [run])

    def test_a_crash_does_not_match(self):
        run = record(*scoped(139, used=AT_THE_CAP, stall=HELD_STALL))
        self.assertEqual(state(run), "failed")
        self.assertEqual(fast.not_as_expected([run]), [run])

    def test_the_held_timeout_still_blocks_where_a_baseline_completed(self):
        cand = rg.evidence([{"n": 1, "report": {"records": [held()]}}], CASE)
        base = rg.evidence([{"n": 2, "report": {"records": [{**record(0, "loaded"), "status": "ok"}]}}], CASE)
        row = rg.compare_case(cand, base, "base", set())
        self.assertIn("stops at its declared outcome (timeout) where base completed", row["blocks"])


class TwoReports(unittest.TestCase):
    """Spec: fast.py's comparison of two reports (suitekit/compare.py) judges a case as
    the gate does: two runs that each ended as the case declares are a known issue, however each
    ended, with no timing verdict and no acceptance failure."""

    def compared(self, base: dict, new: dict) -> compare.Out:
        out = compare.Out()
        compare.compare_cases(compare.pooled({"records": [base], "reps": 1}),
                              compare.pooled({"records": [new], "reps": 1}), 2.0, out)
        return out

    def test_two_held_timeouts_are_a_known_issue(self):
        out = self.compared(held(), held())
        self.assertEqual(out.regressions, 0)
        self.assertTrue(any("known issue" in line for line in out.lines["Answers and completeness"]))

    def test_a_kill_and_a_held_timeout_are_a_known_issue(self):
        out = self.compared(killed(), held())
        self.assertEqual(out.regressions, 0)
        self.assertTrue(any("known issue" in line for line in out.lines["Answers and completeness"]))

    def test_a_kill_and_a_timeout_below_the_cap_still_fail(self):
        out = self.compared(killed(), record(*scoped(TIMED_OUT, used=20 << 30, stall=0.0)))
        self.assertEqual(out.regressions, 1)


if __name__ == "__main__":
    unittest.main()
