"""/verify's runner (scripts/verify/verify.py; the brief, 20 Oct 2026: agents prove HTTP-level
behaviour by driving a real server, not by claiming it): its checks fail when they should, each
with a message that says what was expected and what came; a failed run still stops its server
and removes its data; and a scenario that would check less than it says is refused before
anything starts. The server here is a stand-in process speaking the few routes the self-test
reads, so nothing is built.

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

# nrese-server's routes the self-test and the doctor read, answered as it answers them.
STAND_IN = r'''
import json, sys
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

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        media, body = ROUTES.get(self.path.split("?")[0], ("application/problem+json", {"status": 404}))
        data = json.dumps(body).encode()
        self.send_response(404 if media == "application/problem+json" else 200)
        self.send_header("Content-Type", media)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

HTTPServer((host, int(port)), Handler).serve_forever()
'''


class SelfTest(unittest.TestCase):
    """One self-test run against the stand-in; its tests read what it left."""

    @classmethod
    def setUpClass(cls):
        cls.dir = Path(tempfile.mkdtemp(prefix="verify-test-"))
        stand_in = cls.dir / "stand_in.py"
        stand_in.write_text(STAND_IN, encoding="utf-8")
        target = {"command": [sys.executable, str(stand_in)],
                  "binary": {"path": str(stand_in), "sha256": "0" * 64, "revision": None}}
        original, verify.target_of = verify.target_of, lambda args: target
        cls.evidence = cls.dir / "evidence"
        out = io.StringIO()
        try:
            with contextlib.redirect_stdout(out):
                cls.code = verify.main(["selftest", "--server", str(stand_in), "--evidence", str(cls.evidence),
                                        "--start-timeout", "30"])
        finally:
            verify.target_of = original
        cls.output = out.getvalue()
        cls.summary = json.loads((cls.evidence / "summary.json").read_text(encoding="utf-8"))
        cls.steps = {s["name"]: s for s in cls.summary["scenarios"][0]["steps"]}

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.dir, ignore_errors=True)

    def test_every_wrong_expectation_fails_with_its_message(self):
        self.assertEqual(self.code, 0, self.output)
        self.assertIn("self-test passed", self.output)
        self.assertEqual(self.steps["wrong status"]["failures"], ["status: expected 418, got 200"])
        self.assertEqual(self.steps["wrong rows"]["failures"], ["rows: expected 3, got 2"])
        self.assertTrue(self.steps["control: the query's two rows"]["ok"])
        # The summary's path is the last line, and it says which steps failed and why.
        printed = Path(self.output.strip().splitlines()[-1].removeprefix("summary: "))
        self.assertIn('json $.head.vars: expected ["y"], got ["x"]', printed.read_text(encoding="utf-8"))

    def test_a_failed_run_still_stops_the_server_and_removes_its_data(self):
        result = self.summary["scenarios"][0]
        self.assertFalse(result["ok"])  # its wrong steps failed
        cleanup = result["cleanup"]
        self.assertTrue(cleanup["stopped"] and cleanup["removed"], cleanup)
        self.assertFalse(Path(cleanup["directory"]).exists())
        with self.assertRaises(OSError):
            socket.create_connection(("127.0.0.1", result["launch"]["port"]), timeout=2).close()


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
