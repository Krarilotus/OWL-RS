"""/verify's runner (scripts/verify/verify.py; the brief, 20 Oct 2026: agents prove HTTP-level
behaviour by driving a real server, not by claiming it). A verification tool's worst defect is
a false pass, so: every kind of check fails when it should and holds when it should, each failure
with its message, and an incomplete exchange fails before its expectations are read (the self-
test, run here against a stand-in for nrese-server and its own fault server); a failed or
interrupted run still stops its server, removes its data and writes a failing summary; a start
is retried only when its own log says its port was taken, after its process is reaped; and a
scenario that would check less than it says is refused before anything starts. Nothing is built.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import re
import shutil
import socket
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts/verify"))
spec = importlib.util.spec_from_file_location("verify_runner", ROOT / "scripts/verify/verify.py")
verify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify)
from verifykit import scenario  # noqa: E402
from verifykit.server import LaunchError, Server  # noqa: E402

# nrese-server's routes the self-test and the doctor read, answered as it answers them (compact
# JSON), its words when its port is taken, and with VERIFY_STAND_IN=never-ready a /readyz of 503.
STAND_IN = r'''
import json, os, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

host, port = next(a.split("=", 1)[1] for a in sys.argv if a.startswith("server.bind_address=")).rsplit(":", 1)
ROWS = {"head": {"vars": ["x"]}, "results": {"bindings": [{"x": {"type": "literal", "value": v}} for v in "ab"]}}
ROUTES = {
    "/healthz": ("application/json", {"status": "ok"}),
    "/readyz": ("application/json", {"status": "ready", "consistency": "consistent", "revision": 0, "quad_count": 0}),
    "/version": ("application/json", {"service": "stand-in", "version": "0", "store_mode": "on-disk"}),
    "/api/v1/repositories": ("application/json", [{"id": "nrese", "default": True}]),
    "/dataset/query": ("application/sparql-results+json", ROWS),
}
if os.environ.get("VERIFY_STAND_IN") == "never-ready":
    ROUTES["/readyz"] = ("application/json", {"status": "starting"})

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        media, body = ROUTES.get(self.path.split("?")[0], ("application/problem+json", {"status": 404}))
        data = json.dumps(body, separators=(",", ":")).encode()
        status = 404 if media == "application/problem+json" else 503 if body == {"status": "starting"} else 200
        self.send_response(status)
        self.send_header("Content-Type", media)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

class Server(HTTPServer):
    allow_reuse_address = False  # on Windows SO_REUSEADDR would take a port another socket holds

try:
    server = Server((host, int(port)), Handler)
except OSError as e:
    print(f"failed to bind {host}:{port}: {e}", flush=True)
    sys.exit(1)
server.serve_forever()
'''


class StandIn:
    """The stand-in as a server command, in a directory of its own."""

    def __init__(self):
        self.dir = Path(tempfile.mkdtemp(prefix="verify-test-"))
        self.script = self.dir / "stand_in.py"
        self.script.write_text(STAND_IN, encoding="utf-8")
        self.target = {"command": [sys.executable, str(self.script)],
                       "binary": {"path": str(self.script), "sha256": "0" * 64, "revision": None}}

    def main(self, *argv: str) -> tuple[int, str]:
        """verify.py's main with the stand-in as its server: its exit code and output."""
        original, verify.target_of = verify.target_of, lambda args: self.target
        out = io.StringIO()
        try:
            with contextlib.redirect_stdout(out):
                code = verify.main([*argv[:1], "--server", str(self.script), "--start-timeout", "30", *argv[1:]])
        finally:
            verify.target_of = original
        return code, out.getvalue()

    def remove(self):
        shutil.rmtree(self.dir, ignore_errors=True)


def refused(port: int) -> bool:
    try:
        socket.create_connection(("127.0.0.1", port), timeout=2).close()
        return False
    except OSError:
        return True


class SelfTest(unittest.TestCase):
    """One self-test run against the stand-in and the fault server; its tests read what it left."""

    @classmethod
    def setUpClass(cls):
        cls.stand_in = StandIn()
        cls.evidence = cls.stand_in.dir / "evidence"
        cls.code, cls.output = cls.stand_in.main("selftest", "--evidence", str(cls.evidence))
        cls.summary = json.loads((cls.evidence / "summary.json").read_text(encoding="utf-8"))
        cls.steps = {s["name"]: s for r in cls.summary["scenarios"] for s in r["steps"]}

    @classmethod
    def tearDownClass(cls):
        cls.stand_in.remove()

    def test_every_check_and_every_fault_goes_as_it_must(self):
        self.assertEqual(self.code, 0, self.output)
        self.assertIn("self-test passed", self.output)
        self.assertEqual(self.steps["status, wrong"]["failures"], ["status: expected 418, got 200"])
        self.assertTrue(self.steps["rows"]["ok"])
        # A body two bytes of its announced hundred: the exchange's failure, not the body's check.
        self.assertEqual(self.steps["truncated body"]["failures"],
                         ["incomplete response: 2 of 100 body bytes, then the connection closed"])
        record = json.loads((self.evidence / "selftest-faults" / "steps" / "01-truncated-body.json").read_text())
        body = record["response"]["body"]
        self.assertEqual((body["saved_complete"], body["transport_complete"]), (True, False))
        # The summary's path is the last line, and it says which steps failed and why.
        printed = Path(self.output.strip().splitlines()[-1].removeprefix("summary: "))
        self.assertIn('json $.head.vars: expected ["y"], got ["x"]', printed.read_text(encoding="utf-8"))

    def test_a_failed_run_still_stops_the_server_and_removes_its_data(self):
        result = self.summary["scenarios"][0]
        self.assertFalse(result["ok"])  # its wrong rows failed
        cleanup = result["cleanup"]
        self.assertTrue(cleanup["stopped"] and cleanup["removed"], cleanup)
        self.assertFalse(Path(cleanup["directory"]).exists())
        self.assertTrue(refused(result["launch"]["port"]))


