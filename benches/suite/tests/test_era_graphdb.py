"""Spec: owner callable contract (23 Oct 2026) and GraphDB 11.5 bulk validation:
unchanged external shapes validate existing data; preserve the original HTTP outcome
and every response byte, including nonconformance, API errors and truncated transfers.
This is a transport contract test, not evidence of native GraphDB SHACL support.
"""
from contextlib import nullcontext
import hashlib
import importlib.util
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch
from types import SimpleNamespace
from typing import get_type_hints

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches/suite"))
from suitekit.adapters import Context, Endpoint
from suitekit.runtime import Measured, Mount

SPEC = importlib.util.spec_from_file_location(
    "era_graphdb", ROOT / "benches/breadth/era-shacl/graphdb.py")
graphdb = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(graphdb)


class GraphdbBulkContract(unittest.TestCase):
    def test_bulk_response_preserves_nonconformance_errors_and_incomplete_bytes(self):
        # Owner's exact callable boundary must use the current published API types.
        self.assertEqual(get_type_hints(graphdb.validate_existing), {
            "ctx": Context, "endpoint": Endpoint, "data": Path, "shapes": Path,
            "report": Path, "return": tuple[Measured, int | None]})
        # Independently authored RDF: a violation is still a successful validation API
        # response. A must parse conformance/truncation/result bags from the saved RDF.
        rdf = (b"@prefix sh: <http://www.w3.org/ns/shacl#> .\n"
               b"@prefix rdf4j: <http://rdf4j.org/schema/rdf4j#> .\n"
               b"[] a sh:ValidationReport; sh:conforms false; rdf4j:truncated false;\n"
               b" sh:result [a sh:ValidationResult; sh:focusNode <urn:bad>;\n"
               b" sh:sourceConstraintComponent sh:MinCountConstraintComponent] .\n")
        # R1 wire bodies are independent HTTP framing examples, not library output.
        cases = (("nonconforming", 200, rdf, 0, True, None, True),
                 ("api-error", 400, b"native rejected shapes\n", 0, False, None, True),
                 ("short-transfer", 200, rdf[:47], 19, False, None, False),
                 ("chunked-missing-trailer", 200, b"X", 0, False, b"1\r\nX\r\n0\r\n", False),
                 ("chunked-partial-delimiter", 200, b"X", 0, False, b"1\r\nX\r", False),
                 ("chunked-complete", 200, b"X", 0, True, b"1\r\nX\r\n0\r\n\r\n", True),
                 ("chunked-complete-provenance-change", 200, b"X", 0, True,
                  b"1\r\nX\r\n0\r\n\r\n", True))
        shapes_bytes = b"@prefix sh: <http://www.w3.org/ns/shacl#> .\n<urn:s> a sh:NodeShape .\n"
        row_specs = {
            "nonconforming": "HTTP200 violation report is API-success; preserve unchanged shapes and exact RDF",
            "api-error": "HTTP400 is API-error even with complete transport; preserve original status and error bytes",
            "short-transfer": "HTTP200 with missing declared bytes is incomplete; retain exact received prefix and fail API completion",
            "chunked-missing-trailer": "Root R1: zero chunk without final trailer terminator is incomplete; retain entity X/status200 and fail completion",
            "chunked-partial-delimiter": "Root R1: partial framing CR is never entity data; retain exactly X/status200 and fail completion",
            "chunked-complete": "Root R1 valid control: complete chunked framing returns exactly X/status200 with successful completion",
            "chunked-complete-provenance-change": "Root F1: a source-comment-only provenance change cannot discard valid entity X/status200 or reject complete transport",
        }
        for name, status, body, missing, expected_ok, chunk_wire, expected_complete in cases:
            with self.subTest(case=name), tempfile.TemporaryDirectory() as temporary:
                work = Path(temporary)
                data, shapes, report = (work / p for p in ("data.ttl", "shapes.ttl", "report.ttl"))
                data.write_bytes(b"<urn:bad> <urn:p> <urn:v> .\n")
                shapes.write_bytes(shapes_bytes)
                observed = []

                class Handler(BaseHTTPRequestHandler):
                    protocol_version = "HTTP/1.1"

                    def do_POST(self):
                        observed.append((self.path, self.headers.get("Content-Type"),
                                         self.headers.get("Accept"),
                                         self.rfile.read(int(self.headers["Content-Length"]))))
                        self.send_response(status)
                        self.send_header("Content-Type", "text/turtle" if status == 200 else "text/plain")
                        if chunk_wire is None:
                            self.send_header("Content-Length", str(len(body) + missing))
                        else:
                            self.send_header("Transfer-Encoding", "chunked")
                        self.send_header("Connection", "close")
                        self.end_headers()
                        self.wfile.write(body if chunk_wire is None else chunk_wire)
                        self.wfile.flush()
                        self.close_connection = True

                    def log_message(self, *_):
                        pass

                server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                worker = threading.Thread(target=server.serve_forever, daemon=True)
                worker.start()
                ctx = Context(SimpleNamespace(dry=False), ROOT, work, work,
                              Mount(str(work), "/data"), [], 2, {}, "graphdb-contract")
                endpoint = Endpoint(f"http://127.0.0.1:{server.server_port}/repositories/bench")
                source_context, source_variant, provenance = nullcontext(), None, None
                parser_methods = (graphdb.http.client.HTTPResponse.read1,
                                  graphdb.http.client.HTTPResponse._read1_chunked,
                                  graphdb.http.client.HTTPResponse._get_chunk_left)
                if name == "chunked-complete-provenance-change":
                    # F1 changes only on-disk provenance, never the loaded parser or
                    # independent wire/expected entity. The installed file is untouched.
                    original_source = Path(graphdb.http.client.__file__).read_bytes()
                    source_variant = original_source + b"\n# F1: provenance-only comment.\n"
                    variant_path = work / "http-client-comment-only.py"
                    variant_path.write_bytes(source_variant)
                    provenance = {
                        "change": "appended_comment_only; loaded parser unchanged",
                        "original_source_sha256": hashlib.sha256(original_source).hexdigest(),
                        "changed_source_sha256": hashlib.sha256(source_variant).hexdigest(),
                    }
                    source_context = patch.object(graphdb.http.client, "__file__", str(variant_path))
                try:
                    with source_context:
                        measured, actual_status = graphdb.validate_existing(ctx, endpoint, data, shapes, report)
                finally:
                    server.shutdown()
                    server.server_close()
                    worker.join(timeout=2)
                receipt = json.loads(report.with_suffix(".ttl.http.json").read_text())
                # Fleet's ordinary output root retains authored and observed bytes
                # before assertions, so a failing row keeps its original evidence.
                if os.environ.get("FLEET_OUT"):
                    artifacts = Path(os.environ["FLEET_OUT"]) / "transport-table" / name
                    artifacts.mkdir(parents=True, exist_ok=False)
                    for filename, payload in (
                        ("expected-request.ttl", shapes_bytes),
                        ("actual-request.ttl", observed[0][3] if observed else b""),
                        ("expected-response.raw", body),
                        ("actual-response.raw", report.read_bytes()),
                        ("wire-body.raw", body if chunk_wire is None else chunk_wire),
                        ("data.ttl", data.read_bytes()),
                    ):
                        (artifacts / filename).write_bytes(payload)
                    (artifacts / "http.json").write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
                    if source_variant is not None:
                        (artifacts / "http-client-comment-only.py").write_bytes(source_variant)
                        (artifacts / "provenance.json").write_text(
                            json.dumps(provenance, indent=2) + "\n", encoding="utf-8")
                    row = {
                        "spec": row_specs[name],
                        "applicability": provenance,
                        "expected": {"path": "/rest/repositories/bench/validate/text",
                                     "content_type": "text/turtle", "accept": "text/turtle",
                                     "http_status": status, "api_ok": expected_ok,
                                     "transport_complete": expected_complete,
                                     "transfer_encoding": "chunked" if chunk_wire is not None else None,
                                     "declared_response_bytes": len(body) + missing if chunk_wire is None else None,
                                     "received_response_bytes": len(body)},
                        "actual": {"request": list(observed[0][:3]) if observed else None,
                                   "http_status": actual_status, "measured": vars(measured),
                                   "transport_complete": receipt["transport_complete"],
                                   "response_bytes": receipt["response_bytes"]},
                    }
                    (artifacts / "row.json").write_text(json.dumps(row, indent=2) + "\n", encoding="utf-8")
                if provenance is not None:
                    self.assertNotEqual(provenance["original_source_sha256"],
                                        provenance["changed_source_sha256"])
                    self.assertEqual(parser_methods, (graphdb.http.client.HTTPResponse.read1,
                                                     graphdb.http.client.HTTPResponse._read1_chunked,
                                                     graphdb.http.client.HTTPResponse._get_chunk_left))
                self.assertIsInstance(measured, Measured)
                self.assertEqual(actual_status, status)
                self.assertEqual(measured.ok, expected_ok)
                self.assertFalse(measured.timed_out)
                self.assertEqual(report.read_bytes(), body)
                self.assertEqual(shapes.read_bytes(), shapes_bytes)
                self.assertEqual(observed, [("/rest/repositories/bench/validate/text",
                                             "text/turtle", "text/turtle", shapes_bytes)])
                self.assertEqual(receipt["http_status"], status)
                self.assertEqual(receipt["response_bytes"], len(body))
                self.assertEqual(receipt["transport_complete"], expected_complete)
                self.assertEqual(receipt["report_semantics"], "unverified")
                self.assertEqual(receipt["timer_kind"], "rest_full_existing_data_response_wall")
                self.assertNotIn("engine_validation_ms", receipt)
                self.assertEqual(data.read_bytes(), b"<urn:bad> <urn:p> <urn:v> .\n")


if __name__ == "__main__":
    unittest.main()
