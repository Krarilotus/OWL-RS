"""A fast case's peak is its process's, and a comparison of peaks respects their spread."""
import contextlib
import importlib.util
import io
import json
import tempfile
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "fast_memory_contract", Path(__file__).resolve().parents[2] / "fast" / "fast.py"
)
fast = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fast)

# rl-lubm100 in the 11 Oct 2026 A/B on office-pc (157931a and a57e11c): the container's peak,
# and the perf lab's peak (the last phase's) and phase peaks. The container's held what the
# host had not cached of the 2.3 GB input; the process's peak is the reasoning phase's.
AB = [(4700, 1332, {"load": 1789, "reason": 2023, "queries": 1332}),
      (2729, 1342, {"load": 1760, "reason": 2105, "queries": 1341}),
      (4697, 1331, {"load": 1734, "reason": 2109, "queries": 1331}),
      (3510, 1339, {"load": 1784, "reason": 2099, "queries": 1339})]


def old_record(container, peak, phases, rep=1):
    """A record as fast.py wrote it before 12 Oct 2026: the container's peak as the case's."""
    return {"case": "rl-lubm100", "rep": rep, "status": "ok", "metric": 1500.0,
            "samples": {"reason": [1500.0] * 3}, "checks": [], "routes": [],
            "peak_mib": container, "phase_peaks": phases,
            "result": {"peak_mib": peak, "cgroup_peak_mib": container}}


def report(peaks):
    records = [{"case": "c", "rep": rep, "status": "ok", "metric": 1.0, "samples": {"q": [1.0] * 4},
                "checks": [], "routes": [], "peak_mib": peak, "container_peak_mib": None}
               for rep, peak in enumerate(peaks, 1)]
    return {"reps": len(peaks), "records": records}


def comparison(base, new):
    out = io.StringIO()
    with patch.object(fast, "load_report", side_effect=[base, new]), contextlib.redirect_stdout(out):
        code = fast.compare(SimpleNamespace(base="base", new="new"))
    return code, out.getvalue()


class FastMemory(unittest.TestCase):
    def test_the_case_peak_is_the_processs_not_the_containers(self):
        container, peak, phases = AB[0]
        result = {"peak_mib": peak, "cgroup_peak_mib": container,
                  "phases": {k: {"peak_mib": v} for k, v in phases.items()}}
        case = {"name": "peaks", "data": "none", "args": [], "queries": "", "metric": "load_s", "cap_gb": 2}
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)

            def measured(*args, **kwargs):
                (scratch / "peaks.json").write_text(json.dumps(result))
                return 0, "", "", 1

            with patch.object(fast, "SCRATCH", scratch), patch.object(fast, "container", measured):
                record = fast.measure(case, "peaks", 1, {}, {}, "none")
        self.assertEqual(record["peak_mib"], 2023)
        self.assertEqual(record["container_peak_mib"], 4700)

    def test_old_records_give_their_processs_peak(self):
        records = [old_record(*entry, rep=i) for i, entry in enumerate(AB, 1)]
        peaks = fast.pooled({"reps": len(records), "records": records})["rl-lubm100"]["peaks"]
        self.assertEqual(peaks, [2023, 2105, 2109, 2099])
        self.assertLess(fast.peak_spread(peaks), 0.05)
        self.assertGreater(fast.peak_spread([c for c, _, _ in AB]), fast.UNSTABLE_PEAK)

    def test_peaks_varying_past_the_bound_are_not_compared(self):
        code, text = comparison(report([2375, 4700]), report([2400, 2410]))
        self.assertEqual(code, 1)
        self.assertIn("memory NOT COMPARABLE (peaks vary 98% between repetitions)", text)
        self.assertNotIn("MORE MEMORY", text)

    def test_more_memory_counts_beyond_the_spread_of_peaks(self):
        # Base peaks within 20 %: 8 % more is inside that spread, 25 % beyond it.
        self.assertEqual(comparison(report([1000, 1200]), report([1250, 1300]))[0], 0)
        code, text = comparison(report([1000, 1200]), report([1450, 1500]))
        self.assertEqual(code, 1)
        self.assertIn("MORE MEMORY", text)


if __name__ == "__main__":
    unittest.main()
