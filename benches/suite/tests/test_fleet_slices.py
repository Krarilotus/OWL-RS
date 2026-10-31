"""The kits under fleet (the owner, 14 Oct 2026, after a whole-host OOM on Office: a scope and a
container beside the jobs, counted nowhere): a scope the suite or the fast suite starts goes into
the step's slice (FLEET_SLICE) and is ended with its task, nothing in it outliving the command;
a container is capped within what the job declared (the slice's limit), never uncapped.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches/suite"))
from suitekit import adapters, runtime  # noqa: E402
from suitekit.runtime import Mount  # noqa: E402


class Scopes(unittest.TestCase):
    def test_host_binaries_use_the_contenders_cap(self):
        with tempfile.TemporaryDirectory() as tmp:
            rt = runtime.RootfsRuntime(Path(tmp), 18950, Path(tmp))
            capped = rt.argv(runtime.Spec("nrese", None, ["/bin/nrese-server", "serve"], memory="26g"))
            self.assertEqual(capped[:4], ["systemd-run", "--user", "--scope", "-q"])
            self.assertIn("MemoryMax=26G", capped)
            self.assertEqual(capped[-2:], ["/bin/nrese-server", "serve"])
            self.assertIn("nrese", rt.units)
            self.assertEqual(rt.argv(runtime.Spec("tool", None, ["/bin/tool"])), ["/bin/tool"])

    def test_nothing_outlives_a_scoped_run(self):
        with tempfile.TemporaryDirectory() as tmp:
            rt = runtime.RootfsRuntime(Path(tmp), 18950, Path(tmp))
            spec = runtime.Spec("nrese", None, ["/bin/true"], memory="1g")
            ended = []
            with patch.object(runtime, "end_scope", ended.append), \
                    patch.object(runtime.RootfsRuntime, "popen", lambda self, argv, spec, log: Mock(pid=123)), \
                    patch.object(runtime.RootfsRuntime, "kill"), \
                    patch.object(runtime.os, "killpg", side_effect=ProcessLookupError, create=True), \
                    patch.object(runtime, "reap", lambda process, timeout: (0, None)), \
                    patch.object(runtime.RootfsRuntime, "peak", lambda self, spec: None):
                rt.run(spec, Path(tmp) / "log", 10)
            self.assertEqual(len(ended), 1)
            self.assertTrue(ended[0].startswith("nrese-nrese-"))
            self.assertFalse(rt.cleanup_errors)
            self.assertNotIn("nrese", rt.units)


class Containers(unittest.TestCase):
    def context(self, settings):
        return adapters.Context(None, ROOT, ROOT, ROOT, Mount("-", "/data"), [], 3600, settings, "t")

    def test_a_container_is_capped_within_the_declared_memory(self):
        with patch.object(adapters, "docker_memory_bytes", lambda: 30 * 2**30):
            with patch.object(adapters, "slice_limit_mib", lambda: 17_600):
                self.assertEqual(self.context({}).memory, "16576m")
                self.assertEqual(self.context({"DOCKER_MEMORY": "0"}).memory, "16576m")
                self.assertEqual(self.context({"DOCKER_MEMORY": "12g"}).memory, "12g")
            with patch.object(adapters, "slice_limit_mib", lambda: None):
                self.assertEqual(self.context({}).memory, f"{30 * 1024 * 9 // 10}m")
                self.assertIsNone(self.context({"DOCKER_MEMORY": "0"}).memory)
                self.assertEqual(self.context({"DOCKER_MEMORY": "40g"}).memory, "40g")

    def test_an_explicit_cap_never_passes_the_fleet_slice(self):
        """Defect 5 (audit, 18 Oct 2026): an explicit DOCKER_MEMORY bypassed the slice's ceiling.
        A container Docker can't put in the slice is counted nowhere: one past the ceiling is
        refused, not shrunk (PROTOCOL.md §3: no silent cap)."""
        with patch.object(adapters, "docker_memory_bytes", lambda: 30 * 2**30):
            with patch.object(adapters, "slice_limit_mib", lambda: 17_600):
                for setting in ("40g", "16577m", f"{16_577 * 2**20}", "20000M"):
                    with self.assertRaisesRegex(ValueError, "slice"):
                        self.context({"DOCKER_MEMORY": setting}).memory
                self.assertEqual(self.context({"DOCKER_MEMORY": "16576m"}).memory, "16576m")
                self.assertEqual(self.context({"DOCKER_MEMORY": "16g"}).memory, "16g")
                with self.assertRaisesRegex(ValueError, "DOCKER_MEMORY"):
                    self.context({"DOCKER_MEMORY": "lots"}).memory


if __name__ == "__main__":
    unittest.main()
