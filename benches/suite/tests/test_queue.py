"""The measurement queue (benches/suite/queue.py): when a job may start, what keeps one out,
and a job's run from its checkout to done, failed or queued again.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import datetime
import importlib.util
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

SUITE = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("queue_runner", SUITE / "queue.py")
queue = importlib.util.module_from_spec(spec)
spec.loader.exec_module(queue)

AT = datetime.datetime(2026, 10, 12, 22, 30)
REGISTRY = {"host": {
    "office-pc": {"roles": ["measure"], "has": ["docker"], "queue": {"dir": "~/q", "windows": ["21:00-06:00"]}},
    "khaleesi": {"roles": ["measure"], "has": []},
}}


def job(**changes):
    return {"id": "j", "group": "sparql", "host": "office-pc", "checkout": "suite/sparql",
            "commands": [["python3", "benches/fast/fast.py", "compete", "--cases", "c"]], "needs": ["docker"],
            **changes}


def fixture_environment():
    """A foreign repository must never inherit the caller's hook-local Git context."""
    discovery = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    local = subprocess.check_output(["git", "rev-parse", "--local-env-vars"],
                                    env=discovery, text=True).splitlines()
    return {k: v for k, v in os.environ.items() if k not in local}


class Windows(unittest.TestCase):
    def test_weekends_have_windows_of_their_own(self):
        settings = {"windows": ["04:30-21:30"], "weekend_windows": []}
        monday, saturday = datetime.datetime(2026, 10, 12, 10, 0), datetime.datetime(2026, 10, 17, 10, 0)
        self.assertIsNone(queue.open_for(job(), settings, monday, lambda: False))
        self.assertIn("outside", queue.open_for(job(), settings, monday.replace(hour=23), lambda: False))
        self.assertIn("outside", queue.open_for(job(), settings, saturday, lambda: False))
        self.assertIsNone(queue.open_for(job(), {"windows": ["04:30-21:30"]}, saturday, lambda: False))

    def test_how_long_a_host_keeps_results(self):
        self.assertEqual(queue.keep_days({"keep": "same-day"}), 0)
        self.assertEqual(queue.keep_days({"keep": "7-days"}), 7)
        self.assertIsNone(queue.keep_days({}))

    def test_a_window_across_midnight(self):
        self.assertTrue(queue.in_windows(["21:00-06:00"], AT))
        self.assertTrue(queue.in_windows(["21:00-06:00"], AT.replace(hour=5, minute=59)))
        self.assertFalse(queue.in_windows(["21:00-06:00"], AT.replace(hour=12)))
        self.assertTrue(queue.in_windows(["00:00-24:00"], AT.replace(hour=12)))

    def test_retreat_free_compilers_and_windows_decide(self):
        with tempfile.TemporaryDirectory() as tmp:
            retreat, free = Path(tmp) / "RETREAT", Path(tmp) / "FREE"
            settings = {"windows": ["21:00-06:00"], "retreat": str(retreat), "free": str(free)}
            idle = lambda: False  # noqa: E731
            self.assertIsNone(queue.open_for(job(), settings, AT, idle))
            self.assertIn("compiler", queue.open_for(job(), settings, AT, lambda: True))
            self.assertIn("outside", queue.open_for(job(), settings, AT.replace(hour=12), idle))
            free.touch()
            self.assertIsNone(queue.open_for(job(), settings, AT.replace(hour=12), idle))
            retreat.touch()
            self.assertIn("owners", queue.open_for(job(), settings, AT, idle))
            self.assertIn("owners", queue.open_for(job(window="any"), settings, AT, idle))


class Admission(unittest.TestCase):
    def test_what_keeps_a_job_out(self):
        self.assertEqual(queue.job_problems(job(), REGISTRY), [])
        for changes, words in [({"host": "draco"}, "not in hosts.toml"), ({"host": "khaleesi"}, "no queue"),
                               ({"needs": ["java"]}, "lacks java"), ({"window": "now"}, "quiet or any"),
                               ({"commands": [["python3", "benches/suite/suite.py", "run", "--systems", "qlever"]]},
                                "captures its answers first")]:
            with self.subTest(changes=changes):
                self.assertTrue(any(words in p for p in queue.job_problems(job(**changes), REGISTRY)))


