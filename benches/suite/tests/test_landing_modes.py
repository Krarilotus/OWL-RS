"""Landing timing plans and second rounds with fixture passes."""
import contextlib
import io
import json
import math
import os
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


def command(*args):
    out, err = io.StringIO(), io.StringIO()
    with patch.object(sys, "argv", ["release_gate.py", *map(str, args)]), \
            contextlib.redirect_stdout(out), contextlib.redirect_stderr(err), patch.object(out, "reconfigure", create=True):
        try:
            code = gate_module.main()
        except SystemExit as stop:
            code = stop.code
    return code, out.getvalue(), err.getvalue()


def timed_steps(job, action="block"):
    return [s for s in job["steps"] if s[2:3] == [action]]


def fixture_measure(rows):
    return {case: {"route": route, "paths": [f"crates/{path}"], "seconds": seconds}
            for case, route, path, seconds in rows}


class PlanningFixtures(unittest.TestCase):
    def setUp(self):
        for owner, member, value in [(gate_module, "data_job", ({"roles": ["measure"], "cores": 16, "ram_gb": 24}, {}, {})),
                                     (gate_module, "side_inputs", []), (gate_module, "bench_inputs", []),
                                     (gate_module, "checkout_rev", HARNESS),
                                     (gate_module.hosts_registry, "overlay_env", {})]:
            self.enterContext(patch.object(owner, member, return_value=value))

    def plan(self, measure, mode=None, changed=(), **options):
        catalog = [{"name": name, "cap_gb": 4} for name in measure or ["a"]]
        with patch.object(gate_module, "git_line", side_effect=lambda _, action, *args:
                          "\0".join(changed) if action == "diff" else "b" * 40):
            try:
                jobs = gate_module.jobs(REVS["cand"], HARNESS, "repo", "test", options.pop("blocks", 2),
                                        options.pop("cases", None), REVS["base"], REVS["base"],
                                        checks=False, big_gb=None, measure=measure, catalog=catalog,
                                        **({"mode": mode} if mode else {}), **options)
            except (SystemExit, TypeError) as error:
                return None, str(error)
        return jobs["test-timed"], gate_module.set_plan(jobs, "test", REVS["cand"], HARNESS,
                                                         REVS["base"], REVS["base"], measure)


