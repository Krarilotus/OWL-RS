"""GraphDB-private ERA bulk-validation boundary; lifecycle stays in suitekit.

GraphDB 11.5 documents /validate/text as bulk validation of an existing repository
with external shapes. It is distinct from commit-time ShaclSail validation. The
caller must load the exact data with the existing Graphdb adapter first (ruleset
none for ERA), retain that load receipt, and pass unchanged Turtle shapes. This
call does not union named graphs, rewrite shapes, load data, or infer eligibility.

Measured.ms is REST request/upload + server work + response transfer/export wall.
There is no separately observed native validation timer. HTTP success does not
establish conformance, complete result semantics, or an unlimited report setting:
the owning checker must inspect the entire retained RDF, including truncation.
https://graphdb.ontotext.com/documentation/11.5/shacl-validation.html
"""
from __future__ import annotations

import http.client
import json
from pathlib import Path
import time
from urllib.parse import urlsplit

from suitekit.adapters import Context, Endpoint
from suitekit.runtime import Measured


class _ChunkedResponse(http.client.HTTPResponse):
    """Keep framing exceptions separate from the stdlib read1 entity reader.

    In the inspected CPython 3.12/3.13 read1 paths, _get_chunk_left reads only
    framing before _read1_chunked reads entity bytes. Its partial bytes must
    never reach the entity writer. Trailer EOF is not a terminator. Runtime
    provenance belongs in producer receipts, never in response acceptance.
    """

    def _get_chunk_left(self):
        try:
            return super()._get_chunk_left()
        except http.client.IncompleteRead as error:
            raise http.client.HTTPException(f"incomplete chunk framing: {error}") from error

    def _read_and_discard_trailer(self):
        while True:
            trailer = self.fp.readline(http.client._MAXLINE + 1)
            if len(trailer) > http.client._MAXLINE:
                raise http.client.LineTooLong("trailer line")
            if trailer in (b"\r\n", b"\n"):
                return
            if not trailer:
                raise http.client.HTTPException("incomplete chunk trailer terminator")


