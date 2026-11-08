"""Cell attempts and receipts (suitekit/receipts.py): what a cell ran, under the attempt written
before its work, kept per cell by the ledger. Specified by Harness notes
2026-11-08-coverage-refresh/design.md r2, its doubt review (F1-F8) and review cell-receipts r1
(R1-R6): an interrupted attempt stays incomplete and no older receipt stands in for it (F1, R2),
answers are the attempt's own (F2, R3), a licensed system's results stay local whatever its
outcome (R1), images are the ones run (R4), the code is everything the repository holds for it
(F3, R5), and what can't be evidenced is said unknown, never an empty map (R6)."""
from __future__ import annotations

import contextlib
import hashlib
import io
import json
import os
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from suitekit import driver, ledger, manifest, receipts  # noqa: E402
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


class Crash:
    """What a host crash leaves of a results directory, from the calls its manifest writers made:
    a file's bytes once its fsync returned, under a name already on disk; a new name or a rename
    once its directory was synced, which Windows can't (`windows`): there it is lost. What is
    there at the start counts as long on disk. `fail` fails the fsync publishing an attempt."""

    def __init__(self, directory: Path, windows: bool):
        self.directory, self.windows, self.names, self.pending = directory, windows, {}, {}
        self.disk = {p.name: p.read_bytes() for p in directory.iterdir()}

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
            if not self.windows:
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
        self.assertEqual(manifest.read_attempts(self.run), [{"cell": "lubm/1/x", **a2}])

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

    def suite(self, runs=2, skip_build=True):
        args = driver.arguments(["--runtime", "process", "--data", str(self.root / "data"), "--results",
                                 str(self.root / "results"), "--systems", "x", "--workloads", "w",
                                 "--runs", str(runs), *(["--skip-build"] if skip_build else [])])
        suite = driver.Suite(args, {"x": {"capabilities": ["load"], "publish": "free"}},
                             {"w": {"needs": ["load"], "state": "ready", "kit": ""}}, self.root)
        suite.say = Mock()
        suite.manifest = driver.Manifest(suite.results, self.root, [], args, "process", {})
        return suite

    def cells(self, suite):
        return json.loads((suite.results / "manifest.json").read_text(encoding="utf-8"))["invocations"][-1].get("cells", {})

    def journal(self, suite):
        lines = (suite.results / "attempts.jsonl").read_text(encoding="utf-8").splitlines()
        return {a["cell"]: a for a in map(json.loads, filter(None, lines))}

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
        self.assertEqual(self.cells(suite)["w/1/x"]["images"],
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
        self.assertEqual(cells["w/1/x"]["answers"], "wrong")
        self.assertNotIn("answers", cells.get("w/2/x", {}))

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
        file's name never reached the disk: on Windows (no directory flush: a rename or new name there
        is lost) then no receipt stands; on POSIX I2's directory sync puts it there, B's with it."""
        for windows, case in [(w, c) for w in (False, True) for c in ("flush fails", "crash", "journal lost")]:
            with self.subTest(windows=windows, case=case):
                self.setUp()
                # A checkout, as a run's always is (the ledger reads its dirty count).
                subprocess.run(["git", "init", "-q", str(self.root)], capture_output=True, check=True)
                first = self.suite(runs=1)
                self.run_tiers(first, [Plan("w", "1"), Plan("w", "2")], self.system())
                rows = (first.results / "results.csv").read_text(encoding="utf-8")
                i1 = json.loads((first.results / "manifest.json").read_text(encoding="utf-8"))["invocations"][0]["cells"]
                again, started, disk = self.suite(runs=1), [], Crash(first.results, windows)
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
                         None if case == "journal lost" and windows else i1["w/2/x"]["attempt"])

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
        """Guard: the file's bytes are flushed (fsync) before the replace, and on POSIX the directory
        after it, so a rename that survives a crash names a whole file (receipts; attempts don't
        rest on it)."""
        suite, calls = self.suite(runs=1), []
        real_fsync, real_replace = driver.os.fsync, driver.os.replace
        with patch("suitekit.manifest.os.fsync", side_effect=lambda fd: (calls.append("fsync"), real_fsync(fd))[1]), \
                patch("suitekit.manifest.os.replace", side_effect=lambda a, b: (calls.append("replace"), real_replace(a, b))[1]):
            suite.manifest.update(cells={"w/1/x": {"attempt": "a", "started": "t", "complete": False}})
        self.assertEqual(calls, ["fsync", "replace"] + (["fsync"] if driver.os.name != "nt" else []))


class Code(unittest.TestCase):
    def test_a_cell_s_code_changes_with_anything_the_repository_holds_for_it(self):
        """R5: commit-latency's driver reads queries, the harness and GraphDB's configuration outside
        its kit's directory, LUBM's preparation sources scripts/lib: a change to any of them changes
        the code of every cell (coarse and exact); without a commit to read, it is unknown."""
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
            paths = receipts.CODE_PATHS + receipts.BUILD_PATHS
            before = receipts.object_ids(root, commit(), paths)
            for f, changed in (("benches/reasoning/queries/lubm/q1.rq", "benches"),
                               ("benches/competitors/graphdb/repo-owl2-rl.ttl", "benches"),
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