class LandingPlans(PlanningFixtures):
    # Spec: design §4 C and slice 2 §2 say --mode C -> all 15 landing cases, each in eight blocks.
    def test_mode_c_plans_the_landing_set(self):
        # Use the working calibration to exercise the same fixture before and after commit.
        gate = gate_module
        measure = gate.read_measure((gate.HERE / "measure.toml").read_text(encoding="utf-8"))
        with tempfile.TemporaryDirectory() as tmp, \
                patch.object(gate, "read_measure", return_value=measure), \
                patch.object(gate.hosts_registry, "write_job") as write:
            code, plan, error = command("jobs", "HEAD", "--base", "HEAD^", "--mode", "C", "--no-checks",
                                        "--out", tmp)
        members = list(write.call_args.args[3]) if write.called else []
        observed = [(set(j["env"].get("GATE_CASES", "").split(",")), len(timed_steps(j))) for j in members]
        self.assertEqual(observed, [(LANDING, 8)], error)
        self.assertEqual(code, 0, error)
        self.assertIn("mode C", plan)

    # Spec: design §4 B says a touched case runs first and the longest-waiting cases fill at most
    # five cases and 32 seconds, skipping a case that would exceed the remaining time.
    def test_b_uses_mandatory_then_oldest_cases_within_budget(self):
        measure = fixture_measure([(name, "query", name, seconds) for name, seconds in
                                   [("new", 5), ("heavy", 15), ("old", 7), ("next", 6), ("small", 1),
                                    ("mandatory", 18), ("unused", 15)]])
        with tempfile.TemporaryDirectory() as tmp:
            old, new = Path(tmp) / "old.json", Path(tmp) / "new.json"
            for path, cases in [(old, ["old", "next", "small", "heavy"]), (new, ["new", "mandatory"]),
                                (Path(tmp) / "previous.json", ["unused"])]:
                path.write_text(json.dumps({"planned": cases, "word": "cleared", "tree": "a" * 40}), encoding="utf-8")
            job, plan = self.plan(measure, "B", ["crates/mandatory/src/lib.rs"],
                                  history=[Path(tmp) / "previous.json", old, new])
        self.assertEqual(job and job["env"]["GATE_CASES"], "mandatory,old,next,small", plan)
        self.assertEqual(len(timed_steps(job)), 8)
        self.assertIn("32", plan)

    # Spec: design §4 B says unmapped, query/write overlap and more than five mandatory each
    # run the full C set, with the triggering cause in the plan.
    def test_b_expands_to_c_for_each_required_cause(self):
        for changes, rows, why in [(["src/lib.rs"], [("a", "query", "a", 1)], "unmapped"),
                                   (["crates/shared/lib.rs"], [("a", "query", "shared", 1),
                                                               ("b", "write", "shared", 1)], "query and write"),
                                   (["crates/shared/lib.rs"], [(str(i), "load", "shared", 1) for i in range(6)],
                                    "6 mandatory")]:
            with self.subTest(cause=why):
                measure = fixture_measure([*rows, ("rotating", "load", "other", 1)])
                job, plan = self.plan(measure, "B", changes)
                self.assertEqual(job and set(job["env"]["GATE_CASES"].split(",")), set(measure), plan)
                self.assertEqual(len(timed_steps(job)), 8)
                self.assertIn("mode C", plan)
                self.assertIn(why, plan)

    # Spec: slice 2 §3 exempts docs paths -> no mandatory cases, only the ordinary rotation.
    def test_docs_only_uses_the_rotation_alone(self):
        measure = fixture_measure([(str(i), "query", str(i), 1) for i in range(6)])
        job, plan = self.plan(measure, "B", ["docs/explainer.md"])
        self.assertEqual(job and job["env"]["GATE_CASES"], "0,1,2,3,4", plan)
        self.assertIn("mandatory: none", plan)

    # Spec: design §4 B says mandatory cases run past the budget and their length is visible.
    def test_mandatory_cases_are_kept_over_budget(self):
        measure = fixture_measure([("a", "load", "shared", 23), ("b", "load", "shared", 19),
                                   ("c", "query", "other", 1)])
        job, plan = self.plan(measure, "B", ["crates/shared/lib.rs"])
        self.assertEqual(job and job["env"]["GATE_CASES"], "a,b", plan)
        self.assertIn("42 s", plan)

    # Spec: design §3 D5 and slice 2 §5 say a three-layout job needs whole layout cycles ->
    # two blocks are refused, while six visit layouts 1, 2 and 3 twice.
    def test_blocks_must_cover_every_layout_evenly(self):
        measure = fixture_measure([("a", "load", "a", 1)])
        job, reason = self.plan(measure, blocks=2, layouts=3)
        self.assertIsNone(job, reason)
        self.assertIn("multiple", reason)
        job, plan = self.plan(measure, blocks=6, layouts=3)
        self.assertEqual(job and [s[5] for s in timed_steps(job)], ["1", "2", "3", "1", "2", "3"], plan)
        self.assertEqual(job["env"]["GATE_LAYOUTS"], "3")

    # Spec: slice 2 §1 preserves today's no-mode job. The legacy two-side, uncalibrated
    # selection builds, times two plain blocks, reports and cleans up in that order.
    def test_no_mode_preserves_the_legacy_plan(self):
        job, plan = self.plan({}, cases="a")
        steps = [step[2] if step[:2] == ["bash", "scripts/release-gate.sh"] else step[1] for step in job["steps"]]
        self.assertEqual(steps, ["-c", "block", "block", "report", "clean"], plan)
        self.assertEqual([s[3:7] for s in timed_steps(job)], [["1", HARNESS, "1", "-"], ["2", HARNESS, "1", "-"]])
        self.assertEqual(job["env"]["GATE_CASES"], "a")


