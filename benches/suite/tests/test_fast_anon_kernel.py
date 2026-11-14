"""A native case's anon + kernel peak (fast.py's AnonKernelSampler and process): a case of a few
tens of milliseconds is read, and a case without a reading fails at once. F2, 13 Nov 2026: main's
ql-memberships-reuse (0.05 s) had none in four of six passes, sampled every 50 ms, and its memory
series came out incomplete only in the report."""
import os
import tempfile
import time
import types
import unittest
from pathlib import Path
from unittest.mock import patch

from test_runtime_lifetime import load_fast


class Sampler(unittest.TestCase):
    def sample(self, scope_found_s: float, spike: tuple[float, float], stop_s: float):
        """The sampler on a fake clock that each wait advances: the scope's path appears at
        `scope_found_s`, its reading is 30 MiB within `spike` and 4 KiB otherwise, and it is
        stopped at `stop_s`. The peak and the clock at each look at memory.stat."""
        fast = load_fast()
        clock, looks = [0.0], []
        with tempfile.TemporaryDirectory() as temporary:
            group = Path(temporary)
            (group / "memory.stat").write_text("anon 4096\n")
            where = group / "where"

            def appear():
                if clock[0] >= scope_found_s and not where.exists():
                    where.write_text(str(group))

            class Halt:
                def is_set(self):
                    return clock[0] >= stop_s

                def wait(self, seconds):
                    clock[0] += seconds
                    appear()

            def reading(_stat):
                looks.append(clock[0])
                return (30 << 20) if spike[0] <= clock[0] < spike[1] else 4096

            appear()
            with patch.object(fast, "anon_kernel_bytes", reading), \
                    patch.object(fast, "time", types.SimpleNamespace(monotonic=lambda: clock[0])):
                sampler = fast.AnonKernelSampler(where)
                sampler.halt = Halt()
                sampler.run()
        return sampler.peak, looks

    def test_a_peak_within_the_first_50_ms_of_the_scope_is_read(self):
        # Spec: the scope's peak is its largest anon + kernel; a process that ends within one
        # 50 ms period still has it. The scope appears at 7 ms, its peak lasts from 20 to 35 ms.
        peak, looks = self.sample(0.007, (0.020, 0.035), 0.060)
        self.assertEqual(peak, 30 << 20, f"looks at {looks}")

    def test_after_its_first_quarter_second_a_scope_is_read_every_50_ms(self):
        # Spec: the dense looks cost a case its first quarter second only; after it, 50 ms as before.
        _, looks = self.sample(0.0, (1.0, 1.0), 1.0)
        later = [t for t in looks if t > 0.3]
        self.assertTrue(13 <= len(later) <= 15, f"{len(later)} looks between 0.3 and 1 s")
        self.assertGreater(len([t for t in looks if t < 0.25]), 200)

    @unittest.skipUnless(hasattr(os, "sched_setaffinity") and len(os.sched_getaffinity(0)) > 1,
                         "thread affinity on more than one CPU (Linux)")
    def test_the_sampler_keeps_off_the_case_s_cpus(self):
        # Spec: the timed command runs on its pinned CPUs (taskset); the harness's sampler does not.
        fast = load_fast()
        mine = sorted(os.sched_getaffinity(0))
        seen = []

        def reading(_stat):
            seen.append(os.sched_getaffinity(0))
            return 4096

        with tempfile.TemporaryDirectory() as temporary:
            group = Path(temporary)
            (group / "memory.stat").write_text("anon 4096\n")
            (group / "where").write_text(str(group))
            with patch.object(fast, "anon_kernel_bytes", reading):
                sampler = fast.AnonKernelSampler(group / "where", avoid=str(mine[0]))
                sampler.start()
                time.sleep(0.02)
                sampler.stop()
        self.assertTrue(seen)
        self.assertTrue(all(mine[0] not in cpus for cpus in seen), seen[:1])
        self.assertIn(mine[0], os.sched_getaffinity(0), "the harness's own thread moved")


class Process(unittest.TestCase):
    def run_case(self, peak, measured=True):
        fast = load_fast()
        done = types.SimpleNamespace(returncode=0, stdout="answer", stderr="")
        with tempfile.TemporaryDirectory() as temporary, \
                patch.multiple(fast, native_dirs=lambda: {}, FAST_DATA=Path(temporary),
                               scope_argv=lambda *_: ["scope", "--"], native=lambda command, _: command,
                               end_scope=lambda *_: None, pinned_cpus=lambda: None,
                               AnonKernelSampler=lambda where, **_: types.SimpleNamespace(start=lambda: None,
                                                                                          stop=lambda: peak)), \
                patch.object(fast.subprocess, "run", return_value=done):
            return fast.process("case", 1, ["command"], measured=measured)

    def test_a_case_without_a_reading_fails_at_once(self):
        # Spec (13 Nov 2026): the dimension is measured on every pass, or the pass fails loudly.
        for peak in (None, 0):
            with self.subTest(peak=peak):
                code, _, error, _ = self.run_case(peak)
                self.assertNotEqual(code, 0)
                self.assertIn("no anon+kernel reading of the case's scope", error)

    def test_a_case_with_a_reading_passes_and_reports_it(self):
        code, _, error, _ = self.run_case(1 << 20)
        self.assertEqual(code, 0)
        self.assertIn(f"fast-anon-kernel-peak {1 << 20}", error)

    def test_an_unmeasured_command_needs_no_reading(self):
        # Spec: data generation and scratch removal are no case's measurement.
        code, _, error, _ = self.run_case(None, measured=False)
        self.assertEqual(code, 0)
        self.assertNotIn("no anon+kernel reading", error)


if __name__ == "__main__":
    unittest.main()
