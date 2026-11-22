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
    """Answers a repository PUT at once and a POST naming "fail" with HTTP 500; holds every other POST until the
    test ends."""
    release = threading.Event()

    def log_message(self, *args):
        pass

    def do_PUT(self):
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        self.send_response(204)
        self.end_headers()

    def do_POST(self):
        if b"fail" in self.rfile.read(int(self.headers.get("Content-Length", 0))):
            self.send_response(500)
            self.end_headers()
        else:
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
            self.assertIn("no answer within 1 s", step.note)

    # Spec (as above): when one file of a load fails and a later one runs out of time, the load is a failure.
    def test_a_failed_file_before_a_late_one_is_a_failure(self):
        base = self.url("/sparql")
        anzo = adapters.adapter("anzograph")
        with tempfile.TemporaryDirectory() as tmp, mock.patch.object(anzo, "start"), \
                mock.patch.object(anzo, "server", None, create=True), \
                mock.patch.object(adapters, "memory_now", return_value=(None, "", "")):
            ctx = SimpleNamespace(dry=False, timeout_s=1, logs=Path(tmp), name=str, data=SimpleNamespace(source=tmp),
                                  extra=[], listen=lambda port, fixed: port, setting=lambda key, default: default,
                                  runtime=SimpleNamespace(url=lambda spec: base[:-len("/sparql")]))
            step = anzo.load(ctx, "store", ["/data/fail.nt", "/data/late.nt"], "none")
            self.assertEqual((step.measured.rc, step.measured.timed_out), (1, False), step)
            self.assertIn("fail.nt: HTTP 500", step.note)

    # Spec (as above, for GraphDB's GeoSPARQL index, built by an update after the server starts): an index request
    # that gets no answer within the cell's bound makes the index a timeout.
    def test_an_index_past_its_bound_is_a_timeout(self):
        base = self.url("")
        graphdb = adapters.adapter("graphdb")
        with mock.patch.object(graphdb, "licence"), mock.patch.object(graphdb, "images", return_value=["image"]), \
                mock.patch.object(graphdb, "env"), mock.patch.object(graphdb, "record_configuration"), \
                mock.patch.object(graphdb, "start", side_effect=lambda ctx, spec, endpoint: endpoint):
            ctx = SimpleNamespace(dry=False, timeout_s=1, services={"geosparql"}, name=str, memory=None,
                                  listen=lambda port, fixed: port, mounts=lambda *mounts: list(mounts),
                                  runtime=SimpleNamespace(url=lambda spec: base))
            endpoint = graphdb.serve(ctx, "store", "none")
            self.assertEqual(endpoint.index_status, "timeout", endpoint)
            self.assertIn("no answer within 1 s", endpoint.index_note)


if __name__ == "__main__":
    unittest.main()