class LandingRoundJob(PlanningFixtures):
    def write_passes(self, directory, contrasts, round_number=0, other=False):
        def records_of(side, n):
            contrast = contrasts[(n - 1) // 4] if side == "cand" else 0
            reading = record(samples={"q06": [10.123 * math.exp(contrast)]})
            return [reading, record(case=OTHER, samples={"_": [10.123]})] if other else [reading]

        directory.mkdir(parents=True, exist_ok=True)
        for p in passes(records_of, blocks=len(contrasts), sides=SIDES):
            data = p["report"]
            for row in [data, *data["records"]]:
                for key in ("started_ns", "finished_ns"):
                    row[key] += round_number * 1_000_000
            data["timing_inputs"] = {"overlay": None, "floor": None}
            data["machine"].pop("fleet_host", None)
            data["machine"]["overlay"] = None
            (directory / p["path"]).write_text(json.dumps(data), encoding="utf-8")

    def first_report(self, root, contrasts, other=False):
        self.write_passes(root / "passes", contrasts, other=other)
        plan = root / "plan.json"
        family = [*FAMILY, f"base|{OTHER}|time/_"] if other else FAMILY
        plan.write_text(json.dumps({"run": [CASE, OTHER] if other else [CASE], "family": family}), encoding="utf-8")
        gate_module.attempt_record("timed", root)
        _, _, error = command("report", "--passes", root / "passes", "--candidate", "cand",
                              "--side", f"cand={REVS['cand']}", "--side", f"base={REVS['base']}",
                              "--side", f"main={REVS['base']}", "--require", "base,main", "--min-blocks", 8,
                              "--harness-revision", HARNESS, "--family", plan, "--out", root / "release-gate.md",
                              "--json", root / "release-gate.json")
        self.assertFalse(error)
        return json.loads((root / "release-gate.json").read_bytes())

    # Spec: design §4 report says each case skipped by B has its last word and tree if supplied;
    # a case absent from history still appears in the report.
    def test_reports_show_the_unrun_landing_cases(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            prior = root / "older.json"
            prior.write_text(json.dumps({"tree": "f" * 40, "word": "cleared", "planned": [OTHER]}), encoding="utf-8")
            rows = [(CASE, "query", "a", 32), (OTHER, "query", "b", 3), ("unknown", "query", "c", 2)]
            job, _ = self.plan(fixture_measure(rows), "B", ["crates/a/lib.rs"], history=[prior])
            with patch.dict(os.environ, job["env"] if job else {}):
                report = self.first_report(root, CALM)
            md = (root / "release-gate.md").read_text(encoding="utf-8")
        self.assertEqual(report.get("landing", {}).get("not_run"),
                         {OTHER: {"word": "cleared", "tree": "f" * 40}, "unknown": {"word": None, "tree": None}})
        self.assertIn(f"{OTHER}: cleared on tree {'f' * 40}", md)
        self.assertIn("unknown: no previous reading", md)

    # Spec: design §3 D3 says the same job's last step leaves a cleared round alone, or times
    # only screened cases for eight blocks on the same binaries and writes the paired word.
    def test_last_step_with_cleared_and_screened_reports(self):
        rows = [(CASE, "query", "a", 3), (OTHER, "query", "b", 3)]
        for contrasts, word in [(CALM, "cleared"), (LOSS, "confirmed loss")]:
            with self.subTest(word=word), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                job, plan = self.plan(fixture_measure(rows), "C")
                step = job["steps"][-1][2:] if job else ["clean"]
                env = {**(job["env"] if job else {}), "FLEET_OUT": str(root)}
                with patch.dict(os.environ, env):
                    first = self.first_report(root, contrasts, other=True)
                    bash_steps = []
                    real_run = gate_module.subprocess.run

                    def runner(argv, **kwargs):
                        if argv[:2] != ["bash", "scripts/release-gate.sh"]:
                            return real_run(argv, **kwargs)
                        bash_steps.append(argv)
                        if argv[2:4] == ["block", "8"]:
                            self.write_passes(Path(kwargs["env"]["FLEET_OUT"]) / "passes", contrasts, 1)

                    with patch.object(gate_module.subprocess, "run", side_effect=runner):
                        code, _, error = command(*step)
                final = json.loads((root / "release-gate.json").read_bytes())
                self.assertEqual((code, final["word"]), (0, word), error + plan)
                if word == "cleared":
                    self.assertEqual((bash_steps, final), ([], first))
                else:
                    self.assertEqual(first["word"], "needs a second round")
                    self.assertEqual([s[3] for s in bash_steps if s[2] == "plan-cases"], [CASE])
                    round_two = [s for s in bash_steps if s[2] == "block"]
                    self.assertEqual([s[3] for s in round_two], list(map(str, range(1, 9))))
                    self.assertEqual([s[4:6] + s[7:] for s in round_two],
                                     [s[4:6] + s[7:] for s in timed_steps(job)])
                    self.assertEqual(final["verified_binary_hashes"], first["verified_binary_hashes"])
                    self.assertEqual(final["confirmed"], [MEASURED])


if __name__ == "__main__":
    unittest.main()
