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
import errno
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

BENCHES = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(BENCHES / "suite"))
sys.path.insert(0, str(BENCHES / "reasoning" / "datalog"))

from suitekit import adapters, compare, driver, runtime, standings, versus  # noqa: E402
from suitekit.adapters import Endpoint, Step  # noqa: E402
from suitekit.schema import FIELDS, Result, Writer, read  # noqa: E402
from suitekit.summary import summarise  # noqa: E402

MIB = 1024 * 1024


def done(rc=0, out="", err=""):
    return subprocess.CompletedProcess(["fake"], rc, out, err)


def retrying(action, *args):
    """An action on a file a sampler may hold open this instant (Windows refuses it then)."""
    deadline = time.monotonic() + 5
    while True:
        try:
            return action(*args)
        except PermissionError:
            if time.monotonic() > deadline:
                raise
            time.sleep(0.001)


def put(stat: Path, text: str):
    """memory.stat replaced whole, as the kernel's reads never see it half-written."""
    new = stat.with_name("memory.stat.new")
    new.write_text(text, encoding="utf-8")
    retrying(os.replace, new, stat)


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
                put(stat, "anon 300\nfile 1\nkernel 50\n")
                self.wait_for(sampler, sampler.reads + 2)
                put(stat, "anon 10\nfile 1\nkernel 1\n")
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

    def test_a_sampler_that_breaks_mid_run_has_no_peak_and_says_why(self):
        """Spec: the master's go of Wed 11 Nov, kind `unknown` instead of a quiet fallback. A reading of
        120 by hand, then a memory.stat the sampler can't parse (kernel `x`): the readings after the
        first are missing, so 120 is no peak; there is none, and why names the fault."""
        with tempfile.TemporaryDirectory() as tmp:
            stat = Path(tmp) / "memory.stat"
            stat.write_text("anon 100\nkernel 20\n", encoding="utf-8")
            sampler = runtime.ContainerMemory("c", locate=lambda: Path(tmp), period_s=0.001)
            try:
                self.wait_for(sampler, 1)
                put(stat, "anon 300\nkernel x\n")
                sampler.thread.join(timeout=5)
            finally:
                peak = sampler.stop()
        self.assertIsNone(peak)
        self.assertIn("ValueError", sampler.why)

    def test_a_reading_that_fails_while_the_cgroup_stands_leaves_no_peak(self):
        """Spec: no quiet fallback (the master's go, Wed 11 Nov). A reading of 120 by hand, then, while
        the cgroup's directory still stands (the container runs on), a memory.stat that is gone or reads
        without `anon`: the readings after the first are missing, so 120 is no peak."""
        for broken in ("gone", "no anon"):
            with self.subTest(broken=broken), tempfile.TemporaryDirectory() as tmp:
                group = Path(tmp)
                stat = group / "memory.stat"
                stat.write_text("anon 100\nkernel 20\n", encoding="utf-8")
                sampler = runtime.ContainerMemory("c", locate=lambda: group, period_s=0.001)
                try:
                    self.wait_for(sampler, 1)
                    if broken == "gone":
                        retrying(stat.unlink)
                    else:
                        put(stat, "file 5\n")
                    sampler.thread.join(timeout=5)
                finally:
                    peak = sampler.stop()
                self.assertIsNone(peak)
                self.assertTrue(sampler.why)

    def test_a_cgroup_it_cannot_inspect_is_no_evidence_that_the_container_ended(self):
        """Only a cgroup directory that is gone (ENOENT) ends the readings with their peak standing. A
        reading of 120, then a reading and a look at the directory that both fail otherwise (no access,
        an I/O error): the container may run on, so 120 is no peak. (Red on Linux, where a failed stat
        made `Path.exists` false; Windows' exists doesn't go through os.stat.)"""
        for failure in (PermissionError(errno.EACCES, "no access"), OSError(errno.EIO, "I/O error")):
            with self.subTest(failure=failure.strerror), tempfile.TemporaryDirectory() as tmp:
                group = Path(tmp)
                reads, real_stat = [], os.stat

                def read_text(path, *args, **kwargs):
                    reads.append(path)
                    if len(reads) == 1:
                        return "anon 100\nkernel 20\n"
                    raise failure

                def stat_of(path, *args, **kwargs):
                    if Path(path) == group:
                        raise failure
                    return real_stat(path, *args, **kwargs)

                with patch.object(Path, "read_text", read_text), patch.object(os, "stat", stat_of):
                    sampler = runtime.ContainerMemory("c", locate=lambda: group, period_s=0.001)
                    sampler.thread.join(timeout=5)
                    peak = sampler.stop()
                self.assertIsNone(peak)
                self.assertIn(failure.strerror, sampler.why)

    def test_a_container_whose_cgroup_is_removed_keeps_the_peak_read_before(self):
        """Linux removes a container's cgroup directory once its processes are gone: the readings
        before it are the container's whole life. 120 by hand, then the directory removed: 120."""
        with tempfile.TemporaryDirectory() as tmp:
            group = Path(tmp) / "docker-0a1b.scope"
            group.mkdir()
            (group / "memory.stat").write_text("anon 100\nkernel 20\n", encoding="utf-8")
            sampler = runtime.ContainerMemory("c", locate=lambda: group, period_s=0.001)
            try:
                self.wait_for(sampler, 1)
                retrying(shutil.rmtree, group)
                sampler.thread.join(timeout=5)
            finally:
                peak = sampler.stop()
        self.assertEqual(peak, 120)


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
        self.assertEqual(measured.peak_mib, 200)
        self.assertEqual(measured.memory_kind, "anon-kernel")
        self.assertIn("docker stats 900 MiB", measured.memory_note)

    def test_an_unreadable_memory_stat_is_unknown_never_the_sampled_figure(self):
        measured = self.load(None, why="cgroup v1")
        self.assertIsNone(measured.peak_mib)
        self.assertEqual(measured.memory_kind, "unknown")
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