class Interruption(unittest.TestCase):
    def test_an_interrupted_run_writes_a_failing_summary_and_cleans_up(self):
        stand_in = StandIn()
        self.addCleanup(stand_in.remove)
        file = stand_in.dir / "three.toml"
        file.write_text("\n".join(f'[[step]]\nname = "health {i}"\npath = "/healthz"\nexpect = {{ status = 200 }}\n'
                                  for i in range(1, 4)), encoding="utf-8")
        calls, original = [], verify.client.exchange

        def exchange(*args):  # SIGTERM arriving during the second step
            calls.append(args)
            if len(calls) == 2:
                raise verify.Interrupted("terminated (SIGTERM)")
            return original(*args)

        verify.client.exchange = exchange
        try:
            code, output = stand_in.main("run", "--evidence", str(stand_in.dir / "evidence"), str(file))
        finally:
            verify.client.exchange = original
        self.assertEqual(code, 3, output)
        summary = json.loads((stand_in.dir / "evidence" / "summary.json").read_text(encoding="utf-8"))
        self.assertTrue(summary["title"].startswith("/verify INTERRUPTED"), summary["title"])
        result = summary["scenarios"][0]
        self.assertEqual(result["error"], "interrupted: terminated (SIGTERM)")
        self.assertEqual([(s["name"], s["ok"]) for s in result["steps"]], [("health 1", True), ("health 2", False)])
        self.assertTrue(result["cleanup"]["removed"] and result["cleanup"]["stopped"], result["cleanup"])


class Launch(unittest.TestCase):
    def setUp(self):
        self.stand_in = StandIn()
        self.addCleanup(self.stand_in.remove)

    def server(self, **env) -> Server:
        server = Server(self.stand_in.target["command"], {}, env, self.stand_in.dir / "server.log", start_timeout=5)
        self.addCleanup(server.cleanup)
        return server

    def test_a_start_whose_port_was_taken_is_reaped_then_retried(self):
        with socket.socket() as taken:
            taken.bind(("127.0.0.1", 0))
            taken.listen()
            server = self.server()
            server.port = taken.getsockname()[1]  # as if another process took it after our probe
            server.start()
        first, second = server.attempts
        self.assertEqual((first["outcome"], first["exit_code"]), ("failed to bind", 1))
        self.assertEqual(second["outcome"], "ready")
        self.assertNotEqual(second["port"], first["port"])

    def test_a_start_that_isnt_ready_is_stopped_not_taken_for_a_bind_failure(self):
        server = self.server(VERIFY_STAND_IN="never-ready")
        # An earlier start's words in the same log mustn't decide this one.
        server.log.write_text("failed to bind 127.0.0.1:1 (an earlier start)\n", encoding="utf-8")
        server.start_timeout = 1.5
        with self.assertRaisesRegex(LaunchError, "not ready after 1.5 s"):
            server.start()
        [attempt] = server.attempts
        self.assertTrue(attempt["outcome"].startswith("not ready"), attempt)
        self.assertIsNotNone(attempt["exit_code"])  # stopped and reaped
        self.assertTrue(refused(attempt["port"]))


class Refusals(unittest.TestCase):
    def test_a_scenario_that_would_check_less_than_it_says_is_refused(self):
        step = {"name": "s", "path": "/healthz", "expect": {"status": 200}}
        cases = [
            ({**step, "expect": {"status": 200, "stauts": 201}}, "unknown key 'stauts'"),
            ({**step, "expect": {"status": 200, "body": {"contians": "x"}}}, "unknown key 'contians'"),
            ({**step, "expect": {"status": 200, "json": {"$.a": {}}}}, "an empty predicate checks nothing"),
            ({k: v for k, v in step.items() if k != "expect"}, "no expect.status"),
            ({**step, "path": "/x/${repo}"}, "${repo} is neither"),
        ]
        for case, message in cases:
            with self.subTest(message=message), self.assertRaisesRegex(scenario.ScenarioError, re.escape(message)):
                scenario.from_dict({"step": [case]}, "case")


if __name__ == "__main__":
    unittest.main()
