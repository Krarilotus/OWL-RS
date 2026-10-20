"""Owned benchmark cleanup: no engines, Docker daemon, systemd or remote jobs required."""
from __future__ import annotations

import signal
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from suitekit import driver, runtime  # noqa: E402
from suitekit.adapters import Endpoint  # noqa: E402
from suitekit.workloads import Plan  # noqa: E402


def done(rc=0, out="", err=""):
    return subprocess.CompletedProcess(["fake"], rc, out, err)


class Cleanup(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.scratch = self.root / "scratch"
        self.rt = runtime.ProcessRuntime(self.scratch, 18950, echo=Mock())

    def suite(self, dry=False):
        args = driver.arguments(["--runtime", "process", "--data", str(self.root),
                                 "--results", str(self.root / "results"),
                                 *(["--dry-run"] if dry else [])])
        suite = driver.Suite(args, {}, {}, self.root)
        self.rt = suite.runtime
        self.rt.echo = Mock()
        suite.say, suite.emit, suite.writes = Mock(), Mock(), Mock()
        suite.publish = lambda key: "fixture"
        return suite

    def test_failed_deletion_retains_store_and_attempts_the_next(self):
        blocked, other = self.rt.new_store("blocked"), self.rt.new_store("other")
        original = runtime.shutil.rmtree

        def remove(path, *args, **kwargs):
            if str(path) == blocked:
                raise PermissionError("blocked")
            return original(path, *args, **kwargs)

        with patch.object(runtime.shutil, "rmtree", side_effect=remove):
            self.rt.cleanup()
        self.assertEqual(self.rt.created_stores, [blocked])
        self.assertTrue(Path(blocked).exists())
        self.assertFalse(Path(other).exists())
        self.rt.cleanup()
        self.rt.cleanup()
        self.assertEqual(self.rt.created_stores, [])

    def test_run_collision_cannot_take_ownership_of_existing_scratch(self):
        now = driver.datetime.datetime.now()
        with patch.object(driver.datetime, "datetime") as clock:
            clock.now.return_value = now
            suite = self.suite(dry=True)
            sentinel = suite.results / "sentinel"
            sentinel.write_text("belongs to first run")
            with self.assertRaises(FileExistsError):
                self.suite(dry=True)
            self.assertEqual(sentinel.read_text(), "belongs to first run")
            suite.cleanup()
        self.assertFalse(suite.scratch.exists())

    def test_remaining_worker_prevents_another_system_or_repetition(self):
        for retain in (False, True):
            with self.subTest(retain=retain):
                suite = self.suite()
                suite.systems = {"first": {}, "second": {}}
                suite.workloads = {"fixture": {}}
                suite.args.systems, suite.args.runs = "first,second", 2
                system = Mock(regimes=["none"])
                system.supports.return_value = None
                system.images.return_value, system.builds.return_value = [], []

                def cycle(*args):
                    if retain:
                        self.rt.processes["retained"] = runtime.Server(runtime.Spec("retained", None, []))

                with patch.object(driver, "plans", return_value=[Plan("fixture", "small")]), \
                        patch.object(driver, "adapter", return_value=system), \
                        patch.object(suite, "build_harness", return_value=True), \
                        patch.object(suite, "ensure_data", return_value=None), \
                        patch.object(suite, "cycle", side_effect=cycle) as ran:
                    self.assertEqual(suite.run(), 1 if retain else 0)
                    self.assertEqual(ran.call_count, 1 if retain else 4)
                self.assertEqual(bool(self.rt.processes), retain)
                self.rt.processes.clear()  # No real process was started.
                suite.cleanup()

    def test_collision_and_unowned_paths_are_never_deleted(self):
        shared = self.root / "shared"
        shared.mkdir()
        (shared / "sentinel").write_text("shared")
        owned = self.rt.new_store("owned")
        for name in ("owned", "../shared", str(shared), "."):
            with self.subTest(name=name), self.assertRaises((ValueError, FileExistsError)):
                self.rt.new_store(name)
        with self.assertRaises(RuntimeError):
            self.rt.remove_store(str(shared))
        self.assertTrue(Path(owned).exists())
        self.assertEqual((shared / "sentinel").read_text(), "shared")

    def test_symlink_escape_is_retained(self):
        source = Path(self.rt.new_store("owned"))
        shared = self.root / "shared"
        shared.mkdir()
        source.rmdir()
        try:
            source.symlink_to(shared, target_is_directory=True)
        except OSError:
            self.skipTest("symlink creation unavailable")
        with self.assertRaises(ValueError):
            self.rt.remove_store(str(source))
        self.assertEqual(self.rt.created_stores, [str(source)])
        self.assertTrue(shared.is_dir())
        source.unlink()

    def test_link_validation_is_enforced_without_symlink_privileges(self):
        source = self.rt.new_store("owned")
        with patch.object(Path, "is_symlink", return_value=True):
            with self.assertRaises(ValueError):
                self.rt.remove_store(source)
        with patch.object(Path, "resolve", return_value=self.root / "outside"):
            with self.assertRaises(ValueError):
                self.rt.remove_store(source)
        self.assertTrue(Path(source).exists())
        self.assertEqual(self.rt.created_stores, [source])

    def test_cleanup_waits_for_workers_and_retains_failed_owners(self):
        source = self.rt.new_store("store")
        servers = [runtime.Server(runtime.Spec(name, None, []), Mock()) for name in ("bad", "good")]
        self.rt.processes.update((s.spec.name, s) for s in servers)

        def stop(server):
            if server.spec.name == "bad":
                raise PermissionError("cannot stop bad")
            self.rt.processes.pop(server.spec.name)

        with patch.object(self.rt, "stop", side_effect=stop) as stopped:
            self.rt.cleanup()
        self.assertEqual(stopped.call_count, 2)
        self.assertEqual(list(self.rt.processes), ["bad"])
        self.assertTrue(Path(source).exists())
        self.assertEqual(self.rt.created_stores, [source])

    def test_process_is_not_forgotten_when_reap_fails(self):
        server = runtime.Server(runtime.Spec("loader", None, []), Mock())
        self.rt.processes["loader"] = server
        with patch.object(self.rt, "kill"), patch.object(runtime, "reap", return_value=(None, None)):
            with self.assertRaises(RuntimeError):
                self.rt.stop(server)
        self.assertIn("loader", self.rt.processes)

    def test_success_timing_excludes_teardown_but_timeout_waits_for_it(self):
        for rc in (0, None):
            with self.subTest(rc=rc):
                clock = [0]

                def reap(*args):
                    clock[0] = 1
                    return rc, None

                def sample():
                    clock[0] = 2
                    return 17

                def stop(server):
                    clock[0] = 4
                    self.rt.processes.pop(server.spec.name)

                with patch.object(self.rt, "popen", return_value=Mock(pid=123)), \
                        patch.object(runtime, "Sampler", return_value=Mock(stop=sample)), \
                        patch.object(runtime, "reap", side_effect=reap), \
                        patch.object(runtime.time, "monotonic", side_effect=lambda: clock[0]), \
                        patch.object(self.rt, "stop", side_effect=stop):
                    result = self.rt.run(runtime.Spec("loader", None, ["unused"]), self.root / "log", 1)
                self.assertEqual(result.timed_out, rc is None)
                self.assertEqual(result.ms, 1000 if rc == 0 else 4000)

    def test_interrupted_process_and_rootfs_runs_stop_and_reap(self):
        for scoped in (False, True):
            with self.subTest(scoped=scoped):
                rt = (runtime.RootfsRuntime(self.scratch, 1, self.root, echo=Mock()) if scoped else self.rt)
                spec = runtime.Spec("loader", None, ["unused"], memory="1g" if scoped else None)
                sampler = Mock(stop=Mock(return_value=17))
                with patch.object(rt, "popen", return_value=Mock(pid=123)), \
                        patch.object(rt, "kill"), \
                        patch.object(runtime.os, "killpg", side_effect=ProcessLookupError, create=True), \
                        patch.object(runtime, "Sampler", return_value=sampler), \
                        patch.object(runtime, "end_scope") as ended, \
                        patch.object(runtime, "reap", side_effect=[SystemExit(143), (0, None)]) as reaped:
                    with self.assertRaises(SystemExit) as raised:
                        rt.run(spec, self.root / "log", 1)
                self.assertEqual(raised.exception.code, 143)
                self.assertEqual(reaped.call_count, 2)
                self.assertTrue(sampler.stop.called)
                self.assertFalse(rt.processes)
                if scoped:
                    ended.assert_called_once()
                    self.assertFalse(rt.units)

    def test_partial_scope_sampling_keeps_process_owned_until_cleanup(self):
        rt = runtime.RootfsRuntime(self.scratch, 1, self.root, echo=Mock())
        sampler = Mock(stop=Mock(return_value=17))
        spec = runtime.Spec("loader", None, ["unused"], memory="1g")
        with patch.object(rt, "popen", return_value=Mock(pid=123)), \
                patch.object(runtime, "Sampler", return_value=sampler), \
                patch.object(rt, "peak", side_effect=RuntimeError("scope sampler")), \
                patch.object(rt, "kill", side_effect=PermissionError("cannot stop yet")), \
                patch.object(runtime, "end_scope"):
            with self.assertRaisesRegex(RuntimeError, "scope sampler"):
                rt.run(spec, self.root / "log", 1)
        self.assertIn("loader", rt.processes)
        with patch.object(rt, "kill"), patch.object(runtime, "reap", return_value=(0, None)), \
                patch.object(runtime.os, "killpg", side_effect=ProcessLookupError, create=True):
            rt.cleanup()
        self.assertFalse(rt.processes)

    def test_retained_worker_name_cannot_be_overwritten(self):
        self.rt.processes["loader"] = runtime.Server(runtime.Spec("loader", None, []), Mock())
        with patch.object(self.rt, "popen") as started:
            with self.assertRaises(FileExistsError):
                self.rt.start(runtime.Spec("loader", None, ["unused"]), self.root / "log")
        started.assert_not_called()
        rt = runtime.RootfsRuntime(self.scratch, 1, self.root, echo=Mock())
        rt.units["loader"] = "retained"
        with patch.object(runtime, "end_scope") as ended, patch.object(rt, "popen") as started:
            with self.assertRaises(FileExistsError):
                rt.run(runtime.Spec("loader", None, ["unused"], memory="1g"), self.root / "log", 1)
        ended.assert_not_called()
        started.assert_not_called()
        self.assertEqual(rt.units, {"loader": "retained"})

    def test_scope_failure_retains_scope_and_store(self):
        rt = runtime.RootfsRuntime(self.scratch, 1, self.root, echo=Mock())
        source = rt.new_store("store")
        rt.units["loader"] = "owned-unit"
        with patch.object(runtime, "end_scope", side_effect=PermissionError("denied")):
            rt.cleanup()
        self.assertEqual(rt.units, {"loader": "owned-unit"})
        self.assertEqual(rt.created_stores, [source])
        self.assertTrue(Path(source).exists())

    def test_scope_uses_kill_and_requires_verified_gone_state(self):
        for state in ("LoadState=not-found", "ActiveState=inactive", "ActiveState=active", ""):
            with self.subTest(state=state), patch.object(runtime, "run_quiet",
                    side_effect=[done(), done(1, err="denied or gone"), done(0, state)]) as commands, \
                    patch.object(runtime.time, "monotonic", side_effect=[0, 31]):
                if state in ("LoadState=not-found", "ActiveState=inactive"):
                    runtime.end_scope("owned")
                else:
                    with self.assertRaises(RuntimeError):
                        runtime.end_scope("owned")
                self.assertIn("ControlGroup", commands.call_args_list[0].args[0])
                self.assertIn("kill", commands.call_args_list[1].args[0])
        with patch.object(runtime, "run_quiet", side_effect=[done(), done(), done(1, "LoadState=not-found")]):
            with self.assertRaises(RuntimeError):
                runtime.end_scope("owned")

    def test_scope_waits_for_asynchronous_termination(self):
        with patch.object(runtime, "run_quiet", side_effect=[done(), done(), done(0, "ActiveState=active"),
                          done(0, "ActiveState=inactive")]) as commands, patch.object(runtime.time, "sleep") as waited:
            runtime.end_scope("owned")
        self.assertEqual(commands.call_count, 4)
        waited.assert_called_once()

    def test_process_group_waits_for_asynchronous_termination(self):
        server = runtime.Server(runtime.Spec("loader", None, []), Mock(pid=123))
        self.rt.processes["loader"] = server
        with patch.object(runtime.os, "name", "posix"), patch.object(self.rt, "kill"), \
                patch.object(runtime.signal, "SIGKILL", 9, create=True), \
                patch.object(runtime, "reap", return_value=(0, None)), \
                patch.object(runtime.os, "killpg", side_effect=[None, ProcessLookupError], create=True) as group, \
                patch.object(runtime.time, "sleep") as waited:
            self.rt.stop(server)
        self.assertEqual(group.call_count, 2)
        waited.assert_called_once()
        self.assertFalse(self.rt.processes)

    def test_rootfs_scope_cleanup_preserves_primary_process_stop_error(self):
        rt = runtime.RootfsRuntime(self.scratch, 1, self.root, echo=Mock())
        rt.units["loader"] = "owned-unit"
        primary = PermissionError("primary process stop")
        with patch.object(runtime.ProcessRuntime, "stop", side_effect=primary), \
                patch.object(runtime, "end_scope", side_effect=RuntimeError("secondary scope cleanup")):
            with self.assertRaises(PermissionError) as raised:
                rt.stop(runtime.Server(runtime.Spec("loader", None, [])))
        self.assertIs(raised.exception, primary)
        self.assertEqual(rt.units, {"loader": "owned-unit"})
        self.assertIn("secondary scope cleanup", str(rt.cleanup_errors))

    def test_scoped_memory_excludes_systemd_wrapper_peak(self):
        rt = runtime.RootfsRuntime(self.scratch, 1, self.root, echo=Mock())
        spec = runtime.Spec("loader", None, ["unused"], memory="1g")
        with patch.object(rt, "popen", return_value=Mock(pid=123)), patch.object(rt, "kill"), \
                patch.object(runtime.os, "killpg", side_effect=ProcessLookupError, create=True), \
                patch.object(runtime, "Sampler", return_value=Mock(stop=Mock(return_value=17))) as sample, \
                patch.object(runtime, "reap", return_value=(0, 999)), patch.object(runtime, "end_scope"):
            result = rt.run(spec, self.root / "log", 1)
            self.assertEqual(result.peak_mib, 17)
            sample.assert_called_once()
            for remove_unit in (False, True):
                server = rt.start(spec, self.root / "log")
                if remove_unit:
                    rt.end_unit(spec.name)
                self.assertEqual(rt.stop(server), 17)
            for unscoped in (rt, runtime.ProcessRuntime(self.scratch, 1)):
                spec = runtime.Spec("unscoped", None, ["unused"])
                with patch.object(unscoped, "popen", return_value=Mock(pid=123)), patch.object(unscoped, "kill"):
                    self.assertEqual(unscoped.run(spec, self.root / "log", 1).peak_mib, 999)
                    self.assertEqual(unscoped.stop(unscoped.start(spec, self.root / "log")), 999)

    def test_docker_collisions_never_predelete_shared_resources(self):
        rt = runtime.DockerRuntime(self.scratch, 1)
        rt.by_id = False  # images by tag: the pins are test_job_namespace's
        with patch.object(runtime, "run_quiet", return_value=done(0, "shared\n")) as command, \
                patch.object(runtime.subprocess, "run") as started:
            with self.assertRaises(FileExistsError):
                rt.new_store("shared")
            with self.assertRaises(FileExistsError):
                rt.start(runtime.Spec("shared", "image", ["unused"]), self.root / "log")
        started.assert_not_called()
        self.assertFalse(any("rm" in call.args[0] for call in command.call_args_list))

    def test_docker_removal_failures_and_daemon_errors_retain_ownership(self):
        rt = runtime.DockerRuntime(self.scratch, 1, echo=Mock())
        rt.by_id = False  # images by tag: the pins are test_job_namespace's
        rt.containers.update((name, runtime.Server(runtime.Spec(name, "image", []))) for name in ("bad", "good"))
        rt.created_stores.append("owned-volume")

        def command(argv):
            if "ls" in argv:
                return done(0, "bad\n")
            return done(1 if argv[-1] == "bad" else 0, err="denied")

        with patch.object(runtime, "run_quiet", side_effect=command):
            rt.cleanup()
        self.assertEqual(set(rt.containers), {"bad"})
        self.assertEqual(rt.created_stores, ["owned-volume"])
        rt.containers.clear()
        with patch.object(runtime, "run_quiet", side_effect=[done(1), done(1, err="daemon")]):
            with self.assertRaises(subprocess.CalledProcessError):
                rt.remove_store("owned-volume")
        self.assertEqual(rt.created_stores, ["owned-volume"])
        with patch.object(runtime, "run_quiet", side_effect=[done(1), done()]):
            rt.remove_store("owned-volume")
        self.assertFalse(rt.created_stores)

    def test_docker_stats_is_stopped_when_wait_is_interrupted(self):
        rt = runtime.DockerRuntime(self.scratch, 1, echo=Mock())
        rt.by_id = False  # images by tag: the pins are test_job_namespace's
        sampler = Mock()
        commands = []

        def command(argv):
            commands.append(argv)
            return done(0, "interrupted log\n" if "logs" in argv else "")

        with patch.object(rt, "names", return_value=[]), patch.object(rt, "sample", return_value=sampler), \
                patch.object(runtime.subprocess, "run", side_effect=[done(), KeyboardInterrupt()]), \
                patch.object(runtime, "run_quiet", side_effect=command):
            with self.assertRaises(KeyboardInterrupt):
                rt.run(runtime.Spec("loader", "image", ["unused"]), self.root / "log", 1)
        sampler.stop.assert_called_once()
        self.assertFalse(rt.containers)
        self.assertEqual((self.root / "log").read_text(), "interrupted log\n")
        self.assertLess(next(i for i, c in enumerate(commands) if "logs" in c),
                        next(i for i, c in enumerate(commands) if "rm" in c))

    def test_docker_stats_start_failure_kills_and_reaps_its_child(self):
        process = Mock()
        thread = Mock(ident=None, start=Mock(side_effect=KeyboardInterrupt()))
        with patch.object(runtime.subprocess, "Popen", return_value=process), \
                patch.object(runtime.threading, "Thread", return_value=thread):
            with self.assertRaises(KeyboardInterrupt):
                runtime.DockerStats("owned")
        process.kill.assert_called_once()
        process.wait.assert_called_once()

    def test_docker_start_and_stop_interruption_keeps_sampler_owned(self):
        rt = runtime.DockerRuntime(self.scratch, 1, echo=Mock())
        rt.by_id = False  # images by tag: the pins are test_job_namespace's
        sampler = Mock(stop=Mock(side_effect=[KeyboardInterrupt(), 17]))
        spec = runtime.Spec("server", "image", ["unused"])
        with patch.object(rt, "names", return_value=[]), patch.object(runtime.subprocess, "run", return_value=done()), \
                patch.object(rt, "sample", return_value=sampler):
            server = rt.start(spec, self.root / "log")
        self.assertIs(rt.containers["server"], server)
        with patch.object(runtime, "run_quiet", return_value=done()):
            with self.assertRaises(KeyboardInterrupt):
                rt.stop(server)
            self.assertIs(rt.containers["server"].sampler, sampler)
            rt.cleanup()
        self.assertEqual(sampler.stop.call_count, 2)
        self.assertFalse(rt.containers)

    def test_docker_sampler_startup_failure_keeps_partial_server_owned(self):
        rt = runtime.DockerRuntime(self.scratch, 1, echo=Mock())
        rt.by_id = False  # images by tag: the pins are test_job_namespace's
        with patch.object(rt, "names", return_value=[]), patch.object(runtime.subprocess, "run", return_value=done()), \
                patch.object(rt, "sample", side_effect=KeyboardInterrupt()):
            with self.assertRaises(KeyboardInterrupt):
                rt.start(runtime.Spec("server", "image", ["unused"]), self.root / "log")
        self.assertIn("server", rt.containers)
        with patch.object(runtime, "run_quiet", return_value=done()):
            rt.cleanup()
        self.assertFalse(rt.containers)

    def test_suite_retains_unknown_scratch_and_failed_store(self):
        suite = self.suite()
        source = self.rt.new_store("store")
        sentinel = suite.scratch / "unknown"
        sentinel.write_text("historical")
        with patch.object(runtime.shutil, "rmtree", side_effect=PermissionError("blocked")):
            suite.cleanup()
        self.assertTrue(Path(source).exists())
        suite.cleanup()
        self.assertEqual(sentinel.read_text(), "historical")
        self.assertTrue(self.rt.cleanup_errors)

    def test_dry_run_removes_only_its_results_before_empty_scratch(self):
        suite = self.suite(dry=True)
        suite.cleanup()
        suite.cleanup()
        self.assertFalse(suite.scratch.exists())
        self.assertFalse(self.rt.cleanup_errors)

    def test_attempt_never_serializes_context_secrets(self):
        def stop(ctx):
            raise PermissionError("denied")

        self.rt.attempt(stop, SimpleNamespace(settings={"SECRET": "never-log-this"}))
        self.assertNotIn("never-log-this", str(self.rt.cleanup_errors))
        self.assertNotIn("never-log-this", str(self.rt.echo.call_args_list))

    def test_cycle_preserves_server_peak(self):
        suite = self.suite()
        plan = SimpleNamespace(workload="fixture", tier="tiny", kind="writes", mounts=[], small=True, services=[])
        system = Mock(serve=Mock(return_value=Endpoint("http://fixture")), stop=Mock(return_value=23))
        suite.cycle(plan, "nrese", system, "none", 1)
        self.assertTrue(any(call.kwargs.get("task") == "serve" and call.kwargs.get("peak_mib") == 23
                            for call in suite.emit.call_args_list))

    def test_cycle_stop_failure_still_attempts_other_owned_workers(self):
        suite = self.suite()
        plan = SimpleNamespace(workload="fixture", tier="tiny", kind="writes", mounts=[], small=True, services=[])
        system = Mock(serve=Mock(return_value=Endpoint("http://fixture")),
                      stop=Mock(side_effect=PermissionError("adapter stop")))
        servers = [runtime.Server(runtime.Spec(name, None, []), Mock()) for name in ("bad", "good")]
        self.rt.processes.update((s.spec.name, s) for s in servers)

        def stop(server):
            if server.spec.name == "bad":
                raise PermissionError("runtime stop")
            self.rt.processes.pop("good")

        with patch.object(self.rt, "stop", side_effect=stop) as stopped:
            suite.cycle(plan, "nrese", system, "none", 1)
        self.assertEqual(stopped.call_count, 2)
        self.assertTrue(self.rt.created_stores)
        self.assertTrue(all(Path(s).exists() for s in self.rt.created_stores))
        self.assertTrue(self.rt.cleanup_errors)

    def test_answer_write_failure_cannot_skip_cleanup_or_claim_success(self):
        suite = self.suite()
        source = self.rt.new_store("owned")
        suite.run = Mock(return_value=0)
        suite.answers = Mock(write=Mock(side_effect=ValueError("answers")))
        manifest = Mock()
        with patch.object(driver, "Suite", return_value=suite), \
                patch.object(driver, "Manifest", return_value=manifest), patch.object(driver.signal, "signal"):
            self.assertEqual(driver.main([], {}, {}, self.root), 1)
        self.assertFalse(Path(source).exists())
        manifest.finish.assert_called_once_with(1)

    def test_final_verdict_follows_cleanup_and_preserves_primary_failures(self):
        for failure in (None, ValueError("primary"), SystemExit(143), KeyboardInterrupt(), SystemExit(0), 7):
            with self.subTest(failure=failure):
                suite = self.suite()
                manifest = Mock()
                if isinstance(failure, BaseException):
                    suite.run = Mock(side_effect=failure)
                else:
                    suite.run = Mock(return_value=failure or 0)
                source = self.rt.new_store("blocked")
                suite.answers = Mock(write=Mock(side_effect=ValueError("answers"))) if isinstance(failure, ValueError) else None
                events = []
                manifest.update.side_effect = lambda **kw: events.append(("cleanup", kw))
                manifest.finish.side_effect = lambda code: events.append(("finish", code))
                with patch.object(driver, "Suite", return_value=suite), \
                        patch.object(driver, "Manifest", return_value=manifest), \
                        patch.object(driver.signal, "signal") as handler, \
                        patch.object(runtime.shutil, "rmtree", side_effect=PermissionError("blocked")):
                    if isinstance(failure, (ValueError, SystemExit)) and not (isinstance(failure, SystemExit) and failure.code == 0):
                        with self.assertRaises(type(failure)) as raised:
                            driver.main([], {}, {}, self.root)
                        self.assertIs(raised.exception, failure)
                    else:
                        self.assertEqual(driver.main([], {}, {}, self.root),
                                         130 if isinstance(failure, KeyboardInterrupt) else failure if isinstance(failure, int) else 1)
                self.assertEqual(events[-1], ("finish", 143 if isinstance(failure, SystemExit) and failure.code else
                                             130 if isinstance(failure, KeyboardInterrupt) else failure if isinstance(failure, int) else 1))
                cleanup = events[-2][1]["cleanup"]
                self.assertIn(source, cleanup["residuals"]["stores"])
                self.assertTrue(cleanup["errors"])
                with self.assertRaises(SystemExit) as terminated:
                    handler.call_args.args[1](signal.SIGTERM, None)
                self.assertEqual(terminated.exception.code, 143)
                suite.answers = None
                suite.cleanup()


if __name__ == "__main__":
    unittest.main()
