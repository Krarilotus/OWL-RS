"""A container's memory by the owner's kind: memory in use, never cache (13 Oct 2026; across
systems, `anon-kernel`: a container's anonymous plus kernel memory from its cgroup's memory.stat,
suitekit/compare.py). The Docker runtime reads it for every container; where it can't, a row says
`unknown` and why, never the `docker stats` figure in its place; and no ratio between engines is
drawn from any other kind (11 Nov 2026: `docker stats` counts active page cache, charged to the
container that touched a page first, and ranked three engines differently on two hosts).

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import csv
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from suitekit import driver, runtime, standings, versus  # noqa: E402
from suitekit.adapters import Endpoint  # noqa: E402
from suitekit.schema import Result, Writer, read  # noqa: E402

MIB = 1024 * 1024


def done(rc=0, out="", err=""):
    return subprocess.CompletedProcess(["fake"], rc, out, err)


class CgroupOfAProcess(unittest.TestCase):
    def test_the_v2_line_names_the_directory_and_nothing_else_does(self):
        """Linux's cgroup v2 (Documentation/admin-guide/cgroup-v2.rst, "/proc/$PID/cgroup"): one line
        `0::PATH`, PATH under the cgroup2 mount. A v1-only listing has no such line; a path that
        leaves the mount is no cgroup of the host's."""
        with tempfile.TemporaryDirectory() as tmp:
            listing = Path(tmp) / "cgroup"
            for text, expected in (
                    ("0::/system.slice/docker-0a1b.scope\n", Path("/sys/fs/cgroup/system.slice/docker-0a1b.scope")),
                    ("12:memory:/docker/0a1b\n11:cpu,cpuacct:/docker/0a1b\n", None),
                    ("0::/../escape\n", None)):
                with self.subTest(text=text):
                    listing.write_text(text, encoding="utf-8")
                    self.assertEqual(runtime.cgroup_dir(listing), expected)


class ContainerMemoryPeak(unittest.TestCase):
    def wait_for(self, sampler, reads):
        deadline = time.monotonic() + 5
        while sampler.reads < reads and time.monotonic() < deadline:
            time.sleep(0.005)
        self.assertGreaterEqual(sampler.reads, reads)

    def test_the_peak_is_anonymous_plus_kernel_never_the_file_cache(self):
        """Three readings by hand: anon 100 + kernel 20 = 120 (file 9,000 not counted), then 300 + 50
        = 350, then 10 + 1 = 11. The peak is 350 bytes, though the file cache was largest first."""
        with tempfile.TemporaryDirectory() as tmp:
            stat = Path(tmp) / "memory.stat"
            stat.write_text("anon 100\nfile 9000\nkernel 20\n", encoding="utf-8")
            sampler = runtime.ContainerMemory("c", locate=lambda: Path(tmp), period_s=0.001)
            try:
                self.wait_for(sampler, 1)
                stat.write_text("anon 300\nfile 1\nkernel 50\n", encoding="utf-8")
                self.wait_for(sampler, sampler.reads + 2)
                stat.write_text("anon 10\nfile 1\nkernel 1\n", encoding="utf-8")
                self.wait_for(sampler, sampler.reads + 2)
            finally:
                peak = sampler.stop()
        self.assertEqual(peak, 350)

    def test_a_container_without_a_readable_cgroup_has_no_peak_and_says_why(self):
        def locate():
            raise OSError("no /proc here")

        sampler = runtime.ContainerMemory("c", locate=locate, period_s=0.001)
        time.sleep(0.02)
        self.assertIsNone(sampler.stop())
        self.assertIn("no /proc here", sampler.why)


