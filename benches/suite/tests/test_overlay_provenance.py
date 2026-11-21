"""The public/private boundary at the benchmark tools' entry points."""
from __future__ import annotations

import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest.mock import patch

import isolation
import test_abba as abba_fixture
import test_push_gate as shell_fixture
import test_release_gate as release_fixture
import test_summary as suite_fixture

import abba
from suitekit import compare, hosts, manifest
import suite

rg = release_fixture.gate_module
REGISTRY = isolation.OVERLAY_HOSTS
CHANNELS = ("NRESE_BENCH_HOSTS", "NRESE_BENCH_FLOORS", "NRESE_BENCH_OVERLAY_REV")


def digest(hosts_text=REGISTRY, floors=""):
    return "sha256:" + hashlib.sha256(json.dumps([hosts_text, floors], ensure_ascii=False).encode("utf-8")).hexdigest()


def command(cli, argv):
    """An entry point with real arguments and a captured terminal."""
    with patch.object(sys, "argv", argv), contextlib.redirect_stdout(io.TextIOWrapper(io.BytesIO())):
        try:
            return cli()
        except SystemExit as error:
            return error.code if isinstance(error.code, int) else 1
        except ValueError:
            return 1


class OverlayProvenance(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.env = isolation.inherit("NRESE_BENCH_", "BENCH_INPUTS_", "FLEET_")
        self.enterContext(patch.dict(os.environ, self.env, clear=True))

    def test_plan_defaults_outside_the_public_tree_and_refuses_an_explicit_public_destination(self):
        # Spec: the public/private bench plan keeps overlay-bearing job specs out of the public tree.
        checkout = self.root / "public"
        revision = isolation.repo(checkout, 1)[0]
        inputs = self.root / "private"
        isolation.overlay_repo(inputs)
        os.environ["BENCH_INPUTS_GIT"] = str(inputs)
        base = ["release_gate.py", "jobs", revision, "--checkout", str(checkout), "--base", revision,
                "--main", revision, "--plan", "--id", "overlay-test"]
        # Only preparation/calibration is stubbed. Argument defaults, writer and fleet check path are real.
        job = {"id": "overlay-test-timed", "requires": {}, "inputs": [{"name": "checkout", "source": {"git": str(checkout)}}],
               "env": hosts.overlay_env(), "steps": [], "kind": "owlrs.timed"}
        checked = []
        with (contextlib.chdir(checkout), patch.object(rg.cases_registry, "load_at", return_value=[]),
              patch.object(rg, "jobs", return_value={job["id"]: job}),
              patch.object(rg, "set_plan", return_value="fixture plan"),
              patch.object(rg, "fleet_call", side_effect=lambda _, path: (checked.append(Path(path).resolve()) or 0, ""))):
            self.assertEqual(command(rg.main, base), 0)
            self.assertFalse(list(checkout.glob("*.set.toml")), "the default exposed the job's env to Git")
            self.assertEqual(checked, [inputs / "overlay/jobs/overlay-test.set.toml"])
            self.assertIn("NRESE_BENCH_HOSTS", tomllib.loads(checked[0].read_text(encoding="utf-8"))["job"][0]["env"])
            self.assertNotEqual(command(rg.main, base + ["--out", str(checkout / "plans")]), 0)
            self.assertFalse((checkout / "plans").exists())
            external = self.root / "plans"
            self.assertEqual(command(rg.main, base + ["--out", "../plans"]), 0)
            self.assertTrue((external / "overlay-test.set.toml").is_file())
            extra = self.root / "extra"
            job["inputs"].append({"name": "extra", "source": {"git": str(inputs)}})
            self.assertNotEqual(command(rg.main, base + ["--out", str(extra)]), 0)
            self.assertFalse(extra.exists(), "fleet does not stage a second Git input")

    def test_fleet_ledger_uses_the_results_provenance_and_all_refuses_legacy_results(self):
        # Spec: a fetched private result or an unclassified legacy run never enters public runs/.
        checkout = self.root / "public"
        (checkout / "benches/cases").mkdir(parents=True)
        private = self.root / "private"
        isolation.overlay_repo(private)
        results = self.root / "result"
        suite_fixture.write(results, [suite_fixture.load(), suite_fixture.query("q", 10)])
        data = {"invocations": [{"invocation": "one", "started": "2026-11-21", "finished": "2026-11-21",
                 "machine": {"host": "timed-host", "fleet_host": "timed-host", "overlay": digest()},
                 "git": {}, "argv": [], "protocol": dict(runs=1, query_runs=1, cache="cold", order="fixed",
                                                         seed=0, timeout_s=2, query_timeout_s=2, runtime="native")}]}
        (results / "manifest.json").write_text(json.dumps(data), encoding="utf-8")
        with (patch.object(suite, "HERE", checkout / "benches/suite"),
              patch.object(suite, "load", return_value=({}, {})), patch.object(suite, "check", return_value=[])):
            argv = ["suite.py", "ledger", str(results)]
            os.environ["BENCH_INPUTS_GIT"] = str(private)
            self.assertEqual(command(lambda: suite.main(argv), argv), 0)
            public = checkout / "benches/runs"
            self.assertFalse((public / "result.toml").exists(), "private machine identity written into the public tree")
            self.assertTrue((private / "overlay/runs/result.toml").is_file())
            del os.environ["BENCH_INPUTS_GIT"]
            self.assertNotEqual(command(lambda: suite.main(argv), argv), 0)
            legacy = checkout / "benches/suite/results/legacy"
            suite_fixture.write(legacy, [suite_fixture.load()])
            (legacy / "manifest.json").unlink()
            all_argv = ["suite.py", "ledger", "--all"]
            self.assertNotEqual(command(lambda: suite.main(all_argv), all_argv), 0)
            self.assertFalse(list(public.glob("*.toml")))

    def test_distinct_overlay_contents_never_compare_even_with_host_override(self):
        # Spec: time is comparable only when the runs used the same overlay.
        def recorded(folder, cpus):
            (folder / "hosts.toml").write_text(REGISTRY + f'cpus = "{cpus}"\n', encoding="utf-8")
            with patch.dict(os.environ, {"NRESE_BENCH_PRIVATE": str(folder)}):
                machine = manifest.machine("native")
            report = suite_fixture.LoadedInputs().report(100, {}, case="rl-hierarchy")
            report["machine"].update(machine)
            return report

        first, second = self.root / "one", self.root / "two"
        first.mkdir()
        second.mkdir()
        plain = recorded(first, "0"), recorded(second, "1")
        repo = self.root / "inputs"
        isolation.overlay_repo(repo)
        dirty = recorded(repo / "overlay", "0"), recorded(repo / "overlay", "1")
        for case, pair in [("folders", plain), ("dirty edits", dirty)]:
            for allow in (False, True):
                with self.subTest(case=case, allow=allow):
                    with contextlib.redirect_stdout(io.StringIO()) as said:
                        code = compare.compare_reports(*pair, allow_host_change=allow)
                    self.assertEqual(code, 2, said.getvalue())
                    with contextlib.redirect_stdout(io.StringIO()) as said:
                        code = compare.compare_reports(pair[0], pair[0], allow_host_change=allow)
                    self.assertEqual(code, 0, said.getvalue())

    def test_partial_and_conflicting_envelopes_refuse_registry_and_floor_reads(self):
        # Spec: the job's three channels are one checked input, and an explicit folder is another.
        folder = self.root / "folder"
        folder.mkdir()
        (folder / "hosts.toml").write_text(REGISTRY, encoding="utf-8")
        full = dict(zip(CHANNELS, (REGISTRY, "", digest())))
        for case, settings in [
            ("floor alone", {CHANNELS[1]: '[timed-host]\nsource = "fixture"\n'}),
            ("missing floors", {k: v for k, v in full.items() if k != CHANNELS[1]}),
            ("bad digest", {**full, CHANNELS[2]: "unknown"}),
            ("two sources", {**full, "NRESE_BENCH_PRIVATE": str(folder)}),
        ]:
            for read in (hosts.load, lambda **kw: hosts.floor("timed-host", **kw)):
                with self.subTest(case=case, reader=read), self.assertRaises(ValueError):
                    read(environ=settings)

    def test_git_floor_read_failure_is_not_an_empty_policy(self):
        # Spec: a Git overlay with an unreadable floors path must fail at resolution.
        repo = self.root / "inputs"
        (repo / "overlay").mkdir(parents=True)
        (repo / "overlay/noise-floor").write_text("this should be a directory", encoding="utf-8")
        isolation.overlay_repo(repo)
        env = {"BENCH_INPUTS_GIT": str(repo)}
        with self.assertRaises(SystemExit):
            hosts.overlay_env(env)
        with self.assertRaises(ValueError):
            hosts.load(environ=env)

    def test_abba_replays_the_applied_floor_in_an_overlay_free_shell(self):
        # Spec: a saved round gives the same verdict without the registry the job ran with.
        fixture = abba_fixture.Abba()
        fixture.repo = self.root
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        directory = self.root / "inputs"
        overlay = directory / "overlay"
        floors = overlay / "noise-floor"
        floors.mkdir(parents=True)
        floor = {"source": "fixture", "cases": {"lab-olympics": {"time/" + q: 1.0 for q in abba_fixture.SERIES}}}
        (floors / "shared-a.toml").write_text(
            rg.write_floor({**floor, "host": "shared-a", "blocks": 8}, "fixture"), encoding="utf-8")
        isolation.overlay_repo(directory)
        with (patch.dict(os.environ, {"NRESE_BENCH_PRIVATE": str(overlay)}),
              patch.object(rg, "harness_tables", release_fixture.LOCAL_TABLES, create=True)):
            saved = fixture.round_(abba_fixture.records(b=1.08), host="shared-a")
            in_job, text = fixture.verdict(saved)
        with patch.object(rg, "harness_tables", release_fixture.LOCAL_TABLES, create=True):
            replay, replay_text = fixture.verdict(saved)
        self.assertEqual((in_job, replay), (abba.NO_LOSS, abba.NO_LOSS), text + "\n" + replay_text)
        self.assertIn("or shared-a's where larger", replay_text)
        saved_manifest = saved / "abba-manifest.json"
        data = json.loads(saved_manifest.read_text(encoding="utf-8"))
        data.pop("timing_inputs", None)
        saved_manifest.write_text(json.dumps(data), encoding="utf-8")
        with patch.object(rg, "harness_tables", release_fixture.LOCAL_TABLES, create=True):
            self.assertEqual(fixture.verdict(saved)[0], abba.NO_VERDICT)

    def test_release_confirm_and_verdict_refuse_rounds_using_different_overlays(self):
        # Spec: equal-looking results from different overlays cannot confirm or clear a loss.
        rounds, paths = [], []
        for index in range(2):
            reading = release_fixture.run_gate(release_fixture.passes(),
                                               context={**release_fixture.CONTEXT, "round": index})
            reading["timing_inputs"] = {"overlay": digest(REGISTRY + f'cpus = "{index}"\n'), "floor": None}
            rounds.append(reading)
            path = self.root / f"round-{index}.json"
            path.write_text(json.dumps(reading), encoding="utf-8")
            paths.append(str(path))
        argv = ["release_gate.py", "confirm", "--reports", *paths, "--out", str(self.root / "reports")]
        self.assertNotEqual(command(rg.main, argv), 0)
        self.assertFalse((self.root / "reports").exists())
        ok, text = rg.verdict(rounds, release_fixture.Rounds.W3C, release_fixture.Rounds.GUARDS)
        self.assertFalse(ok, text)
        self.assertIn("different private overlays", text)

    @unittest.skipUnless(shell_fixture.BASH, "needs Bash")
    def test_bad_overlay_stops_remote_before_ssh_and_gate_before_submit(self):
        # Spec: an unsuccessful policy read cannot grant remote admission or a gate verdict.
        fixture = shell_fixture.FleetGate()
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        marker = self.root / "ssh-called"
        ssh = fixture.fake / "bin/ssh"
        ssh.write_text(f'#!/bin/sh\ntouch "{marker.as_posix()}"\nexit 255\n', encoding="utf-8", newline="\n")
        ssh.chmod(0o755)
        malformed = self.root / "malformed"
        malformed.mkdir()
        (malformed / "hosts.toml").write_text("[fleet\n", encoding="utf-8")
        fixture.env["NRESE_BUILD_HOST"] = "dev-host"
        fixture.job(1, "dev-host", "failed", code=1, steps=[{"name": "test", "code": 1}])
        for overlay in (self.root / "missing", malformed):
            fixture.env["NRESE_BENCH_PRIVATE"] = str(overlay)
            with self.subTest(overlay=overlay.name, caller="remote"):
                done = subprocess.run([shell_fixture.BASH, "scripts/lib/remote.sh", "run", "true"], cwd=fixture.repo,
                                      env=fixture.env, capture_output=True, text=True, timeout=20)
                self.assertEqual((done.returncode, marker.exists()), (2, False), done.stderr)
            with self.subTest(overlay=overlay.name, caller="gate"):
                code, where, count, err = fixture.gate()
                self.assertEqual((code, where, count), (75, "", 0), err)
        # The Git source also controls remote admission. A known fleet-only host never reaches SSH.
        inputs = self.root / "inputs"
        isolation.overlay_repo(inputs, '[fleet]\nbuilds_by_fleet_only = ["dev-host"]\n')
        fixture.env.pop("NRESE_BENCH_PRIVATE")
        fixture.env["BENCH_INPUTS_GIT"] = str(inputs)
        marker.unlink(missing_ok=True)
        done = subprocess.run([shell_fixture.BASH, "scripts/lib/remote.sh", "run", "true"], cwd=fixture.repo,
                              env=fixture.env, capture_output=True, text=True, timeout=20)
        self.assertEqual((done.returncode, marker.exists()), (2, False), done.stderr)
        self.assertIn("is a fleet host", done.stderr)

    @unittest.skipUnless(shell_fixture.BASH, "needs Bash")
    def test_unconfirmed_failure_without_an_eligible_target_stays_unconfirmed(self):
        # Spec: an unconfirmed failure needs an eligible second host before another job can run.
        fixture = shell_fixture.FleetGate()
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        fixture.env["NRESE_GATE_UNCONFIRMED_HOSTS"] = "dev-host"
        fixture.job(1, "dev-host", "failed", code=1, steps=[{"name": "test", "code": 1}])
        for target in ("missing", "malformed", "only the first host"):
            with self.subTest(target=target):
                fixture.env["NRESE_BENCH_PRIVATE"] = str(self.root / target)
                fixture.env.pop("NRESE_GATE_CONFIRM_HOSTS", None)
                if target == "malformed":
                    (self.root / target).mkdir()
                    (self.root / target / "hosts.toml").write_text("[fleet\n", encoding="utf-8")
                if target == "only the first host":
                    fixture.env["NRESE_GATE_CONFIRM_HOSTS"] = "dev-host"
                (fixture.fake / "n").unlink(missing_ok=True)
                code, where, count, err = fixture.gate()
                self.assertEqual((code, where, count), (75, "", 1), err)
