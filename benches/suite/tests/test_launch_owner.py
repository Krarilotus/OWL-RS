"""The suite's one container launcher (suitekit/ns.py run_args) in a fleet job that declares its containers.

Spec: fleet Spec 9 (fleet-scheduler docs/spec.md; declared.py check(), from fb46ffb): a container made during a
declared job's step without the exact `fleet.job=<id>` label is unknown and stops the step; one with it needs a
declared container of its own, matched by its memory (within two pages). The master, 8 Nov 2026 (#132 r4 B1;
notes/2026-11-08-suite-fleet-launch-owner.md): every container the suite starts goes through one owner that gives it
both, and nothing else builds a `docker run`. Docker never runs here: every command is recorded, none executed."""
from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches" / "suite"))
from suitekit import adapters, manifest, ns  # noqa: E402
from suitekit.runtime import DockerRuntime, Mount, Spec  # noqa: E402

# A POSIX bash: Git's on Windows, not WSL's launcher (as test_job_namespace).
BASH = next((b for b in (["C:/Program Files/Git/bin/bash.exe"] if os.name == "nt" else []) if Path(b).is_file()),
            shutil.which("bash"))
JOB = "Consol-SCC@office"  # case and punctuation kept: the watcher reads the label's exact value
DECLARED = {"FLEET_JOB": JOB, "NRESE_SUITE_MEMORY_GB": "1.5"}  # a fraction: Docker's `1.5g` isn't exact bytes
BYTES = f"{3 * 2**29}b"  # 1.5 GiB


class Docker:
    """Every command run, recorded and none executed; docker answers as its callers read it."""

    def __init__(self):
        self.calls = []

    def __call__(self, argv, *args, **kwargs):
        self.calls.append([str(a) for a in argv])
        return subprocess.CompletedProcess(argv, 0, "1234\t/v\n" if "du" in argv else "", "")

    def runs(self):
        return [c for c in self.calls if c[1:2] == ["run"]]


