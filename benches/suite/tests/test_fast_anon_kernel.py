"""A native case's anon + kernel peak (fast.py's AnonKernelSampler and process): a case of a few
tens of milliseconds is read, and a case without a reading fails at once. F2, 13 Nov 2026: main's
ql-memberships-reuse (0.05 s) had none in four of six passes, sampled every 50 ms, and its memory
series came out incomplete only in the report."""
import os
import subprocess
import sys
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


class HugePages(unittest.TestCase):
    """Reported only (14 Nov 2026: F2's slower clients-lookups, the host's state unrecorded): a
    process's transparent huge pages, from the scope's memory.stat the sampler reads anyway."""

    def run_scope(self, stop_s, procs):
        """The sampler on a fake clock until `stop_s`; the scope holds 6 MiB of huge pages until
        0.1 s and 2 MiB after, having faulted 3, then 5 in all; `procs(t)`: the processes in the
        scope at t, (pid, name). The sampler and each look for new processes: (t, known pids)."""
        fast = load_fast()
        clock, looks = [0.0], []
        with tempfile.TemporaryDirectory() as temporary:
            group = Path(temporary)
            stat = group / "memory.stat"
            write = lambda thp, faults: stat.write_text(
                f"anon 8388608\nanon_thp {thp}\nkernel 4096\nthp_fault_alloc {faults}\nthp_collapse_alloc 1\n")
            write(6 << 20, 3)
            (group / "where").write_text(str(group))

            class Halt:
                def is_set(self):
                    return clock[0] >= stop_s

                def wait(self, seconds):
                    clock[0] += seconds
                    if clock[0] >= 0.1:
                        write(2 << 20, 5)

            def modes(where, known):
                self.assertEqual(where, group)
                looks.append((clock[0], set(known)))
                return [{"pid": pid, "name": name, "thp_enabled": 1} for pid, name in procs(clock[0])
                        if (pid, name) not in known]

            with patch.object(fast, "time", types.SimpleNamespace(monotonic=lambda: clock[0])), \
                    patch.object(fast, "thp_modes", modes):
                sampler = fast.AnonKernelSampler(group / "where")
                sampler.halt = Halt()
                sampler.run()
        return sampler.thp, looks

    def test_the_scope_s_huge_pages_at_their_peak_and_its_faults_and_collapses_so_far(self):
        # Spec (cgroup v2 memory.stat): anon_thp is the anonymous huge pages charged now;
        # thp_fault_alloc and thp_collapse_alloc count events since the cgroup began.
        thp, _ = self.run_scope(0.5, lambda t: [(7, "perf_lab")])
        self.assertEqual({k: v for k, v in thp.items() if k != "thp_enabled"},
                         {"anon_thp_peak_bytes": 6 << 20, "thp_fault_alloc": 5, "thp_collapse_alloc": 1})

    def test_each_process_new_in_the_scope_has_its_thp_enabled_with_its_pid(self):
        # Spec (Sol r1, 14 Nov 2026; fast.py's setup cases): a shell runs `setup && case` in one
        # scope; the setup (a perf_lab, its child) comes first, then the shell execs the case's
        # perf_lab under its own pid. Each process (pid and name) is kept, with when it was first
        # seen. The looks start after the dense first quarter second and come once a second.
        procs = lambda t: [(6, "perf_lab"), (7, "bash")] if t < 1.0 else [(7, "perf_lab")]
        thp, looks = self.run_scope(2.6, procs)
        self.assertEqual([(m["pid"], m["name"], m["thp_enabled"]) for m in thp["thp_enabled"]],
                         [(6, "perf_lab", 1), (7, "bash", 1), (7, "perf_lab", 1)])
        self.assertTrue(0.25 <= thp["thp_enabled"][0]["seen_s"] < 0.35, thp)
        self.assertTrue(1.0 <= thp["thp_enabled"][2]["seen_s"] < 1.4, thp)
        self.assertEqual(len(looks), 3, looks)
        self.assertGreaterEqual(looks[0][0], 0.25)
        self.assertTrue(all(b[0] - a[0] >= 1.0 for a, b in zip(looks, looks[1:])), looks)
        self.assertEqual(looks[-1][1], {(6, "perf_lab"), (7, "bash"), (7, "perf_lab")})

    @unittest.skipUnless(Path("/proc/self/status").exists()
                         and "THP_enabled" in Path("/proc/self/status").read_text(), "Linux 5.0+")
    def test_a_process_under_prctl_thp_disable_reads_thp_enabled_0(self):
        # Spec (proc(5), PR_SET_THP_DISABLE): a process that disabled THP for itself shows THP_enabled 0.
        fast = load_fast()
        child = subprocess.Popen([sys.executable, "-c", "import ctypes, time\n"
                                  "assert ctypes.CDLL(None).prctl(41, 1, 0, 0, 0) == 0\n"
                                  "print(flush=True)\ntime.sleep(5)"], stdout=subprocess.PIPE)
        try:
            child.stdout.readline()
            with tempfile.TemporaryDirectory() as temporary:
                (Path(temporary) / "cgroup.procs").write_text(f"{child.pid}\n")
                modes = fast.thp_modes(Path(temporary), set())
                again = fast.thp_modes(Path(temporary), {(m["pid"], m["name"]) for m in modes})
        finally:
            child.kill()
            child.wait()
        self.assertEqual([(m["pid"], m["thp_enabled"]) for m in modes], [(child.pid, 0)], modes)
        self.assertEqual(again, [], "a process already known (pid and name) is not recorded again")

    def test_a_case_s_record_carries_them(self):
        thp = {"anon_thp_peak_bytes": 6 << 20, "thp_fault_alloc": 5,
               "thp_enabled": [{"pid": 7, "name": "perf_lab", "thp_enabled": 1, "seen_s": 0.26}]}
        code, _, error, _ = Process().run_case(1 << 20, thp=thp)
        self.assertEqual(code, 0)
        fast = load_fast()
        observation = fast.invocation_observation({"name": "c", "metric": "whole_ms"}, {}, error, {})
        self.assertEqual(observation["thp"], thp)


class Process(unittest.TestCase):
    def run_case(self, peak, measured=True, thp=None):
        fast = load_fast()
        done = types.SimpleNamespace(returncode=0, stdout="answer", stderr="")
        with tempfile.TemporaryDirectory() as temporary, \
                patch.multiple(fast, native_dirs=lambda: {}, FAST_DATA=Path(temporary),
                               scope_argv=lambda *_: ["scope", "--"], native=lambda command, _: command,
                               end_scope=lambda *_: None, pinned_cpus=lambda: None,
                               AnonKernelSampler=lambda where, **_: types.SimpleNamespace(
                                   start=lambda: None, stop=lambda: peak, thp=thp or {})), \
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
