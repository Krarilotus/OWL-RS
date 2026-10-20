"""Local scope-contract fixtures: mock systemd, never launch native workloads."""
import json
import ast
import os
from pathlib import Path
import runpy
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from suitekit import runtime


class ScopeLifetime(unittest.TestCase):
    def test_unbounded_existing_prefix_is_unchanged(self):
        self.assertEqual(runtime.scope_argv("case-1", "6g", {"FLEET_SLICE": "fleet-example.slice"}),
            ["systemd-run", "--user", "--scope", "-q", "--slice=fleet-example.slice", "--unit=case-1",
             "-p", "MemoryMax=6G", "-p", "MemorySwapMax=0", "-p", "OOMPolicy=continue", "--"])

    def test_rules_interval_reserves_shutdown_without_cutting_engine_allowance(self):
        argv = runtime.scope_argv("rules-block-1", "14g", {"FLEET_SLICE": "fleet-example.slice"},
                                  interval_s=75 * 60, stop_s=30)
        self.assertIn("RuntimeMaxSec=4380", argv)
        self.assertIn("TimeoutStopSec=30", argv)
        self.assertIn("KillMode=control-group", argv)
        self.assertIn("SendSIGKILL=yes", argv)
        self.assertEqual(argv[-1], "--")

    def test_worker_binds_to_exact_parent_with_its_own_stop_allowance(self):
        argv = runtime.scope_argv("worker-2", "12g", {"FLEET_SLICE": "fleet-example.slice",
            "SUITE_SCOPE_PARENT": "dictionary-block-1", "SUITE_SCOPE_STOP_S": "7"})
        for value in ("BindsTo=dictionary-block-1.scope", "After=dictionary-block-1.scope",
                      "TimeoutStopSec=7", "MemoryMax=12G", "MemorySwapMax=0"):
            self.assertIn(value, argv)
        self.assertFalse(any("RuntimeMaxSec=" in value for value in argv), "worker must not renew the block deadline")

    def test_bad_or_recursive_settings_fail_before_launch(self):
        for interval, stop in ((120, 30), (0, 1), (float("inf"), 1), (float("nan"), 1),
                               (300, 0), (300, -1), (300, float("inf"))):
            with self.subTest(interval=interval, stop=stop), self.assertRaises(ValueError):
                runtime.scope_argv("outer", "1g", {}, interval_s=interval, stop_s=stop)
        for parent in ("worker", "../other", "other.scope/escape"):
            with self.subTest(parent=parent), self.assertRaises(ValueError):
                runtime.scope_argv("worker", "1g", {"SUITE_SCOPE_PARENT": parent})
        with self.assertRaises(ValueError):
            runtime.scope_argv("other", "1g", {"SUITE_SCOPE_PARENT": "outer"}, interval_s=300)

    def test_entry_prefixes_whole_existing_launcher_and_preserves_inherited_lease(self):
        descriptor = os.open(__file__, os.O_RDONLY)
        self.addCleanup(os.close, descriptor)
        os.set_inheritable(descriptor, True)
        class Executed(BaseException):
            pass
        captured = {}
        def execute(program, argv, environ):
            captured.update(program=program, argv=argv, environ=dict(environ))
            self.assertTrue(os.get_inheritable(descriptor), "entry dropped an existing inheritable lease")
            raise Executed()
        with patch.dict(os.environ, {"FLEET_SLICE": "fleet-example.slice"}, clear=True), \
                patch.object(sys, "argv", [str(runtime.__file__), "rules-block-1", "14g", "4500", "30",
                                          "bash", "existing-launch.sh", "block", "1"]), \
                patch.object(os, "execvpe", side_effect=execute):
            with self.assertRaises(Executed):
                runpy.run_path(runtime.__file__, run_name="__main__")
        self.assertEqual(captured["argv"][-4:], ["bash", "existing-launch.sh", "block", "1"])
        self.assertEqual(captured["environ"]["SUITE_SCOPE_PARENT"], "rules-block-1")
        self.assertEqual(captured["environ"]["SUITE_SCOPE_STOP_S"], "30")

    def test_entry_requires_fleet_and_refuses_nested_deadline(self):
        for environ in ({}, {"FLEET_SLICE": "fleet-example.slice", "SUITE_SCOPE_PARENT": "old"}):
            with patch.dict(os.environ, environ, clear=True), \
                    patch.object(sys, "argv", [str(runtime.__file__), "outer", "1g", "300", "2", "sh"]), \
                    patch.object(os, "execvpe") as execute:
                with self.assertRaises(ValueError):
                    runpy.run_path(runtime.__file__, run_name="__main__")
                execute.assert_not_called()

    def test_original_fast_process_forwards_only_explicit_leases_and_keeps_900_seconds(self):
        source = Path(__file__).resolve().parents[2] / "fast/fast.py"
        tree = ast.parse(source.read_text())
        function = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "process")
        namespace = {"CASE_TIMEOUT_S": 900}
        exec("from __future__ import annotations\n" + ast.unparse(function), namespace)
        with tempfile.TemporaryDirectory() as temporary:
            completed = types.SimpleNamespace(returncode=0, stdout="", stderr="")
            import re
            namespace.update(native_dirs=lambda: {}, FAST_DATA=Path(temporary), os=os, time=__import__("time"),
                scope_argv=lambda *_: ["scope", "--"], native=lambda command, _: command,
                end_scope=lambda *_: None, SCOPE_PEAK=re.compile("never-match"), sys=sys, ENV={},
                SCOPE_PRESSURE=re.compile("never-match"), random=__import__("random"),
                AnonKernelSampler=lambda where: types.SimpleNamespace(start=lambda: None, stop=lambda: None),
                pinned_cpus=lambda: None,
                subprocess=types.SimpleNamespace(run=lambda *a, **kw: completed,
                    TimeoutExpired=TimeoutError, CompletedProcess=types.SimpleNamespace))
            observed = []
            namespace["subprocess"].run = lambda *a, **kw: observed.append(kw) or completed
            for descriptors in ("", "9 14"):
                with patch.dict(os.environ, {"SUITE_LEASE_FDS": descriptors}):
                    self.assertEqual(namespace["process"]("case", 6, ["original-command"])[0], 0)
                self.assertEqual(observed[-1]["pass_fds"], tuple(map(int, descriptors.split())))
                self.assertEqual(observed[-1]["timeout"], 900)
                # Every process an environment pad of its own size (its initial stack moves).
                self.assertLess(len(observed[-1]["env"]["NRESE_LAYOUT_PAD"]), 4096)
            # Pinned (hosts.toml `cpus`): the command, inside its scope, on those CPUs alone.
            argv = []
            namespace["subprocess"].run = lambda *a, **kw: argv.append(a[0]) or completed
            namespace["pinned_cpus"] = lambda: "8-15,24-31"
            namespace["process"]("case", 6, ["original-command"])
            self.assertEqual(argv[-1][:5], ["scope", "--", "taskset", "-c", "8-15,24-31"])

    def test_populated_owned_cgroup_prevents_success(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            group = base / "owned"
            group.mkdir()
            (group / "cgroup.events").write_text("populated 1\nfrozen 0\n")
            calls = []
            def systemctl(command, timeout):
                calls.append(command)
                stdout = "/owned\n" if "ControlGroup" in command else "LoadState=loaded\nActiveState=failed\n"
                return types.SimpleNamespace(returncode=0, stdout=stdout, stderr="")
            with patch.object(runtime, "run_quiet", side_effect=systemctl), \
                    patch.object(runtime, "Path", return_value=base):
                for _ in range(2):
                    with self.assertRaisesRegex(RuntimeError, "still populated"):
                        runtime.end_scope("owned")
                (group / "cgroup.events").write_text("populated 0\nfrozen 0\n")
                runtime.end_scope("owned")
            self.assertTrue(all(command[-1] == "owned.scope" for command in calls))

    def test_release_refuses_missing_events_or_unverified_ownership(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            (base / "owned").mkdir()
            reply = lambda argv, _: types.SimpleNamespace(returncode=0,
                stdout="/owned\n" if "ControlGroup" in argv else "LoadState=loaded\nActiveState=inactive\n", stderr="")
            with patch.object(runtime, "run_quiet", side_effect=reply), patch.object(runtime, "Path", return_value=base):
                with self.assertRaises(FileNotFoundError):
                    runtime.end_scope("owned")
        for group, code in (("/../unowned", 0), ("relative", 0), ("", 1)):
            with patch.object(runtime, "run_quiet", return_value=types.SimpleNamespace(
                    returncode=code, stdout=group, stderr="probe failed")) as call:
                with self.assertRaisesRegex(RuntimeError, "ownership unverified"):
                    runtime.end_scope("owned")
                self.assertEqual(call.call_count, 1)

    def test_unrelated_scope_is_never_stopped(self):
        replies = [types.SimpleNamespace(returncode=0, stdout="", stderr=""),
                   types.SimpleNamespace(returncode=1, stdout="", stderr="gone"),
                   types.SimpleNamespace(returncode=0, stdout="LoadState=not-found\nActiveState=inactive\n", stderr="")]
        with patch.object(runtime, "run_quiet", side_effect=replies) as run:
            runtime.end_scope("exact-owned")
        self.assertTrue(all(call.args[0][-1] == "exact-owned.scope" for call in run.call_args_list))


if __name__ == "__main__":
    unittest.main()
