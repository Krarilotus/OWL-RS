"""Cell receipts (suitekit/receipts.py): what a cell ran, written when it completes, kept per cell by
the ledger. Specified by Harness notes 2026-11-08-coverage-refresh/design.md r2 and its doubt review
(design-review-sol.md F1, F2, F3, F4, F8): an interrupted cell has none (F1), a recorded wrong answer
stands against counts that look right (F2), the code it ran includes what its commands and system
read (F3), its data is the bytes it read (F4), and each cell keeps its own invocation (F8)."""
from __future__ import annotations

import hashlib
import json
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from suitekit import driver, ledger  # noqa: E402
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


class Ledger(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.run = self.root / "run"
        self.run.mkdir()

    def write(self, lines, invocations, verdicts=None):
        (self.run / "results.csv").write_text(HEADER + "".join(lines), encoding="utf-8")
        (self.run / "manifest.json").write_text(json.dumps({"schema": 1, "invocations": invocations}), encoding="utf-8")
        if verdicts is not None:
            (self.run / "answers.json").write_text(json.dumps({"verdicts": verdicts}), encoding="utf-8")
        with patch("suitekit.versus.versus", return_value=[]), patch("suitekit.cases.load", return_value=[]):
            return {p["system"]: p for p in ledger.from_results(self.run, self.root)["pairs"]}

    def test_each_cell_keeps_its_own_invocation_s_receipt(self):
        """F8: cell A ran at C1, cell B was added later at C2 in the same directory. The record's commit
        is both; each cell's receipt is its own invocation's, and survives the record's TOML."""
        a = {"complete": True, "commit": "c1" * 20, "code": {"benches/suite": "t1"}, "data": {"/data/x.nt": None}}
        b = {"complete": True, "commit": "c2" * 20, "code": {"benches/suite": "t2"}, "data": {}}
        pairs = self.write([row("oxigraph"), row("jena")],
                           [invocation("2026-11-08T01:00:00", "c1" * 20, {"lubm/1/oxigraph": a}),
                            invocation("2026-11-08T02:00:00", "c2" * 20, {"lubm/1/jena": b})])
        self.assertEqual(pairs["oxigraph"]["receipt"]["commit"], "c1" * 20)
        self.assertEqual(pairs["jena"]["receipt"]["commit"], "c2" * 20)
        with patch("suitekit.versus.versus", return_value=[]), patch("suitekit.cases.load", return_value=[]):
            text = ledger.dump(ledger.from_results(self.run, self.root))
        back = {p["system"]: p for p in tomllib.loads(text)["pairs"]}
        self.assertEqual(back["oxigraph"]["receipt"]["code"], {"benches/suite": "t1"})
        self.assertEqual(back["oxigraph"]["receipt"]["data"], {})  # not known: absent, never "None"

    def test_a_cell_its_run_didn_t_finish_has_no_receipt(self):
        """F1: the invocation stopped before the tier completed, so it wrote no receipt for it."""
        pairs = self.write([row("oxigraph")], [invocation("2026-11-08T01:00:00", "c1" * 20, {})])
        self.assertEqual(pairs["oxigraph"]["outcome"], "ok")
        self.assertNotIn("receipt", pairs["oxigraph"])

    def test_a_recorded_wrong_answer_stands_against_counts_that_look_right(self):
        """F2: the rows count right, answers.json says one query's values are wrong."""
        verdicts = [{"workload": "lubm", "tier": "1", "system": "oxigraph", "query": "q1", "verdict": "ok"},
                    {"workload": "lubm", "tier": "1", "system": "oxigraph", "query": "q2", "verdict": "wrong"},
                    {"workload": "lubm", "tier": "1", "system": "jena", "query": "q1", "verdict": "deviates"}]
        pairs = self.write([row("oxigraph"), row("oxigraph", task="query", item="q2", rows="3"), row("jena")],
                           [invocation("2026-11-08T01:00:00", "c1" * 20, {})], verdicts)
        self.assertEqual(pairs["oxigraph"]["outcome"], "ok")
        self.assertEqual(pairs["oxigraph"]["answers"], "wrong")
        self.assertEqual(pairs["jena"]["answers"], "deviates")

    def test_a_licensed_system_s_answer_verdict_stays_local(self):
        verdicts = [{"workload": "lubm", "tier": "1", "system": "graphdb", "query": "q1", "verdict": "ok"}]
        pairs = self.write([row("graphdb", publish="permission")],
                           [invocation("2026-11-08T01:00:00", "c1" * 20, {})], verdicts)
        self.assertEqual(pairs["graphdb"]["outcome"], "restricted")
        self.assertNotIn("answers", pairs["graphdb"])


class Driver(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        (self.root / "data").mkdir()

    def suite(self, runs=2):
        args = driver.arguments(["--runtime", "process", "--data", str(self.root / "data"), "--results",
                                 str(self.root / "results"), "--systems", "x", "--workloads", "w",
                                 "--runs", str(runs), "--skip-build"])
        suite = driver.Suite(args, {"x": {"capabilities": ["load"], "publish": "free"}},
                             {"w": {"needs": ["load"], "state": "ready", "kit": ""}}, self.root)
        suite.say = Mock()
        suite.manifest = driver.Manifest(suite.results, self.root, [], args, "process", {})
        return suite

    def test_a_tier_s_receipts_are_written_when_it_completes_and_an_interrupted_tier_has_none(self):
        """F1 and per-cell progress: tier 1 ran both repetitions; tier 2 was interrupted in its second.
        Tier 1's cell has its receipt, with its own outcome; tier 2's has none."""
        suite = self.suite()
        tiers = [Plan("w", "1", inputs=["/data/a.nt"]), Plan("w", "2")]
        (self.root / "data" / "a.nt").write_bytes(b"<a> <b> <c> .\n")
        system = Mock(regimes={"none": ""}, definition_paths=(), images=Mock(return_value=[]),
                      builds=Mock(return_value=[]), supports=Mock(return_value=None))

        def cycle(plan, key, system, regime, run):
            if plan.tier == "2" and run == 2:
                raise KeyboardInterrupt
            suite.emit(dict(date=suite.date, host="h", runtime="process", system=key, version="-", publish="free",
                            workload=plan.workload, tier=plan.tier, regime="none", run=run), task="load", ms=5)

        with patch.object(driver, "plans", return_value=tiers), patch.object(driver, "adapter", return_value=system), \
                patch.object(suite, "cycle", side_effect=cycle), patch.object(suite, "build_harness", return_value=True), \
                patch.object(suite, "ensure_data", return_value=None), patch.object(suite, "sif_missing", return_value=None), \
                self.assertRaises(KeyboardInterrupt):
            suite.run()
        cells = json.loads((suite.results / "manifest.json").read_text(encoding="utf-8"))["invocations"][-1].get("cells", {})
        self.assertEqual(sorted(cells), ["w/1/x"])
        receipt = cells["w/1/x"]
        self.assertTrue(receipt["complete"])
        self.assertEqual(receipt["outcome"]["outcome"], "ok")
        self.assertEqual(receipt["outcome"]["runs"], 2)
        self.assertEqual(receipt["data"], {"/data/a.nt": hashlib.sha256(b"<a> <b> <c> .\n").hexdigest()})

    def test_each_input_is_hashed_once_a_run_and_a_missing_one_is_not_known(self):
        suite = self.suite()
        (self.root / "data" / "a.nt").write_bytes(b"x")
        with patch.object(driver.Store, "hashes", autospec=True, side_effect=driver.Store.hashes) as hashes:
            first = suite.data_hashes(["/data/a.nt", "/data/gone.nt"])
            again = suite.data_hashes(["/data/a.nt"])
        self.assertEqual(first, {"/data/a.nt": hashlib.sha256(b"x").hexdigest(), "/data/gone.nt": None})
        self.assertEqual(again, {"/data/a.nt": hashlib.sha256(b"x").hexdigest()})
        self.assertEqual(hashes.call_count, 1)


class Code(unittest.TestCase):
    def test_a_cell_s_code_is_every_path_it_runs_and_changes_with_any_of_them(self):
        """F3: its system's resources (Nemo's rules), its queries, its kit, the scripts its commands
        name (a file's directory); not an interpreter, an option or a tier."""
        from suitekit import receipts
        with tempfile.TemporaryDirectory() as d:
            root = Path(d).resolve()
            for f in ("benches/suite/x.py", "benches/cases/c.toml", "benches/reasoning/nemo/owl2rl.rls",
                      "benches/reasoning/queries/lubm/q1.rq", "benches/reasoning/prepare-lubm.sh",
                      "benches/fast/fast.py", "benches/fast/lib.py", "benches/nrese-bench-harness/Cargo.toml"):
                (root / f).parent.mkdir(parents=True, exist_ok=True)
                (root / f).write_text("1\n", encoding="utf-8")
            plan = Plan("lubm", "1", queries=root / "benches/reasoning/queries/lubm",
                        prepare=["bash", "benches/reasoning/prepare-lubm.sh", "1"],
                        driver=[[sys.executable, "benches/fast/fast.py", "run", "--cases", "a,b"]])
            paths = receipts.code_paths(root, plan, "benches/reasoning", ("benches/reasoning/nemo",))
            self.assertEqual(paths, ["benches/cases", "benches/fast", "benches/nrese-bench-harness", "benches/reasoning",
                                     "benches/reasoning/nemo", "benches/reasoning/queries/lubm", "benches/suite"])
            git = lambda *a: subprocess.run(["git", "-C", d, *a], capture_output=True, text=True,  # noqa: E731
                                            check=True).stdout.strip()
            git("init", "-q")
            commit = lambda: (git("add", "-A"), git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q",  # noqa: E731
                                                    "-m", "x"), git("rev-parse", "HEAD"))[2]
            before = receipts.object_ids(root, commit(), paths)
            (root / "benches/reasoning/nemo/owl2rl.rls").write_text("2\n", encoding="utf-8")
            after = receipts.object_ids(root, commit(), paths)
            self.assertEqual({p for p in paths if before[p] != after[p]}, {"benches/reasoning/nemo", "benches/reasoning"})
            self.assertTrue(all(before.values()))
            self.assertEqual(receipts.object_ids(root, "", ["benches/suite"]), {"benches/suite": None})


if __name__ == "__main__":
    unittest.main()
