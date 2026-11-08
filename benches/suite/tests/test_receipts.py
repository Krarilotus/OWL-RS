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
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import tomllib
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from suitekit import driver, ledger, manifest, receipts, schema  # noqa: E402
from suitekit.workloads import Plan  # noqa: E402

ROOT = Path(__file__).resolve().parents[3]  # this repository


def no_cargo_config_at_or_above(path) -> bool:
    """Whether no .cargo/config or .cargo/config.toml sits in `path` or any directory above it: a plain
    walk, the test's own, independent of the producer's cargo_configs."""
    here = os.path.abspath(path)
    while True:
        if any(os.path.isfile(os.path.join(here, ".cargo", name)) for name in ("config", "config.toml")):
            return False
        parent = os.path.dirname(here)
        if parent == here:
            return True
        here = parent

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

    def system(self, images=(), script_builds=()):
        # definition_paths: what the reviewed head's adapters had (r1), so its red reaches the verdicts.
        return Mock(regimes={"none": ""}, images=Mock(return_value=list(images)), builds=Mock(return_value=[]),
                    script_builds=Mock(return_value=list(script_builds)),
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
        # Docker's own answers mocked like every other docker call here: a host without docker runs
        # this too (gate-windows-59532fff57-1794140734, khaleesi: FileNotFoundError).
        with patch.object(driver.ns, "pinned", return_value={"img:t": "sha256:A"}), \
                patch.object(driver, "run_quiet", return_value=Mock(returncode=0, stdout="\n", stderr="")):
            self.run_tiers(suite, [Plan("w", "1")], self.system(images=["img:t", "img:u"]))
        images = self.cells(suite)["w/1/x"]["images"]  # each with its source since F5 (below)
        self.assertEqual({i: v["id"] for i, v in images.items()},
                         {"img:t": "sha256:A", "img:u": {"unknown": "not in the run's pin map"}})

    def test_a_receipt_names_the_source_commit_an_image_was_built_at(self):
        """Orchestrator, 8 Nov 13:39: an image built from a source checkout (images.sh: MillenniumDB,
        G-CARE) carries its source commit beside its context's tree, both read off the id that ran,
        so a build at another commit than its Dockerfile declares is visible as such."""
        labels = {"sha256:A": ("benches/competitors/millenniumdb@" + "t" * 40, "ecbf6dde" + "0" * 32),
                  "sha256:B": ("benches/competitors/jena@" + "t" * 40, "")}

        def inspect(command, *a, **k):
            built, commit = labels[command[-1]]
            fields = command[-2].count("{{index")
            return Mock(returncode=0, stdout=(built if fields == 1 else f"{built}|{commit}") + "\n", stderr="")
        for pinned, expected in (("sha256:A", f"built from {labels['sha256:A'][0]}, source commit {labels['sha256:A'][1]}"),
                                 ("sha256:B", f"built from {labels['sha256:B'][0]}")):
            with self.subTest(pinned), patch.object(self, "root", self.root / pinned[-1]):
                (self.root / "data").mkdir(parents=True)
                suite = self.suite(runs=1)
                with patch.object(driver.ns, "pinned", return_value={"img:t": pinned}), \
                        patch.object(driver, "run_quiet", side_effect=inspect):
                    self.run_tiers(suite, [Plan("w", "1")], self.system(images=["img:t"]))
                self.assertEqual(self.cells(suite)["w/1/x"]["images"]["img:t"]["source"], expected)

    def test_an_image_that_compiles_a_source_of_its_own_needs_its_source_commit_label(self):
        """Review image-source-commit r1 F2 (orchestrator 18:26): an image its adapter declares built
        from a source chosen apart from its context (script_builds: MillenniumDB's, by images.sh)
        is known only with its source commit; its context's tree alone says unknown, why. An image
        built from its context alone (builds: Jena's) is known by that tree, as before."""
        tree = "t" * 40
        labels = {"sha256:M": (f"benches/competitors/millenniumdb@{tree}", ""),
                  "sha256:N": (f"benches/competitors/millenniumdb@{tree}", "ecbf6dde" + "0" * 32),
                  "sha256:J": (f"benches/competitors/jena@{tree}", "")}

        def inspect(command, *a, **k):
            built, commit = labels[command[-1]]
            return Mock(returncode=0, stdout=f"{built}|{commit}\n", stderr="")
        declared = [("img:m", "competitors/millenniumdb", "MDB_COMMIT")]
        for mdb, expected in (("sha256:M", None),
                              ("sha256:N", f"built from benches/competitors/millenniumdb@{tree}, source commit "
                                           + "ecbf6dde" + "0" * 32)):
            with self.subTest(mdb), patch.object(self, "root", self.root / mdb[-1]):
                (self.root / "data").mkdir(parents=True)
                suite = self.suite(runs=1)
                with patch.object(driver.ns, "pinned", return_value={"img:m": mdb, "img:j": "sha256:J"}), \
                        patch.object(driver, "run_quiet", side_effect=inspect):
                    self.run_tiers(suite, [Plan("w", "1")], self.system(images=["img:m", "img:j"],
                                                                        script_builds=declared))
                images = self.cells(suite)["w/1/x"]["images"]
                if expected is None:
                    self.assertIn("no source commit", images["img:m"]["source"]["unknown"])
                else:
                    self.assertEqual(images["img:m"]["source"], expected)
                self.assertEqual(images["img:j"]["source"], f"built from benches/competitors/jena@{tree}")

    def test_the_millenniumdb_adapter_declares_the_image_images_sh_builds_at_its_dockerfile_s_commit(self):
        """Review r1 F2: the declaration is the adapter's own; its tag is the one images.sh builds
        (nrese-bench/millenniumdb:<the declared commit's first seven>), so declaration, Dockerfile
        and tag can't drift apart unnoticed."""
        ctx = driver.Context(None, ROOT, ROOT, ROOT, driver.Mount("-", "/data"), [], 0, {}, "images")
        mdb = driver.adapter("millenniumdb")
        (tag, context, arg), = mdb.script_builds(ctx)
        self.assertEqual(tag, mdb.images(ctx)[0])
        declared = {line.split("=", 1)[1].strip() for line in
                    (ROOT / "benches" / context / "Dockerfile").read_text(encoding="utf-8").splitlines()
                    if line.startswith(f"ARG {arg}=")}
        self.assertEqual(len(declared), 1)
        self.assertEqual(tag, f"nrese-bench/millenniumdb:{declared.pop()[:7]}")

    def test_a_receipt_says_why_an_image_s_source_couldn_t_be_read(self):
        """#161's Windows gate (gate-windows-59532fff57-1794140734): reading an image's label raised
        FileNotFoundError on a host without docker, and the receipt was never written. Without docker,
        or with its inspect failing, the source is unknown with the reason; nothing raises."""
        for name, effect, why in (("no docker", FileNotFoundError(2, "No such file or directory: 'docker'"),
                                   "docker not available"),
                                  ("inspect fails", Mock(returncode=1, stdout="", stderr="Error: No such image: sha256:A"),
                                   "No such image")):
            with self.subTest(name), patch.object(self, "root", self.root / name.replace(" ", "-")):
                (self.root / "data").mkdir(parents=True)
                suite = self.suite(runs=1)
                quiet = {"side_effect": effect} if isinstance(effect, Exception) else {"return_value": effect}
                with patch.object(driver.ns, "pinned", return_value={"img:t": "sha256:A"}), \
                        patch.object(driver, "run_quiet", **quiet):
                    self.run_tiers(suite, [Plan("w", "1")], self.system(images=["img:t"]))
                source = self.cells(suite)["w/1/x"]["images"]["img:t"]["source"]
                self.assertEqual(set(source), {"unknown"})
                self.assertIn(why, source["unknown"])

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
        self.checkout(self.root, "suite")  # a checkout to export, as a run's source is (P1e)
        with patch.dict(driver.os.environ, {"NRESE_TARGET_CPU": "znver3", "RUSTFLAGS": "-C opt-level=2"}):
            suite = self.suite(runs=1, skip_build=False)
        suite.manifest.update(settings={"JAVA_HEAP": "8g", "NRESE_BENCH_SCRATCH": "/x", "NRESE_BENCH_KEEP": "1"})
        nrese = Mock(spec=driver.Nrese, key="nrese", images=Mock(return_value=[]), builds=Mock(return_value=[]),
                     script_builds=Mock(return_value=[]),
                     regimes={"none": ""}, supports=Mock(return_value=None),
                     source=Mock(return_value=self.root), rust_image=Mock(return_value="rust:1.98.1-bookworm"),
                     target_volume="v")
        with patch.dict(driver.os.environ, {"NRESE_TARGET_CPU": "znver3", "RUSTFLAGS": "-C opt-level=2"}), \
                patch.object(suite, "host_command", return_value=0), patch.object(driver.ns, "docker"), \
                patch.object(driver.ns, "pinned", return_value={"rust:1.98.1-bookworm": "sha256:" + "r" * 64}), \
                patch.object(driver, "toolchain_in", return_value=dict(self.DECLARED)), \
                patch.object(suite.args, "runtime", "docker"):  # pinned before any build, as a run's are
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

    def checkout(self, path: Path, version: str, harness: str = "") -> Path:
        """A source checkout of its own: what the builds compile (crates, Cargo.toml, Cargo.lock,
        rust-toolchain.toml, .cargo, the harness's own workspace, and the launcher), committed."""
        path.mkdir(parents=True, exist_ok=True)
        if not (path / ".git").exists():
            self.git(path, "init", "-q")
        for name, text in {"crates/a/src/lib.rs": version, "Cargo.toml": "[workspace]\n", "Cargo.lock": version,
                           "rust-toolchain.toml": '[toolchain]\nchannel = "1.98.1"\n', ".cargo/config.toml": version,
                           "benches/nrese-bench-harness/Cargo.toml": "[workspace]\n",
                           "benches/nrese-bench-harness/Cargo.lock": version,
                           "benches/nrese-bench-harness/src/main.rs": harness or version,
                           "scripts/cargo-guarded.sh": version, "scripts/lib/target-cpu.sh": version,
                           "scripts/lib/disk.sh": version}.items():
            (path / name).parent.mkdir(parents=True, exist_ok=True)
            (path / name).write_text(text, encoding="utf-8")
        self.git(path, "add", "-A")
        self.git(path, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x")
        return path

    @staticmethod
    def git(path: Path, *args: str) -> str:
        done = subprocess.run(["git", "-C", str(path), *args], capture_output=True, text=True, check=True)
        return done.stdout.strip()

    DECLARED = {"rustc": "1.98.1", "cargo": "1.98.1", "host": "x86_64-unknown-linux-gnu"}  # the fixture's channel

    def clean_root(self) -> Path:
        """A directory of this test's own under a root with no cargo config at or above it, for a
        build's export: the precondition of every `controlled: true`, checked first by the test's own
        walk. A fleet Windows job's TEMP is inside its checkout (whose .cargo a build there would
        read), so its own output directory and the user's temporary directory are tried too. None
        on this host: the test fails, saying so; its expectation never bends to the host."""
        local = os.environ.get("LOCALAPPDATA")
        tried = []
        for base in (tempfile.gettempdir(), os.environ.get("FLEET_OUT"), local and os.path.join(local, "Temp"),
                     str(Path.home())):
            if not base or not os.path.isdir(base):
                continue
            tried.append(base)
            if no_cargo_config_at_or_above(base):
                root = Path(tempfile.mkdtemp(prefix="p1e-clean-", dir=base)).resolve()
                self.addCleanup(shutil.rmtree, root, True)
                self.assertTrue(no_cargo_config_at_or_above(root))
                return root
        self.fail(f"no directory on this host without a cargo config at or above it; tried {tried}")

    def built(self, source: Path | None = None, results: str = "results", toolchain=None,
              settings: dict | None = None, export_base: Path | None = None) -> dict:
        """The receipt's `build` of one NRESE cell after this run built NRESE (from `source`, else the
        repository) and the harness (from the repository), into `results`, both host builds. Each
        build's directory, file list there and environment are in self.builds; the toolchain probe
        answers `toolchain` (the fixture's declared one by default)."""
        suite = self.suite(runs=1, skip_build=False, results=results)
        suite.settings.update(settings or {})
        self.suite_built = suite
        nrese = Mock(spec=driver.Nrese, key="nrese", images=Mock(return_value=[]), builds=Mock(return_value=[]),
                     script_builds=Mock(return_value=[]), regimes={"none": ""}, supports=Mock(return_value=None),
                     source=Mock(return_value=source or self.root), target_volume="v",
                     target_dir=Mock(return_value=self.root / "nrese-target"))
        self.builds = []

        def build(command, log=None, *a, cwd=None, environ=None, **k):
            files = sorted(f.relative_to(cwd).as_posix() for f in Path(cwd).rglob("*") if f.is_file()) if cwd else []
            self.builds.append({"cwd": Path(cwd) if cwd else None, "files": files, "environ": environ})
            return 0
        with patch.object(suite, "host_command", side_effect=build), contextlib.ExitStack() as stack, \
                patch.object(driver, "toolchain_here", return_value=toolchain or dict(self.DECLARED)):
            if export_base is not None:
                stack.enter_context(patch.object(driver, "build_export_base", return_value=export_base))
            self.assertTrue(suite.build_nrese(nrese))
            self.assertTrue(suite.build_harness())
        self.assertEqual(suite.made_exports, [])  # each export went with its build
        self.assertTrue(all(not b["cwd"].exists() for b in self.builds if b["cwd"]))
        plan = Plan("w", "1")
        suite.receipts(plan, [("x", nrese)], suite.attempts(plan, [("x", nrese)]))
        return self.cells(suite)["w/1/x"]["build"]

    def test_a_build_names_the_trees_it_compiled_at_its_own_commit(self):
        """Orchestrator, 8 Nov (P1d, design r2 F10; r3 F10): each build records the git ids of its
        program's declared dependency envelope (P1d review r1 F2: what its build can read, not an exact
        read set) at its own source's commit, so a receipt is compared by content. NRESE: crates,
        Cargo.toml, Cargo.lock, rust-toolchain.toml, .cargo, from its source, which can be another
        checkout. The harness, a workspace of its own built with --manifest-path whose path
        dependencies are crates/ members inheriting from the root Cargo.toml: its workspace
        directory, crates, Cargo.toml, rust-toolchain.toml, .cargo, from the repository. Both: the
        launcher, scripts/cargo-guarded.sh and what it sources (r4 B)."""
        self.checkout(self.root, "suite")
        source = self.checkout(self.root / "data" / "nrese-src", "nrese")
        build = self.built(source)
        launcher = ("scripts/cargo-guarded.sh", "scripts/lib/target-cpu.sh", "scripts/lib/disk.sh")  # r4 B
        paths = {"nrese": ("crates", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo", *launcher),
                 "harness": ("benches/nrese-bench-harness", "crates", "Cargo.toml", "rust-toolchain.toml", ".cargo",
                             *launcher)}
        for program, checkout in (("nrese", source), ("harness", self.root)):
            with self.subTest(program):
                self.assertIn("sources", build[program], "the build names its sources")
                self.assertEqual(build[program]["sources"], {p: self.git(checkout, "rev-parse", f"HEAD:{p}")
                                                            for p in paths[program]})
                self.assertEqual(build[program].get("commit"), self.git(checkout, "rev-parse", "HEAD"))
        self.assertTrue(all("sources" in build[p] for p in ("nrese", "harness")), "both builds name their sources")
        self.assertNotEqual(build["nrese"]["sources"]["crates"], build["harness"]["sources"]["crates"])

    def test_a_harness_built_from_other_source_says_so_where_nrese_s_trees_are_the_same(self):
        """P2 design check r3 F10 (orchestrator 17:39): two commits that differ only in the harness's
        src give different harness sources and the same NRESE ones: a harness built at another commit
        with NRESE's five trees unchanged is never read as the same build."""
        self.checkout(self.root, "suite", harness="fn main() {}\n")
        first = self.built()
        self.checkout(self.root, "suite", harness="fn main() { println!(); }\n")
        second = self.built(results="results-2")
        for build in (first, second):
            self.assertTrue(all("sources" in build[p] for p in ("nrese", "harness")), "both builds name their sources")
        self.assertNotEqual(first["harness"]["sources"]["benches/nrese-bench-harness"],
                            second["harness"]["sources"]["benches/nrese-bench-harness"])
        self.assertEqual(first["nrese"]["sources"], second["nrese"]["sources"])

    def test_a_checkout_that_moves_during_the_run_and_stays_moved_has_no_commit(self):
        """P2 design r4 R1 (orchestrator 17:47): as a tier's receipts are written the checkout is read
        again; one on another commit than the run started from, or changed since a clean start, gives
        the receipt no commit but why, so it is never current. (A run whose results directory is in the
        checkout and not ignored starts dirty, as the manifest records it after making that directory:
        unqualified already, so only a clean start's changes are the guard's.)"""
        for case in ("stays", "moved", "edited"):
            with self.subTest(case):
                self.setUp()
                (self.root / ".gitignore").write_text("data/\nresults/\n", encoding="utf-8")
                (self.root / "a.txt").write_text("a\n", encoding="utf-8")
                self.git(self.root, "init", "-q")
                self.git(self.root, "add", "-A")
                self.git(self.root, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "start")
                start = self.git(self.root, "rev-parse", "HEAD")
                suite = self.suite(runs=1)
                self.assertEqual(suite.manifest.entry["git"]["dirty"], 0)  # a clean start: the guard's case

                def move(plan, run):
                    (self.root / "a.txt").write_text("b\n", encoding="utf-8")
                    if case == "moved":
                        self.git(self.root, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "b")
                    return False
                self.run_tiers(suite, [Plan("w", "1")], self.system(),
                               stop=move if case != "stays" else lambda p, r: False)
                commit = self.cells(suite)["w/1/x"]["commit"]
                if case == "stays":
                    self.assertEqual(commit, start)
                else:
                    self.assertIsInstance(commit, dict)
                    self.assertIn({"moved": "moved during the run", "edited": "changed during the run"}[case],
                                  commit["unknown"])

    def test_a_build_names_what_its_build_scripts_read_from_outside_git(self):
        """P2 design check r4 A (orchestrator 17:56): crates/nrese-server/build.rs embeds every file of
        the ignored apps/nrese-console/dist, so equal trees with other console bytes are another binary.
        build_inputs names them: "no files" when the directory is missing or empty (the same empty
        table), else a digest of every file's path and bytes, which changes with them. P1e (orchestrator
        18:34): a build compiles a clean export of its commit, which never holds the ignored bundle, so
        whatever the checkout has, the build's record is "no files", the default build's."""
        self.checkout(self.root, "suite")
        dist = self.root / "apps" / "nrese-console" / "dist"
        seen = []
        for case, files in (("missing", None), ("empty", {}), ("bundle a", {"index.html": "a"}),
                            ("bundle b", {"index.html": "b"}), ("nested", {"index.html": "b", "assets/x.js": "x"})):
            if files is not None:
                dist.mkdir(parents=True, exist_ok=True)
                for name, text in files.items():
                    (dist / name).parent.mkdir(parents=True, exist_ok=True)
                    (dist / name).write_text(text, encoding="utf-8")
            seen.append((case, driver.build_inputs(self.root, "nrese")["apps/nrese-console/dist"]))
            build = self.built(results=f"results-{len(seen)}")
            for field in ("inputs", "sources"):
                self.assertIn(field, build["nrese"], f"NRESE's build names its {field}")
            self.assertIn("inputs", build["harness"], "the harness's build names its inputs")
            self.assertEqual(build["nrese"]["inputs"], {"apps/nrese-console/dist": "no files"}, case)
            self.assertEqual(build["harness"]["inputs"], {})
            self.assertFalse(any(f.startswith("apps/nrese-console/dist/") for b in self.builds for f in b["files"]))
        values = dict(seen)
        self.assertEqual(values["missing"], "no files")
        self.assertEqual(values["empty"], "no files")
        self.assertEqual(driver.expected_build_inputs("nrese"), {"apps/nrese-console/dist": "no files"})
        self.assertEqual(driver.expected_build_inputs("harness"), {})
        self.assertRegex(values["bundle a"], "^sha256:[0-9a-f]{64}$")
        self.assertEqual(len({values["bundle a"], values["bundle b"], values["nested"], "no files"}), 4)

    def test_a_host_build_runs_cargo_in_the_controlled_environment_only(self):
        """P2 design check r4 F15 (orchestrator 17:56; P2 second lead 18:00, an allowlist): cargo,
        rustc and every build script see only the variables the build owner names, so encoded flags
        (the counting allocator), a profile override, cc's knobs or a compile-time revision string
        never change a binary whose record says default. What a build is told to use is kept; its
        target directory is its program's own."""
        self.checkout(self.root, "suite")
        given = {"CARGO_ENCODED_RUSTFLAGS": "--cfg\x1falloc_profile", "CARGO_PROFILE_RELEASE_OPT_LEVEL": "0",
                 "CRATE_CC_NO_DEFAULTS": "1", "CFLAGS": "-O0", "RUSTC_WRAPPER": "w", "RUSTUP_TOOLCHAIN": "nightly",
                 "NRESE_BUILD_REVISION": "r", "SOMETHING_ELSE": "x",
                 "RUSTFLAGS": "-g", "NRESE_TARGET_CPU": "znver3", "CARGO_TARGET_DIR": str(self.root / "t"),
                 "CARGO_HOME": str(self.root / "data" / "cargo-home"), "NRESE_QUIET_DIR": "q", "FLEET_SLICE": "s"}
        base = self.clean_root()
        with patch.dict(driver.os.environ, given):
            build = self.built(export_base=base)
            expected = dict(driver.os.environ)
        self.assertEqual(len(self.builds), 2)  # NRESE's and the harness's host builds
        for made in self.builds:
            environment = made["environ"]
            self.assertIsNotNone(environment)  # a full environment, never one merged onto this process's
            for name in ("CARGO_ENCODED_RUSTFLAGS", "CARGO_PROFILE_RELEASE_OPT_LEVEL", "CRATE_CC_NO_DEFAULTS", "CFLAGS",
                         "RUSTC_WRAPPER", "RUSTUP_TOOLCHAIN", "NRESE_BUILD_REVISION", "SOMETHING_ELSE"):
                self.assertNotIn(name, environment)
            for name in ("RUSTFLAGS", "NRESE_TARGET_CPU", "CARGO_HOME", "NRESE_QUIET_DIR", "FLEET_SLICE", "PATH"):
                self.assertEqual(environment.get(name), expected.get(name), name)
        self.assertEqual(self.builds[0]["environ"]["CARGO_TARGET_DIR"], str(self.root / "nrese-target"))
        self.assertEqual((build["nrese"]["target_cpu"], build["nrese"]["rustflags_env"]), ("znver3", "-g"))
        self.assertIs(build["nrese"]["controlled"], True)

    def test_a_build_compiles_a_clean_export_of_its_commit(self):
        """P1e (orchestrator 18:34; P2 design check r5 F17): a build runs in a fresh export of its
        checkout's commit, never the worktree: a file git ignores (a legacy .cargo/config with
        opt-level 0, excluded through .git/info/exclude) or a tracked file changed since the commit
        isn't there; the export is outside the checkout and goes with the build."""
        self.checkout(self.root, "suite")
        (self.root / ".git" / "info").mkdir(parents=True, exist_ok=True)
        (self.root / ".git" / "info" / "exclude").write_text(".cargo/config\n", encoding="utf-8")
        (self.root / ".cargo" / "config").write_text("[profile.release]\nopt-level = 0\n", encoding="utf-8")
        (self.root / "Cargo.lock").write_text("changed since the commit", encoding="utf-8")
        base = self.clean_root()
        with patch.dict(driver.os.environ, {"CARGO_HOME": str(self.root / "no-cargo-home")}):
            build = self.built(export_base=base)
        for made in self.builds:
            self.assertFalse(made["cwd"].resolve().is_relative_to(self.root.resolve()), made["cwd"])
            self.assertIn(".cargo/config.toml", made["files"])
            self.assertNotIn(".cargo/config", made["files"])
        self.assertIs(build["nrese"]["controlled"], True)
        self.assertEqual(self.export_text("Cargo.lock"), "suite")  # the commit's, not the worktree's

    def export_text(self, name: str) -> str:
        """`name` as an export of self.root's HEAD has it."""
        with tempfile.TemporaryDirectory() as d:
            driver.export_commit(self.root, self.git(self.root, "rev-parse", "HEAD"), Path(d))
            return (Path(d) / name).read_text(encoding="utf-8")

    def test_an_export_s_files_are_newer_than_any_earlier_build_whatever_their_commit_s_date(self):
        """P1e: cargo decides freshness by mtimes; an export stamped with its commit's (an older
        commit's) time would read as older than the last build's outputs and keep a stale binary. Every
        file and directory of an export has the time it was made."""
        self.checkout(self.root, "suite")
        old = subprocess.run(["git", "-C", str(self.root), "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q",
                              "--allow-empty", "-m", "old"], capture_output=True, text=True, check=True,
                             env={**os.environ, "GIT_COMMITTER_DATE": "2000-01-01T00:00:00", "GIT_AUTHOR_DATE":
                                  "2000-01-01T00:00:00"})
        self.assertEqual(old.returncode, 0)
        began = time.time() - 2
        with tempfile.TemporaryDirectory() as d:
            driver.export_commit(self.root, self.git(self.root, "rev-parse", "HEAD"), Path(d))
            stamps = [f.stat().st_mtime for f in Path(d).rglob("*")]
        self.assertTrue(stamps)
        self.assertGreaterEqual(min(stamps), began)

    def test_a_build_whose_commit_can_t_be_exported_doesn_t_run(self):
        """P1e attack 6: no commit (not a checkout), or an export that fails: the build doesn't start,
        says why, and nothing of it is left; never a build from the worktree in its place."""
        suite = self.suite(runs=1, skip_build=False)
        nrese = Mock(spec=driver.Nrese, key="nrese", source=Mock(return_value=self.root), target_volume="v",
                     target_dir=Mock(return_value=self.root / "t"))
        with patch.object(suite, "host_command", return_value=0) as run:
            self.assertFalse(suite.build_nrese(nrese))  # self.root is no checkout here
            self.checkout(self.root, "suite")
            with patch.object(driver, "export_commit", side_effect=OSError("disk full")):
                self.assertFalse(suite.build_harness())
        run.assert_not_called()
        self.assertEqual(suite.made_exports, [])
        said = " ".join(str(c.args[0]) for c in suite.say.call_args_list)
        self.assertIn("no commit to export", said)
        self.assertIn("disk full", said)

    def test_a_build_running_another_toolchain_than_declared_is_uncontrolled(self):
        """P2 design check r5 F16: a saved rustup override for the build directory, or a PATH straight at
        another toolchain's bin, selects another compiler. The build records what rustc and cargo
        report there (toolchain_here) and is controlled only when both are rust-toolchain.toml's
        channel. A PATH whose first rustc and cargo report 1.97.0: unknown, naming both."""
        self.checkout(self.root, "suite")
        bin_ = self.root / "data" / "other-toolchain"
        bin_.mkdir(parents=True)
        for tool in ("rustc", "cargo"):
            if os.name == "nt":
                (bin_ / f"{tool}.cmd").write_text(f"@echo {tool} 1.97.0\r\n@echo release: 1.97.0\r\n@echo host: x\r\n",
                                                  encoding="utf-8")
            else:
                (bin_ / tool).write_text(f"#!/bin/sh\necho '{tool} 1.97.0'\necho 'release: 1.97.0'\necho 'host: x'\n",
                                         encoding="utf-8", newline="\n")
                (bin_ / tool).chmod(0o755)
        seen = driver.toolchain_here(self.root, {**os.environ, "PATH": str(bin_)})
        self.assertEqual(seen, {"rustc": "1.97.0", "cargo": "1.97.0", "host": "x"})
        build = self.built(toolchain=seen)
        for program in ("nrese", "harness"):
            with self.subTest(program):
                self.assertEqual(build[program]["toolchain"], seen)
                self.assertIsInstance(build[program]["controlled"], dict, "another toolchain isn't controlled")
                self.assertIn("rustc 1.97.0 selected, rust-toolchain.toml declares 1.98.1",
                              build[program]["controlled"]["unknown"])
                self.assertIn("cargo 1.97.0 selected", build[program]["controlled"]["unknown"])
        self.assertEqual(driver.declared_toolchain(self.root), "1.98.1")
        self.assertIn("no rustc", driver.toolchain_here(self.root, {"PATH": str(self.root / "nothing")})["unknown"])

    def test_cargo_config_outside_the_export_makes_a_build_uncontrolled(self):
        """P2 design check r4 F15, r5 F18: cargo also reads .cargo/config(.toml) in every ancestor of the
        directory it builds in and in its Cargo home; a build where one is there is `controlled`
        unknown, naming it. A relative CARGO_HOME is the child's, resolved against the build's own
        directory, never this process's (the suite launched from elsewhere). Neither: controlled."""
        self.checkout(self.root, "suite")
        outer = self.clean_root()  # outside the checkout, no config at or above it
        base = outer / "exports"
        base.mkdir()
        home = outer / "cargo-home"
        home.mkdir()
        for case in ("none", "ancestor", "cargo home", "relative cargo home"):
            with self.subTest(case), patch.object(driver, "build_export_base", return_value=base):
                env = {"CARGO_HOME": str(home)}
                if case == "ancestor":
                    (outer / ".cargo").mkdir()
                    (outer / ".cargo" / "config.toml").write_text("[build]\n", encoding="utf-8")
                if case == "cargo home":
                    (outer / ".cargo" / "config.toml").unlink()
                    (home / "config.toml").write_text("[build]\n", encoding="utf-8")
                if case == "relative cargo home":
                    (home / "config.toml").unlink()
                    env = {"CARGO_HOME": "relative-home"}
                    elsewhere = outer / "elsewhere"
                    (elsewhere / "relative-home").mkdir(parents=True)  # where this process would look: empty
                with patch.dict(driver.os.environ, env):
                    if case == "relative cargo home":
                        # The child's: inside its export, which a commit can hold; here, a config put there
                        # by the commit itself, so the build reads it.
                        (self.root / "relative-home").mkdir()
                        (self.root / "relative-home" / "config.toml").write_text("[build]\n", encoding="utf-8")
                        self.checkout(self.root, "suite")
                        cwd = os.getcwd()
                        os.chdir(elsewhere)
                        try:
                            build = self.built(results=f"results-{case.replace(' ', '-')}")
                        finally:
                            os.chdir(cwd)
                    else:
                        build = self.built(results=f"results-{case.replace(' ', '-')}")
                controlled = build["nrese"]["controlled"]
                if case == "none":
                    self.assertIs(controlled, True)
                    continue
                self.assertIsInstance(controlled, dict, f"{case}: a config outside the export isn't controlled")
                if case == "relative cargo home":
                    self.assertIn("relative-home", controlled["unknown"])
                    self.assertNotIn("elsewhere", controlled["unknown"])
                else:
                    where = outer / ".cargo" / "config.toml" if case == "ancestor" else home / "config.toml"
                    self.assertIn(str(where.resolve()), controlled["unknown"])

    def test_an_export_goes_beside_its_checkout_and_a_config_there_is_named(self):
        """P1e (build-controlled-build-3, phuoc-yu): the launcher's free-disk guard measures the
        filesystem it runs from, the export's; one in the system's temporary directory (a small /tmp)
        refused a build whose target had room. The export goes beside the build's own checkout,
        wherever TEMP is, on the filesystem the guard always measured. Cargo reads a .cargo/config
        above it there too: a build with one beside its checkout says so, naming the file the test
        wrote (the layout of a fleet Windows job's checkout, master 22:34)."""
        outer = self.clean_root()
        source = self.checkout(outer / "repo", "nrese")
        (outer / ".cargo").mkdir()
        (outer / ".cargo" / "config.toml").write_text("[build]\n", encoding="utf-8")
        self.checkout(self.root, "suite")
        elsewhere = self.clean_root()  # TEMP, somewhere else
        with patch.object(driver.tempfile, "gettempdir", return_value=str(elsewhere)), \
                patch.dict(driver.os.environ, {"CARGO_HOME": str(self.root / "no-cargo-home")}):
            build = self.built(source=source)
        self.assertEqual(self.builds[0]["cwd"].resolve().parent, outer.resolve())  # NRESE's, beside its checkout
        self.assertEqual(self.builds[1]["cwd"].resolve().parent, self.root.resolve().parent)  # the harness's
        controlled = build["nrese"]["controlled"]
        self.assertIsInstance(controlled, dict, "a config above the export isn't controlled")
        self.assertIn(str((outer / ".cargo" / "config.toml").resolve()), controlled["unknown"])

    def test_an_export_is_the_commit_s_tree_byte_for_byte_whatever_git_s_attributes_say(self):
        """P1e review r1 F1 (master 23:12): git archive applies export-ignore and export-subst, from
        attributes outside the commit too (core.attributesFile, .git/info/attributes), and replace refs
        swap an object under its id: an export without .cargo/config.toml read as controlled. The
        export holds every file of the commit's tree with its committed bytes, nothing else; a build
        in it reads its committed .cargo/config.toml and is controlled."""
        self.checkout(self.root, "suite")
        (self.root / "version.txt").write_text("$Format:%H$\n", encoding="utf-8", newline="\n")
        self.git(self.root, "add", "version.txt")
        self.git(self.root, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "subst")
        commit = self.git(self.root, "rev-parse", "HEAD")
        attributes = self.root / "data" / "attributes"
        attributes.write_text(".cargo/config.toml export-ignore\nversion.txt export-subst\n", encoding="utf-8")
        (self.root / ".git" / "info").mkdir(parents=True, exist_ok=True)
        (self.root / ".git" / "info" / "attributes").write_text("Cargo.lock export-ignore\n", encoding="utf-8")
        lock = self.git(self.root, "rev-parse", "HEAD:Cargo.lock")
        other = subprocess.run(["git", "-C", str(self.root), "hash-object", "-w", "--stdin"], input="replaced",
                               capture_output=True, text=True, check=True).stdout.strip()
        self.git(self.root, "replace", lock, other)
        external = {"GIT_CONFIG_COUNT": "1", "GIT_CONFIG_KEY_0": "core.attributesFile",
                    "GIT_CONFIG_VALUE_0": str(attributes)}
        expected = {path: self.git(self.root, "--no-replace-objects", "cat-file", "blob", f"{commit}:{path}")
                    for path in self.git(self.root, "ls-tree", "-r", "--name-only", commit).splitlines()}
        with tempfile.TemporaryDirectory() as d, patch.dict(driver.os.environ, external):
            driver.export_commit(self.root, commit, Path(d))
            got = {f.relative_to(d).as_posix(): f.read_text(encoding="utf-8").strip()
                   for f in Path(d).rglob("*") if f.is_file()}
        self.assertEqual(set(got), set(expected))  # .cargo/config.toml and Cargo.lock both there
        self.assertEqual(got, {k: v.strip() for k, v in expected.items()})  # $Format$ kept, Cargo.lock not replaced
        base = self.clean_root()
        with patch.dict(driver.os.environ, {**external, "CARGO_HOME": str(self.root / "no-cargo-home")}):
            build = self.built(export_base=base)
        for made in self.builds:
            self.assertIn(".cargo/config.toml", made["files"])
        self.assertIs(build["nrese"]["controlled"], True)

    def test_an_export_refuses_what_isn_t_a_plain_file(self):
        """P1e review r1 F1, attack 14: a symlink (or submodule) entry is refused, never followed nor
        dropped: the build doesn't run."""
        self.checkout(self.root, "suite")
        target = subprocess.run(["git", "-C", str(self.root), "hash-object", "-w", "--stdin"], input="Cargo.toml",
                                capture_output=True, text=True, check=True).stdout.strip()
        self.git(self.root, "update-index", "--add", "--cacheinfo", f"120000,{target},link")
        self.git(self.root, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "a link")
        with tempfile.TemporaryDirectory() as d, self.assertRaises(OSError) as refused:
            driver.export_commit(self.root, self.git(self.root, "rev-parse", "HEAD"), Path(d))
        self.assertIn("link", str(refused.exception))

    def test_the_commit_exported_is_the_one_the_build_s_record_names(self):
        """P1e review r1 F1: the export binds to the commit built_from recorded, never HEAD read again
        (a checkout that moves between the two would build another commit than its record names)."""
        self.checkout(self.root, "suite")
        recorded = self.git(self.root, "rev-parse", "HEAD")
        suite = self.suite(runs=1, skip_build=False)
        states = iter([{"commit": recorded, "dirty": 0}] + [{"commit": "f" * 40, "dirty": 0}] * 5)
        with patch.object(driver, "checkout_state", side_effect=lambda *a: next(states)), \
                patch.object(driver, "export_commit") as exported, \
                patch.object(suite, "host_command", return_value=0), \
                patch.object(driver, "toolchain_here", return_value=dict(self.DECLARED)):
            suite.build_harness()
        self.assertEqual(exported.call_args.args[1], recorded)

    def test_a_build_that_raises_still_has_its_record_in_the_manifest(self):
        """P1e review r1 F2: a builder that raises (an OSError at launch, a log it can't create, an
        interrupt) still leaves its record in the run's manifest."""
        suite = self.suite(runs=1, skip_build=False)

        def raising(*a):
            suite.built["nrese"] = {"commit": "a" * 40, "controlled": True}
            raise OSError("no bash")
        nrese = Mock(spec=driver.Nrese, key="nrese", regimes={"none": ""}, images=Mock(return_value=[]),
                     builds=Mock(return_value=[]), script_builds=Mock(return_value=[]),
                     supports=Mock(return_value=None), definition_paths=())
        with patch.object(suite, "build_nrese", side_effect=raising), \
                patch.object(driver, "plans", return_value=[Plan("w", "1")]), \
                patch.object(driver, "adapter", return_value=nrese), \
                patch.object(suite, "ensure_data", return_value=None), \
                patch.object(suite, "sif_missing", return_value=None), self.assertRaises(OSError):
            suite.run()
        entry = json.loads((suite.results / "manifest.json").read_text(encoding="utf-8"))["invocations"][-1]
        self.assertEqual(entry.get("builds"), {"nrese": {"commit": "a" * 40, "controlled": True}})

    def test_an_export_past_its_deadline_stops_its_git_and_fails(self):
        """P1e review r1 F4: one deadline for the whole export, its git children killed at it."""
        self.checkout(self.root, "suite")
        with tempfile.TemporaryDirectory() as d, self.assertRaises(OSError) as late:
            driver.export_commit(self.root, self.git(self.root, "rev-parse", "HEAD"), Path(d), timeout=0)
        self.assertIn("deadline", str(late.exception))

    def test_an_export_refuses_two_directory_spellings_one_filesystem_merges(self):
        """P1e sol-loop r3 S3: `Dir/a` and `dir/b` are two trees in git, one directory where case
        folds (Windows): the export refuses rather than merge them (case folding emulated here)."""
        self.checkout(self.root, "suite")
        blob = subprocess.run(["git", "-C", str(self.root), "hash-object", "-w", "--stdin"], input="x",
                              capture_output=True, text=True, check=True).stdout.strip()
        for path in ("Dir/a", "dir/b"):
            self.git(self.root, "update-index", "--add", "--cacheinfo", f"100644,{blob},{path}")
        self.git(self.root, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "two spellings")
        with tempfile.TemporaryDirectory() as d, patch.object(driver.os.path, "normcase", side_effect=str.lower), \
                self.assertRaises(OSError) as refused:
            driver.export_commit(self.root, self.git(self.root, "rev-parse", "HEAD"), Path(d))
        self.assertIn("same", str(refused.exception))

    def test_a_stalled_export_is_killed_at_its_deadline(self):
        """P1e sol-loop r3 S4: a cat-file that never answers is killed at the export's deadline, and
        the export fails saying so, well within it plus a margin."""
        self.checkout(self.root, "suite")
        real = subprocess.Popen
        started = []

        def stalled(argv, *a, **k):
            if "cat-file" in argv:
                child = real([sys.executable, "-c", "import time; time.sleep(120)"], *a, **k)
                started.append(child)
                return child
            return real(argv, *a, **k)
        began = time.monotonic()
        with tempfile.TemporaryDirectory() as d, patch.object(driver.subprocess, "Popen", side_effect=stalled), \
                self.assertRaises(OSError) as late:
            driver.export_commit(self.root, self.git(self.root, "rev-parse", "HEAD"), Path(d), timeout=2)
        self.assertLess(time.monotonic() - began, 30)
        self.assertIn("deadline", str(late.exception))
        self.assertEqual(len(started), 1)
        self.assertIsNotNone(started[0].poll())  # killed, not left running

    def test_every_compile_time_variable_our_sources_read_is_set_by_cargo_or_never_passed(self):
        """P2 second lead 17:59 (rule: fix recurring mistakes mechanically): a variable our Rust sources
        read at compile time (env!, option_env!) or a build script reads (env::var) changes the binary;
        each is one cargo sets for the build, or controlled_environment doesn't pass it. A new one
        fails here until the build owner decides."""
        import re
        names = set()
        sources = [*ROOT.glob("crates/**/src/**/*.rs"), *ROOT.glob("benches/nrese-bench-harness/src/**/*.rs")]
        for path in sources:
            names |= set(re.findall(r'(?:option_env|env)!\(\s*"([A-Za-z0-9_]+)"', path.read_text(encoding="utf-8")))
        for path in ROOT.glob("crates/**/build.rs"):
            names |= set(re.findall(r'env::var(?:_os)?\(\s*"([A-Za-z0-9_]+)"', path.read_text(encoding="utf-8")))
        self.assertIn("NRESE_BUILD_REVISION", names)  # the scan finds today's one (draft_check.rs)
        cargo_sets = re.compile(r"^(CARGO_.*|OUT_DIR|TARGET|HOST|PROFILE|OPT_LEVEL|DEBUG|NUM_JOBS|RUSTC|RUSTDOC)$")
        for name in sorted(names):
            if not cargo_sets.match(name):
                with self.subTest(name):
                    self.assertNotIn(name, driver.controlled_environment({name: "x"}))

    def test_a_run_s_manifest_holds_what_it_built_whether_or_not_a_cell_ran(self):
        """Orchestrator 18:09 (P1e's Windows proof): the build records a cell's receipt copies are in
        its invocation's manifest entry too (`builds`), as the builds end: a run whose cells all skip
        (no data) still says what it built and how."""
        suite = self.suite(runs=1)
        record = {"commit": "c" * 40, "dirty": 0, "controlled": True}

        def built():
            suite.built["harness"] = record
            return True
        with patch.object(driver.Suite, "build_harness", side_effect=built):
            with patch.object(driver, "plans", return_value=[Plan("w", "1", inputs=["/data/missing.nt"])]), \
                    patch.object(driver, "adapter", return_value=self.system()), \
                    patch.object(suite, "sif_missing", return_value=None):
                suite.run()
        entry = json.loads((suite.results / "manifest.json").read_text(encoding="utf-8"))["invocations"][-1]
        self.assertEqual(entry["builds"], {"harness": record})
        self.assertEqual(entry.get("cells", {}), {})  # its one cell skipped: no receipt

    def test_a_relative_target_directory_is_the_checkout_s_never_the_export_s(self):
        """P1e sol-loop r1 S1: a build now runs in its export, which goes after the build; a relative
        CARGO_TARGET_DIR ("target") resolved there would put the binary in the export, removed with it,
        while the run executes the checkout's, maybe stale. Both owners resolve it against the
        repository: the harness's build and the harness the run executes, and NRESE's target_dir (its
        binary and its build) alike."""
        self.checkout(self.root, "suite")
        self.built(settings={"CARGO_TARGET_DIR": "target"})
        self.assertEqual(self.builds[1]["environ"]["CARGO_TARGET_DIR"], str(self.root / "target"))  # the harness's
        self.assertEqual(Path(self.suite_built.harness()),
                         self.root / "target" / "release" / f"nrese-bench-harness{driver.EXE}")
        nrese = driver.adapter("nrese")
        for given, expected in (("target", ROOT / "target"), (str(self.root / "t"), self.root / "t")):
            with self.subTest(given):
                ctx = driver.Context(None, ROOT, ROOT, ROOT, driver.Mount("-", "/data"), [], 0,
                                     {"CARGO_TARGET_DIR": given}, "images")
                self.assertEqual(nrese.target_dir(ctx), expected)

    def test_a_run_records_each_build_as_it_ends_a_failed_one_too(self):
        """P1e sol-loop r1 S2: the manifest's `builds` is written after each build, so a run whose
        harness build fails after NRESE's succeeded still says what it built and how."""
        suite = self.suite(runs=1, skip_build=False)
        records = {program: {"commit": "a" * 40, "controlled": True} for program in ("nrese", "harness")}

        def build(key, ok):
            def run(*a):
                suite.built[key] = records[key]
                return ok
            return run
        nrese = Mock(spec=driver.Nrese, key="nrese", regimes={"none": ""}, images=Mock(return_value=[]),
                     builds=Mock(return_value=[]), script_builds=Mock(return_value=[]),
                     supports=Mock(return_value=None), definition_paths=())
        with patch.object(suite, "build_nrese", side_effect=build("nrese", True)), \
                patch.object(suite, "build_harness", side_effect=build("harness", False)), \
                patch.object(driver, "plans", return_value=[Plan("w", "1")]), \
                patch.object(driver, "adapter", return_value=nrese), \
                patch.object(suite, "ensure_data", return_value=None), \
                patch.object(suite, "sif_missing", return_value=None):
            self.assertEqual(suite.run(), 1)
        entry = json.loads((suite.results / "manifest.json").read_text(encoding="utf-8"))["invocations"][-1]
        self.assertEqual(entry.get("builds"), records)

    def test_build_settings_are_read_from_the_environment_given_only(self):
        """P2 design check r3 F14 (orchestrator 17:39): the default build's settings come from the build
        owner with an empty environment, whatever this process's environment holds; a given one is read.
        (Its witness against an owner that takes `env` but reads os.environ is a mutation of that one
        read, run in the proof: P1d review r1 F1.)"""
        with patch.dict(driver.os.environ, {"NRESE_TARGET_CPU": "znver3", "RUSTFLAGS": "-C opt-level=2"}):
            self.assertEqual(driver.build_settings({}), {"target_cpu": "native", "rustflags_env": ""})
        self.assertEqual(driver.build_settings({"NRESE_TARGET_CPU": "portable", "RUSTFLAGS": "-g"}),
                         {"target_cpu": "portable", "rustflags_env": "-g"})

    def test_a_docker_build_runs_in_its_rust_image_s_pinned_id(self):
        """Orchestrator, 8 Nov (P1d): the Rust image a Docker build compiles in is the run's pinned
        one (ns.pinned, as every container's), by id, never its tag looked up again, and the receipt
        names that id; a build whose image isn't pinned doesn't start. P1e: the source it mounts is the
        export of its commit, never the worktree, and its toolchain is the image's, checked."""
        self.checkout(self.root, "suite")  # a checkout to export, as a run's source is (P1e)
        suite = self.suite(runs=1, skip_build=False)
        nrese = Mock(spec=driver.Nrese, key="nrese", source=Mock(return_value=self.root),
                     rust_image=Mock(return_value="rust:1.98.1-bookworm"), target_volume="v")
        for pins, built in (({"rust:1.98.1-bookworm": "sha256:" + "r" * 64}, True), ({}, False)):
            with self.subTest(pinned=bool(pins)), patch.object(suite.args, "runtime", "docker"), \
                    patch.object(suite, "host_command", return_value=0) as run, patch.object(driver.ns, "docker"), \
                    patch.object(driver, "toolchain_in", return_value=dict(self.DECLARED)) as probe, \
                    patch.object(driver.ns, "pinned", return_value=pins):
                self.assertIs(suite.build_nrese(nrese), built)
                if built:
                    command = run.call_args.args[0]
                    self.assertIn("sha256:" + "r" * 64, command)
                    self.assertNotIn("rust:1.98.1-bookworm", command)
                    self.assertEqual(suite.built["nrese"]["rust_image_id"], "sha256:" + "r" * 64)
                    mounted = command[command.index("-v") + 1].rsplit(":/src", 1)[0]
                    self.assertNotEqual(Path(mounted).resolve(), self.root.resolve())
                    self.assertEqual(probe.call_args.args[1].as_posix(), mounted)
                    self.assertEqual(suite.built["nrese"]["toolchain"], self.DECLARED)
                    self.assertIs(suite.built["nrese"]["controlled"], True)
                    self.assertEqual(suite.made_exports, [])
                else:
                    run.assert_not_called()

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

    def test_a_pinned_image_runs_its_pin_or_the_run_stops_before_pinning(self):
        """P1c review r1 F1, F2: the digest pull's and the retag's exit codes were ignored, so with a
        mismatching image already holding the tag a failed pull went on, and the run timed the old
        content. Modelled on a local docker's state (tags, ids, digests): the tag ends on the pinned
        content, or the run stops (non-zero) before any image is pinned or timed."""
        pinned_id, old_id = "sha256:" + "1" * 64, "sha256:" + "2" * 64
        self.pins(f'[pins]\n"qlever/qlever:latest" = "{self.PIN}"\n')
        for name, pull_ok, tag_ok in (("pinned", True, True), ("the pull fails", False, True), ("the retag fails", True, False)):
            with self.subTest(name):
                tags = {"qlever/qlever:latest": old_id}  # present, another build
                digests = {old_id: {"sha256:" + "e" * 64}}

                def docker(command, *a, **k):
                    if command[:2] == ["docker", "pull"]:
                        if not pull_ok:
                            return 1
                        tags[command[-1]] = pinned_id
                        digests[pinned_id] = {self.PIN}
                    elif command[:2] == ["docker", "tag"]:
                        if not tag_ok or command[-2] not in tags:  # docker: no such source image
                            return 1
                        tags[command[-1]] = tags[command[-2]]
                    return 0

                def inspect(command, *a, **k):
                    held = digests.get(tags.get(command[-1], command[-1]), set())
                    return Mock(returncode=0, stdout=json.dumps(sorted(f"qlever/qlever@{d}" for d in held)) + "\n")
                suite = self.suite(runs=1, results=f"results-{name.replace(' ', '-')}")
                pinning = Mock()
                with patch.object(suite.args, "runtime", "docker"), patch.object(suite, "host_command", side_effect=docker), \
                        patch.object(suite.runtime, "image_present", side_effect=lambda image: image in tags), \
                        patch.object(driver, "run_quiet", side_effect=inspect), patch.object(driver.ns, "pin_images", pinning), \
                        patch.object(driver, "plans", return_value=[Plan("w", "1")]), \
                        patch.object(driver, "adapter", return_value=self.system(images=["qlever/qlever:latest"])), \
                        patch.object(suite, "cycle") as cycle, patch.object(suite, "build_harness", return_value=True), \
                        patch.object(suite, "ensure_data", return_value=None), \
                        patch.object(suite, "sif_missing", return_value=None):
                    code = suite.run()
                if pull_ok and tag_ok:
                    self.assertEqual(tags["qlever/qlever:latest"], pinned_id)
                    pinning.assert_called_once()
                else:
                    self.assertEqual(code, 1)
                    pinning.assert_not_called()
                    cycle.assert_not_called()

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
        nrese = Mock(spec=driver.Nrese, key="nrese", images=Mock(return_value=[]), builds=Mock(return_value=[]),
                     script_builds=Mock(return_value=[]))
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