class Hosts(unittest.TestCase):
    def test_a_host_without_timing_takes_counts_and_preparation_only(self):
        registry = {"host": {"gamergrill": {"roles": ["prepare"], "has": [], "queue": {"dir": "~/q", "timing": False}}}}
        timed = job(host="gamergrill", needs=[])
        self.assertTrue(any("no timed job" in p for p in queue.job_problems(timed, registry)))
        self.assertEqual(queue.job_problems({**timed, "window": "any"}, registry), [])

    def test_results_of_an_earlier_day_are_removed_on_a_same_day_host(self):
        with tempfile.TemporaryDirectory() as tmp:
            q = queue.Queue(Path(tmp) / "q", Path(tmp), Path(tmp), Path(tmp))
            for name, day in (("old", "2026-10-11"), ("new", "2026-10-12")):
                results = Path(tmp) / "results" / name
                results.mkdir(parents=True)
                (q.directory / "done" / f"{name}.toml").write_text(queue.toml(
                    {"id": name, "commands": [["true"]], "finished": f"{day}T23:00:00", "results": str(results)}),
                    encoding="utf-8")
            q.expire(datetime.date(2026, 10, 12))
            self.assertFalse((Path(tmp) / "results" / "old").exists())
            self.assertTrue((Path(tmp) / "results" / "new").exists())
            self.assertIn("unfetched", queue.read_job(q.directory / "done" / "old.toml")["note"])


