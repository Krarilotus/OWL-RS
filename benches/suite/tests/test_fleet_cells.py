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
        self.sets = {}
        self.addCleanup(patch.stopall)
        patch.object(abba, "git", side_effect=lambda root, *args: "" if args[0] == "status"
                     else "b" * 40 if args[-1].startswith("b" * 40) else A).start()
        patch.object(abba.rg, "side_inputs", return_value=[]).start()
        patch.object(abba.rg, "fleet_call", side_effect=self.fleet).start()

    def fleet(self, *args):
        self.calls.append(args)
        if args[0] == "status":
            return (0, json.dumps(self.sets[args[1]])) if args[1] in self.sets else (1, json.dumps({"error": f"no job {args[1]}"}))
        if args[0] == "submit":
            doc = tomllib.loads(Path(args[1]).read_text())
            key = "set:" + doc["set"]["id"]
            self.sets[key] = {"id": key, "members": [{"id": j["id"]} for j in doc["job"]]}
        if "--json" in args:
            return 0, json.dumps({"jobs": [{"fleet": {"verdict": "now"}, "hosts": []}]})
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
        self.assertEqual(sum(c[0] == "submit" for c in self.calls), 1)
        self.assertIn("request", first)
        self.assertEqual(self.run_plan("--references", "oxigraph,jena", "--budget-hours", "50", "--submit")[0], 0)
        self.assertEqual(len(list(self.out.glob("*.set.toml"))), 1)  # S1: equivalent selection, one request
        self.assertEqual(self.run_plan("--nrese", "b" * 40, "--budget-hours", "50")[0], 0)
        tokens = {tomllib.loads(p.read_text())["set"]["request"] for p in self.out.glob("*.set.toml")}
        self.assertEqual(len(tokens), 2)

    def test_job_declares_resources_identity_answers_and_private_landing(self):
        # Fleet owner 8 Nov: spec 9, exclusive host, memory-only containers covered by step RAM.
        code, text = self.run_plan("--budget-hours", "50", "--plan")
        self.assertEqual(code, 0, text)
        job = self.document()["job"][0]
        self.assertEqual(job["kind"], "owlrs.timed")
        self.assertEqual((job["class"], job.get("untimed_steps", [])), ("exclusive", []))
        self.assertEqual(job["spec"], 9)
        self.assertEqual(job["containers"], [{"name": "suite", "memory_gb": 28}])
        self.assertEqual(job["priority"], 600)
        self.assertFalse(job["requires"].get("hosts"))
        self.assertEqual(job["inputs"][0]["source"]["rev"], A)
        self.assertEqual([n["ram_gb"] for n in job["step_needs"]], [28] * len(job["steps"]))
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
        with patch.object(abba.rg, "fleet_call", side_effect=lambda *a: (1, "refused") if a[0] == "check" else self.fleet(*a)) as call:
            code, text = self.run_plan("--budget-hours", "50", "--submit")
        self.assertEqual(code, 1, text)
        self.assertNotIn("submit", [c.args[0] for c in call.call_args_list])

    def test_member_boundary_also_refuses_restricted_outputs(self):
        # Spec point 7: direct fleet-step calls cannot create restricted export bundles.
        patch("suitekit.fleet_cells.subprocess.run", side_effect=OSError("no external work in boundary fixture")).start()
        patch("suitekit.fleet_cells.manifest.git", return_value={}).start()
        patch("suitekit.fleet_cells.manifest.machine", return_value={}).start()
        for chosen, reason in ((["graphdb"], "restricted"), ([], "exactly one"), (["qlever", "oxigraph"], "exactly one")):
            with self.subTest(systems=chosen):
                payload = {"group": {"systems": chosen, "references": []}, "result_root": str(self.out), "member": "bad", "nrese": A, "harness": A, "deadline_min": 1}
                with patch.dict(os.environ, {"SUITE_CELL": json.dumps(payload), "FLEET_OUT": str(self.out / "export"), "NRESE_GATE_DIR": str(self.out / "gate")}):
                    output = io.StringIO()
                    with contextlib.redirect_stderr(output):
                        code = suite.main(["suite.py", "fleet", "--_member"])
                self.assertEqual(code, 2, output.getvalue())
                self.assertIn(reason, output.getvalue())

    def test_cells_have_separate_jobs_and_adapter_capabilities(self):
        # Master 21:24: one job per cell; resource basis and workload needs remain explicit.
        code, text = self.run_plan("--systems", "qlever,jena", "--budget-hours", "80")
        self.assertEqual(code, 0, text)
        jobs = self.document()["job"]
        self.assertEqual(len(jobs), 2)
        for job in jobs:
            payload = json.loads(job["env"]["SUITE_CELL"])
            self.assertEqual(len(payload["group"]["systems"]), 1)
            system = payload["group"]["systems"][0]
            self.assertEqual("java" in job["requires"]["has"], system == "jena")
            self.assertEqual(job["requires"]["ram_gb"], 28)  # sparql.toml's declared cap, no flat overhead
            self.assertEqual(job["requires"]["cores"], 2)  # cargo-guarded.sh's documented floor
        self.assertIn("resource basis", text)

    def test_permanently_unplaceable_cell_is_omitted_from_the_set(self):
        # Master 21:24: one impossible member cannot sink the placeable cells' set.
        def checked(*args):
            if "--json" in args and "qlever" in args[1]:
                return 1, json.dumps({"jobs": [{"fleet": {"verdict": "never"},
                                              "hosts": [{"host": "h", "reason": "needs more RAM"}]}]})
            return self.fleet(*args)
        with patch.object(abba.rg, "fleet_call", side_effect=checked):
            code, text = self.run_plan("--systems", "qlever,jena", "--budget-hours", "80")
        self.assertEqual(code, 0, text)
        self.assertEqual(len(self.document()["job"]), 1)
        self.assertIn("omit qlever", text)
        self.assertIn("needs more RAM", text)

    def test_bsbm_java_is_a_workload_dependency_for_qlever_too(self):
        # Master's clarification: keep BIBM's actual Java need, separately from adapter needs.
        code, text = self.run_plan("--workloads", "bsbm", "--tiers", "bsbm=bsbm-bi-100m", "--budget-hours", "20")
        self.assertEqual(code, 0, text)
        self.assertIn("java", self.document()["job"][0]["requires"]["has"])
        self.assertIn("adapter needs ['docker']", text)
        self.assertIn("workload/driver needs ['java']", text)

    def test_fleet_asks_do_not_cap_native_measurement_cpus(self):
        # Opus B1: native suite.py run has no cap, even with a per-system historical core ask.
        from suitekit import fleet_cells
        make = fleet_cells.jobs
        def measured(groups, args):
            groups[0]["measured"] = {"cores": 7}
            return make(groups, args)
        with patch.object(fleet_cells, "jobs", side_effect=measured):
            code, text = self.run_plan("--systems", "jena,qlever", "--budget-hours", "80")
        self.assertEqual(code, 0, text)
        for job in self.document()["job"]:
            self.assertNotIn("DOCKER_CPUS", job["env"])
            self.assertEqual(job["requires"]["cores"], 2)

    def test_identity_excludes_budget_owner_and_checkout(self):
        # Opus N1: same measured cells/trees/inputs retain identity across administration changes.
        from suitekit import fleet_cells
        self.assertEqual(self.run_plan("--budget-hours", "50", "--submit")[0], 0)
        first, make = self.document()["set"], fleet_cells.jobs
        def elsewhere(groups, args):
            args.checkout = self.out / "another-worktree"
            return make(groups, args)
        with patch.object(fleet_cells, "jobs", side_effect=elsewhere):
            code, text = self.run_plan("--budget-hours", "49", "--owner", "another-owner", "--submit")
        self.assertEqual(code, 0, text)
        self.assertIn("reuse set:" + first["id"], text)
        self.assertEqual(sum(c[0] == "submit" for c in self.calls), 1)

    def test_existing_set_is_reused_before_check_or_submit(self):
        # Opus N1: a repeat returns the existing set/member ids, including after capacity closes.
        self.assertEqual(self.run_plan("--budget-hours", "50", "--submit")[0], 0)
        existing = self.sets["set:" + self.document()["set"]["id"]]
        with patch.object(abba.rg, "fleet_call", side_effect=lambda *a: (0, json.dumps(existing)) if a[0] == "status" else (1, "must not recheck/submit")) as call:
            code, text = self.run_plan("--budget-hours", "50", "--submit")
        self.assertEqual(code, 0, text)
        self.assertEqual([c.args[0] for c in call.call_args_list], ["status"])
        self.assertTrue(all(j["id"] in text for j in existing["members"]))


if __name__ == "__main__":
    unittest.main()