def validate_existing(ctx: Context, endpoint: Endpoint, data: Path, shapes: Path,
                      report: Path) -> tuple[Measured, int | None]:
    """Validate the full existing post-state, preserving raw RDF/API-error bytes.

    Returns the existing Measured (transport/API completion only) and original
    HTTP status, or None if no status was received. A sibling .http.json retains
    transport outcome and actual timing boundaries. Inputs and existing outputs
    are never overwritten. The existing context timeout bounds network operations;
    the caller's Fleet step must also cap the whole lifecycle. No automatic retry,
    redirect, result cap, or inferred engine-validation timer is introduced.
    """
    if ctx.dry:
        raise ValueError("native validation cannot produce a dry-run measurement")
    url = urlsplit(endpoint.query)
    prefix, separator, repository = url.path.rpartition("/repositories/")
    if (url.scheme not in ("http", "https") or not url.hostname or url.username
            or url.password or url.query or url.fragment or not separator
            or not repository or "/" in repository):
        raise ValueError("expected a repository query endpoint without credentials or query parameters")
    if ctx.timeout_s <= 0:
        raise ValueError("validation requires the existing positive context timeout")
    data_stat, shape_stat = data.stat(), shapes.stat()
    metadata_path = report.with_suffix(report.suffix + ".http.json")
    if report.exists() or metadata_path.exists():
        raise FileExistsError("validation output paths must be new")
    metadata = {
        "role": "full-existing-post-state-native-dataset",
        "timer_kind": "rest_full_existing_data_response_wall",
        "timer_includes": ["request_upload", "server_validation_and_serialization",
                           "response_transfer_and_file_export"],
        "timer_start": "before_exclusive_output_open_and_http_request",
        "timer_end": "after_raw_flush_and_response_close_before_metadata_export",
        "timeout_s": ctx.timeout_s,
        "timeout_scope": "socket_operation_with_between_operation_deadline_checks",
        "data": {"path": str(data), "bytes": data_stat.st_size,
                 "mtime_ns": data_stat.st_mtime_ns,
                 "load_identity": "caller_must_match_retained_load_receipt"},
        "shapes": {"path": str(shapes), "bytes": shape_stat.st_size,
                   "mtime_ns": shape_stat.st_mtime_ns},
        "report_semantics": "unverified",
        "report_limits": "unverified_native_configuration",
        "http_status": None,
        "response_bytes": 0,
        "transport_complete": False,
        "transport_error": None,
        "request_to_headers_ms": None,
        "response_transfer_export_ms": None,
    }
    connection_type = http.client.HTTPSConnection if url.scheme == "https" else http.client.HTTPConnection
    connection = connection_type(url.hostname, url.port, timeout=ctx.timeout_s)
    connection.response_class = _ChunkedResponse
    status, rc, timed_out = None, 1, False
    response = None
    started = time.monotonic()
    deadline = started + ctx.timeout_s
    headers_at = None

    def remaining():
        timeout = deadline - time.monotonic()
        if timeout <= 0:
            raise TimeoutError("validation response deadline exceeded")
        if connection.sock is not None:
            connection.sock.settimeout(timeout)
        connection.timeout = timeout

    # Exclusive opens preserve foreign outputs. The response is streamed, with no
    # decoding, RDF normalization, result limit or potentially large in-memory copy.
    with report.open("xb") as raw, metadata_path.open("x", encoding="utf-8") as receipt:
        try:
            with shapes.open("rb") as source:
                remaining()
                connection.putrequest("POST", f"{prefix}/rest/repositories/{repository}/validate/text")
                connection.putheader("Content-Type", "text/turtle")
                connection.putheader("Accept", "text/turtle")
                connection.putheader("Content-Length", str(shape_stat.st_size))
                connection.putheader("Connection", "close")
                connection.endheaders()
                while chunk := source.read(64 * 1024):
                    remaining()
                    connection.send(chunk)
            remaining()
            response = connection.getresponse()
            headers_at = time.monotonic()
            status = response.status
            metadata.update(http_status=status,
                            content_type=response.getheader("Content-Type"),
                            content_length=response.getheader("Content-Length"),
                            transfer_encoding=response.getheader("Transfer-Encoding"),
                            request_to_headers_ms=(headers_at - started) * 1000)
            try:
                while True:
                    remaining()
                    chunk = response.read1(64 * 1024)
                    if not chunk:
                        break
                    raw.write(chunk)
                    metadata["response_bytes"] += len(chunk)
            except http.client.IncompleteRead as error:
                raw.write(error.partial)
                metadata["response_bytes"] += len(error.partial)
                raise
            # read(amt) may silently reach EOF before Content-Length. Preserve that
            # partial report but never classify the transfer as complete.
            if not response.chunked and response.length not in (None, 0):
                raise http.client.IncompleteRead(b"", response.length)
            metadata["transport_complete"] = True
            rc = 0 if 200 <= status < 300 else 1
            metadata["outcome"] = "http_success" if rc == 0 else "http_error"
        except (OSError, http.client.HTTPException) as error:
            timed_out = isinstance(error, TimeoutError)
            rc = 124 if timed_out else 1
            metadata["transport_error"] = {"type": type(error).__name__, "message": str(error)}
            metadata["outcome"] = "timeout" if timed_out else "transport_error"
        finally:
            if response is not None:
                response.close()
            connection.close()
            raw.flush()
            finished = time.monotonic()
            elapsed = (finished - started) * 1000
            metadata["rest_wall_ms"] = elapsed
            if headers_at is not None:
                metadata["response_transfer_export_ms"] = (finished - headers_at) * 1000
            metadata.update(rc=rc, timed_out=timed_out)
            json.dump(metadata, receipt, indent=2)
            receipt.write("\n")
    return Measured(elapsed, None, rc, timed_out), status
