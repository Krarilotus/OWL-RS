"""The release gate's report (benches/fast/release_gate.py; the owner, 13 Oct 2026): every
slowdown listed, small ones too; a regression, a failed check or a memory growth fails the
gate; a known issue failing as declared doesn't; passes of more than one configuration fail
it. Scheduling is owned by Fleet.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import importlib.util
import contextlib
import io
import json
import hashlib
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
from test_queue import fixture_environment

ROOT = Path(__file__).resolve().parents[3]


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


gate = load("release_gate", ROOT / "benches/fast/release_gate.py")
fast = load("release_fast", ROOT / "benches/fast/fast.py")


def fixture_expected(names, revision):
    return {"harness_revision": "harness-sha", "engine_revision": revision,
            "sha256": {"release/examples/perf_lab": "d" * 64},
            "planned_cases": list(names), "all_cases": list(names),
            "case_manifest": {"fingerprint": "fixture-definitions", "series": {n: ["q"] for n in names}},
            "outcomes": {n: "gave-up" if n == "known" else "ok" for n in names}}


def release(candidate, baselines, not_run=None, diagnostic_passes=None, expectations=None):
    if expectations is None:
        expectations = {name: fixture_expected(candidate.get("planned_cases", []), r.get("revision"))
                        for name, r in [("candidate", candidate), *baselines.items()]}
    return gate.gate(candidate, baselines, not_run, diagnostic_passes, expectations)


def report(tmp: Path, name: str, rev: str, cases: dict[str, tuple[float, int, str]], host: str = "Phuoc-Yu") -> Path:
    """A one-repetition pass: per case (median ms, peak MiB, status)."""
    records = []
    for case, (ms, peak, status) in cases.items():
        expected = "gave-up" if case == "known" else "ok"
        records.append({"case": case, "rep": 1, "status": status, "expected_outcome": expected, "metric": ms,
                        "samples": {"q": [ms * f for f in (0.99, 1.0, 1.01)]}, "peak_mib": peak,
                        "memory_kind": "lifetime", "checks": [{"path": "answer", "ok": status == "ok",
                                                               "expected": "x", "got": "y"}], "routes": []})
    path = tmp / f"{name}.json"
    path.write_text(json.dumps({"label": name, "started": "2026-10-14T06:00:00", "runtime": "native",
                                "machine": {"host": host}, "git": {"commit": "harness-sha", "dirty": 0},
                                "git_finished": {"commit": "harness-sha", "dirty": 0},
                                "binaries": {"revision": rev, "sha256": {"release/examples/perf_lab": "d" * 64}, "verified": True},
                                "binaries_finished": {"revision": rev, "sha256": {"release/examples/perf_lab": "d" * 64}, "verified": True},
                                "case_manifest": fixture_expected(cases, rev)["case_manifest"], "reps": 1, "planned_cases": list(cases),
                                "all_cases": list(cases), "finished": "2026-10-14T06:01:00",
                                "records": records}), encoding="utf-8")
    return path


class Coverage(unittest.TestCase):
    def paths(self, tmp, side, count=4, cases=None):
        return [report(tmp, f"{side}{i}", side, cases or {"a": (1, 1, "ok"), "b": (1, 1, "ok")})
                for i in range(count)]

    def edit(self, path, change):
        value = json.loads(path.read_text())
        change(value)
        path.write_text(json.dumps(value))

    def test_incomplete_or_inexact_passes_never_certify_release(self):
        changes = [
            ("marker", lambda r: r.pop("finished")),
            ("missing case", lambda r: r["records"].pop()),
            ("duplicate case", lambda r: r["records"].append(r["records"][0])),
            ("unexpected case", lambda r: r["records"][0].update(case="unexpected")),
            ("wrong repetition", lambda r: r["records"][0].update(rep=2)),
            ("wrong repetitions", lambda r: r.update(reps=2)),
            ("missing plan", lambda r: r.pop("planned_cases")),
            ("duplicate plan", lambda r: r["planned_cases"].append("a")),
        ]
        for name, change in changes:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as tmp:
                t = Path(tmp)
                cand, base = self.paths(t, "c"), self.paths(t, "b")
                self.edit(cand[0], change)
                result = release(gate.pool(cand), {"base": gate.pool(base)})
                self.assertFalse(result["passed"])
                self.assertTrue(result["coverage"]["candidate"])
                self.assertIn("c0.json", gate.render(result))

    def test_matching_interrupted_subsets_and_missing_catalog_cannot_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp)
            cand, base = self.paths(t, "c"), self.paths(t, "b")
            for path in cand + base:
                self.edit(path, lambda r: (r["records"].pop(), r.pop("finished")))
            result = release(gate.pool(cand), {"base": gate.pool(base)})
            self.assertFalse(result["passed"])
            self.assertIn("Incomplete evidence", gate.render(result))
            for path in cand + base:
                self.edit(path, lambda r: r.update(finished="done", planned_cases=["a"]))
            self.assertFalse(release(gate.pool(cand), {"base": gate.pool(base)})["passed"])

    def test_missing_or_duplicate_passes_cannot_satisfy_the_campaign(self):
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp)
            cand, base = self.paths(t, "c"), self.paths(t, "b")
            for paths in (cand[:3], cand[:3] + [cand[0]], []):
                with self.subTest(paths=paths):
                    self.assertFalse(release(gate.pool(paths), {"base": gate.pool(base)})["passed"])
            self.edit(cand[3], lambda r: r.update(label="c0"))
            self.assertFalse(release(gate.pool(cand), {"base": gate.pool(base)})["passed"])
            result = release(gate.pool(self.paths(t, "fresh")), {"base": gate.pool(base[:3])})
            self.assertFalse(result["passed"])
            self.assertIn("3 complete passes", ";".join(result["coverage"]["base"]))

    def test_changed_plans_between_passes_or_sides_are_incomplete(self):
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp)
            cand = self.paths(t, "c")
            base = self.paths(t, "b", cases={"a": (1, 1, "ok")})
            result = release(gate.pool(cand), {"base": gate.pool(base)})
            self.assertFalse(result["passed"])
            self.assertIn("case plan differs between sides", result["coverage"]["base"])
            self.edit(cand[3], lambda r: (r["records"].pop(), r.update(planned_cases=["a"], all_cases=["a"])))
            self.assertIn("case plan differs between passes/sides", ";".join(gate.pool(cand)["coverage_errors"]))

    def test_diagnostics_keep_campaign_requirement_and_never_accept_release(self):
        with tempfile.TemporaryDirectory() as tmp:
            for count in (1, 4):
                t = Path(tmp)
                cand, base = gate.pool(self.paths(t, "c", count)), gate.pool(self.paths(t, "b", count))
                result = release(cand, {"base": base}, diagnostic_passes=count)
                self.assertEqual(result["required_passes"], 4)
                self.assertFalse(result["passed"])
                self.assertIn("DIAGNOSTIC ONLY", gate.render(result))
                self.assertIn("NOT RELEASE ACCEPTED", gate.render(result))

    def test_complete_quality_failures_remain_data_and_unobserved_not_run_stays_open(self):
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp)
            cand = gate.pool(self.paths(t, "c", cases={"a": (1, 1, "wrong")}))
            base = gate.pool(self.paths(t, "b", cases={"a": (1, 1, "ok")}))
            self.assertEqual(cand["coverage_errors"], [])
            result = release(cand, {"base": base}, {"absent": "missing input"})
            self.assertFalse(result["passed"])
            self.assertIn("a", result["candidate_failures"])
            self.assertEqual(result["not_run"], {"absent": "missing input"})

    def test_real_fast_checkpoint_declares_unrun_cases_and_has_no_completion_marker(self):
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp)
            args = SimpleNamespace(cases=None, areas=None, label="partial", out=str(t / "pass.json"), reps=1)
            first = {"case": "a", "rep": 1, "status": "ok", "expected_outcome": "ok",
                     "checks": [], "routes": [], "samples": {"q": [1]}, "metric": 1}
            with patch.object(fast, "RUNTIME", "native"), patch.object(fast, "native_dirs", return_value={}), \
                    patch.object(fast, "load_cases", return_value=[{"name": "a"}, {"name": "b"}]), \
                    patch.object(fast, "REPORTS", t), patch.object(fast, "BASELINES", t), \
                    patch.object(fast, "binaries", return_value={"revision": "engine"}), \
                    patch.object(fast.manifest, "git", return_value={"commit": "harness"}), \
                    patch.object(fast.manifest, "machine", return_value={"host": "fixture"}), \
                    patch.object(fast, "run_case", side_effect=[first, KeyboardInterrupt]), \
                    contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(KeyboardInterrupt):
                    fast.run(args)
            checkpoint = json.loads((t / "pass.json").read_text())
            self.assertEqual(checkpoint["planned_cases"], ["a", "b"])
            self.assertEqual(checkpoint["all_cases"], ["a", "b"])
            self.assertNotIn("finished", checkpoint)
            self.assertIn("completion marker missing", gate.pass_errors(checkpoint))


class Integrity(unittest.TestCase):
    paths, edit = Coverage.paths, Coverage.edit
    def test_bound_positive_and_each_provenance_counterexample(self):
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp)
            cand, base = self.paths(t, "cand"), self.paths(t, "base")
            expected = {n: fixture_expected(["a", "b"], rev) for n, rev in (("candidate", "cand"), ("base", "base"))}
            original = [p.read_text() for p in cand + base]
            self.assertTrue(release(gate.pool(cand), {"base": gate.pool(base)}, expectations=expected)["passed"])
            self.assertFalse(gate.gate(gate.pool(cand), {"base": gate.pool(base)})["passed"])
            mutations = {
                "self-declared singleton": lambda r: (r.update(planned_cases=["a"], all_cases=["a"]), r["records"].pop()),
                "missing provenance": lambda r: (r.pop("git"), r.pop("binaries")),
                "changed engine": lambda r: r["binaries"].update(revision="stale"),
                "changed harness": lambda r: r["git"].update(commit="stale"),
                "dirty harness": lambda r: r["git"].update(dirty=1),
                "missing dirty state": lambda r: r["git"].pop("dirty"),
                "changed hashes": lambda r: r["binaries"]["sha256"].update({"release/examples/perf_lab": r["label"][-1] * 64}),
                "missing hashes": lambda r: r["binaries"].pop("sha256"),
                "unverified hashes": lambda r: r["binaries"].update(verified=False),
                "changed during pass": lambda r: r["binaries_finished"]["sha256"].update({"release/examples/perf_lab": "0" * 64}),
                "dirty after pass": lambda r: r["git_finished"].update(dirty=1),
                "missing end provenance": lambda r: r.pop("git_finished"),
                "changed definitions": lambda r: r["case_manifest"].update(fingerprint="stale"),
                "changed outcome": lambda r: r["records"][0].update(expected_outcome="wrong"),
            }
            for name, mutation in mutations.items():
                with self.subTest(name=name):
                    for p, raw in zip(cand + base, original): p.write_text(raw)
                    for p in cand + base: self.edit(p, mutation)
                    result = release(gate.pool(cand), {"base": gate.pool(base)}, expectations=expected)
                    self.assertFalse(result["passed"])
                    self.assertTrue(any(result["coverage"].values()))
            for p, raw in zip(cand + base, original): p.write_text(raw)
            missing_side = {**expected, "main": fixture_expected(["a", "b"], "main")}
            self.assertFalse(release(gate.pool(cand), {"base": gate.pool(base)}, expectations=missing_side)["passed"])

    def test_successful_series_are_exact_valid_and_complete(self):
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp)
            cand, base = self.paths(t, "cand"), self.paths(t, "base")
            original = [p.read_text() for p in cand + base]
            mutations = [lambda r: r.update(metric=None, samples={}), lambda r: r.update(metric=float("nan")),
                         lambda r: r.update(metric=-1), lambda r: r.update(metric=True)]
            mutations += [lambda r, v=v: r.update(samples={"q": v})
                          for v in ([], [None], [-1], [float("inf")], [float("nan")], [True], ["1"], "invalid")]
            mutations += [lambda r: r.update(samples={"q": [1], "omitted-query": [100000]})]
            for mutation in mutations:
                for p, raw in zip(cand + base, original): p.write_text(raw)
                self.edit(base[1], lambda r: mutation(r["records"][0]))
                result = release(gate.pool(cand), {"base": gate.pool(base)})
                self.assertFalse(result["passed"])
                self.assertTrue(result["coverage"]["base"])
                self.assertFalse(result["baselines"]["base"]["cases"]["a"]["compared"])
            for p, raw in zip(cand + base, original): p.write_text(raw)
            expected = {n: fixture_expected(["a", "b"], rev) for n, rev in (("candidate", "cand"), ("base", "base"))}
            for value in expected.values(): value["case_manifest"]["series"]["a"] = ["q", "omitted-query"]
            for p in cand + base:
                self.edit(p, lambda r: r.update(case_manifest=expected["candidate"]["case_manifest"]))
            result = release(gate.pool(cand), {"base": gate.pool(base)}, expectations=expected)
            self.assertFalse(result["passed"])  # symmetry alone cannot hide the same omission on both sides

    def test_failed_baseline_without_timing_is_complete_quality_data(self):
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp)
            cand, base = self.paths(t, "cand"), self.paths(t, "base")
            for p in base:
                self.edit(p, lambda r: r["records"][0].update(status="wrong", metric=None, samples={}))
            result = release(gate.pool(cand), {"base": gate.pool(base)})
            self.assertTrue(result["passed"])
            self.assertIn("a", result["baselines"]["base"]["failures"])
            self.assertEqual(result["baselines"]["base"]["cases"]["a"], {"compared": False})

    def test_reserved_duplicate_names_and_incomplete_candidate_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp)
            cand, base = self.paths(t, "cand"), self.paths(t, "base")
            self.edit(cand[0], lambda r: r.pop("finished"))
            with self.assertRaisesRegex(ValueError, "reserved"):
                gate.gate(gate.pool(cand), {"candidate": gate.pool(base)})
            for extras in (["--baseline", "candidate=p"], ["--baseline", "base=p", "--baseline", "base=p"],
                           ["--baseline", "base=p", "--engine", "base=base=bin"]):
                argv = ["gate", "--candidate", "p", "--harness-revision", "harness-sha",
                        "--engine", "candidate=cand=bin", "--engine", "base=base=bin", *extras]
                with patch.object(sys, "argv", argv), patch.object(gate, "expectation", return_value={}), \
                        patch.object(gate, "pool", return_value={}), contextlib.redirect_stderr(io.StringIO()):
                    with self.assertRaises(SystemExit) as error: gate.main()
                    self.assertEqual(error.exception.code, 2)

    def test_manifest_reads_definitions_and_query_files_without_preparation(self):
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp)
            registry = t / "benches/cases"; registry.mkdir(parents=True)
            casefile = registry / "fixture.toml"
            casefile.write_text('[[case]]\nname="a"\nmetric="sum_p50_ms"\nqueries="set"\nargs=["--runs","3"]\n')
            queries = t / "benches/fast/queries/set"; queries.mkdir(parents=True)
            (queries / "q.rq").write_text("SELECT * WHERE {?s ?p ?o}")
            with patch.object(gate.manifest, "git", return_value={"commit": "harness", "dirty": 0}), \
                    patch.object(gate.manifest, "binaries", return_value={"verified": True, "sha256": {"perf": "d" * 64}}):
                expected = gate.expectation(t, "harness", "engine", t)
                self.assertEqual(expected["case_manifest"]["series"], {"a": ["q"]})
                (queries / "second.rq").write_text("ASK {?s ?p ?o}")
                changed = gate.expectation(t, "harness", "engine", t)
                self.assertNotEqual(expected["case_manifest"], changed["case_manifest"])
                casefile.write_text(casefile.read_text().replace('"3"', '"7"'))
                self.assertNotEqual(changed["case_manifest"], gate.expectation(t, "harness", "engine", t)["case_manifest"])
                with self.assertRaises(SystemExit): gate.expectation(t, "harness", "engine", t, "unknown")
            for source in ({"commit": "wrong", "dirty": 0}, {"commit": "harness", "dirty": 1}, {"commit": "harness", "dirty": None}):
                with patch.object(gate.manifest, "git", return_value=source), self.assertRaises(ValueError):
                    gate.expectation(t, "harness", "engine", t)

    def test_build_records_verify_real_wrapper_and_missing_provenance(self):
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp)
            examples = t / "release/examples"; examples.mkdir(parents=True)
            for name in ("perf_lab", "perf_lab.real", "ql_print_check"): (examples / name).write_bytes(name.encode())
            self.assertFalse(fast.manifest.binaries(t, "engine")["verified"])
            (t / "REVISION").write_text("engine")
            sums = {p.relative_to(t).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest() for p in examples.iterdir()}
            (t / "SHA256SUMS").write_text("\n".join(f"{v}  {k}" for k, v in sums.items()))
            self.assertTrue(fast.manifest.binaries(t, "engine")["verified"])
            self.assertFalse(fast.manifest.binaries(t, "other-engine")["verified"])
            (examples / "perf_lab.real").write_bytes(b"changed")
            self.assertFalse(fast.manifest.binaries(t, "engine")["verified"])
            (t / "SHA256SUMS").write_text("bad  ../../outside")
            self.assertFalse(fast.manifest.binaries(t, "engine")["verified"])


class Runner(unittest.TestCase):
    def test_interrupted_pass_is_rejected_archived_then_only_complete_pass_is_resumed(self):
        bash = Path("C:/Program Files/Git/bin/bash.exe") if os.name == "nt" else shutil.which("bash")
        if not bash or not Path(bash).is_file():
            self.skipTest("Bash unavailable for release-runner contract")
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp)
            bin_dir, gate_dir, out = t / "bin", t / "gate", t / "out"
            bin_dir.mkdir()
            target = gate_dir / "bin-12345678"
            target.mkdir(parents=True)
            revision = "1234567890123456789012345678901234567890"
            (target / "REVISION").write_text(revision)
            executable = target / "release/examples/perf_lab"
            executable.parent.mkdir(parents=True)
            executable.write_bytes(b"fixture, never executed")
            (target / "SHA256SUMS").write_bytes(f"{hashlib.sha256(executable.read_bytes()).hexdigest()}  release/examples/perf_lab\n".encode())
            harness = gate_dir / "src-12345678" / "benches" / "fast"
            harness.mkdir(parents=True)
            (bin_dir / "git").write_text('#!/bin/sh\n[ "$1" != status ] || exit 0\n[ "$2" != --short=8 ] || { echo 12345678; exit 0; }\necho 1234567890123456789012345678901234567890\n')
            launcher = bin_dir / "fixture-python.py"
            launcher.write_text("import sys, runpy\nsys.dont_write_bytecode=True\n"
                                f"sys.path.insert(0, {str(ROOT / 'benches/suite')!r})\n"
                                "from suitekit import manifest\n"
                                f"manifest.git=lambda root: {{'commit':{revision!r},'dirty':0}}\n"
                                "sys.argv=sys.argv[1:]; runpy.run_path(sys.argv[0],run_name='__main__')\n")
            (bin_dir / "python3").write_text(f'#!/bin/sh\nexec "{Path(sys.executable).as_posix()}" "{launcher.as_posix()}" "$@"\n')
            for tool in bin_dir.iterdir():
                tool.chmod(0o755)
            registry = harness.parent / "cases"
            registry.mkdir()
            (registry / "fixture.toml").write_text('[[case]]\nname="a"\nmetric="x"\n[[case]]\nname="b"\nmetric="x"\n')
            (harness / "fast.py").write_text(
                "import json, os, pathlib, sys, importlib.util\n"
                f"spec=importlib.util.spec_from_file_location('fixture_fast', {str(ROOT / 'benches/fast/fast.py')!r})\n"
                "fast=importlib.util.module_from_spec(spec); spec.loader.exec_module(fast)\n"
                "root=pathlib.Path(__file__).resolve().parents[2]\n"
                "catalog=fast.cases_registry.load(root/'benches/cases')\n"
                "names=[c['name'] for c in catalog]; plan=sys.argv[sys.argv.index('--cases')+1].split(',') if '--cases' in sys.argv else names\n"
                "source={'commit':os.environ['NRESE_FAST_REVISION'], 'dirty':0}\n"
                "build=fast.manifest.binaries(pathlib.Path(os.environ['NRESE_FAST_BIN']), source['commit'])\n"
                "r={'reps':1, 'planned_cases':plan, 'all_cases':names, 'case_manifest':fast.case_manifest(catalog,root), "
                "'git':source, 'git_finished':source, 'binaries':build, 'binaries_finished':build, 'records':[]}\n"
                "complete = os.environ['SIM_COMPLETE'] == '1'\n"
                "for n in (plan if complete else plan[:1]): r['records'].append({'case':n,'rep':1,'status':'ok','metric':1,'samples':{'_':[1]}})\n"
                "if complete: r.update(finished='done')\n"
                "out = pathlib.Path(os.environ['NRESE_BENCH_REPORTS']) / 'fast'\n"
                "out.mkdir(parents=True, exist_ok=True)\n"
                "(out / 'day-gate-1-cand.json').write_text(json.dumps(r))\n"
                "assert fast.BASELINES == out/'compact' and not fast.BASELINES.is_relative_to(root)\n"
                "fast.BASELINES.mkdir(parents=True,exist_ok=True); (fast.BASELINES/'fixture.json').write_text(json.dumps(r))\n"
                "print('synthetic pass evidence')\n"
                "sys.exit(0 if complete else 130)\n")
            fixture_bin = "/" + bin_dir.drive[0].lower() + bin_dir.as_posix()[2:] if os.name == "nt" else str(bin_dir)
            env = {**fixture_environment(), "NRESE_GATE_DIR": gate_dir.as_posix(), "FLEET_OUT": out.as_posix(),
                   "FIXTURE_BIN": fixture_bin, "SIM_COMPLETE": "0"}
            command = [str(bash), "-c", 'export PATH="$FIXTURE_BIN:$PATH"; source scripts/release-gate.sh "$@"',
                       "fixture", "pass", "1", "cand", "fixture", "fixture"]
            partial = subprocess.run(command, env=env, cwd=ROOT, capture_output=True, text=True, timeout=20)
            self.assertEqual(partial.returncode, 1, partial.stdout + partial.stderr)
            evidence = out / "fast" / "day-gate-1-cand.json"
            self.assertNotIn("finished", json.loads(evidence.read_text()))
            self.assertTrue((out / "fast/compact/fixture.json").exists())
            env["SIM_COMPLETE"] = "1"
            completed = subprocess.run(command, env=env, cwd=ROOT, capture_output=True, text=True, timeout=20)
            self.assertEqual(completed.returncode, 0, completed.stdout + completed.stderr)
            archived = list(out.glob("interrupted-gate-1-cand-*"))
            self.assertEqual(len(archived), 1)
            self.assertNotIn("finished", json.loads((archived[0] / evidence.name).read_text()))
            self.assertIn("synthetic pass evidence", (archived[0] / "pass-1-cand.log").read_text())
            env["SIM_COMPLETE"] = "0"  # would fail if the completed pass were executed again
            resumed = subprocess.run(command, env=env, cwd=ROOT, capture_output=True, text=True, timeout=20)
            self.assertEqual(resumed.returncode, 0, resumed.stdout + resumed.stderr)
            self.assertIn("complete before", resumed.stdout)
            self.assertIn("finished", json.loads(evidence.read_text()))
            env["SIM_COMPLETE"] = "1"
            for mutation in (
                lambda r: r["binaries"].update(revision="old-engine"),
                lambda r: r["git"].update(commit="old-harness"),
                lambda r: r["case_manifest"].update(fingerprint="old-definitions"),
                lambda r: r["binaries"]["sha256"].update({"release/examples/perf_lab": "0" * 64}),
            ):
                r = json.loads(evidence.read_text()); mutation(r); evidence.write_text(json.dumps(r))
                stale = subprocess.run(command, env=env, cwd=ROOT, capture_output=True, text=True, timeout=20)
                self.assertEqual(stale.returncode, 0, stale.stdout + stale.stderr)
                self.assertIn("stale evidence preserved", stale.stdout)
            env["GATE_CASES"] = "a"
            selected = subprocess.run(command, env=env, cwd=ROOT, capture_output=True, text=True, timeout=20)
            self.assertEqual(selected.returncode, 0, selected.stdout + selected.stderr)
            self.assertEqual(json.loads(evidence.read_text())["planned_cases"], ["a"])
            self.assertEqual(len(list(out.glob("interrupted-gate-1-cand-*"))), 6)
            (out / "release-gate.md").write_text("old accepted summary")
            (out / "release-gate.json").write_text('{"passed":true}')
            (out / "report.log").write_text("old report log")
            executable.write_bytes(b"no longer the verified build")
            rejected = subprocess.run(command[:-5] + ["report", "fixture", "base=fixture"],
                                      env=env, cwd=ROOT, capture_output=True, text=True, timeout=20)
            self.assertNotEqual(rejected.returncode, 0)
            prior = list(out.glob("prior-report-*"))
            self.assertEqual(len(prior), 1)
            self.assertEqual((prior[0] / "release-gate.md").read_text(), "old accepted summary")
            self.assertEqual((prior[0] / "report.log").read_text(), "old report log")
            self.assertFalse((out / "release-gate.json").exists())


class Report(unittest.TestCase):
    def test_harness_and_engine_revisions_stay_separate(self):
        result = self.run_gate({"a": (100.0, 500, "ok")}, {"a": (100.0, 500, "ok")})
        self.assertEqual(result["candidate"], "cand")
        self.assertEqual(result["harness_revision"], "harness-sha")
        self.assertEqual(result["baselines"]["base"]["revision"], "base")
        self.assertEqual(result["baselines"]["base"]["harness_revision"], "harness-sha")
        self.assertIn("Harness revision: harness-sha; engine revision: cand.", gate.render(result))

    def test_passes_from_different_harnesses_are_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            paths = [report(Path(tmp), str(i), "cand", {"a": (100.0, 500, "ok")}) for i in range(2)]
            changed = json.loads(paths[1].read_text())
            changed["git"]["commit"] = "other-harness-sha"
            paths[1].write_text(json.dumps(changed))
            with self.assertRaisesRegex(SystemExit, "different harness revisions"):
                gate.pool(paths)

    def test_sides_using_different_harnesses_cannot_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            cand = gate.pool([report(Path(tmp), "c", "cand", {"a": (100.0, 500, "ok")})])
            base = gate.pool([report(Path(tmp), "b", "base", {"a": (100.0, 500, "ok")})])
            base["harness_revision"] = "other-harness-sha"
            result = release(cand, {"base": base})
            self.assertFalse(result["passed"])
            self.assertIn("NOT ONE HARNESS", gate.render(result))

    def run_gate(self, cand_cases, base_cases, host="Phuoc-Yu"):
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp)
            cand = gate.pool([report(t, f"c{i}", "cand", cand_cases) for i in range(4)])
            base = gate.pool([report(t, f"b{i}", "base", base_cases, host if i == 3 else "Phuoc-Yu") for i in range(4)])
            return release(cand, {"base": base})

    def test_small_slowdowns_are_listed_and_pass_a_regression_fails(self):
        base = {"a": (100.0, 500, "ok"), "b": (50.0, 300, "ok"), "known": (10.0, 10, "gave-up")}
        small = self.run_gate({"a": (103.0, 500, "ok"), "b": (49.0, 300, "ok"), "known": (10.0, 10, "gave-up")}, base)
        self.assertTrue(small["passed"])
        a = small["baselines"]["base"]["cases"]["a"]["time"]
        self.assertAlmostEqual(a["ratio"], 1.03, places=2)
        self.assertEqual(a["verdict"], "tie")
        self.assertIn("| a | 1.030 |", gate.render(small))
        self.assertEqual(small["known_issues"], {"known": "gave-up"})
        slow = self.run_gate({"a": (130.0, 500, "ok"), "b": (50.0, 300, "ok"), "known": (10.0, 10, "gave-up")}, base)
        self.assertFalse(slow["passed"])
        self.assertEqual(slow["baselines"]["base"]["cases"]["a"]["time"]["verdict"], "regression")

    def test_a_known_issue_the_candidate_passes_is_fixed_not_failed(self):
        # 13 Oct 2026: rdf12-join-2g, declared memory-limit, passed on v2.0 core's head.
        base = {"a": (100.0, 500, "ok"), "known": (10.0, 10, "gave-up")}
        result = self.run_gate({"a": (100.0, 500, "ok"), "known": (12.0, 10, "ok")}, base)
        self.assertTrue(result["passed"])
        self.assertEqual(result["fixed"], {"known": "gave-up"})
        self.assertIn("fixed: update the case's declared outcome", gate.render(result))

    def test_a_baseline_without_routes_is_compared_on_answers_and_times(self):
        # main at f9b515e8 predates the harness's --routes: its cases are off-route, their answers fine.
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp)
            cand = gate.pool([report(t, f"c{i}", "cand", {"a": (100.0, 500, "ok")}) for i in range(4)])
            base = gate.pool([report(t, f"b{i}", "main", {"a": (110.0, 500, "off-route")}) for i in range(4)])
            for r in base["records"]:
                r["checks"] = [{"path": "answer", "ok": True}]
                r["routes"] = [{"path": "operators", "ok": False}]
            self.assertIn("a", self.unchecked_gate(cand, base, False)["baselines"]["main"]["failures"])
            ok = self.unchecked_gate(cand, base, True)
            self.assertTrue(ok["baselines"]["main"]["cases"]["a"]["compared"])
            self.assertLess(ok["baselines"]["main"]["cases"]["a"]["time"]["ratio"], 1.0)

    def test_a_peak_measured_over_other_phases_is_not_compared(self):
        # 14 Oct 2026: 0f46563's perf_lab gave the classification no peak window of its own:
        # classify-store-horn 266-283 MiB against 3,390, while the cgroup peaks were 3,283-3,427.
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp)
            cand = gate.pool([report(t, f"c{i}", "cand", {"h": (1400.0, 3390, "ok")}) for i in range(4)])
            base = gate.pool([report(t, f"b{i}", "base", {"h": (1430.0, 280, "ok")}) for i in range(4)])
            for side, phases, cgroup in ((cand, {"load": 94, "classification": 3390}, 3425), (base, {"load": 94}, 3320)):
                for r in side["records"]:
                    r["phase_peaks"], r["cgroup_peak_mib"] = phases, cgroup
            result = release(cand, {"base": base})
            memory = result["baselines"]["base"]["cases"]["h"]["memory"]
            # Judged by the cgroup peak, the lifetime one shown beside, not comparable.
            self.assertEqual(memory["judged_by"], "cgroup")
            self.assertFalse(memory["grew"])
            self.assertAlmostEqual(memory["cgroup_ratio"], 3425 / 3320)
            self.assertFalse(memory["lifetime"]["comparable"])
            self.assertTrue(result["passed"])
            self.assertIn("lifetime peaks over other phases: baseline load, candidate classification, load",
                          gate.render(result))
            # Without cgroup peaks the lifetime peak decides: not across other phases...
            for side in (cand, base):
                for r in side["records"]:
                    r["cgroup_peak_mib"] = None
            alone = release(cand, {"base": base})["baselines"]["base"]["cases"]["h"]["memory"]
            self.assertEqual((alone["judged_by"], alone["comparable"], alone["grew"]), ("lifetime", False, False))
            # ... but over the same phases, where a growth fails the gate.
            for r in base["records"]:
                r["phase_peaks"] = {"load": 94, "classification": 280}
            self.assertTrue(release(cand, {"base": base})["baselines"]["base"]["cases"]["h"]["memory"]["grew"])

    def test_a_cgroup_growth_fails_the_gate(self):
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp)
            cand = gate.pool([report(t, f"c{i}", "cand", {"a": (100.0, 500, "ok")}) for i in range(4)])
            base = gate.pool([report(t, f"b{i}", "base", {"a": (100.0, 500, "ok")}) for i in range(4)])
            for side, cgroup in ((cand, 900), (base, 600)):
                for r in side["records"]:
                    r["cgroup_peak_mib"] = cgroup
            result = release(cand, {"base": base})
            self.assertTrue(result["baselines"]["base"]["cases"]["a"]["memory"]["grew"])
            self.assertFalse(result["passed"])

    def test_cases_not_run_are_said_and_keep_the_gate_open(self):
        # 14 Oct 2026: five SPARQL dataset cases had no data on Phuoc-Yu.
        base_cases = {"a": (100.0, 500, "ok"), "w": (10.0, 10, "failed")}
        result = self.run_gate({"a": (100.0, 500, "ok"), "w": (10.0, 10, "failed")}, base_cases)
        self.assertIn("w", result["candidate_failures"])
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp)
            cand = gate.pool([report(t, f"c{i}", "cand", {"a": (100.0, 500, "ok"), "w": (10.0, 10, "failed")})
                              for i in range(4)])
            base = gate.pool([report(t, f"b{i}", "base", base_cases) for i in range(4)])
            held = release(cand, {"base": base}, {"w": "no data on Phuoc"})
        self.assertEqual(held["candidate_failures"], {})
        self.assertEqual(held["not_run"], {"w": "no data on Phuoc"})
        self.assertFalse(held["passed"])
        self.assertEqual(held["baselines"]["base"]["cases"]["w"], {"compared": False, "not_run": "no data on Phuoc"})
        self.assertIn("- w: not run (no data on Phuoc)", gate.render(held))

    @staticmethod
    def unchecked_gate(cand, base, unchecked):
        return release(cand, {"main": gate.routes_unchecked(base) if unchecked else base})

    def test_a_failed_check_memory_growth_or_two_configurations_fail_it(self):
        base = {"a": (100.0, 500, "ok")}
        self.assertFalse(self.run_gate({"a": (100.0, 500, "wrong")}, base)["passed"])
        grown = self.run_gate({"a": (100.0, 700, "ok")}, base)
        self.assertTrue(grown["baselines"]["base"]["cases"]["a"]["memory"]["grew"])
        self.assertFalse(grown["passed"])
        self.assertFalse(self.run_gate({"a": (100.0, 500, "ok")}, base, host="Office-PC")["passed"])




if __name__ == "__main__":
    unittest.main()