SAMPLED_NOTE = "docker stats 900 MiB, page cache included, not compared"


class LoadsAndKitsKeepTheKind(unittest.TestCase):
    """Every row written from a Docker peak carries that peak's kind and note, not only the driver's:
    a load that stops its server (Virtuoso, RDF4J), a load whose server serves on (AnzoGraph, in
    memory) and a kit's own rows (the Datalog kit's maintenance). Without the kind a reader takes an
    anon + kernel peak for a `docker stats` one (compare.memory_kinds)."""

    def docker(self, peak_mib):
        rt = Mock(last_memory=("", ""))
        rt.url.return_value = "http://fixture"
        rt.exec.return_value = (0, "ok")

        def stop(server):
            rt.last_memory = ("anon-kernel", SAMPLED_NOTE)
            return peak_mib
        rt.stop.side_effect = stop
        return rt

    def test_a_load_that_stops_its_server_carries_the_kind_of_its_peak(self):
        for adapter in (adapters.Virtuoso(), adapters.Rdf4j()):
            with self.subTest(adapter=adapter.key), tempfile.TemporaryDirectory() as tmp:
                ctx = SimpleNamespace(runtime=self.docker(120), logs=Path(tmp), dry=False, timeout_s=5,
                                      name=lambda s: s, listen=lambda port, **kw: port)
                adapter.server = object()
                with patch.object(adapter, "spec", return_value=runtime.Spec("load", "image", [])), \
                        patch.object(adapter, "start", return_value=Endpoint("http://q", "http://u")), \
                        patch.object(adapters, "server_log", return_value="Server online at 1111"), \
                        patch.object(adapters, "put", return_value=201):
                    measured = adapter.load(ctx, "store", [], "none").measured
                self.assertEqual(measured.peak_mib, 120)
                self.assertEqual(measured.memory_kind, "anon-kernel")
                self.assertIn("docker stats 900 MiB", measured.memory_note)

    def test_a_load_into_a_running_server_reads_anon_kernel_not_docker_stats(self):
        """AnzoGraph's load ends with its server still serving: its peak so far, 120 MiB anon + kernel,
        while `docker stats` read 900 MiB (in the note only)."""
        adapter = adapters.Anzograph()
        with tempfile.TemporaryDirectory() as tmp:
            ctx = SimpleNamespace(runtime=self.docker(None), logs=Path(tmp), dry=False, timeout_s=5,
                                  name=lambda s: s, listen=lambda port, **kw: port,
                                  data=SimpleNamespace(source="data"), extra=[])
            adapter.server = runtime.Server(runtime.Spec("serve", "image", []), sampler=SimpleNamespace(peak=900),
                                            memory=SimpleNamespace(peak=120 * MIB, why=""))
            with patch.object(adapter, "start", return_value=Endpoint("http://q", "http://q")), \
                    patch.object(adapter, "images", return_value=["image"]):
                measured = adapter.load(ctx, "store", [], "none").measured
        self.assertEqual(measured.peak_mib, 120)
        self.assertEqual(measured.memory_kind, "anon-kernel")
        self.assertIn("docker stats 900 MiB", measured.memory_note)

    def test_a_kit_writes_the_kind_on_its_load_and_serve_rows(self):
        import maintain
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            rt = self.docker(300)
            rt.scratch = str(root / "scratch")
            rt.new_store.return_value = "store"
            loaded = Step(runtime.Measured(1000.0, 200, 0, False, "anon-kernel", SAMPLED_NOTE))

            def serve(nrese, ctx, store, regime):
                nrese.server = object()
                return Endpoint("http://q", "http://u")

            writer = Mock()
            args = SimpleNamespace(stamp="s", timeout_s=5, date="d", host="h", cpus=1, size=1, samples=0)
            with patch.object(maintain.RulesNrese, "load", return_value=loaded), \
                    patch.object(maintain.RulesNrese, "serve", autospec=True, side_effect=serve), \
                    patch.object(maintain.datalog, "load_program", return_value={}), \
                    patch.object(maintain.P, "to_n3", return_value=""), \
                    patch.object(maintain.datalog, "data_dir", return_value=root), \
                    patch.object(maintain.datalog, "workload", return_value="w"), \
                    patch.object(maintain.datalog, "tier", return_value="t"), \
                    patch.object(maintain, "closure_size", return_value=10), \
                    patch.object(maintain, "samples", return_value=[]):
                maintain.run_case(args, {"name": "case"}, 1, rt, writer, root / "results")
        rows = {r.task: r for r in (c.args[0] for c in writer.write.call_args_list) if r.task in ("load", "serve")}
        self.assertEqual([(rows[t].peak_mib, rows[t].memory_kind) for t in ("load", "serve")],
                         [(200, "anon-kernel"), (300, "anon-kernel")])
        self.assertIn("docker stats 900 MiB", rows["load"].note)
        self.assertIn("docker stats 900 MiB", rows["serve"].note)


