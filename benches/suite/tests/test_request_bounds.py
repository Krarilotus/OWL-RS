"""A suite request that gets no answer within its bound is recorded as a timeout, with the bound in its note; one
that fails for another reason is a failure that names the reason. A missing reading is never shown as the other
system's fault."""
from __future__ import annotations

import http.server
import socket
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches/suite"))

from suitekit import adapters  # noqa: E402


class Holding(http.server.BaseHTTPRequestHandler):
    """Answers a repository PUT at once; holds every POST until the test ends."""
    release = threading.Event()

    def log_message(self, *args):
        pass

    def do_PUT(self):
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        self.send_response(204)
        self.end_headers()

    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        self.release.wait(10)


class Bounds(unittest.TestCase):
    def url(self, path: str) -> str:
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Holding)
        server.daemon_threads = True
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(lambda: (Holding.release.set(), server.shutdown(), server.server_close(),
                                 Holding.release.clear()))
        return f"http://127.0.0.1:{server.server_address[1]}{path}"

    # Spec (the suite's statuses, schema.py: ok, failed, timeout): a count that gets no answer within the cell's
    # bound is a timeout, and its note gives the bound.
    def test_a_count_past_its_bound_is_a_timeout(self):
        url = self.url("/sparql")
        n, _, error = adapters.count_all(SimpleNamespace(dry=False, timeout_s=1), adapters.Endpoint(url, url))
        self.assertEqual(error, "no answer within 1 s")
        self.assertIsNone(n)
        self.assertTrue(adapters.timed_out(0, error))

    # Spec (as above): a count with no server to answer is a failure that says so, not a timeout.
    def test_a_count_with_no_server_is_a_failure_that_names_it(self):
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            url = f"http://127.0.0.1:{s.getsockname()[1]}/sparql"  # closed again before the request
        n, _, error = adapters.count_all(SimpleNamespace(dry=False, timeout_s=5), adapters.Endpoint(url, url))
        self.assertTrue(error.startswith("no response: "), error)
        self.assertIsNone(n)
        self.assertFalse(adapters.timed_out(0, error))

    # Spec (as above, for a load): a SPARQL LOAD that gets no answer within the cell's bound makes the load a
    # timeout (the driver reads Measured.timed_out), and its log gives the bound.
    def test_a_load_past_its_bound_is_a_timeout(self):
        base = self.url("/repo")
        endpoint = adapters.Endpoint(base, base + "/statements")
        rdf4j = adapters.adapter("rdf4j")
        with tempfile.TemporaryDirectory() as tmp, \
                mock.patch.object(rdf4j, "spec"), mock.patch.object(rdf4j, "endpoint"), \
                mock.patch.object(rdf4j, "start", return_value=endpoint), \
                mock.patch.object(rdf4j, "stop_memory", return_value=(None, "", "")):
            ctx = SimpleNamespace(dry=False, timeout_s=1, logs=Path(tmp), name=str, listen=lambda port: port)
            started = time.monotonic()
            step = rdf4j.load(ctx, "store", ["/data/input.nt"], "none")
            self.assertLess(time.monotonic() - started, 9, "the load waited past its bound")
            self.assertTrue(step.measured.timed_out, step)
            self.assertIn("no answer within 1 s", (Path(tmp) / "load.log").read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