class LaunchOwner(unittest.TestCase):
    def owned(self, argv):
        """The job's label, exactly once, and the declared memory without swap."""
        pairs = list(zip(argv, argv[1:]))
        self.assertEqual([v for k, v in pairs if k == "--label" and v.startswith("fleet.job=")], [f"fleet.job={JOB}"], argv)
        self.assertEqual([v for k, v in pairs if k in ("--memory", "--memory-swap")], [BYTES, BYTES], argv)

    def test_the_calibration_containers_are_the_jobs_with_its_memory(self):
        docker = Docker()
        with patch.dict(os.environ, DECLARED), patch("subprocess.run", docker), \
                patch.object(ns, "pin_images"), patch.object(ns, "image_id", return_value="sha256:cal"):
            self.assertIsNotNone(manifest.machine("docker", ROOT)["container_start_ms"])
        self.assertEqual(len(docker.runs()), 3)
        for argv in docker.runs():
            self.owned(argv)

    def test_the_store_size_helper_is_the_jobs_and_never_beside_a_live_container(self):
        # One live container per declaration (declared.py check()): the helper runs between a load and a serve.
        docker = Docker()
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ, DECLARED), patch("subprocess.run", docker):
            runtime = DockerRuntime(Path(tmp), 0)
            self.assertEqual(runtime.store_bytes("nrese-store-x"), 1234)
            self.owned(docker.runs()[0])
            runtime.containers["serving"] = object()
            with self.assertRaises(RuntimeError):
                runtime.store_bytes("nrese-store-x")
        self.assertEqual(len(docker.runs()), 1)

    def test_a_measured_container_takes_the_declaration_and_another_memory_is_refused(self):
        docker = Docker()
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ, DECLARED), patch("subprocess.run", docker), \
                patch.object(DockerRuntime, "names", return_value=[]), patch.object(DockerRuntime, "sample"):
            runtime = DockerRuntime(Path(tmp), 0)
            runtime.start(Spec("serve", "sha256:fixture", ["true"]), None)  # an adapter without memory= (Anzograph's)
            self.owned(docker.runs()[0])
            with self.assertRaises(ValueError):  # the watcher would stop the step at it: never started
                runtime.start(Spec("other", "sha256:fixture", ["true"], memory="2g"), None)
            self.assertNotIn("other", runtime.containers)
        self.assertEqual(len(docker.runs()), 1)

    def test_the_declared_memory_is_not_held_to_the_steps_slice(self):
        # #132 B2: the fleet takes declared memory out of the slice (job.rs memory_cap): 28 GB declared, ~2.8 GB slice.
        ctx = adapters.Context(SimpleNamespace(kind="docker", dry=False), ROOT, ROOT, ROOT, Mount("-", "/data"), [], 0,
                               {}, "x")
        with patch.dict(os.environ, {"FLEET_JOB": JOB, "NRESE_SUITE_MEMORY_GB": "28"}), \
                patch.object(adapters, "slice_limit_mib", return_value=2867), \
                patch.object(adapters, "docker_memory_bytes", return_value=64 * 2**30):
            self.assertEqual(ctx.memory, f"{28 * 2**30}b")

    def test_outside_a_declared_job_launches_keep_their_own_memory(self):
        # A fleet job without a declaration (abba's, a queue's): labelled, its own memory; a declaration's memory
        # outside a fleet job: ignored.
        for env, label in (({"FLEET_JOB": JOB}, True), ({"NRESE_SUITE_MEMORY_GB": "1.5"}, False), ({}, False)):
            with self.subTest(env=env):
                argv = ns.run_args(ROOT, "suite", "x", "4g", environ=env)
                self.assertEqual(argv[argv.index("--memory"):argv.index("--memory") + 4],
                                 ["--memory", "4g", "--memory-swap", "4g"])
                self.assertEqual(f"fleet.job={JOB}" in argv, label)
        for value in ("lots", "0", "-2", "nan"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                ns.run_args(ROOT, "suite", environ={"FLEET_JOB": JOB, "NRESE_SUITE_MEMORY_GB": value})

    @unittest.skipUnless(BASH, "needs bash")
    def test_a_makers_container_carries_the_jobs_label_once(self):
        # ns_labels gives the label (#132 r3); ns_maker_args added its own beside it: one owner of it.
        done = subprocess.run([BASH, "-c", f'. "{(ROOT / "scripts/lib/ns.sh").as_posix()}"; '
                               'ns_maker_args - "$1" sparql; printf "%s\\n" "${NS_ARGS[@]}"', "-", ROOT.as_posix()],
                              env={**os.environ, **DECLARED, "NRESE_MAKER_MEMORY_GB": "8", "NRESE_MAKER_CPUS": "8"},
                              capture_output=True, text=True, timeout=60)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(done.stdout.splitlines().count(f"fleet.job={JOB}"), 1, done.stdout)

    def test_no_container_start_is_built_outside_the_launch_owner(self):
        """A `docker run`/`create` argv spelled out anywhere in the suite's code but ns.py bypasses the owner. Seen:
        the literal forms (the docker executable or ns.executable() followed by "run"/"create", ns.docker(["run" ...).
        Not seen: an argv assembled from variables; the shell makers have their own owner (release_gate held_makers)."""
        launch = re.compile(r'''(["']docker["']|executable\(\))\s*,\s*["'](run|create)["']|'''
                            r'''\.docker\(\s*\[\s*["'](run|create)["']''')
        suite = ROOT / "benches" / "suite"
        found = [f"{path.relative_to(ROOT).as_posix()}:{n}"
                 for path in sorted(suite.rglob("*.py")) if "tests" not in path.relative_to(suite).parts
                 and path.name != "ns.py"
                 for n, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1) if launch.search(line)]
        self.assertEqual(found, [])


if __name__ == "__main__":
    unittest.main()
