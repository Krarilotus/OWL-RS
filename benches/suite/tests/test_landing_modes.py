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
from test_landing_timing import CASE, MEASURED, SIDES  # noqa: E402
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


def git_fixture(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def commit_fixture(root):
    git_fixture(root, "add", "-A")
    git_fixture(root, "-c", "user.name=t", "-c", "user.email=t@example.org", "commit", "-q", "-m", "fixture")


def fixture_bash():
    if os.name == "nt":
        git_core = Path(subprocess.check_output(["git", "--exec-path"], text=True).strip())
        return str(git_core.parents[2] / "bin" / "bash.exe")
    return shutil.which("bash")


class PlanningFixtures(unittest.TestCase):
    def setUp(self):
        for owner, member, value in [(gate_module, "data_job", ({"roles": ["measure"], "cores": 16, "ram_gb": 24}, {}, {})),
                                     (gate_module, "side_inputs", []), (gate_module, "bench_inputs", []),
                                     (gate_module, "checkout_rev", HARNESS),
                                     (gate_module.hosts_registry, "overlay_env", {})]:
            self.enterContext(patch.object(owner, member, return_value=value))

    def plan(self, blocks=2, layouts=1, mode=None, main=None):
        measure = gate_module.read_measure((gate_module.HERE / "measure.toml").read_text(encoding="utf-8")) if mode else {}
        catalog = gate_module.cases_registry.load() if mode else [{"name": "a", "cap_gb": 4}]
        main = main or REVS["base"]
        try:
            jobs = gate_module.jobs(REVS["cand"], HARNESS, "repo", "test", blocks, None if mode else "a",
                                    REVS["base"], main, checks=False, big_gb=None, layouts=layouts,
                                    measure=measure, catalog=catalog, mode=mode)
        except SystemExit as error:
            return None, str(error)
        return jobs["test-timed"], gate_module.set_plan(jobs, "test", REVS["cand"], HARNESS,
                                                         REVS["base"], main, measure)


class LandingPlans(PlanningFixtures):
    def cli_plan(self, changed=None, *, checkout=gate_module.ROOT, harness="HEAD"):
        gate = gate_module
        measure = gate.read_measure((gate.HERE / "measure.toml").read_text(encoding="utf-8"))
        real_run = subprocess.run

        def runner(argv, **kwargs):
            if changed is not None and argv[:1] == ["git"] and argv[3:4] == ["diff"]:
                stdout = changed + "\0"
                if not (kwargs.get("encoding") or kwargs.get("text")):
                    stdout = stdout.encode("utf-8")
                return subprocess.CompletedProcess(argv, 0, stdout)
            return real_run(argv, **kwargs)

        with tempfile.TemporaryDirectory() as tmp, \
                patch.object(gate, "read_measure", return_value=measure), \
                patch.object(gate.cases_registry, "load_at", return_value=gate.cases_registry.load()), \
                patch.object(subprocess, "run", side_effect=runner), \
                patch.object(gate.hosts_registry, "write_job") as write:
            code, plan, error = command("jobs", "HEAD", "--base", "HEAD^", "--harness", harness, "--mode", "C",
                                        "--checkout", checkout, "--no-checks", "--out", tmp)
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

    # Spec: fix pass 2 H2. A product change with an older harness that does not mark landing reports is refused.
    def test_mode_c_refuses_a_harness_without_the_report_marker(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            git_fixture(root, "init", "-q")
            runner = root / "benches" / "fast" / "release_gate.py"
            runner.parent.mkdir(parents=True)
            runner.write_text('summary["word"] = "cleared"\n', encoding="utf-8")
            commit_fixture(root)
            runner.write_text('summary["landing"] = True\n', encoding="utf-8")
            (root / "Cargo.lock").touch()
            commit_fixture(root)
            members, code, plan, error = self.cli_plan(checkout=root, harness="HEAD^")
        self.assertEqual(members, [], error or plan)
        self.assertNotEqual(code, 0)
        self.assertIn("harness", str(code) + error)

    # Spec: fix pass 2 H5. The leading space in a Git filename is significant, so " Cargo.lock" plans nothing.
    def test_git_filename_with_a_leading_space_is_not_the_lockfile(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            git_fixture(root, "init", "-q")
            runner = root / "benches" / "fast" / "release_gate.py"
            runner.parent.mkdir(parents=True)
            runner.write_text('summary["landing"] = True\n', encoding="utf-8")
            commit_fixture(root)
            (root / " Cargo.lock").touch()
            commit_fixture(root)
            members, code, plan, error = self.cli_plan(checkout=root)
        self.assertEqual(members, [], error or plan)
        self.assertEqual(code, 0, error)
        self.assertIn("no product change: nothing to time", plan.splitlines())

    # Spec: design §3 D5. Two blocks are refused for three layouts. Six cover each layout twice.
    def test_blocks_must_cover_every_layout_evenly(self):
        job, reason = self.plan(blocks=2, layouts=3)
        self.assertIsNone(job, reason)
        self.assertIn("multiple", reason)
        job, plan = self.plan(blocks=6, layouts=3)
        self.assertEqual(job and [s[5] for s in timed_steps(job)], ["1", "2", "3", "1", "2", "3"], plan)

    # Spec: fix pass 2 H4. With three distinct timed sides, four requested blocks round to six
    # before the planner checks that the schedule covers all three layouts.
    def test_layout_validation_uses_the_rounded_block_count(self):
        job, reason = self.plan(blocks=4, layouts=3, main=REVS["main"])
        self.assertEqual(job and [s[5] for s in timed_steps(job)], ["1", "2", "3", "1", "2", "3"], reason)

    # Spec: fix pass 1 R2. A request for zero blocks is refused before it produces a build plan.
    def test_zero_blocks_are_refused(self):
        job, reason = self.plan(blocks=0)
        self.assertIsNone(job, reason)
        self.assertIn("positive", reason)

    # Spec: fix pass 1 R3. The preparation-only revision builds once even when timed sides use three layouts.
    def test_only_timed_revisions_build_extra_layouts(self):
        job, plan = self.plan(blocks=6, layouts=3)
        self.assertIsNotNone(job, plan)
        bash = fixture_bash()
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
    def write_passes(self, directory, contrasts, round_number=0, other=False, failed_other=False, fault=None,
                     extra_query=False):
        def records_of(side, n):
            contrast = contrasts[(n - 1) // 4] if side == "cand" else 0
            samples = {"q06": [10.123 * math.exp(contrast)]}
            if extra_query:
                samples["q07"] = [10.123]
            reading = record(samples=samples)
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
        self.write_passes(root / "passes", first_contrasts, other=True, failed_other=failed_other,
                          extra_query=fault == "missing evidence")
        first_path = root / "release-gate.json"
        family = [MEASURED, f"base|{OTHER}|time/_"]
        if fault == "missing evidence":
            family.append(f"base|{CASE}|time/q07")
        (root / "plan.json").write_text(json.dumps({"run": [CASE, OTHER], "family": family}), encoding="utf-8")
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

    # Spec: design §3 D3, fix pass 1 R1/R4/R5, and fix pass 2 H1. A screen runs exactly the named cases and uses the
    # first report's harness. A bad second round leaves the first word standing, and cleanup comes last.
    # Missing declared evidence refuses publication, and a confirmed fixture has no evidence errors.
    def test_last_step_with_cleared_and_screened_reports(self):
        runs = [(CALM, None, "cleared"), (LOSS, None, "confirmed loss"),
                (LOSS, "schedule", "needs a second round"), (LOSS, "provenance", "needs a second round"),
                (LOSS, "missing evidence", "needs a second round")]
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
                        self.assertEqual(final["errors"], [])

    # Spec: fix pass 1 R6. A failed case omitted from round 2 remains a correctness failure and blocker.
    def test_pair_preserves_failures_for_cases_round_two_did_not_time(self):
        with tempfile.TemporaryDirectory() as tmp:
            code, first, final, _, _, _ = self.continue_fixture(Path(tmp), second_contrasts=CALM, failed_other=True)
        self.assertEqual(final["candidate_failures"].get(OTHER), first["candidate_failures"][OTHER])
        self.assertEqual((code, final["word"], final["passed"]), (0, "cleared", False))
        self.assertTrue(any(line.startswith(f"{OTHER}:") for line in final["blocking"]))
        self.assertTrue(any(line.startswith(f"{OTHER}:") for line in final["other_blocking"]))


class ShellReport(unittest.TestCase):
    def setUp(self):
        self.bash = fixture_bash()
        self.assertTrue(self.bash, "the report adapter requires bash")
        tmp = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.repo = tmp / "repo"
        self.repo.mkdir()
        git_fixture(self.repo, "init", "-q")
        files = {
            "scripts/release-gate.sh": (gate_module.ROOT / "scripts/release-gate.sh").read_bytes(),
            "scripts/lib/gate-publish.sh": b"",
            "scripts/lib/gate-key.sh": b"",
            "benches/suite/suitekit/checkout.py": b"",
            "benches/fast/release_gate.py": b"""
import argparse
import json
from pathlib import Path

p = argparse.ArgumentParser()
for flag in ("round-one", "family", "json", "out"):
    p.add_argument("--" + flag, type=Path, required=True)
args, _ = p.parse_known_args()
first = json.loads(args.round_one.read_bytes())
family = json.loads(args.family.read_bytes())
args.json.write_text(json.dumps({"word": first["word"], "family": family["family"]}), encoding="utf-8")
args.out.write_text(first["word"], encoding="utf-8")
raise SystemExit(first.get("exit", 0))
""",
        }
        for rel, data in files.items():
            path = self.repo / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        commit_fixture(self.repo)
        self.head = git_fixture(self.repo, "rev-parse", "HEAD")

        shim = tmp / "tools" / "python3"
        shim.parent.mkdir()
        shim.write_bytes(b'#!/usr/bin/env bash\nexec "$FIXTURE_PYTHON" "$@"\n')
        shim.chmod(0o755)
        gate, self.out = tmp / "gate", tmp / "results"
        gate.mkdir()
        self.out.mkdir()
        self.plan = json.dumps({"family": [MEASURED], "not_run": {}, "elsewhere": {}})
        (gate / "plan.json").write_text(self.plan, encoding="utf-8")
        self.json_out = self.out / "release-gate.json"
        self.md_out = self.out / "release-gate.md"
        self.env = {**os.environ, "NRESE_GATE_DIR": gate.as_posix(), "FLEET_OUT": self.out.as_posix(),
                    "GATE_BIN": (tmp / "bin").as_posix(), "FIXTURE_PYTHON": Path(sys.executable).as_posix(),
                    "PATH": os.pathsep.join([str(shim.parent), str(Path(self.bash).parent), os.environ["PATH"]])}
        for key in ("FLEET_HOST", "GATE_HOST", "NRESE_BENCH_PRIVATE", "BENCH_INPUTS_GIT", "NRESE_BENCH_HOSTS",
                    "NRESE_BENCH_FLOORS", "NRESE_BENCH_OVERLAY_REV"):
            self.env.pop(key, None)

    def run_report(self, first):
        return subprocess.run([self.bash, *GATE[1:], "report", self.head, "--round-one", str(first),
                               f"cand={self.head}", f"base={self.head}"], cwd=self.repo,
                              env=self.env, capture_output=True, text=True, timeout=60)

    # Spec: fix pass 2 H3. A relative input and its companion plan resolve in the launching checkout.
    def test_relative_round_one_path_is_anchored_to_the_launcher(self):
        first = self.repo / "first" / "release-gate.json"
        first.parent.mkdir()
        first.write_text('{"word":"needs a second round","exit":1}', encoding="utf-8")
        (first.parent / "plan.json").write_text(self.plan, encoding="utf-8")
        result = self.run_report(first.relative_to(self.repo).as_posix())
        self.assertTrue(self.json_out.is_file(), result.stdout + result.stderr)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(json.loads(self.json_out.read_bytes()), {"word": "needs a second round", "family": [MEASURED]})

    # Spec: fix pass 2 H3. A first-round path resolving to the output is refused with both files intact.
    def test_round_one_and_output_alias_preserves_the_existing_report(self):
        prior = '{"word":"needs a second round"}'
        self.json_out.write_text(prior, encoding="utf-8")
        self.md_out.write_text("prior report", encoding="utf-8")
        result = self.run_report("../results/../results/release-gate.json")
        self.assertTrue(self.json_out.is_file(), result.stdout + result.stderr)
        self.assertEqual(self.json_out.read_text(encoding="utf-8"), prior)
        self.assertEqual(self.md_out.read_text(encoding="utf-8"), "prior report")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("output", result.stdout + result.stderr)

    # Spec: fix pass 2 H1. A fatal reporter exit must not leave an accepted report, even if it wrote files first.
    def test_fatal_reporter_exit_leaves_no_paired_output(self):
        first = self.repo / "release-gate.json"
        first.write_text('{"word":"needs a second round","exit":2}', encoding="utf-8")
        (first.parent / "plan.json").write_text(self.plan, encoding="utf-8")
        result = self.run_report(first.as_posix())
        self.assertEqual((self.json_out.exists(), self.md_out.exists()), (False, False), result.stdout + result.stderr)
        self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
