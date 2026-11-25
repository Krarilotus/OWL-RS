"""Landing timing plans and second rounds with fixture passes."""
import contextlib
import io
import json
import math
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_landing_timing import CASE, FAMILY, MEASURED, SIDES  # noqa: E402
from test_release_gate import HARNESS, REVS, gate_module, passes, record  # noqa: E402

LANDING = {"load-entities", "load-threads", "q-snowflake", "q-star", "op-group", "commits-plain",
           "commits-rdfs", "custom-tc", "rl-single-thread", "rl-clique", "ql-lubm10", "dl-pipeline",
           "shacl-lubm", "deletes-tail", "writes-under-readers"}
OTHER = "lab-yago-tiny"
LOSS = [0.05 + i * 0.001 for i in range(8)]
CALM = [(-1) ** i * (i + 1) / 1000 for i in range(8)]
GATE = ["bash", "scripts/release-gate.sh"]


def command(*args):
    out, err = io.StringIO(), io.StringIO()
    with patch.object(sys, "argv", ["release_gate.py", *map(str, args)]), \
            contextlib.redirect_stdout(out), contextlib.redirect_stderr(err), patch.object(out, "reconfigure", create=True):
        try:
            code = gate_module.main()
        except SystemExit as stop:
            code = stop.code
    return code, out.getvalue(), err.getvalue()


def timed_steps(job):
    return [s for s in job["steps"] if s[:3] == [*GATE, "block"]]


class PlanningFixtures(unittest.TestCase):
    def setUp(self):
        for owner, member, value in [(gate_module, "data_job", ({"roles": ["measure"], "cores": 16, "ram_gb": 24}, {}, {})),
                                     (gate_module, "side_inputs", []), (gate_module, "bench_inputs", []),
                                     (gate_module, "checkout_rev", HARNESS),
                                     (gate_module.hosts_registry, "overlay_env", {})]:
            self.enterContext(patch.object(owner, member, return_value=value))

    def plan(self, blocks=2, layouts=1, mode=None):
        measure = gate_module.read_measure((gate_module.HERE / "measure.toml").read_text(encoding="utf-8")) if mode else {}
        catalog = gate_module.cases_registry.load() if mode else [{"name": "a", "cap_gb": 4}]
        with patch.object(gate_module, "git_line", return_value="crates/nrese-store/src/lib.rs\0"):
            try:
                jobs = gate_module.jobs(REVS["cand"], HARNESS, "repo", "test", blocks, None if mode else "a",
                                        REVS["base"], REVS["base"], checks=False, big_gb=None, layouts=layouts,
                                        measure=measure, catalog=catalog, mode=mode)
            except SystemExit as error:
                return None, str(error)
        return jobs["test-timed"], gate_module.set_plan(jobs, "test", REVS["cand"], HARNESS,
                                                         REVS["base"], REVS["base"], measure)


