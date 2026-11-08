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
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from suitekit import driver, ledger, receipts  # noqa: E402
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
        cells = self.cells(suite)
        self.assertEqual(sorted(cells), ["w/1/x", "w/2/x"])
        self.assertFalse(cells["w/2/x"]["complete"])
        self.assertEqual(sorted(cells["w/2/x"]), ["attempt", "complete", "started"])
        receipt = cells["w/1/x"]
        self.assertTrue(receipt["complete"])
        self.assertEqual(receipt["outcome"]["outcome"], "ok")
        self.assertEqual(receipt["outcome"]["runs"], 2)
        self.assertEqual(receipt["answers"], "not captured")  # --answers off
        self.assertEqual(receipt["data"], {"/data/a.nt": hashlib.sha256(b"<a> <b> <c> .\n").hexdigest()})
        self.assertNotEqual(receipt["attempt"], cells["w/2/x"]["attempt"])

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
        self.assertNotIn("answers", cells["w/2/x"])

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


    def test_an_attempt_that_can_t_be_published_durably_starts_no_work(self):
        """Fix check r2 (review cell-receipts r2, R2): the attempt was written with a replace and no
        fsync, so after a host crash the older manifest could come back and I1's receipt stand for
        I2's interrupted attempt. I1 completed A (tier 1) and B (tier 2); I2 attempts A again and
        publishing its attempt fails at the file's fsync or at the replace: no work of A starts, no
        row is written, A's evidence stays I1's, B's receipt is kept."""
        for fault in ("fsync", "replace"):
            with self.subTest(fault=fault):
                self.setUp()
                # A checkout, as a run's always is (the ledger reads its dirty count).
                subprocess.run(["git", "init", "-q", str(self.root)], capture_output=True, check=True)
                first = self.suite(runs=1)
                self.run_tiers(first, [Plan("w", "1"), Plan("w", "2")], self.system())
                rows = (first.results / "results.csv").read_text(encoding="utf-8")
                again, started = self.suite(runs=1), []
                with patch(f"suitekit.manifest.os.{fault}", side_effect=OSError(f"{fault} failed")), \
                        self.assertRaises(OSError):
                    self.run_tiers(again, [Plan("w", "1")], self.system(),
                                   stop=lambda plan, run: started.append(plan.tier) or False)
                self.assertEqual(started, [])
                self.assertEqual((first.results / "results.csv").read_text(encoding="utf-8"), rows)
                with patch("suitekit.versus.versus", return_value=[]), patch("suitekit.cases.load", return_value=[]):
                    pairs = {p["tier"]: p for p in ledger.from_results(first.results, self.root)["pairs"]}
                i1 = json.loads((first.results / "manifest.json").read_text(encoding="utf-8"))["invocations"][0]["cells"]
                self.assertEqual(pairs["1"]["receipt"]["attempt"], i1["w/1/x"]["attempt"])
                self.assertTrue(pairs["2"]["receipt"]["complete"])

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
        """The file's bytes are flushed (fsync) before the replace, and on POSIX the directory
        after it, so a rename that survives a crash names a whole file."""
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