class DockerRows(unittest.TestCase):
    """What the Docker runtime reports for a container: anon + kernel in MiB as the peak, kind
    `anon-kernel`, the `docker stats` peak only beside it in the note; `unknown` where memory.stat
    was unreadable, with no peak in its place."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.rt = runtime.DockerRuntime(self.root / "scratch", 1, echo=Mock())
        self.rt.by_id = False  # images by tag: the pins are test_job_namespace's

    def load(self, anon_bytes, why=""):
        sampled = Mock(stop=Mock(return_value=900))
        memory = Mock(stop=Mock(return_value=anon_bytes), why=why)
        with patch.object(self.rt, "names", return_value=[]), patch.object(self.rt, "sample", return_value=sampled), \
                patch.object(self.rt, "memory", return_value=memory, create=True), \
                patch.object(runtime.subprocess, "run", side_effect=[done(), done(out="0\n")]), \
                patch.object(runtime, "run_quiet", return_value=done()):
            return self.rt.run(runtime.Spec("loader", "image", ["unused"]), self.root / "log", 5)

    def test_a_loader_reports_anon_kernel_with_the_sampled_figure_beside_it(self):
        measured = self.load(200 * MIB)
        self.assertEqual((measured.peak_mib, measured.memory_kind), (200, "anon-kernel"))
        self.assertIn("docker stats 900 MiB", measured.memory_note)

    def test_an_unreadable_memory_stat_is_unknown_never_the_sampled_figure(self):
        measured = self.load(None, why="cgroup v1")
        self.assertEqual((measured.peak_mib, measured.memory_kind), (None, "unknown"))
        self.assertIn("cgroup v1", measured.memory_note)
        self.assertIn("docker stats 900 MiB", measured.memory_note)

    def test_a_server_stops_with_its_anon_kernel_peak_and_kind(self):
        sampled = Mock(stop=Mock(return_value=900))
        memory = Mock(stop=Mock(return_value=300 * MIB), why="")
        with patch.object(self.rt, "names", return_value=[]), patch.object(self.rt, "sample", return_value=sampled), \
                patch.object(self.rt, "memory", return_value=memory, create=True), \
                patch.object(runtime.subprocess, "run", return_value=done()):
            server = self.rt.start(runtime.Spec("server", "image", ["unused"]), self.root / "log")
        with patch.object(runtime, "run_quiet", return_value=done(out="1.5GiB / 31GiB\n")):
            peak = self.rt.stop(server)
        self.assertEqual(peak, 300)
        kind, note = self.rt.last_memory
        self.assertEqual(kind, "anon-kernel")
        self.assertIn("docker stats 1536 MiB", note)


class DriverRows(unittest.TestCase):
    """The driver writes the runtime's kind on a serve row, and a serve row of kind `unknown` without
    a peak (it said nothing before: a row without a peak wasn't written)."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()

    def serve_rows(self, peak, memory):
        args = driver.arguments(["--runtime", "process", "--data", str(self.root), "--results", str(self.root / "results")])
        suite = driver.Suite(args, {}, {}, self.root)
        suite.runtime.echo = Mock()
        suite.say, suite.emit, suite.writes = Mock(), Mock(), Mock()
        suite.publish = lambda key: "fixture"

        def stop(ctx):
            suite.runtime.last_memory = memory
            return peak

        plan = SimpleNamespace(workload="fixture", tier="tiny", kind="writes", mounts=[], small=True, services=[])
        system = Mock(serve=Mock(return_value=Endpoint("http://fixture")), stop=Mock(side_effect=stop))
        suite.cycle(plan, "nrese", system, "none", 1)
        return [c.kwargs for c in suite.emit.call_args_list if c.kwargs.get("task") == "serve"]

    def test_a_serve_row_carries_the_kind_and_the_sampled_note(self):
        rows = self.serve_rows(300, ("anon-kernel", "docker stats 900 MiB, page cache included"))
        self.assertEqual([(r["peak_mib"], r["memory_kind"]) for r in rows], [(300, "anon-kernel")])
        self.assertIn("docker stats 900 MiB", rows[0]["note"])

    def test_an_unknown_kind_is_written_without_a_peak(self):
        rows = self.serve_rows(None, ("unknown", "memory.stat unreadable: cgroup v1"))
        self.assertEqual([(r["peak_mib"], r["memory_kind"]) for r in rows], [("", "unknown")])

    def test_the_row_schema_keeps_the_kind(self):
        with tempfile.TemporaryDirectory() as tmp:
            writer = Writer(Path(tmp) / "results.csv")
            writer.write(Result(date="d", host="h", runtime="docker", system="s", version="-", publish="free",
                                workload="w", tier="t", regime="none", run=1, task="serve", peak_mib=12,
                                memory_kind="anon-kernel"))
            self.assertEqual(read(Path(tmp) / "results.csv")[0]["memory_kind"], "anon-kernel")


def suite_run(directory: Path, kinds: dict[str, str]):
    """A run of tier `c~plain`: NRESE and QLever, three runs, one query each, and a serve peak of
    NRESE 400 MiB against QLever's 200 MiB, each of the kind given (empty: no kind column value).
    Written as CSV by hand, so the rows read the same whatever the schema module's fields."""
    header = ["date", "host", "runtime", "system", "version", "publish", "workload", "tier", "regime", "run",
              "task", "item", "repeat", "status", "ms", "peak_mib", "rows", "bytes", "note", "cache", "order",
              "recall", "memory_kind"]
    rows = []
    for system, peak in (("nrese", 400), ("qlever", 200)):
        for n in (1, 2, 3):
            base = dict(date="d", host="h", runtime="docker", system=system, version="-", publish="free",
                        workload="fast", tier="c~plain", regime="none", run=n, status="ok",
                        cache="off" if system == "nrese" else "-", order="-")
            rows.append({**base, "task": "load", "ms": "1000.00"})
            rows += [{**base, "task": "query", "item": "q1", "repeat": repeat, "ms": "10.00", "rows": 1}
                     for repeat in range(1, 4)]
            rows.append({**base, "task": "serve", "peak_mib": peak, "memory_kind": kinds[system]})
    with open(directory / "results.csv", "w", newline="", encoding="utf-8") as f:
        out = csv.DictWriter(f, header)
        out.writeheader()
        out.writerows(rows)


class NoRankingOnOtherKinds(unittest.TestCase):
    def entry(self, kinds):
        with tempfile.TemporaryDirectory() as tmp:
            suite_run(Path(tmp), kinds)
            return {e["system"]: e for e in versus.versus(Path(tmp))}["qlever"]

    def test_anon_kernel_on_both_sides_gives_the_ratio(self):
        """NRESE 400 MiB against 200 MiB, both anon-kernel: 2.0."""
        memory = self.entry({"nrese": "anon-kernel", "qlever": "anon-kernel"})["memory"]
        self.assertEqual((memory["ratio"], memory["kind"]), (2.0, "anon-kernel"))

    def test_sampled_or_mixed_kinds_give_no_ratio_and_say_so(self):
        """`docker stats` rows (the kind of every Docker row before the kind column: empty) and a
        mix of kinds rank nothing; the entry names the kinds it refused."""
        for kinds in ({"nrese": "", "qlever": ""}, {"nrese": "sampled", "qlever": "sampled"},
                      {"nrese": "anon-kernel", "qlever": "sampled"}):
            with self.subTest(kinds=kinds):
                memory = self.entry(kinds)["memory"]
                self.assertNotIn("ratio", memory)
                self.assertIn("sampled", memory["refused"])

    def test_the_standings_show_a_refused_memory_cell_as_no_ratio(self):
        entry = {"case": "c", "semantics": "plain", "system": "qlever", "answers": "ok", "timed": "1/1",
                 "verdict": "tie", "time": {"ratio": 1.0, "ci": [0.9, 1.1]},
                 "memory": {"kind": "sampled", "refused": "no ratio between engines on sampled"}}
        records = [{"id": "r", "started": "2026-11-11", "host": "h", "commit": "c", "versus": [entry]}]
        text = standings.render(records, {"nrese": {}, "qlever": {"publish": "free"}}, Path("."))
        self.assertIn("no ratio (sampled)", text)


if __name__ == "__main__":
    unittest.main()