class LandingPlans(PlanningFixtures):
    def cli_plan(self, changed):
        gate = gate_module
        measure = gate.read_measure((gate.HERE / "measure.toml").read_text(encoding="utf-8"))
        actual_git = gate.git_line
        with tempfile.TemporaryDirectory() as tmp, \
                patch.object(gate, "read_measure", return_value=measure), \
                patch.object(gate, "git_line", side_effect=lambda checkout, *args:
                             changed + "\0" if args[0] == "diff" else actual_git(checkout, *args)), \
                patch.object(gate.hosts_registry, "write_job") as write:
            code, plan, error = command("jobs", "HEAD", "--base", "HEAD^", "--mode", "C", "--no-checks",
                                        "--out", tmp)
        return (list(write.call_args.args[3]) if write.called else []), code, plan, error

    # Spec: fix pass 1 ruling. Docs-only and tests-only changes write no job and exit 0 with the reason.
    def test_non_product_changes_plan_nothing(self):
        for changed in ["docs/explainer.md", "crates/nrese-store/tests/queries.rs"]:
            with self.subTest(changed=changed):
                members, code, plan, error = self.cli_plan(changed)
                self.assertEqual(members, [], error or plan)
                self.assertEqual(code, 0, error)
                self.assertIn("no product change: nothing to time", plan.splitlines())

    # Spec: fix pass 1 ruling. Nested crate src and Cargo.lock changes run all 15 cases in eight blocks.
    def test_product_changes_plan_every_landing_case(self):
        for changed in ["crates/rdf/nrese-rdf-io/src/lib.rs", "Cargo.lock"]:
            with self.subTest(changed=changed):
                members, code, plan, error = self.cli_plan(changed)
                observed = [(set(j["env"].get("GATE_CASES", "").split(",")), len(timed_steps(j))) for j in members]
                self.assertEqual(observed, [(LANDING, 8)], error)
                self.assertEqual(code, 0, error)
                self.assertIn("mode C", plan)

    # Spec: design §3 D5. Two blocks are refused for three layouts. Six cover each layout twice.
    def test_blocks_must_cover_every_layout_evenly(self):
        job, reason = self.plan(blocks=2, layouts=3)
        self.assertIsNone(job, reason)
        self.assertIn("multiple", reason)
        job, plan = self.plan(blocks=6, layouts=3)
        self.assertEqual(job and [s[5] for s in timed_steps(job)], ["1", "2", "3", "1", "2", "3"], plan)

    # Spec: fix pass 1 R2. A request for zero blocks is refused before it produces a build plan.
    def test_zero_blocks_are_refused(self):
        job, reason = self.plan(blocks=0)
        self.assertIsNone(job, reason)
        self.assertIn("positive", reason)

    # Spec: fix pass 1 R3. The preparation-only revision builds once even when timed sides use three layouts.
    def test_only_timed_revisions_build_extra_layouts(self):
        job, plan = self.plan(blocks=6, layouts=3)
        self.assertIsNotNone(job, plan)
        if os.name == "nt":
            git_core = Path(subprocess.check_output(["git", "--exec-path"], text=True).strip())
            bash = str(git_core.parents[2] / "bin" / "bash.exe")
        else:
            bash = shutil.which("bash")
        self.assertTrue(bash, "the generated build step requires bash")
        with tempfile.TemporaryDirectory() as tmp:
            scripts = Path(tmp) / "scripts"
            scripts.mkdir()
            (scripts / "release-gate.sh").write_bytes(
                b'if [ "$1" = build ]; then printf "%s %s\n" "$2" "${GATE_LAYOUTS:-1}"; fi\n')
            step = job["steps"][0]
            path = str(Path(bash).parent) + os.pathsep + os.environ["PATH"]
            result = subprocess.run([bash, *step[1:]], cwd=tmp, text=True, capture_output=True, check=True,
                                    env={**os.environ, **job["env"], "PATH": path})
        self.assertCountEqual(result.stdout.splitlines(),
                              [f"{REVS['base']} 3", f"{REVS['cand']} 3", f"{HARNESS} 1"])

    # Spec: slice 2 §1 preserves the legacy two-side uncalibrated plan and its step order.
    def test_no_mode_preserves_the_legacy_plan(self):
        job, plan = self.plan()
        steps = [step[2] if step[:2] == GATE else step[1] for step in job["steps"]]
        self.assertEqual(steps, ["-c", "block", "block", "report", "clean"], plan)
        self.assertEqual([s[3:7] for s in timed_steps(job)], [["1", HARNESS, "1", "-"], ["2", HARNESS, "1", "-"]])
        self.assertEqual(job["env"]["GATE_CASES"], "a")


