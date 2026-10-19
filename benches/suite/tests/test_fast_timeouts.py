"""Case wall limits reach every tool and both runtimes without executing benchmarks."""
import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "fast_timeout_contract", Path(__file__).resolve().parents[2] / "fast" / "fast.py")
fast = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fast)


class CaseTimeouts(unittest.TestCase):
    def measure(self, tool, timeout, runtime="docker", **extra):
        case = dict(name="timeout-contract", data="none", args=[], queries="", metric="load_s",
                    cap_gb=2, tool=tool, **extra)
        if timeout is not None:
            case["timeout_s"] = timeout
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)

            def measured(*args, **kwargs):
                (scratch / "timeout-contract.json").write_text(json.dumps({"load_s": 1}))
                return 0, "", "", 1

            with patch.object(fast, "SCRATCH", scratch), patch.object(fast, "RUNTIME", runtime), \
                    patch.object(fast, "native", side_effect=lambda text: text), \
                    patch.object(fast, "container", side_effect=measured) as run:
                record = fast.measure(case, "contract", 1, {}, {}, "none")
            return record, run.call_args_list

    def test_declared_default_and_unlimited_limits_reach_every_tool(self):
        for tool in ("perf_lab", "tableau", "classify-el", "classify-horn"):
            for declared, expected in ((1800, 1800), (None, 900), (0, None)):
                for runtime in ("docker", "native"):
                    with self.subTest(tool=tool, timeout=declared, runtime=runtime):
                        _, calls = self.measure(tool, declared, runtime=runtime)
                        self.assertEqual(len(calls), 1)
                        self.assertEqual(calls[0].kwargs["timeout"], expected)
                        if tool == "tableau":
                            command = calls[0].args[2]
                            self.assertEqual(command[command.index("--timeout") + 1],
                                             str(300 if declared is None else declared))

    def test_setup_repeats_and_sweeps_keep_coverage_and_the_declared_limit(self):
        _, calls = self.measure("perf_lab", 1800, setup=["--setup", "{scratch}"],
                                repeat=2, sweep=[["--runs", "1"], ["--runs", "2"]], env={"X": "1"})
        self.assertEqual(len(calls), 5)  # four measurements, then existing scratch cleanup
        for call in calls[:4]:
            self.assertEqual(call.kwargs["timeout"], 1800)
            self.assertEqual(call.args[3], {"X": "1"})
        self.assertIn("--setup", calls[0].args[2][-1])
        self.assertNotIn("--setup", calls[1].args[2][-1])
        self.assertNotIn("timeout", calls[-1].kwargs)


class RuntimeTimeouts(unittest.TestCase):
    def test_scope_cleanup_failure_cannot_pass_or_hide_a_primary_failure(self):
        primary = ValueError("original failure")
        for outcome, expected in ((0, 1), (7, 7), (subprocess.TimeoutExpired("example", 1), 124),
                                  (primary, None)):
            with self.subTest(outcome=outcome), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                with patch.object(fast, "SCRATCH", root), patch.object(fast, "FAST_DATA", root), \
                        patch.object(fast, "native_dirs", return_value={}), \
                        patch.object(fast, "native", side_effect=lambda text, dirs: text), \
                        patch.object(fast, "end_scope", side_effect=RuntimeError("owned scope remains")), \
                        patch.object(fast.subprocess, "run") as process:
                    if isinstance(outcome, BaseException):
                        process.side_effect = outcome
                    else:
                        process.return_value = SimpleNamespace(returncode=outcome, stdout="answer", stderr="")
                    if expected is None:
                        with self.assertRaises(ValueError) as raised:
                            fast.process("cleanup-contract", 2, ["example"], timeout=1)
                        self.assertIs(raised.exception, primary)
                    else:
                        code, _, error, _ = fast.process("cleanup-contract", 2, ["example"], timeout=1)
                        self.assertEqual(code, expected)
                        self.assertIn("owned scope remains", error)

    def test_limits_reach_docker_and_native_subprocesses(self):
        for runtime in ("docker", "native"):
            for timeout in (1800, None):
                with self.subTest(runtime=runtime, timeout=timeout), tempfile.TemporaryDirectory() as tmp:
                    root = Path(tmp)
                    done = SimpleNamespace(returncode=0, stdout="", stderr="")
                    with patch.object(fast, "RUNTIME", runtime), patch.object(fast, "SCRATCH", root), \
                            patch.object(fast, "FAST_DATA", root), patch.object(fast, "native_dirs", return_value={}), \
                            patch.object(fast, "native", side_effect=lambda text, dirs: text), \
                            patch.object(fast, "end_scope"), patch.object(fast, "docker", return_value=done) as docker, \
                            patch.object(fast.subprocess, "run", return_value=done) as process:
                        self.assertEqual(fast.container("contract", 2, ["example"], timeout=timeout)[0], 0)
                    run = docker if runtime == "docker" else process
                    self.assertEqual(run.call_args.kwargs["timeout"], timeout)

    def test_timeout_status_and_cleanup_survive_forwarding(self):
        for runtime in ("docker", "native"):
            with self.subTest(runtime=runtime), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                with patch.object(fast, "RUNTIME", runtime), patch.object(fast, "SCRATCH", root), \
                        patch.object(fast, "FAST_DATA", root), patch.object(fast, "native_dirs", return_value={}), \
                        patch.object(fast, "native", side_effect=lambda text, dirs: text), \
                        patch.object(fast, "end_scope") as end, patch.object(fast, "pinned", side_effect=lambda tag: tag), \
                        patch.object(fast, "docker", side_effect=[subprocess.TimeoutExpired("example", 1800), None]) as docker, \
                        patch.object(fast.subprocess, "run", side_effect=subprocess.TimeoutExpired("example", 1800)):
                    code, _, err, _ = fast.container("contract", 2, ["example"], timeout=1800)
                self.assertEqual(fast.classify_failure(code, err, {}), "timeout")
                self.assertIn("1800", err)
                if runtime == "native":
                    end.assert_called_once()
                else:
                    self.assertEqual(docker.call_args.args[0], ["rm", "-f", fast.ns.name("fast-contract", fast.ROOT)])


if __name__ == "__main__":
    unittest.main()
