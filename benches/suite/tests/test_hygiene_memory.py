"""The host's memory state a pass meets (scripts/hosts/hygiene.py's memory(), which fast.py records
at a pass's start and end: host_state, host_state_finished). Reported only (14 Nov 2026: F2's
slower clients-lookups, the host's state then unrecorded): the free 2 MiB blocks and the kernel's
huge-page and compaction counters, whose difference over a pass is what it met."""
import importlib.util
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "hygiene", Path(__file__).resolve().parents[3] / "scripts" / "hosts" / "hygiene.py")
hygiene = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hygiene)


class Memory(unittest.TestCase):
    def test_free_blocks_of_2_mib_and_above_over_every_zone(self):
        # Spec (proc(5), /proc/buddyinfo): per node and zone, the free blocks of order 0 to 10;
        # order 9 is 2 MiB with 4 KiB pages. Counted by hand: DMA 1 + 3, DMA32 6 + 100, Normal 7 + 40.
        buddyinfo = ("Node 0, zone      DMA      0      0      0      0      0      0      0      0      1      1      3\n"
                     "Node 0, zone    DMA32      4      3      5      2      1      2      3      4      5      6    100\n"
                     "Node 0, zone   Normal    100     50     20     10      5      3      2      1      0      7     40\n")
        self.assertEqual(hygiene.free_blocks(buddyinfo), 157)

    def test_the_huge_page_and_compaction_counters_that_the_kernel_has(self):
        # Spec: the six counters the reading asks for, as the kernel names them; a kernel without
        # one (an older kernel) leaves it out, never 0.
        vmstat = "nr_free_pages 9\nthp_fault_alloc 5300000\nthp_fault_fallback 2\ncompact_stall 0\ncompact_fail 0\n"
        self.assertEqual(hygiene.counters(vmstat), {"thp_fault_alloc": 5300000, "thp_fault_fallback": 2,
                                                    "compact_stall": 0, "compact_fail": 0})


    def test_an_unreadable_buddyinfo_is_no_reading_and_an_all_zero_one_is_0(self):
        # Spec (Sol r1, 14 Nov 2026): a reading that couldn't be taken is absent, never 0.
        zero = "Node 0, zone   Normal" + "      0" * 11 + "\n"
        for buddyinfo, expected in ((None, None), (zero, 0)):
            with self.subTest(buddyinfo=buddyinfo):
                files = {"/proc/buddyinfo": buddyinfo}
                real = hygiene.read
                hygiene.read = lambda path, default=None: files[path] if path in files else default
                try:
                    found = hygiene.memory().get("free_2mib_blocks")
                finally:
                    hygiene.read = real
                self.assertEqual(found, expected)


if __name__ == "__main__":
    unittest.main()
