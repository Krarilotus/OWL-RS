"""Public/private output, complete comparison evidence, and host admission."""
from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import isolation
import test_overlay_provenance as fixture
import test_push_gate as shell_fixture
import test_summary as rows
import test_verify as verify_fixture
from suitekit import compare


class OverlayBoundary(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        env = isolation.inherit("NRESE_BENCH_", "BENCH_INPUTS_", "NRESE_FAST_", "FLEET_")
        self.enterContext(patch.dict(os.environ, env, clear=True))

    def test_verify_checks_both_plan_destinations_before_writing(self):
        # Spec: an overlay placement key can be saved only outside the public checkout.
        repo = self.root / "public"
        repo.mkdir()
        (repo / "case.toml").write_text('name = "case"\n', encoding="utf-8")
        isolation.repo(repo, 1)
        inputs = self.root / "inputs"
        isolation.overlay_repo(inputs, '[fleet]\nverify = ["checks-fixture"]\n')
        os.environ["BENCH_INPUTS_GIT"] = str(inputs)
        verify = verify_fixture.verify
        argv = ["jobs", "--repo", str(repo), "--rev", "HEAD", "--reason", "fixture"]
        for path, inside in ((repo / "private-plans", True), (self.root / "external", False)):
            with self.subTest(inside=inside):
                args = argv + ["--out", str(path), "case.toml"]
                code = fixture.command(lambda: verify.main(args), ["verify.py", *args])
                if inside:
                    self.assertNotEqual(code, 0)
                    self.assertFalse(path.exists())
                else:
                    self.assertEqual(code, 0)
                    specs = [tomllib.loads(f.read_text(encoding="utf-8")) for f in path.glob("*.toml")]
                    self.assertEqual([job["requires"]["hosts"] for job in specs], [["checks-fixture"]] * 2)

    def fast_fixture(self, effect):
        """Load the real entry point from a neutral checkout and substitute only execution."""
        checkout = isolation.checkout(self.root / "public", "benches/fast/fast.py")
        spec = importlib.util.spec_from_file_location("isolated_fast", checkout / "benches/fast/fast.py")
        fast = importlib.util.module_from_spec(spec)
        with patch.object(sys, "path", list(sys.path)):
            spec.loader.exec_module(fast)
        case = {"name": "rl-hierarchy", "data": "fixture"}
        for name, value in {
            "RUNTIME": "native", "native_dirs": lambda: {}, "load_cases": lambda: [case],
            "select": lambda _catalog, _args: [case], "case_manifest": lambda _catalog: {},
            "host_state": lambda: None, "case_inputs": lambda _case: {}, "binaries": lambda: {},
            "verdict": lambda _case, _record: {"verdict": "ok"},
            "run_case": lambda *_: effect(fast),
        }.items():
            self.enterContext(patch.object(fast, name, value))
        return fast, checkout

    def test_fast_envelope_keeps_the_compact_result_private_and_refuses_public_override(self):
        # Spec: the run's saved private identity determines both evidence destinations.
        os.environ.update(zip(fixture.CHANNELS, (fixture.REGISTRY, "", fixture.digest())))
        self.enterContext(patch.object(Path, "home", return_value=self.root / "home"))
        for override in (False, True):
            with self.subTest(public_override=override):
                if override:
                    os.environ["NRESE_FAST_BASELINES"] = str(self.root / "public/override")
                # Load again so the parent tree's import-time destination sees the same environment.
                fast, checkout = self.fast_fixture(lambda _: {"case": "rl-hierarchy", "status": "ok",
                                                               "rep": 1, "metric": 1, "checks": [], "routes": []})
                out = self.root / f"run-{override}.json"
                args = SimpleNamespace(label="fixture", reps=1, out=str(out), rows=None)
                code = fixture.command(lambda: fast.run(args), ["fast.py", "run"])
                self.assertEqual(code, int(override))
                self.assertFalse(list(checkout.glob("**/run-*.json")))
                if override:
                    self.assertFalse(out.exists())
                else:
                    saved = list((Path.home() / "owlrs/private").rglob(out.name))
                    self.assertEqual(len(saved), 1)
                    self.assertEqual(json.loads(saved[0].read_text(encoding="utf-8"))["machine"]["overlay"],
                                     fixture.digest())

    def test_fast_run_keeps_pin_and_cold_cache_policy_when_the_overlay_changes(self):
        # Spec: every runtime lookup in a recorded run reads the inputs of that run's digest.
        overlay = self.root / "overlay"
        overlay.mkdir()
        registry = overlay / "hosts.toml"
        old = fixture.REGISTRY + '[[host.timed-host.config]]\nsince = "2000-01-01"\ncpus = "1"\ncold_cache = "first"\n'
        new = old.replace('cpus = "1"', 'cpus = "2"').replace('cold_cache = "first"', 'cold_cache = "second"')
        registry.write_text(old, encoding="utf-8")
        os.environ["NRESE_BENCH_PRIVATE"] = str(overlay)
        self.enterContext(patch.object(fixture.hosts.socket, "gethostname", return_value="timed-host"))

        def observed(fast):
            registry.write_text(new, encoding="utf-8")
            return {"case": "rl-hierarchy", "status": "ok", "rep": 1, "metric": 1, "checks": [], "routes": [],
                    "pin": fast.pinned_cpus(), "cache": fast.cold_cache_command()}

        fast, _ = self.fast_fixture(observed)
        out = self.root / "run.json"
        args = SimpleNamespace(label="fixture", reps=1, out=str(out), rows=None)
        self.assertEqual(fixture.command(lambda: fast.run(args), ["fast.py", "run"]), 0)
        saved = json.loads(out.read_text(encoding="utf-8"))
        self.assertEqual((saved["records"][0]["pin"], saved["records"][0]["cache"]), ("1", "first"))
        self.assertEqual(saved["machine"]["overlay"], fixture.digest(old))
        self.assertEqual(fast.pinned_cpus(), "2", "the next run can see the edited overlay")

    def test_private_comparisons_require_evidence_from_every_contributor(self):
        # Spec: comparable numbers with a missing host or manifest still have no private timing verdict.
        machine = {"host": "fixture", "overlay": fixture.digest()}
        fast_report = rows.LoadedInputs().report(100, {}, case="rl-hierarchy")
        private = {**fast_report, "machine": machine}
        base = rows.write(self.root / "base", [rows.load(), rows.query("q", 5)])
        new = rows.write(self.root / "new", [rows.load(), rows.query("q", 5)])
        (base.parent / "manifest.json").write_text(json.dumps({"invocations": [
            {"machine": machine, "started": fast_report["started"], "datasets": {}, "dataset_sha256": {}}
        ]}), encoding="utf-8")
        (new.parent / "manifest.json").unlink()
        for allow in (False, True):
            opts = ["--allow-host-change"] if allow else []
            for missing in ({}, {"machine": machine, "started": None}):
                with self.subTest(allow=allow, json=missing):
                    unknown = {k: v for k, v in fast_report.items() if k != "machine"} | missing
                    with contextlib.redirect_stdout(io.StringIO()):
                        self.assertEqual(compare.compare_reports(private, unknown, allow_host_change=allow), 2)
            with self.subTest(allow=allow, csv="no manifest"):
                # Hold input hashes equal to exercise the host-evidence gate independently.
                with patch.object(compare, "suite_inputs", return_value=({}, {})), contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(compare.main([str(base), str(new), *opts]), 2)

    @unittest.skipUnless(shell_fixture.BASH, "needs Bash")
    def test_absent_policy_stops_admission_and_explicit_empty_allows_it(self):
        # Spec: a missing source or key grants nothing, whereas [] is a known empty policy.
        remote = self.root / "ssh-called"
        gate = shell_fixture.FleetGate()
        gate.setUp()
        self.addCleanup(gate.doCleanups)
        stub = gate.fake / "bin/ssh"
        stub.write_text(f'#!/bin/sh\ntouch "{remote.as_posix()}"\nexit 255\n', encoding="utf-8", newline="\n")
        stub.chmod(0o755)
        gate.env["NRESE_BUILD_HOST"] = "dev-host"
        gate.job(1, "dev-host", "failed", code=1, steps=[{"name": "test", "code": 1}])
        overlay = self.root / "overlay"
        overlay.mkdir()
        for source in ("unset", "no key", "empty"):
            remote.unlink(missing_ok=True)
            (gate.fake / "n").unlink(missing_ok=True)
            gate.env.pop("NRESE_BENCH_PRIVATE", None)
            known = source == "empty"
            if source != "unset":
                policy = "[fleet]\n" + ("builds_by_fleet_only = []\ngate_unconfirmed = []\n" if known else "")
                (overlay / "hosts.toml").write_text(policy, encoding="utf-8")
                gate.env["NRESE_BENCH_PRIVATE"] = str(overlay)
            with self.subTest(source=source, caller="remote"):
                done = subprocess.run([shell_fixture.BASH, "scripts/lib/remote.sh", "run", "true"], cwd=gate.repo,
                                      env=gate.env, capture_output=True, text=True, timeout=20)
                self.assertEqual((done.returncode, remote.exists()), (75, True) if known else (2, False), done.stderr)
            with self.subTest(source=source, caller="gate"):
                code, where, count, error = gate.gate()
                self.assertEqual((code, where, count), (1, "", 1) if known else (75, "", 0), error)
