"""The measurement queue (benches/suite/queue.py): when a job may start, what keeps one out,
and a job's run from its checkout to done, failed or queued again.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import datetime
import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

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


class Windows(unittest.TestCase):
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
        self.tmp = tempfile.TemporaryDirectory()
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
        self.q = queue.Queue(root / "queue", self.repo, root / "results", root / "queue" / "wt")
        self.retreat = root / "RETREAT"

    def tearDown(self):
        self.tmp.cleanup()

    def submit(self, commands):
        path = self.q.directory / "pending" / "j.toml"
        path.write_text(queue.toml({"id": "j", "group": "g", "host": "h", "checkout": "x", "commands": commands}),
                        encoding="utf-8")
        return path, queue.read_job(path)

    def test_a_job_runs_in_its_checkout_and_is_done(self):
        script = "import os, pathlib; pathlib.Path(os.environ['QUEUE_RESULTS'], 'out').write_text(open('hello.txt').read())"
        path, found = self.submit([[sys.executable, "-c", script]])
        self.assertEqual(self.q.run(path, found, self.retreat), "done")
        done = queue.read_job(self.q.directory / "done" / "j.toml")
        self.assertEqual(done["exit"], 0)
        self.assertEqual((Path(done["results"]) / "out").read_text(), "hi\n")
        self.assertFalse((self.q.worktrees / "j").exists())

    def test_a_failure_fails_unless_the_owners_came_back(self):
        path, found = self.submit([[sys.executable, "-c", "raise SystemExit(3)"]])
        self.retreat.touch()
        self.assertEqual(self.q.run(path, found, self.retreat), "pending")
        again = queue.read_job(self.q.directory / "pending" / "j.toml")
        self.assertEqual(again["attempts"], 1)
        self.retreat.unlink()
        self.assertEqual(self.q.run(self.q.directory / "pending" / "j.toml", again, self.retreat), "failed")


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
