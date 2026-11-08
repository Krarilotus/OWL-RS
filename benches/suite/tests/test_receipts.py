"""Cell attempts and receipts (suitekit/receipts.py): what a cell ran, under the attempt written
before its work, kept per cell by the ledger. Specified by Harness notes
2026-11-08-coverage-refresh/design.md r2, its doubt review (F1-F8) and review cell-receipts r1
(R1-R6): an interrupted attempt stays incomplete and no older receipt stands in for it (F1, R2),
answers are the attempt's own (F2, R3), a licensed system's results stay local whatever its
outcome (R1), images are the ones run (R4), the code is everything the repository holds for it
(F3, R5), and what can't be evidenced is said unknown, never an empty map (R6)."""
from __future__ import annotations

import contextlib
import csv
import hashlib
import io
import json
import os
import subprocess
import sys
import tempfile
import threading
import tomllib
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from suitekit import driver, ledger, manifest, receipts, schema  # noqa: E402
from suitekit.workloads import Plan  # noqa: E402

HEADER = ("date,host,runtime,system,version,publish,workload,tier,regime,run,task,item,repeat,status,ms,"
          "peak_mib,rows,bytes,note,cache,order,recall\n")


def row(system, run=1, task="load", item="", status="ok", rows="", publish="free", tier="1"):
    return (f"2026-11-08T04:00,h,docker,{system},-,{publish},lubm,{tier},none,{run},{task},{item},"
            f"{0 if task == 'query' else ''},{status},10,,{rows},,,-,-,\n")


def invocation(started, commit, cells):
    return {"started": started, "finished": started, "argv": [], "machine": {},
            "git": {"commit": commit, "dirty": 0}, "protocol": {"runs": 1, "query_runs": 1, "cache": "cold",
                                                               "order": "fixed", "seed": 1, "timeout_s": 1,
                                                               "query_timeout_s": 1, "runtime": "docker"},
            "cells": cells}


def done(attempt, started, commit, **fields):
    return {"attempt": attempt, "started": started, "complete": True, "commit": commit, "dirty": 0,
            "outcome": {"outcome": "ok"}, "answers": "ok", **fields}


def files(directory: Path) -> dict[str, bytes]:
    """A directory's files' bytes; not its run's lock (Windows' lock is a byte lock: unreadable
    while held), which holds nothing."""
    return {p.name: p.read_bytes() for p in directory.iterdir() if p.name != "manifest.lock"}


class Crash:
    """What a host crash leaves of a results directory, from the calls its manifest writers made:
    a file's bytes once its fsync returned, under a name already on disk; a new name or a rename
    once its directory was synced, unless the directory sync makes nothing durable (`no_dirsync`:
    the suite's on Windows before review r3, a share that can't flush): then it is lost. What is
    there at the start counts as long on disk. `fail` fails the fsync publishing an attempt."""

    def __init__(self, directory: Path, no_dirsync: bool):
        self.directory, self.no_dirsync, self.names, self.pending = directory, no_dirsync, {}, {}
        self.disk = files(directory)

    def patches(self, fail=False):
        real_open, real_fsync, real_replace = open, os.fsync, os.replace

        def opened(path, *args, **kwargs):
            f = real_open(path, *args, **kwargs)
            self.names[f.fileno()] = Path(path).name
            return f

        def fsync(fd):
            name = self.names.get(fd)
            data = (self.directory / name).read_bytes() if name else b""
            if fail and b'"complete": false' in data:
                raise OSError("fsync failed")
            real_fsync(fd)
            if name:
                (self.disk if name in self.disk else self.pending)[name] = data

        def replace(source, target):
            real_replace(source, target)
            self.pending[Path(target).name] = self.pending.pop(Path(source).name, b"")

        def synced(directory):
            if not self.no_dirsync:
                self.disk.update(self.pending)
                self.pending.clear()
        return [patch("suitekit.manifest.open", opened, create=True), patch("suitekit.manifest.os.fsync", fsync),
                patch("suitekit.manifest.os.replace", replace), patch("suitekit.manifest.sync_directory", synced)]

    def reopen(self, directory: Path) -> Path:
        directory.mkdir()
        for name, data in self.disk.items():
            (directory / name).write_bytes(data)
        return directory


