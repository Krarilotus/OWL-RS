"""Spec: slice 1 of the 7 Nov fleet-cells task; planning never measures or submits."""
import contextlib
import io
import json
import os
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import abba
import suite
import isolation

A = "a" * 40


class FleetCells(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.out = Path(self.tmp.name)
        self.calls = []
        self.addCleanup(patch.stopall)
        patch.object(abba, "git", side_effect=lambda root, *args: "" if args[0] == "status"
                     else "b" * 40 if args[-1].startswith("b" * 40) else A).start()
        patch.object(abba.rg, "side_inputs", return_value=[]).start()
        patch.object(abba.rg, "fleet_call", side_effect=self.fleet).start()

    def fleet(self, *args):
        self.calls.append(args)
        return 0, "set:accepted job:member"

    def run_plan(self, *extra):
        output = io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
            code = suite.main(["suite.py", "fleet", "--owner", "suite-fixture", "--nrese", A, "--out", str(self.out),
                               "--systems", "qlever", "--workloads", "sparqloscope", *extra])
        return code, output.getvalue()

    def document(self):
        paths = list(self.out.glob("*.set.toml"))
        self.assertTrue(paths, "fleet must emit the requested reviewable set")
        return tomllib.loads(paths[0].read_text())

    def test_every_tier_and_publication_boundary_has_an_omission(self):
        # Spec points 1/6: available tier selected; unavailable/refused/restricted cells omitted.
        code, text = self.run_plan("--systems", "qlever,graphdb,rdfox", "--tiers",
                                   "sparqloscope=sparqloscope-dblp,sparqloscope=absent", "--budget-hours", "50")
        self.assertEqual(code, 0, text)
        self.assertIn("qlever / sparqloscope / sparqloscope-dblp", text)
        for reason in ("no tier absent", "restricted", "refused"):
            self.assertIn(reason, text)
        members = self.document()["job"]
        self.assertTrue(members)
        self.assertNotIn("graphdb", json.dumps(members))
        self.assertNotIn("rdfox", json.dumps(members))

    def test_budget_prunes_before_any_submission(self):
        # Spec point 4: a zero budget admits no cells; a negative or nonfinite one is invalid.
        code, text = self.run_plan("--budget-hours", "0", "--submit")
        self.assertEqual(code, 0, text)
        self.assertIn("budget", text)
        self.assertFalse(self.calls)
        code, text = self.run_plan("--budget-hours", "30")
        self.assertEqual(code, 0, text)
        self.assertIn("budget", text)  # three 600-minute systems plus build/preparation exceed 30 h
        self.assertFalse(self.calls)
        for value in ("-1", "nan", "inf"):
            with self.assertRaises(SystemExit) as error:
                self.run_plan("--budget-hours", value)
            self.assertEqual(error.exception.code, 2)

    def test_identical_request_reuses_token_changed_source_does_not(self):
        # Spec points 2/6: fleet's durable request token is content-derived, independent of output path.
        code, text = self.run_plan("--budget-hours", "50", "--submit")
        self.assertEqual(code, 0, text)
        first = self.document()["set"]
        self.assertEqual(self.run_plan("--budget-hours", "50", "--submit")[0], 0)
        self.assertEqual(self.document()["set"], first)
        self.assertEqual([c[0] for c in self.calls], ["check", "estimate", "submit"] * 2)
        self.assertIn("request", first)
        self.assertEqual(self.run_plan("--nrese", "b" * 40, "--budget-hours", "50")[0], 0)
        tokens = {tomllib.loads(p.read_text())["set"]["request"] for p in self.out.glob("*.set.toml")}
        self.assertEqual(len(tokens), 2)

    def test_job_declares_resources_identity_answers_and_private_landing(self):
        # Spec points 2/3/7: ordinary unpinned timed jobs carry declared steps and durable outputs.
        code, text = self.run_plan("--budget-hours", "50", "--plan")
        self.assertEqual(code, 0, text)
        job = self.document()["job"][0]
        self.assertEqual(job["kind"], "owlrs.timed")
        self.assertEqual(job["class"], "exclusive")
        self.assertEqual(job["priority"], 600)
        self.assertFalse(job["requires"].get("hosts"))
        self.assertEqual(job["inputs"][0]["source"]["rev"], A)
        self.assertEqual(len(job["step_needs"]), len(job["steps"]))
        for need in job["step_needs"]:
            for field in ("cores", "ram_gb", "disk_gb"):
                self.assertGreater(need[field], 0)
        self.assertGreater(job["step_deadline_min"], 0)
        self.assertIn("private", job["results_at"])
        self.assertIn("unmeasured", text)
        self.assertNotIn("submit", [c[0] for c in self.calls])
        payload = json.loads(job["env"]["SUITE_CELL"])
        self.assertEqual(payload["nrese"], A)
        self.assertEqual(payload["harness"], A)
        self.assertEqual(payload["group"]["references"], ["jena", "oxigraph"])

    def test_check_refusal_cannot_submit(self):
        # Spec point 3: fleet's refusal is final; --submit must not bypass it.
        with patch.object(abba.rg, "fleet_call", return_value=(1, "refused")) as call:
            code, text = self.run_plan("--budget-hours", "50", "--submit")
        self.assertEqual(code, 1, text)
        self.assertEqual([c.args[0] for c in call.call_args_list], ["check"])

    def test_member_boundary_also_refuses_restricted_outputs(self):
        # Spec point 7: direct fleet-step calls cannot create restricted export bundles.
        with patch.dict(os.environ, {"SUITE_CELL": json.dumps({"group": {"systems": ["graphdb"], "references": []}})}):
            output = io.StringIO()
            with contextlib.redirect_stderr(output):
                code = suite.main(["suite.py", "fleet", "--_member"])
        self.assertEqual(code, 2, output.getvalue())
        self.assertIn("restricted", output.getvalue())


if __name__ == "__main__":
    unittest.main()