class Running(unittest.TestCase):
    def setUp(self):
        clean = patch.dict(os.environ, fixture_environment(), clear=True)
        clean.start()
        self.addCleanup(clean.stop)
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.repo = root / "repo.git"
        source = root / "source"
        source.mkdir()
        (source / "hello.txt").write_text("hi\n", encoding="utf-8")
        git = ["git", "-c", "user.email=t@t", "-c", "user.name=t"]
        subprocess.run(["git", "init", "-q", str(source)], check=True)
        subprocess.run([*git, "-C", str(source), "add", "."], check=True)
        subprocess.run([*git, "-C", str(source), "commit", "-q", "-m", "c"], check=True)
        subprocess.run(["git", "clone", "-q", "--bare", str(source), str(self.repo)], check=True)
        subprocess.run(["git", "-C", str(self.repo), "branch", "queue/j", "HEAD"], check=True)
        self.q = queue.Queue(root / "queue", self.repo, root / "results", root / "queue" / "wt", watch=False)
        self.retreat = root / "RETREAT"

    def submit(self, commands):
        path = self.q.directory / "pending" / "j.toml"
        path.write_text(queue.toml({"id": "j", "group": "g", "host": "h", "checkout": "x", "commands": commands}),
                        encoding="utf-8")
        return path, queue.read_job(path)

    def test_a_job_runs_in_its_checkout_and_is_done(self):
        script = ("import os, pathlib; "
                  "assert not any(k in os.environ for k in ('GIT_DIR', 'GIT_INDEX_FILE', 'GIT_WORK_TREE', 'GIT_COMMON_DIR')); "
                  "pathlib.Path(os.environ['QUEUE_RESULTS'], 'out').write_text(open('hello.txt').read())")
        path, found = self.submit([[sys.executable, "-c", script]])
        self.assertEqual(self.q.run(path, found, self.retreat), "done")
        done = queue.read_job(self.q.directory / "done" / "j.toml")
        self.assertEqual(done["exit"], 0)
        self.assertEqual((Path(done["results"]) / "out").read_text(), "hi\n")
        self.assertFalse((self.q.worktrees / "j").exists())
        # Nothing stale stays: the job's checkout ref and its pid file are gone.
        refs = subprocess.run(["git", "-C", str(self.repo), "branch", "--list", "queue/j"], capture_output=True, text=True)
        self.assertEqual(refs.stdout.strip(), "")
        self.assertFalse((self.q.directory / "running" / "j.pid").exists())

    def test_an_owners_stop_is_no_attempt_a_failure_fails(self):
        path, found = self.submit([[sys.executable, "-c", "raise SystemExit(3)"]])
        self.retreat.touch()
        self.assertEqual(self.q.run(path, found, self.retreat), "pending")
        again = queue.read_job(self.q.directory / "pending" / "j.toml")
        self.assertEqual((again.get("next", 0), again["stops"]), (0, 1))
        self.assertIn("partial results kept", again["note"])
        self.retreat.unlink()
        self.assertEqual(self.q.run(self.q.directory / "pending" / "j.toml", again, self.retreat), "failed")

    def test_a_job_runs_chunk_by_chunk_each_its_own_turn(self):
        path, found = self.submit([[sys.executable, "-c", "print(1)"], [sys.executable, "-c", "print(2)"]])
        self.assertEqual(self.q.run(path, found, self.retreat), "pending")
        again = queue.read_job(self.q.directory / "pending" / "j.toml")
        self.assertEqual(again["next"], 1)
        self.assertEqual(self.q.run(self.q.directory / "pending" / "j.toml", again, self.retreat), "done")
        done = queue.read_job(self.q.directory / "done" / "j.toml")
        self.assertEqual(done["turns"], 2)
        self.assertIn("2", (Path(done["results"]) / "queue-2.log").read_text())

    @unittest.skipUnless(Path("/proc").is_dir(), "the quiet watcher watches /proc (Linux)")
    def test_a_timed_chunk_neither_starts_over_unresolved_suspensions_nor_passes_when_its_watcher_left_some(self):
        """Sol, 20 Oct 2026: the queue started the timed command before reconciling an earlier
        watcher's record, and ran it on when that failed; nor did a watcher left with
        suspensions fail the chunk."""
        q = queue.Queue(self.q.directory, self.repo, self.q.results, self.q.worktrees, watch=True)
        marker = Path(self.tmp.name) / "ran"
        command = [[sys.executable, "-c", f"import pathlib; pathlib.Path({str(marker)!r}).write_text('ran')"]]
        quiet = q.results / "j" / "quiet"
        quiet.mkdir(parents=True)
        (quiet / "frozen").write_text("pid 51\n")  # no start time: it can't be settled
        path, found = self.submit(command)
        self.assertEqual(q.run(path, found, self.retreat), "failed")
        note = queue.read_job(q.directory / "failed" / "j.toml")["note"]
        self.assertEqual((marker.exists(), (quiet / "frozen").read_text()), (False, "pid 51\n"))
        self.assertIn("couldn't be resumed", note)

        class Watcher:  # the chunk's own watcher, which ends having left something suspended
            def terminate(self):
                pass

            def wait(self, timeout=None):
                return 1
        (quiet / "frozen").unlink()
        subprocess.run(["git", "-C", str(self.repo), "branch", "-f", "queue/j", "HEAD"], check=True)
        path, found = self.submit(command)
        with patch.object(queue, "quiet_watch", lambda *a: Watcher()):
            self.assertEqual(q.run(path, found, self.retreat), "failed")
        self.assertTrue(marker.exists())
        self.assertIn("left suspensions", queue.read_job(q.directory / "failed" / "j.toml")["note"])

    def test_a_chunk_past_its_limit_is_stopped_and_the_job_must_be_split(self):
        path, found = self.submit([[sys.executable, "-c", "import time; time.sleep(30)"]])
        started = time.monotonic()
        self.assertEqual(self.q.run(path, found, self.retreat, chunk_min=0.02), "failed")
        self.assertLess(time.monotonic() - started, 20)
        self.assertIn("split it", queue.read_job(self.q.directory / "failed" / "j.toml")["note"])