class Ledger(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.run = self.root / "run"
        self.run.mkdir()

    def record(self, lines, invocations, verdicts=None):
        (self.run / "results.csv").write_text(HEADER + "".join(lines), encoding="utf-8")
        (self.run / "manifest.json").write_text(json.dumps({"schema": 1, "invocations": invocations}), encoding="utf-8")
        # Each cell there was attempted first: its record in the attempt journal, in order.
        (self.run / "attempts.jsonl").write_text("".join(
            "\n" + json.dumps({"cell": c, "attempt": a["attempt"], "started": a["started"], "complete": False})
            for i in invocations for c, a in i["cells"].items()), encoding="utf-8")
        if verdicts is not None:
            (self.run / "answers.json").write_text(json.dumps({"verdicts": verdicts}), encoding="utf-8")
        with patch("suitekit.versus.versus", return_value=[]), patch("suitekit.cases.load", return_value=[]):
            return ledger.from_results(self.run, self.root)

    def pairs(self, *args, **kwargs):
        return {p["system"]: p for p in self.record(*args, **kwargs)["pairs"]}

    def test_each_cell_keeps_its_own_invocation_s_receipt(self):
        """F8: cell A ran at C1, cell B was added later at C2 in the same directory. Each cell's
        receipt is its own invocation's, and survives the record's TOML, an unknown as it was."""
        a = done("a1", "2026-11-08T01:00:00.000001", "c1" * 20, code={"benches": "t1"},
                 data={"/rggs/x.ttl": receipts.unknown("outside the datasets: not hashed")})
        b = done("b2", "2026-11-08T02:00:00.000001", "c2" * 20, code={"benches": "t2"}, data={})
        invocations = [invocation("2026-11-08T01:00:00", "c1" * 20, {"lubm/1/oxigraph": a}),
                       invocation("2026-11-08T02:00:00", "c2" * 20, {"lubm/1/jena": b})]
        pairs = self.pairs([row("oxigraph"), row("jena")], invocations)
        self.assertEqual(pairs["oxigraph"]["receipt"]["commit"], "c1" * 20)
        self.assertEqual(pairs["jena"]["receipt"]["commit"], "c2" * 20)
        back = {p["system"]: p for p in tomllib.loads(ledger.dump(self.record([row("oxigraph"), row("jena")],
                                                                              invocations)))["pairs"]}
        self.assertEqual(back["oxigraph"]["receipt"]["data"], {"/rggs/x.ttl": {"unknown": "outside the datasets: not hashed"}})

    def test_an_unfinished_newer_attempt_is_the_cell_s_evidence_never_an_older_receipt(self):
        """R2: I1 completed A and B; I2 attempted A again in the same directory and died before its
        receipt. A's evidence is I2's incomplete attempt; B keeps I1's receipt."""
        i1 = invocation("2026-11-08T01:00:00", "c1" * 20,
                        {"lubm/1/oxigraph": done("a1", "2026-11-08T01:00:01.000000", "c1" * 20),
                         "lubm/1/jena": done("b1", "2026-11-08T01:00:01.000000", "c1" * 20)})
        i2 = invocation("2026-11-08T02:00:00", "c2" * 20,
                        {"lubm/1/oxigraph": {"attempt": "a2", "started": "2026-11-08T02:00:01.000000", "complete": False}})
        pairs = self.pairs([row("oxigraph"), row("oxigraph", run=1), row("jena")], [i1, i2])
        self.assertEqual(pairs["oxigraph"]["receipt"], {"attempt": "a2", "started": "2026-11-08T02:00:01.000000",
                                                        "complete": False})
        self.assertEqual(pairs["jena"]["receipt"]["attempt"], "b1")
        self.assertTrue(pairs["jena"]["receipt"]["complete"])

    def test_a_torn_attempt_record_never_takes_the_next_one_with_it(self):
        """Guard (attempt journal): a crash mid-append tears its record, whose work never started;
        it is skipped, and the next invocation's attempt still stands."""
        (self.run / "attempts.jsonl").write_text('\n{"cell": "lubm/1/x", "attem', encoding="utf-8")
        a2 = {"attempt": "a2", "started": "t", "complete": False}
        manifest.append_attempts(self.run, {"lubm/1/x": a2})
        self.assertEqual(manifest.read_journal(self.run), [{"cell": "lubm/1/x", **a2}])

    def test_answers_are_the_attempt_s_own_never_the_directory_s(self):
        """R3: I1 measured A with full answers, ok; I2 measured A again with --answers off. The
        directory's answers.json is still I1's; A's evidence says not captured, and the pair has
        no answer verdict of the directory's."""
        verdicts = [{"workload": "lubm", "tier": "1", "system": "oxigraph", "query": "q1", "verdict": "ok"}]
        i1 = invocation("2026-11-08T01:00:00", "c1" * 20, {"lubm/1/oxigraph": done("a1", "2026-11-08T01:00:01", "c1" * 20)})
        i2 = invocation("2026-11-08T02:00:00", "c1" * 20, {"lubm/1/oxigraph": done(
            "a2", "2026-11-08T02:00:01", "c1" * 20, answers="not captured")})
        pairs = self.pairs([row("oxigraph")], [i1, i2], verdicts)
        self.assertEqual(pairs["oxigraph"]["receipt"]["answers"], "not captured")
        self.assertNotIn("answers", pairs["oxigraph"])

    def test_a_licensed_system_s_results_stay_local_whatever_its_outcome(self):
        """R1: GraphDB ran with --answers full and its input missing: only skipped rows, its answers
        `incomplete`. Neither its outcome nor its answers are in the record, and the record says it
        holds a licensed system."""
        verdicts = [{"workload": "lubm", "tier": "1", "system": "graphdb", "query": "q1", "verdict": "incomplete"}]
        cells = {"lubm/1/graphdb": done("g1", "2026-11-08T01:00:01", "c1" * 20, answers="incomplete",
                                        outcome={"outcome": "skipped"}, code={"benches": "t1"})}
        text = ledger.dump(self.record([row("graphdb", status="skipped", publish="permission")],
                                       [invocation("2026-11-08T01:00:00", "c1" * 20, cells)], verdicts))
        record = tomllib.loads(text)
        graphdb = next(p for p in record["pairs"] if p["system"] == "graphdb")
        self.assertEqual(graphdb["receipt"]["code"], {"benches": "t1"})
        self.assertNotIn("answers", graphdb["receipt"])
        self.assertNotIn("outcome", graphdb["receipt"])
        self.assertNotIn("answers", graphdb)
        self.assertNotIn("incomplete", text)
        self.assertEqual(record["publishable"], "free systems only")


class Driver(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        (self.root / "data").mkdir()

    def suite(self, runs=2, skip_build=True, results="results"):
        args = driver.arguments(["--runtime", "process", "--data", str(self.root / "data"), "--results",
                                 str(self.root / results), "--systems", "x", "--workloads", "w",
                                 "--runs", str(runs), *(["--skip-build"] if skip_build else [])])
        suite = driver.Suite(args, {"x": {"capabilities": ["load"], "publish": "free"}},
                             {"w": {"needs": ["load"], "state": "ready", "kit": ""}}, self.root)
        suite.say = Mock()
        suite.manifest = driver.Manifest(suite.results, self.root, [], args, "process", {})
        self.addCleanup(getattr(suite.manifest, "close", lambda: None))  # its run's end
        return suite

    def main(self, results, run=None):
        """suite.py run into `results`, its Suite.run replaced by `run`: exit code, stderr."""
        err = io.StringIO()
        with patch.object(driver.Suite, "run", side_effect=run, return_value=0) as ran, \
                contextlib.redirect_stderr(err), contextlib.redirect_stdout(io.StringIO()):
            code = driver.main(["--runtime", "process", "--data", str(self.root / "data"), "--results",
                                str(results), "--systems", "x", "--workloads", "w"],
                               {"x": {"capabilities": ["load"], "publish": "free"}},
                               {"w": {"needs": ["load"], "state": "ready", "kit": ""}}, self.root)
        return code, err.getvalue(), ran.called

    def cells(self, suite):
        return json.loads((suite.results / "manifest.json").read_text(encoding="utf-8"))["invocations"][-1].get("cells", {})

    def journal(self, suite):
        lines = (suite.results / "attempts.jsonl").read_text(encoding="utf-8").splitlines()
        return {a["cell"]: a for a in map(json.loads, filter(None, lines)) if "cell" in a}  # not invocations'

    def system(self, images=()):
        # definition_paths: what the reviewed head's adapters had (r1), so its red reaches the verdicts.
        return Mock(regimes={"none": ""}, images=Mock(return_value=list(images)), builds=Mock(return_value=[]),
                    supports=Mock(return_value=None), definition_paths=())

    def run_tiers(self, suite, tiers, system, stop=lambda plan, run: False):
        def cycle(plan, key, system, regime, run):
            if stop(plan, run):
                raise KeyboardInterrupt
            suite.emit(dict(date=suite.date, host="h", runtime="process", system=key, version="-", publish="free",
                            workload=plan.workload, tier=plan.tier, regime="none", run=run), task="load", ms=5)

        with patch.object(driver, "plans", return_value=tiers), patch.object(driver, "adapter", return_value=system), \
                patch.object(suite, "cycle", side_effect=cycle), patch.object(suite, "build_harness", return_value=True), \
                patch.object(suite, "ensure_data", return_value=None), patch.object(suite, "sif_missing", return_value=None):
            suite.run()

    def test_an_interrupted_tier_s_attempt_stays_incomplete_and_a_finished_one_has_its_receipt(self):
        """F1, R2: tier 1 ran both repetitions; tier 2 was interrupted in its second. Tier 1's cell
        has its receipt, with its own outcome; tier 2's attempt was written first and stays incomplete."""
        suite = self.suite()
        (self.root / "data" / "a.nt").write_bytes(b"<a> <b> <c> .\n")
        with self.assertRaises(KeyboardInterrupt):
            self.run_tiers(suite, [Plan("w", "1", inputs=["/data/a.nt"]), Plan("w", "2")], self.system(),
                           stop=lambda plan, run: plan.tier == "2" and run == 2)
        cells, journal = self.cells(suite), self.journal(suite)
        self.assertEqual(sorted(journal), ["w/1/x", "w/2/x"])
        self.assertEqual(sorted(cells), ["w/1/x"])
        self.assertEqual(sorted(journal["w/2/x"]), ["attempt", "cell", "complete", "started"])
        receipt = cells["w/1/x"]
        self.assertEqual(receipt["attempt"], journal["w/1/x"]["attempt"])
        self.assertTrue(receipt["complete"])
        self.assertEqual(receipt["outcome"]["outcome"], "ok")
        self.assertEqual(receipt["outcome"]["runs"], 2)
        self.assertEqual(receipt["answers"], "not captured")  # --answers off
        self.assertEqual(receipt["data"], {"/data/a.nt": hashlib.sha256(b"<a> <b> <c> .\n").hexdigest()})
        self.assertNotEqual(receipt["attempt"], journal["w/2/x"]["attempt"])

    def test_a_receipt_names_the_image_ids_the_run_pinned(self):
        """R4: the run pinned tag T to A; T was moved to B since. The containers ran A (by its pinned
        id), and so does the receipt; an image the pin map lacks is unknown."""
        suite = self.suite(runs=1)
        suite.manifest.update(images={"img:t": "sha256:B"})
        with patch.object(driver.ns, "pinned", return_value={"img:t": "sha256:A"}):
            self.run_tiers(suite, [Plan("w", "1")], self.system(images=["img:t", "img:u"]))
        images = self.cells(suite)["w/1/x"]["images"]  # each with its source since F5 (below)
        self.assertEqual({i: v["id"] for i, v in images.items()},
                         {"img:t": "sha256:A", "img:u": {"unknown": "not in the run's pin map"}})

    def test_a_run_s_answers_go_into_its_finished_attempts_only(self):
        """R3: the run's own answer verdicts, each cell's worst, into its finished attempts; an
        unfinished attempt keeps none."""
        suite = self.suite(runs=1)
        suite.answers = Mock(verdicts=Mock(return_value=[
            {"workload": "w", "tier": "1", "system": "x", "verdict": "ok"},
            {"workload": "w", "tier": "1", "system": "x", "verdict": "wrong"},
            {"workload": "w", "tier": "2", "system": "x", "verdict": "ok"}]))
        with self.assertRaises(KeyboardInterrupt):
            self.run_tiers(suite, [Plan("w", "1"), Plan("w", "2")], self.system(),
                           stop=lambda plan, run: plan.tier == "2")
        self.assertEqual(self.cells(suite)["w/1/x"]["answers"], "pending")  # until the run wrote them
        suite.answer_receipts()
        cells = self.cells(suite)
        self.assertEqual(cells["w/1/x"]["answers"]["worst"], "wrong")  # its counts since F2 (below)
        self.assertNotIn("answers", cells.get("w/2/x", {}))

    def test_a_cell_s_answers_qualify_only_if_every_query_s_do(self):
        """P2 design review F2: the receipt kept one worst verdict, and `deviates` ranks above
        `incomplete` (answers.PRECEDENCE), so a cell with one query never captured read `deviates`.
        Its verdicts are counted, and it qualifies only when each is ok or deviates."""
        cases = {"one never captured": (["deviates", "incomplete"], False),
                 "one only its reference's": (["ok", "reference"], False),
                 "ok and across datatypes": (["ok", "deviates", "ok"], True)}
        for name, (verdicts, qualified) in cases.items():
            with self.subTest(name), patch.object(self, "root", self.root / name.replace(" ", "-")):
                (self.root / "data").mkdir(parents=True)
                suite = self.suite(runs=1)
                suite.answers = Mock(verdicts=Mock(return_value=[
                    {"workload": "w", "tier": "1", "system": "x", "verdict": v} for v in verdicts]))
                self.run_tiers(suite, [Plan("w", "1")], self.system())
                suite.answer_receipts()
                answers = self.cells(suite)["w/1/x"]["answers"]
                self.assertIs(answers["qualified"], qualified)
                self.assertEqual(sum(answers["verdicts"].values()), len(verdicts))

    def test_a_receipt_holds_its_attempt_s_protocol_build_and_measuring_settings(self):
        """P2 design review F3, F4, F8: the attempt's own protocol (a smoke run isn't the standard
        cell); the build's CPU, flags and Rust image (an empty settings list proved no defaults); and
        the settings that change a measurement, not where the run keeps its files."""
        with patch.dict(driver.os.environ, {"NRESE_TARGET_CPU": "znver3", "RUSTFLAGS": "-C opt-level=2"}):
            suite = self.suite(runs=1, skip_build=False)
        suite.manifest.update(settings={"JAVA_HEAP": "8g", "NRESE_BENCH_SCRATCH": "/x", "NRESE_BENCH_KEEP": "1"})
        nrese = Mock(spec=driver.Nrese, key="nrese", images=Mock(return_value=[]), builds=Mock(return_value=[]),
                     regimes={"none": ""}, supports=Mock(return_value=None),
                     source=Mock(return_value=self.root), rust_image=Mock(return_value="rust:1.98.1-bookworm"),
                     target_volume="v")
        with patch.dict(driver.os.environ, {"NRESE_TARGET_CPU": "znver3", "RUSTFLAGS": "-C opt-level=2"}), \
                patch.object(suite, "host_command", return_value=0), patch.object(driver.ns, "docker"), \
                patch.object(suite.args, "runtime", "docker"):
            self.assertTrue(suite.build_nrese(nrese))
        plan = Plan("w", "1")
        suite.receipts(plan, [("x", nrese)], suite.attempts(plan, [("x", nrese)]))
        receipt = self.cells(suite)["w/1/x"]
        self.assertEqual(receipt["protocol"]["runs"], 1)
        self.assertEqual({k: receipt["build"]["nrese"][k] for k in ("target_cpu", "rustflags_env", "rust_image", "rustflags")},
                         {"target_cpu": "znver3", "rustflags_env": "-C opt-level=2",
                          "rust_image": "rust:1.98.1-bookworm", "rustflags": "-C target-cpu=znver3"})
        self.assertEqual(receipt["settings"], {"JAVA_HEAP": "8g"})
        self.assertTrue({"RUST_IMAGE", "NRESE_TARGET_CPU", "RUSTFLAGS"} <= set(manifest.SETTINGS))

    def test_an_image_built_here_carries_its_context_s_tree(self):
        """P2 design review F5: a build of the suite's own image is labelled with its context's git
        tree id (a clean checkout only), so the image that runs says what it was built from."""
        suite = self.suite(runs=1)
        calls = []

        def docker(command, *args, **kwargs):
            calls.append(command)
            return 0
        for dirty, labelled in ((0, True), (2, False)):
            with self.subTest(dirty=dirty), patch.object(suite.args, "runtime", "docker"), \
                    patch.object(suite, "host_command", side_effect=docker), \
                    patch.object(suite.runtime, "image_present", return_value=False), \
                    patch.object(driver, "checkout_state", return_value={"commit": "c" * 40, "dirty": dirty}), \
                    patch.object(driver.receipts, "object_ids", return_value={"benches/competitors/jena": "t" * 40}):
                calls.clear()
                suite.ensure_images({"nrese-bench/jena:6.2.0": "competitors/jena"})
                build = next(c for c in calls if "build" in c)
                self.assertEqual(f"owlrs.built-from=benches/competitors/jena@{'t' * 40}" in build, labelled)

    def test_an_image_s_source_is_the_pinned_id_s_own(self):
        """P1b review r1 F1: the source was read from the tag, the id from the pin map, and the two
        could be different images: (a) an earlier preparation in the job pinned the tag to A (old,
        unlabelled), and the tag now names B, labelled from the current tree; the containers run A;
        (b) the tag was replaced between its preparation (a build, labelled) and its first pin. The
        receipt pairs the pinned id with that id's own label: A's, or unknown."""
        label = {"sha256:A": "", "sha256:B": "benches/competitors/jena@" + "t" * 40, "sha256:C": "benches/competitors/jena@old",
                 "img:t": "benches/competitors/jena@" + "t" * 40}  # by tag: B's, the current tree's
        inspect = lambda command, *a, **k: Mock(returncode=0, stdout=label.get(command[-1], "") + "\n")  # noqa: E731
        for name, pinned, present in (("an earlier pin to an unlabelled A", "sha256:A", True),
                                      ("replaced after its build, by C", "sha256:C", False)):
            with self.subTest(name), patch.object(self, "root", self.root / name.replace(" ", "-")):
                (self.root / "data").mkdir(parents=True)
                suite = self.suite(runs=1)
                with patch.object(driver.ns, "pinned", return_value={"img:t": pinned}), \
                        patch.object(driver, "run_quiet", side_effect=inspect):
                    # Its preparation: (a) the tag present, naming B; (b) built here, labelled, then moved.
                    with patch.object(suite.args, "runtime", "docker"), \
                            patch.object(suite, "host_command", return_value=0), \
                            patch.object(suite.runtime, "image_present", return_value=present), \
                            patch.object(driver, "checkout_state", return_value={"commit": "c" * 40, "dirty": 0}), \
                            patch.object(driver.receipts, "object_ids",
                                         return_value={"benches/competitors/jena": "t" * 40}):
                        suite.ensure_images({"img:t": "competitors/jena"})
                    self.run_tiers(suite, [Plan("w", "1")], self.system(images=["img:t"]))
                image = self.cells(suite)["w/1/x"]["images"]["img:t"]
                self.assertEqual(image["id"], pinned)
                expected = f"built from {label[pinned]}" if label[pinned] else None
                if expected:
                    self.assertEqual(image["source"], expected)
                else:
                    self.assertEqual(set(image["source"]), {"unknown"})

    PIN = "sha256:" + "d" * 64

    def pins(self, text):
        """This test's checkout's digest pins (benches/suite/images.toml)."""
        (self.root / "benches/suite").mkdir(parents=True, exist_ok=True)
        (self.root / "benches/suite/images.toml").write_text(text, encoding="utf-8")

    def test_a_pinned_image_is_pulled_by_its_digest(self):
        """P1c (orchestrator, 8 Nov 11:22): a tag names no content, its digest does; a pulled image
        with a pin is pulled by that digest and tagged, so the run runs the pinned content (one that
        is present but isn't the pin is pulled again by it); one without a pin is pulled by its tag."""
        self.pins(f'[pins]\n"qlever/qlever:latest" = "{self.PIN}"\n')
        suite = self.suite(runs=1)
        calls = []
        present = {"qlever/qlever:latest": '["qlever/qlever@sha256:' + "e" * 64 + '"]'}
        for image_present in (False, True):
            with self.subTest(present=image_present), patch.object(suite.args, "runtime", "docker"), \
                    patch.object(suite, "host_command", side_effect=lambda c, *a, **k: calls.append(c) or 0), \
                    patch.object(suite.runtime, "image_present", return_value=image_present), \
                    patch.object(driver, "run_quiet", side_effect=lambda c, *a, **k: Mock(
                        returncode=0, stdout=present.get(c[-1], "[]") + "\n")):
                calls.clear()
                suite.ensure_images({"qlever/qlever:latest": None, "other/thing:1": None})
                self.assertIn(["docker", "pull", "-q", f"qlever/qlever@{self.PIN}"], calls)
                self.assertIn(["docker", "tag", f"qlever/qlever@{self.PIN}", "qlever/qlever:latest"], calls)
                if not image_present:
                    self.assertIn(["docker", "pull", "-q", "other/thing:1"], calls)

    def test_a_pulled_image_s_source_is_its_pin_when_the_id_that_ran_holds_it(self):
        """P1c: a pulled image counts only as the content its tag's pin names: the id that ran must
        hold that digest. Pinned and held: "pulled, pinned <digest>"; held another: unknown, naming
        both; no pin: unknown."""
        cases = {"held": ([f"qlever/qlever@{self.PIN}"], True, f"pulled, pinned {self.PIN}"),
                 "another": (["qlever/qlever@sha256:" + "e" * 64], True, None),
                 "no pin": ([f"qlever/qlever@{self.PIN}"], False, None)}
        for name, (digests, pinned, expected) in cases.items():
            with self.subTest(name), patch.object(self, "root", self.root / name.replace(" ", "-")):
                (self.root / "data").mkdir(parents=True)
                self.pins(f'[pins]\n"qlever/qlever:latest" = "{self.PIN}"\n' if pinned else "[pins]\n")
                suite = self.suite(runs=1)

                def inspect(command, *a, **k):
                    out = json.dumps(digests) if "RepoDigests" in command[-2] else ""
                    return Mock(returncode=0, stdout=out + "\n")
                with patch.object(driver.ns, "pinned", return_value={"qlever/qlever:latest": "sha256:ID"}), \
                        patch.object(driver, "run_quiet", side_effect=inspect):
                    self.run_tiers(suite, [Plan("w", "1")], self.system(images=["qlever/qlever:latest"]))
                source = self.cells(suite)["w/1/x"]["images"]["qlever/qlever:latest"]["source"]
                if expected:
                    self.assertEqual(source, expected)
                else:
                    self.assertEqual(set(source), {"unknown"})
                    self.assertIn("pin", source["unknown"])

    def test_a_pin_that_isn_t_a_digest_is_refused(self):
        for text in ('[pins]\n"a:1" = "latest"\n', '[pins]\n"a:1" = "sha256:abc"\n', "pins = 1\n"):
            with self.subTest(text):
                self.pins(text)
                with self.assertRaises(ValueError):
                    driver.image_pins(self.root)

    def test_what_a_cell_can_t_evidence_is_unknown_never_empty(self):
        """R6: a driver plan's inputs, images, build and settings are its child's; an input outside
        the datasets and an external driver aren't hashed; a binary not built here is unknown."""
        suite = self.suite(runs=1)
        suite.receipts(Plan("w", "1", kind="driver"), [("x", None)],
                       suite.attempts(Plan("w", "1", kind="driver"), [("x", None)]))
        child = self.cells(suite)["w/1/x"]
        for field in ("images", "build", "settings", "data"):
            self.assertEqual(set(child[field]), {"unknown"}, field)
        nrese = Mock(spec=driver.Nrese, key="nrese", images=Mock(return_value=[]), builds=Mock(return_value=[]))
        plan = Plan("w", "2", inputs=["/rggs/rg.ttl"], external=["bibm"])
        suite.receipts(plan, [("x", nrese)], suite.attempts(plan, [("x", nrese)]))
        served = self.cells(suite)["w/2/x"]
        self.assertEqual(set(served["data"]), {"/rggs/rg.ttl", "external"})
        self.assertTrue(all(set(v) == {"unknown"} for v in served["data"].values()))
        self.assertEqual(set(served["build"]), {"harness", "nrese"})
        self.assertTrue(all("--skip-build" in v["unknown"] for v in served["build"].values()))

    def test_each_input_is_hashed_once_a_run_and_a_missing_one_is_not_known(self):
        suite = self.suite()
        (self.root / "data" / "a.nt").write_bytes(b"x")
        with patch.object(driver.Store, "hashes", autospec=True, side_effect=driver.Store.hashes) as hashes:
            first = suite.data_hashes(["/data/a.nt", "/data/gone.nt"])
            again = suite.data_hashes(["/data/a.nt"])
        self.assertEqual(first, {"/data/a.nt": hashlib.sha256(b"x").hexdigest(), "/data/gone.nt": None})
        self.assertEqual(again, {"/data/a.nt": hashlib.sha256(b"x").hexdigest()})
        self.assertEqual(hashes.call_count, 1)


    def test_a_newer_attempt_outlives_a_crash_and_one_not_flushed_starts_no_work(self):
        """R2 (reviews cell-receipts r2, r3): I1 completed A (tier 1) and B (tier 2), long on disk; I2
        attempts A again. Its attempt's flush fails: no work of A starts, the rows stay, A is
        incomplete. Or it is flushed, A's work starts and the host dies: reopened with only what was
        on disk, A is I2's incomplete attempt, never I1's receipt, and B keeps I1's. Or the attempts'
        file's name never reached the disk: I2's directory sync puts it there, B's with it; where that
        sync makes nothing durable (a rename or new name is lost), no receipt stands."""
        for no_dirsync, case in [(n, c) for n in (False, True) for c in ("flush fails", "crash", "journal lost")]:
            with self.subTest(no_dirsync=no_dirsync, case=case):
                self.setUp()
                # A checkout, as a run's always is (the ledger reads its dirty count).
                subprocess.run(["git", "init", "-q", str(self.root)], capture_output=True, check=True)
                first = self.suite(runs=1)
                self.run_tiers(first, [Plan("w", "1"), Plan("w", "2")], self.system())
                rows = (first.results / "results.csv").read_text(encoding="utf-8")
                first.manifest.finish(0)
                i1 = json.loads((first.results / "manifest.json").read_text(encoding="utf-8"))["invocations"][0]["cells"]
                again, started, disk = self.suite(runs=1), [], Crash(first.results, no_dirsync)
                if case == "journal lost":
                    disk.disk.pop("attempts.jsonl", None)

                def work(plan, run):
                    started.append(plan.tier)
                    if case != "flush fails":
                        raise KeyboardInterrupt  # the host is gone
                with contextlib.ExitStack() as stack, self.assertRaises((OSError, KeyboardInterrupt)):
                    for p in disk.patches(fail=case == "flush fails"):
                        stack.enter_context(p)
                    self.run_tiers(again, [Plan("w", "1")], self.system(), stop=work)
                self.assertEqual(started, [] if case == "flush fails" else ["1"])
                self.assertEqual((first.results / "results.csv").read_text(encoding="utf-8"), rows)
                with patch("suitekit.versus.versus", return_value=[]), patch("suitekit.cases.load", return_value=[]):
                    record = ledger.from_results(first.results if case == "flush fails"
                                                 else disk.reopen(self.root / "reopened"), self.root)
                pairs = {p["tier"]: p.get("receipt", {}) for p in record["pairs"]}
                self.assertNotEqual(pairs["1"].get("attempt"), i1["w/1/x"]["attempt"])  # never I1's for I2's
                self.assertFalse(pairs["1"].get("complete"))
                self.assertEqual(pairs["2"].get("attempt"),
                         None if case == "journal lost" and no_dirsync else i1["w/2/x"]["attempt"])

    def test_one_run_at_a_time_owns_a_results_directory(self):
        """R2a (review cell-receipts r4): two runs appending to one directory's journal can overwrite
        each other's attempts (Windows' append is a seek, then a write). While a run holds the
        directory, a second is refused with its remedy before anything runs, and the directory stays
        as it was; once the first ends, or fails, the next one runs."""
        results = self.root / "results"
        holder = self.suite(runs=1)  # its run in progress
        before = files(results)
        code, err, ran = self.main(results)
        self.assertEqual((code, ran), (2, False))
        self.assertIn("--results", err)
        self.assertEqual(files(results), before)
        holder.manifest.finish(0)
        self.assertEqual(self.main(results)[0], 0)
        with self.assertRaises(RuntimeError):
            self.main(results, run=RuntimeError("its work failed"))
        self.assertEqual(self.main(results)[0], 0)

    def test_a_batch_directory_whose_journal_lost_records_lets_no_older_receipt_stand(self):
        """R2b (review cell-receipts r4): a batch of D1 and D2. D1's I1 completed A (tier 1) and B
        (tier 2); D2's I2 attempted A later and was interrupted. Then D2's journal is missing, or
        recreated with only a B attempt: what it lost is unknowable, so A is never I1's completed
        receipt. With D2's journal intact, A is I2's incomplete attempt. D2 itself can't vanish: its
        name is synced in the batch directory before its first attempt."""
        subprocess.run(["git", "init", "-q", str(self.root)], capture_output=True, check=True)
        first = self.suite(runs=1, results="batch/d1")
        self.run_tiers(first, [Plan("w", "1"), Plan("w", "2")], self.system())
        i1 = self.cells(first)
        synced, real = [], manifest.sync_directory
        with patch("suitekit.manifest.sync_directory", side_effect=lambda d: (synced.append(Path(d)), real(d))[1]):
            second = self.suite(runs=2, results="batch/d2")
        with self.assertRaises(KeyboardInterrupt):
            self.run_tiers(second, [Plan("w", "1")], self.system(), stop=lambda plan, run: run == 2)
        i2 = self.journal(second)["w/1/x"]["attempt"]
        b = {"cell": "w/2/x", "attempt": "b3", "started": "2026-11-08T23:00:00.000000", "complete": False}
        for lost in ("intact", "missing", "recreated with B only"):
            with self.subTest(journal=lost):
                if lost == "missing":
                    (second.results / "attempts.jsonl").unlink()
                elif lost != "intact":
                    (second.results / "attempts.jsonl").write_text("\n" + json.dumps(b), encoding="utf-8")
                with patch("suitekit.versus.versus", return_value=[]), patch("suitekit.cases.load", return_value=[]):
                    record = ledger.from_results(self.root / "batch", self.root)
                a = next(p for p in record["pairs"] if p["tier"] == "1").get("receipt", {})
                self.assertNotEqual(a.get("attempt"), i1["w/1/x"]["attempt"])  # never I1's for I2's
                self.assertFalse(a.get("complete"))
                if lost == "intact":
                    self.assertEqual(a["attempt"], i2)
        # D2's own name was on disk before any of its attempts (its Manifest): it can't vanish.
        self.assertIn(self.root / "batch", synced)

    def test_two_starters_at_once_never_truncate_the_winner_s_rows(self):
        """Review cell-receipts r5 F1, results-lock r1 R1: two runs started at once into one
        directory. The loser is paused after deciding its results.csv is missing and before
        Writer's open "w" (the order before the fix), or, with the fix, before it reaches the
        directory's lock; the winner takes the directory and writes its header and a known row;
        then the loser goes on. The winner's header and row survive, and the loser is refused."""
        results, decided, go, outcome = self.root / "results", threading.Event(), threading.Event(), {}
        loser: list[threading.Thread] = []
        real_open, real_manifest = open, driver.Manifest

        def pause():
            if threading.current_thread() in loser and not decided.is_set():
                decided.set()
                self.assertTrue(go.wait(30), "the loser was never released")

        def opening(path, mode="r", *args, **kwargs):
            if "w" in mode and Path(path).name == "results.csv":
                pause()  # its decision made: the file was missing or empty
            return real_open(path, mode, *args, **kwargs)

        def manifest_of(*args, **kwargs):
            pause()
            return real_manifest(*args, **kwargs)

        def lose():
            outcome["main"] = self.main(results)
        with patch.object(schema, "open", opening, create=True), patch.object(driver, "Manifest", manifest_of):
            loser.append(threading.Thread(target=lose))
            loser[0].start()
            self.assertTrue(decided.wait(30), "the loser never reached its decision")
            winner = self.suite(runs=1)  # the directory's lock, then its first row
            winner.emit(dict(date=winner.date, host="h", runtime="process", system="x", version="-",
                             publish="free", workload="w", tier="1", regime="none", run=1),
                        task="load", item="the winner's row", ms=5)
            go.set()
            loser[0].join(30)
        self.assertFalse(loser[0].is_alive())
        rows = list(csv.reader(io.StringIO((results / "results.csv").read_text(encoding="utf-8"))))
        self.assertEqual(rows[0], driver.FIELDS)
        self.assertIn("the winner's row", [r[driver.FIELDS.index("item")] for r in rows[1:]])
        code, err, ran = outcome["main"]
        self.assertEqual(code, 2)
        self.assertFalse(ran)
        self.assertIn("another run is writing into it", err)

    def test_a_second_starter_never_opens_the_results_another_run_owns(self):
        """Guard of the order (review cell-receipts r5 F1): while one run owns the directory, a
        second is refused without opening its results.csv at all, and the first's rows stay as
        they were. Its red before the fix is the constructor called, not a lost row: the first
        run's header was already there, so that Writer truncated nothing (results-lock r1 R1)."""
        first = self.suite(runs=1)
        first.emit(dict(date=first.date, host="h", runtime="process", system="x", version="-", publish="free",
                        workload="w", tier="1", regime="none", run=1), task="load", ms=5)
        rows = (first.results / "results.csv").read_bytes()
        opened = []
        real = driver.Writer

        def writer(path, *args, **kwargs):
            opened.append(Path(path))
            return real(path, *args, **kwargs)
        with patch.object(driver, "Writer", side_effect=writer):
            code, err, ran = self.main(first.results)
        self.assertEqual(code, 2)
        self.assertFalse(ran)
        self.assertIn("another run is writing into it", err)
        self.assertEqual(opened, [])
        self.assertEqual((first.results / "results.csv").read_bytes(), rows)

    def test_a_run_into_a_directory_whose_manifest_can_t_be_read_is_refused_and_leaves_it(self):
        """Review cell-receipts r2 (orchestrator, 8 Nov 05:38): a manifest that couldn't be parsed was
        read as empty, and the next write dropped every earlier invocation's evidence. The run is
        refused with its reason, before anything runs, and the file stays byte for byte."""
        results = self.root / "results"
        results.mkdir()
        for text in ('{"schema": 1, "invocations": [{"started": "2026-11-08T01', '["not", "a manifest"]'):
            with self.subTest(manifest=text[:20]):
                (results / "manifest.json").write_text(text, encoding="utf-8")
                err = io.StringIO()
                with patch.object(driver.Suite, "run") as run, contextlib.redirect_stderr(err), \
                        contextlib.redirect_stdout(io.StringIO()):
                    code = driver.main(["--runtime", "process", "--data", str(self.root / "data"),
                                        "--results", str(results), "--systems", "x", "--workloads", "w"],
                                       {"x": {"capabilities": ["load"], "publish": "free"}},
                                       {"w": {"needs": ["load"], "state": "ready", "kit": ""}}, self.root)
                self.assertEqual(code, 2)
                run.assert_not_called()
                self.assertIn("refused", err.getvalue())
                self.assertEqual((results / "manifest.json").read_text(encoding="utf-8"), text)

    def test_a_manifest_is_on_disk_before_it_replaces_the_old_one(self):
        """Guard: the file's bytes are flushed (fsync) before the replace, and the directory after it
        (POSIX: its fsync, seen here; Windows: FlushFileBuffers, not an os call), so a rename that
        survives a crash names a whole file (receipts; attempts don't rest on it)."""
        suite, calls = self.suite(runs=1), []
        real_fsync, real_replace = driver.os.fsync, driver.os.replace
        with patch("suitekit.manifest.os.fsync", side_effect=lambda fd: (calls.append("fsync"), real_fsync(fd))[1]), \
                patch("suitekit.manifest.os.replace", side_effect=lambda a, b: (calls.append("replace"), real_replace(a, b))[1]):
            suite.manifest.update(cells={"w/1/x": {"attempt": "a", "started": "t", "complete": False}})
        self.assertEqual(calls, ["fsync", "replace"] + (["fsync"] if driver.os.name != "nt" else []))


    def test_a_receipt_s_code_is_unchanged_by_what_measuring_writes_into_the_repository(self):
        """P2 design review F1 (orchestrator, 8 Nov 10:50): the code identity was the tree of all of
        benches/, which holds the run records (benches/runs) and the views made from them, so committing
        a run's own record changed the identity its receipt is held to: stale as it landed. It is
        what a cell can execute: those left out, rust-toolchain.toml and .cargo/ in."""
        def write(files):
            for f, text in files.items():
                (self.root / f).parent.mkdir(parents=True, exist_ok=True)
                (self.root / f).write_text(text, encoding="utf-8")
            git("add", "-A")
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "x")
        git = lambda *a: subprocess.run(["git", "-C", str(self.root), *a], capture_output=True, text=True,  # noqa: E731
                                        check=True).stdout
        git("init", "-q")
        write({".gitignore": "results*/\ndata/\ntmp/\n", "benches/suite/x.py": "1\n", "benches/runs/README.md": "r\n",
               "benches/STATUS.md": "s\n", "scripts/lib/ns.sh": "n\n", "rust-toolchain.toml": "t\n",
               ".cargo/config.toml": "c\n", "crates/x/lib.rs": "x\n"})
        runs = iter(range(1, 10))

        def code():
            suite = self.suite(runs=1, results=f"results-{next(runs)}")
            self.run_tiers(suite, [Plan("w", "1")], self.system())
            return self.cells(suite)["w/1/x"]["code"]
        first = code()
        write({"benches/runs/2026-11-08-r1.toml": "id = 'r1'\n", "benches/STATUS.md": "s2\n",
               "benches/STANDINGS.md": "t\n", "benches/CATALOG.md": "c\n", "benches/baselines/b.json": "{}\n"})
        self.assertEqual(code(), first)  # the run's record and its views, committed: the same identity
        for f in ("rust-toolchain.toml", ".cargo/config.toml", "benches/suite/x.py", "scripts/lib/ns.sh"):
            with self.subTest(changed=f):
                write({f: f"{f} changed\n"})
                now = code()
                self.assertNotEqual(now, first)
                first = now


class Code(unittest.TestCase):
    def test_a_cell_s_code_changes_with_anything_the_repository_holds_for_it(self):
        """R5: commit-latency's driver reads queries, the harness and GraphDB's configuration outside
        its kit's directory, LUBM's preparation sources scripts/lib: a change to any of them changes
        the code of every cell (coarse and exact); without a commit to read, it is unknown. Since F1 of
        the P2 design review benches/ counts entry by entry (its records and views left out), so the
        entry that changed is the one named."""
        with tempfile.TemporaryDirectory() as d:
            root = Path(d).resolve()
            files = ("benches/reasoning/maintenance/maintenance.py", "benches/reasoning/queries/lubm/q1.rq",
                     "benches/competitors/graphdb/repo-owl2-rl.ttl", "scripts/lib/ns.sh", "crates/x/src/lib.rs")
            for f in files:
                (root / f).parent.mkdir(parents=True, exist_ok=True)
                (root / f).write_text("1\n", encoding="utf-8")
            git = lambda *a: subprocess.run(["git", "-C", d, *a], capture_output=True, text=True,  # noqa: E731
                                            check=True).stdout.strip()
            git("init", "-q")
            commit = lambda: (git("add", "-A"), git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q",  # noqa: E731
                                                    "-m", "x"), git("rev-parse", "HEAD"))[2]
            first = commit()
            paths = receipts.code_paths(root, first) + receipts.BUILD_PATHS
            before = receipts.object_ids(root, first, paths)
            for f, changed in (("benches/reasoning/queries/lubm/q1.rq", "benches/reasoning"),
                               ("benches/competitors/graphdb/repo-owl2-rl.ttl", "benches/competitors"),
                               ("scripts/lib/ns.sh", "scripts"), ("crates/x/src/lib.rs", "crates")):
                with self.subTest(changed=f):
                    (root / f).write_text(f"{f}\n", encoding="utf-8")
                    after = receipts.object_ids(root, commit(), paths)
                    differ, before = {p for p in paths if before[p] != after[p]}, after
                    self.assertEqual(differ, {changed})
            self.assertEqual(before["Cargo.toml"], {"unknown": "not in its commit"})
            self.assertEqual(receipts.object_ids(root, "", ["benches"]), {"benches": {"unknown": "no commit to read"}})


if __name__ == "__main__":
    unittest.main()
