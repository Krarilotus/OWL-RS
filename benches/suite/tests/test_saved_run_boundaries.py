"""A saved run's privacy and the directory that owns its manifest."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import isolation

sys.path.insert(0, str(isolation.ROOT / "benches/suite"))
from suitekit import hosts, manifest, schema  # noqa: E402


def contents(paths):
    return {path: path.read_bytes() if path.exists() else None for path in paths}


class SavedRunBoundaries(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory(dir=isolation.OWN))).resolve()
        self.public = isolation.checkout(self.root / "public", "benches/suite/suitekit/hosts.py")
        self.env = isolation.inherit("NRESE_", "BENCH_INPUTS_", "FLEET_")
        self.enterContext(patch.dict(os.environ, self.env, clear=True))

    def link(self, path, target, kind="symbolic"):
        try:
            if kind == "symbolic":
                path.symlink_to(target)
            else:
                os.link(target, path)
        except OSError as error:
            if getattr(error, "winerror", None) != 1314:
                raise
            self.skipTest("this Windows account cannot create symbolic links")

    def test_manifest_and_journal_stay_in_the_locked_directory(self):
        # Spec: a private invocation may publish only inside its own locked result directory.
        overlay = self.root / "overlay"
        overlay.mkdir()
        (overlay / "hosts.toml").write_text(isolation.OVERLAY_HOSTS, encoding="utf-8")
        os.environ["NRESE_BENCH_PRIVATE"] = str(overlay)
        args = SimpleNamespace(runs=1, query_runs=1, cache="off", order="fixed", seed=0,
                               timeout_s=30, query_timeout_s=30)

        def publish(path):
            manifest.Manifest(path, self.public, [], args, "process", {}).finish(0)

        for name, kind in (("manifest.json", "symbolic"), ("manifest.json.tmp", "symbolic"),
                           (manifest.ATTEMPTS, "symbolic"), (manifest.ATTEMPTS, "hard")):
            with self.subTest(name=name, kind=kind):
                a = self.root / f"a-{name}-{kind}"
                b = self.root / f"b-{name}-{kind}"
                a.mkdir()
                b.mkdir()
                publish(b)
                target = "manifest.json" if name == "manifest.json.tmp" else name
                self.link(a / name, b / target, kind)
                before = contents((b / "manifest.json", b / manifest.ATTEMPTS, a / manifest.ATTEMPTS))
                refused = False
                try:
                    publish(a)
                except (ValueError, manifest.ManifestError):
                    refused = True
                self.assertEqual(contents(before), before)
                self.assertTrue(refused)

        normal = self.root / "normal"
        normal.mkdir()
        publish(normal)
        entry, = json.loads((normal / "manifest.json").read_text(encoding="utf-8"))["invocations"]
        journal_entry, = manifest.read_journal(normal)
        self.assertEqual(entry["exit"], 0)
        self.assertEqual(entry["invocation"], journal_entry["invocation"])

    def era_results(self, label, provenance):
        out = self.root / label
        reports = out / "reports"
        reports.mkdir(parents=True)
        if provenance:
            machine = {"host": "sample-host", "overlay": hosts.content_id(isolation.OVERLAY_HOSTS, "")}
            (out / "manifest.json").write_text(json.dumps({"invocations": [{"machine": machine}]}), encoding="utf-8")
        writer = schema.Writer(out / "results.csv")
        for engine in ("nrese", "topbraid"):
            writer.write(schema.Result("2026-01-01T00:00", "sample-host", "docker", engine, "-",
                                       "free", "era-shacl", "ES", "none", 1, "validate", item="core"))
            report = reports / f"{engine}-ES-core-r1"
            triples = (
                "<urn:result> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type>"
                " <http://www.w3.org/ns/shacl#ValidationResult> .\n"
                f"<urn:result> <http://www.w3.org/ns/shacl#focusNode> <urn:{engine}> .\n"
            )
            report.with_suffix(".ttl").write_text(triples, encoding="utf-8")
            report.with_suffix(".nt").write_text(triples, encoding="utf-8")
        return out

    def test_era_check_uses_saved_privacy_before_copying_or_rewriting(self):
        # Spec: a saved private or unclassified ERA run cannot publish either CSV into the checkout.
        runner = isolation.ROOT / "benches/breadth/era-shacl/run.py"
        for target, provenance in (("results.raw.csv", True), ("results.raw.csv", False),
                                   ("results.csv", True), (None, True)):
            with self.subTest(target=target, provenance=provenance):
                out = self.era_results(f"{target}-{provenance}", provenance)
                results, raw = out / "results.csv", out / "results.raw.csv"
                source = results.read_bytes()
                if target:
                    public_file = self.public / f"{target}-{provenance}"
                    public_file.write_bytes(source if target == "results.csv" else b"public fixture\n")
                    (out / target).unlink(missing_ok=True)
                    self.link(out / target, public_file)
                    if target == "results.csv":
                        raw.write_bytes(b"previous raw copy\n")
                    before = contents((public_file, raw, results))
                done = subprocess.run([sys.executable, "-I", str(runner), "check", str(out)],
                                      env=self.env, capture_output=True, text=True, timeout=30)
                if target:
                    self.assertEqual(contents(before), before, done.stderr)
                    self.assertNotEqual(done.returncode, 0)
                else:
                    self.assertEqual(done.returncode, 0, done.stderr)
                    self.assertEqual(raw.read_bytes(), source)
                    self.assertEqual([(row["host"], row["status"]) for row in schema.read(results)],
                                     [("sample-host", "wrong"), ("sample-host", "ok")])

    def era_run(self, out):
        """ERA's real `run` into `out` from this (overlay-free, public) shell, every container stubbed."""
        spec = importlib.util.spec_from_file_location("era_shacl_run", isolation.ROOT / "benches/breadth/era-shacl/run.py")
        era = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(era)

        class Runtime:
            def __init__(self, *args, **kwargs):
                pass

            def pin(self, images):
                pass

            def run(self, spec, log, timeout_s):
                log.write_text("Load time: 1.0\nValidation time: 2.0\nData graph size: 5\n", encoding="utf-8")
                return SimpleNamespace(timed_out=False, ok=True, rc=0, ms=3000.0, peak_mib=10,
                                       memory_kind="container", memory_note="")

            def cleanup(self):
                pass

        with patch.object(era.kit, "DockerRuntime", Runtime), patch.object(era.kit, "wait_until_free", lambda: None), \
                patch.object(era.kit, "retreated_since", lambda started: False), \
                patch.object(manifest, "container_start_ms", lambda root: None), \
                patch.object(manifest, "image_ids", lambda images: {}):
            return era.main(["run", "--results", str(out), "--subsets", "ES", "--shapes", "core",
                             "--engines", "nrese", "--runs", "1"])

    def test_era_run_never_saves_earlier_rows_under_another_privacy_class(self):
        # Spec: a reused ERA results directory keeps every invocation whose rows it holds; a run of
        # another privacy class than those rows, or onto rows of unknown provenance, is refused before
        # anything changes, so a later `check` still treats the private rows as private.
        for provenance in (True, False):
            with self.subTest(saved="private" if provenance else "unknown"):
                out = self.era_results(f"reused-{provenance}", provenance)
                public_raw = self.public / f"reused-raw-{provenance}"
                public_raw.write_bytes(b"public fixture\n")
                self.link(out / "results.raw.csv", public_raw)
                watched = (out / "manifest.json", out / "results.csv", public_raw)
                before = contents(watched)
                try:
                    status = self.era_run(out)
                except Exception as error:  # the old code fails later, at its first container
                    status = f"raised {error!r}"
                self.assertEqual(contents(before), before)
                self.assertEqual(status, 2)
                runner = isolation.ROOT / "benches/breadth/era-shacl/run.py"
                done = subprocess.run([sys.executable, "-I", str(runner), "check", str(out)],
                                      env=self.env, capture_output=True, text=True, timeout=30)
                self.assertEqual(contents(before), before, done.stderr)
                self.assertNotEqual(done.returncode, 0)

        # A fresh directory, and the same directory reused by a run of the same (public) class.
        fresh = self.root / "fresh"
        self.assertEqual(self.era_run(fresh), 0)
        self.assertEqual(self.era_run(fresh), 0)
        invocations = json.loads((fresh / "manifest.json").read_text(encoding="utf-8"))["invocations"]
        self.assertEqual(len(invocations), 2)
        self.assertEqual([row["task"] for row in schema.read(fresh / "results.csv")],
                         ["load", "validate", "load", "validate"])