class HookEnvironment(unittest.TestCase):
    def test_hook_context_cannot_redirect_fixture_or_queue_git_commands(self):
        clean = fixture_environment()
        with tempfile.TemporaryDirectory() as tmp:
            outer = Path(tmp)
            subprocess.run(["git", "init", "-q", str(outer)], env=clean, check=True)
            (outer / "sentinel").write_text("outer repository")
            subprocess.run(["git", "-C", str(outer), "add", "sentinel"], env=clean, check=True)
            index = (outer / ".git" / "index").read_bytes()
            hook = {"GIT_DIR": str(outer / ".git"), "GIT_WORK_TREE": str(outer),
                    "GIT_INDEX_FILE": str(outer / ".git" / "index"), "GIT_COMMON_DIR": str(outer / ".git")}
            with patch.dict(os.environ, hook):
                scrubbed = fixture_environment()
                self.assertFalse(set(hook) & set(scrubbed))
                result = unittest.TestResult()
                Running("test_a_job_runs_in_its_checkout_and_is_done").run(result)
                self.assertTrue(result.wasSuccessful(), result.errors + result.failures)
                self.assertEqual({k: os.environ[k] for k in hook}, hook)
            self.assertEqual((outer / ".git" / "index").read_bytes(), index)
            self.assertFalse(list((outer / ".git" / "refs" / "heads").iterdir()))
            self.assertEqual((outer / "sentinel").read_text(), "outer repository")


class Fairness(unittest.TestCase):
    def test_the_next_turn_goes_to_another_group_first(self):
        with tempfile.TemporaryDirectory() as tmp:
            q = queue.Queue(Path(tmp) / "q", Path(tmp), Path(tmp), Path(tmp))
            for name, group in (("dl-ore", "dl"), ("dl-pagoda", "dl"), ("sparql-bsbm", "sparql")):
                (q.directory / "pending" / f"{name}.toml").write_text(
                    queue.toml({"id": name, "group": group, "workload": "ore-2015", "commands": [["true"]]}),
                    encoding="utf-8")
            settings = {"windows": ["00:00-24:00"]}
            self.assertEqual(q.next(settings, AT, lambda: False)[1]["id"], "dl-ore")
            self.assertEqual(q.next(settings, AT, lambda: False, last_group="dl")[1]["id"], "sparql-bsbm")
            self.assertEqual(q.next(settings, AT, lambda: False, last_group="sparql")[1]["id"], "dl-ore")

    def test_long_jobs_are_split_and_day_long_ones_go_elsewhere(self):
        registry = {"host": {"phuoc-yu": {"roles": ["measure"], "has": [], "queue": {
            "dir": "~/q", "chunk_min": 30, "max_job_min": 240}}}}
        one = job(host="phuoc-yu", needs=[], est_min=120)
        self.assertTrue(any("split it" in p for p in queue.job_problems(one, registry)))
        four = {**one, "commands": one["commands"] * 4}
        self.assertEqual(queue.job_problems(four, registry), [])
        day = {**one, "est_min": 600, "commands": one["commands"] * 20}
        self.assertTrue(any("day-long" in p for p in queue.job_problems(day, registry)))


class Order(unittest.TestCase):
    def test_jobs_run_in_the_schedules_order_then_priority(self):
        with tempfile.TemporaryDirectory() as tmp:
            q = queue.Queue(Path(tmp) / "q", Path(tmp), Path(tmp), Path(tmp))
            for name, fields in (("a", {"workload": "w3c-shacl", "priority": 1}), ("b", {"workload": "bsbm"}),
                                 ("c", {"workload": "sparqloscope", "priority": 7}),
                                 ("d", {"commands": [["python3", "benches/fast/fast.py", "compete", "--cases", "q-star"]]}),
                                 ("e", {"workload": "something-new"})):
                (q.directory / "pending" / f"{name}.toml").write_text(
                    queue.toml({"id": name, "commands": [["true"]], **fields}), encoding="utf-8")
            self.assertEqual([j["id"] for _, j in q.jobs("pending")], ["c", "b", "d", "e", "a"])


if __name__ == "__main__":
    unittest.main()