def row(**fields) -> Result:
    return Result(**{**dict(date="d", host="h", runtime="docker", system="s", version="-", publish="free",
                            workload="w", tier="t", regime="none", run=1, task="serve"), **fields})


class ResultsFromAnOlderSchema(unittest.TestCase):
    """A results.csv written before `memory_kind` (its header the schema's less that last column).
    Every reader reads results.csv only (versus, ledger, compare, the driver's kit import): a row
    written anywhere else is read by nobody."""

    def older(self, root: Path) -> Path:
        path = root / "results.csv"
        with open(path, "w", newline="", encoding="utf-8") as f:
            out = csv.DictWriter(f, FIELDS[:-1])
            out.writeheader()
            out.writerow({k: v for k, v in vars(row(peak_mib=7)).items() if k != "memory_kind"})
        return path

    def test_a_row_without_a_kind_is_appended_where_readers_look(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = self.older(Path(tmp))
            Writer(path).write(row(runtime="process", peak_mib=5))
            self.assertEqual([r["peak_mib"] for r in read(path)], ["7", "5"])
            self.assertEqual(sorted(p.name for p in Path(tmp).iterdir()), ["results.csv"])

    def test_a_row_with_a_kind_is_refused_rather_than_written_where_no_reader_looks(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = self.older(Path(tmp))
            with self.assertRaises(ValueError):
                Writer(path).write(row(peak_mib=5, memory_kind="anon-kernel"))
            self.assertEqual([r["peak_mib"] for r in read(path)], ["7"])
            self.assertEqual(sorted(p.name for p in Path(tmp).iterdir()), ["results.csv"])

    def test_compare_draws_no_verdict_from_peaks_of_two_kinds_pooled(self):
        """Such a file holds a Docker serve peak (sampled: it predates the kind); a later run there
        appends a process one (lifetime), base 100 MiB, candidate 400 MiB. Each side pools both
        kinds: compare names them and draws no ratio and no regression from the mixture."""
        with tempfile.TemporaryDirectory() as tmp:
            files = []
            for name, peak in (("base", 100), ("new", 400)):
                (Path(tmp) / name).mkdir()
                path = self.older(Path(tmp) / name)
                Writer(path).write(row(runtime="process", run=2, peak_mib=peak))
                files.append(path)
            out = compare.Out()
            compare.compare_pairs(summarise(read(files[0])), summarise(read(files[1])), 0.1, 2, out,
                                  (compare.memory_kinds([files[0]]), compare.memory_kinds([files[1]])))
        text = "\n".join(out.lines["Memory, by kind"])
        self.assertEqual((bool(re.search(r"\d\.\d\dx", text)), out.regressions), (False, 0))
        self.assertIn("lifetime/sampled", text)


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

    def test_the_standings_rank_a_stored_ratio_only_of_anon_kernel(self):
        """The ledger keeps entries written before the refusal ({ratio 2, kind sampled}); the
        standings render them as stored, so the rule holds there too: no ratio on sampled."""
        for kind, cell in (("sampled", "| no ratio (sampled) |"), ("anon-kernel", "| 2 (anon-kernel) |")):
            with self.subTest(kind=kind):
                entry = {"case": "c", "semantics": "plain", "system": "qlever", "answers": "ok", "timed": "1/1",
                         "verdict": "tie", "time": {"ratio": 1.0, "ci": [0.9, 1.1]},
                         "memory": {"ratio": 2, "kind": kind}}
                records = [{"id": "r", "started": "2026-11-10", "host": "h", "commit": "c", "versus": [entry]}]
                line = next(x for x in standings.render(records, {"nrese": {}, "qlever": {"publish": "free"}},
                                                        Path(".")).splitlines() if x.startswith("| c |"))
                self.assertIn(cell, line)

    def test_compare_draws_no_verdict_from_sampled_memory(self):
        """Two runs of one system, serve peaks 100 then 400 MiB. Of kind sampled (docker stats, active
        page cache included), shown but no ratio and no regression; of kind anon-kernel, compared:
        4.00x, larger, one regression."""
        base = dict(date="d", host="h", runtime="docker", system="nrese", version="-", publish="free",
                    workload="w", tier="t", regime="none", run="1", task="serve", item="", repeat="",
                    status="ok", ms="", rows="", bytes="", note="", cache="-", order="-", recall="")
        key = ("w", "t", "nrese", "serve")
        for kind, ratio, regressions in (("sampled", False, 0), ("anon-kernel", True, 1)):
            with self.subTest(kind=kind):
                out = compare.Out()
                compare.compare_pairs(summarise([{**base, "peak_mib": "100", "memory_kind": kind}]),
                                      summarise([{**base, "peak_mib": "400", "memory_kind": kind}]),
                                      0.1, 2, out, ({key: kind}, {key: kind}))
                text = "\n".join(out.lines["Memory, by kind"])
                self.assertEqual(("4.00x" in text, out.regressions), (ratio, regressions))


if __name__ == "__main__":
    unittest.main()