class LandingRoundJob(PlanningFixtures):
    def write_passes(self, directory, contrasts, round_number=0, other=False, failed_other=False, fault=None):
        def records_of(side, n):
            contrast = contrasts[(n - 1) // 4] if side == "cand" else 0
            reading = record(samples={"q06": [10.123 * math.exp(contrast)]})
            status = "failed" if failed_other and side == "cand" else "ok"
            return [reading, record(case=OTHER, status=status, samples={"_": [10.123]})] if other else [reading]

        directory.mkdir(parents=True, exist_ok=True)
        for p in passes(records_of, blocks=len(contrasts), sides=SIDES):
            data = p["report"]
            for row in [data, *data["records"]]:
                for key in ("started_ns", "finished_ns"):
                    row[key] += round_number * 1_000_000
            data["timing_inputs"] = {"overlay": None, "floor": None}
            data["machine"].pop("fleet_host", None)
            data["machine"]["overlay"] = None
            if fault == "schedule" and data["block"] == 8:
                data["records"] = []
            if fault == "provenance" and p["n"] == 1:
                data["git_finished"]["dirty"] = 1
            (directory / p["path"]).write_text(json.dumps(data), encoding="utf-8")

    def report(self, root, harness, sides, first=None):
        plan = (first.parent if first else root) / "plan.json"
        args = ["report", "--passes", root / "passes", "--candidate", "cand", "--require", "base,main",
                "--min-blocks", 8, "--harness-revision", harness, "--family", plan,
                "--out", root / "release-gate.md", "--json", root / "release-gate.json"]
        for side in sides:
            args.extend(("--side", side))
        if first:
            args.extend(("--round-one", first))
        return command(*args)

    def continue_fixture(self, root, first_contrasts=LOSS, second_contrasts=LOSS, fault=None, failed_other=False):
        job, plan = self.plan(mode="C")
        self.assertIsNotNone(job, plan)
        self.write_passes(root / "passes", first_contrasts, other=True, failed_other=failed_other)
        first_path = root / "release-gate.json"
        (root / "plan.json").write_text(json.dumps({"run": [CASE, OTHER],
                                                   "family": [*FAMILY, f"base|{OTHER}|time/_"]}), encoding="utf-8")
        gate_module.attempt_record("timed", root)
        seen = []
        real_run = gate_module.subprocess.run

        def runner(argv, **kwargs):
            if argv[:2] != GATE:
                return real_run(argv, **kwargs)
            action = argv[2]
            seen.append(argv)
            env = kwargs.get("env", os.environ)
            out = Path(env["FLEET_OUT"])
            if argv[2:4] == ["block", "8"]:
                self.write_passes(out / "passes", second_contrasts, round_number=1, fault=fault)
            if action == "report":
                with patch.dict(os.environ, env):
                    _, _, error = self.report(out, argv[3], argv[6:], Path(argv[5]))
                if error:
                    raise subprocess.CalledProcessError(1, argv, stderr=error)
            return subprocess.CompletedProcess(argv, 0)

        with patch.dict(os.environ, {**job["env"], "FLEET_OUT": str(root)}):
            _, _, error = self.report(root, HARNESS, [f"cand={REVS['cand']}", f"base={REVS['base']}", f"main={REVS['base']}"])
            self.assertFalse(error)
            first = json.loads(first_path.read_bytes())
            report_index = job["steps"].index(timed_step := next(s for s in job["steps"] if s[:3] == [*GATE, "report"]))
            with patch.object(gate_module.subprocess, "run", side_effect=runner):
                for step in job["steps"][report_index + 1:]:
                    if step[:2] == GATE:
                        runner(step)
                    else:
                        code, _, _ = command(*step[2:])
        return code, first, json.loads(first_path.read_bytes()), seen, timed_steps(job), timed_step

    # Spec: design §3 D3 and fix pass 1 R1/R4/R5. A screen runs exactly the named cases and uses the
    # first report's harness. A bad second round leaves the first word standing, and cleanup comes last.
    def test_last_step_with_cleared_and_screened_reports(self):
        runs = [(CALM, None, "cleared"), (LOSS, None, "confirmed loss"),
                (LOSS, "schedule", "needs a second round"), (LOSS, "provenance", "needs a second round")]
        for contrasts, fault, word in runs:
            with tempfile.TemporaryDirectory() as tmp:
                code, first, final, seen, blocks, report = self.continue_fixture(
                    Path(tmp), first_contrasts=contrasts, fault=fault)
            with self.subTest(check="word", word=word, fault=fault):
                self.assertEqual((code == 0, final["word"]), (fault is None, word))
                if contrasts == CALM or fault:
                    self.assertEqual(final, first)
            with self.subTest(check="cleanup", word=word, fault=fault):
                self.assertEqual([s[2] for s in seen].count("clean"), 1)
                self.assertEqual(seen[-1][2], "clean")
            if contrasts == CALM:
                self.assertEqual([s[2] for s in seen], ["clean"])
            else:
                with self.subTest(check="harness", fault=fault):
                    self.assertEqual([s[3] for s in seen if s[2] == "report"], [report[3]])
                with self.subTest(check="blocks", fault=fault):
                    self.assertEqual([s[3] for s in seen if s[2] == "plan-cases"], [CASE])
                    round_two = [s for s in seen if s[2] == "block"]
                    self.assertEqual([s[3] for s in round_two], list(map(str, range(1, 9))))
                    self.assertEqual([s[4:6] + s[7:] for s in round_two], [s[4:6] + s[7:] for s in blocks])
                    self.assertEqual(final["verified_binary_hashes"], first["verified_binary_hashes"])
                    if not fault:
                        self.assertEqual(final["confirmed"], [MEASURED])

    # Spec: fix pass 1 R6. A failed case omitted from round 2 remains a correctness failure and blocker.
    def test_pair_preserves_failures_for_cases_round_two_did_not_time(self):
        with tempfile.TemporaryDirectory() as tmp:
            code, first, final, _, _, _ = self.continue_fixture(Path(tmp), second_contrasts=CALM, failed_other=True)
        self.assertEqual(final["candidate_failures"].get(OTHER), first["candidate_failures"][OTHER])
        self.assertEqual((code, final["word"], final["passed"]), (0, "cleared", False))
        self.assertTrue(any(line.startswith(f"{OTHER}:") for line in final["blocking"]))
        self.assertTrue(any(line.startswith(f"{OTHER}:") for line in final["other_blocking"]))


if __name__ == "__main__":
    unittest.main()
