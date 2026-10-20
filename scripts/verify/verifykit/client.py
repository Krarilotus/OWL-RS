"""One HTTP exchange of a step, and its evidence: the request and the response as sent and
received (headers in order, every value), each body's SHA-256 and length over all of it with its
first bytes in a file of their own, and the times to connect, to the response's headers and to its
last byte. Times are recorded, never judged: /verify proves behaviour, not speed."""
from __future__ import annotations

import base64
import hashlib
import http.client
import json
import socket
import time
import urllib.parse
from pathlib import Path


class Body:
    """A body read in full for the checks (up to its read limit), hashed and counted over every
    byte; `kept` is what the checks see."""

    def __init__(self, read_limit: int):
        self.digest, self.size, self.read_limit, self.parts, self.cut = hashlib.sha256(), 0, read_limit, [], None

    def add(self, chunk: bytes) -> None:
        self.digest.update(chunk)
        room = self.read_limit - min(self.size, self.read_limit)
        if room > 0:
            self.parts.append(chunk[:room])
        self.size += len(chunk)

    @property
    def kept(self) -> bytes:
        return b"".join(self.parts)

    def record(self, file: Path, evidence_bytes: int) -> dict:
        kept = self.kept
        file.write_bytes(kept[:evidence_bytes])
        return {"bytes": self.size, "sha256": self.digest.hexdigest(), "file": file.name,
                "file_bytes": min(len(kept), evidence_bytes), "complete": self.size <= evidence_bytes,
                "beyond_read_limit": self.size > self.read_limit, "cut": self.cut}


def request_of(step: dict, base: str) -> tuple[str, str, list[tuple[str, str]], bytes | None]:
    """The method, URL, headers and body a (substituted) step sends."""
    url = base + step["path"]
    if step["params"]:
        url += ("&" if "?" in url else "?") + urllib.parse.urlencode(step["params"])
    headers = list(step["headers"].items())
    names = {k.lower() for k, _ in headers}
    auth = step.get("auth")
    if auth and "authorization" not in names:
        if "basic" in auth:
            token = base64.b64encode(":".join(auth["basic"]).encode("utf-8")).decode("ascii")
            headers.append(("Authorization", f"Basic {token}"))
        else:
            headers.append(("Authorization", f"Bearer {auth['bearer']}"))
    body = None
    kind, value = step.get("payload", (None, None))
    if kind == "body":
        body = value.encode("utf-8")
    elif kind == "json":
        body = json.dumps(value).encode("utf-8")
        if "content-type" not in names:
            headers.append(("Content-Type", "application/json"))
    elif kind == "form":
        body = urllib.parse.urlencode(value).encode("utf-8")
        if "content-type" not in names:
            headers.append(("Content-Type", "application/x-www-form-urlencoded"))
    return step["method"], url, headers, body


def exchange(step: dict, base: str, files: Path, evidence_bytes: int) -> dict:
    """Sends the step's request and reads its response. The record has `request`, `response`
    (None if there was none), `timings_ms` and `error`; `body` holds the response's kept bytes for
    the checks and is left out of the evidence."""
    method, url, headers, payload = request_of(step, base)
    parts = urllib.parse.urlsplit(url)
    target = parts.path + (f"?{parts.query}" if parts.query else "")
    files.parent.mkdir(parents=True, exist_ok=True)
    sent = Body(len(payload or b""))
    if payload:
        sent.add(payload)
    record = {"request": {"method": method, "url": url, "headers": headers,
                          "body": sent.record(files.with_suffix(".request.body"), evidence_bytes) if payload else None},
              "response": None, "timings_ms": {}, "error": None, "body": b""}
    deadline = time.monotonic() + step["timeout_s"]
    connection = http.client.HTTPConnection(parts.hostname, parts.port, timeout=step["timeout_s"])
    started = time.perf_counter()
    try:
        connection.connect()
        record["timings_ms"]["connect"] = _ms(started)
        connection.putrequest(method, target, skip_accept_encoding=True)
        for name, value in headers:
            connection.putheader(name, value)
        if payload is not None:
            connection.putheader("Content-Length", str(len(payload)))
        connection.endheaders(payload)
        response = connection.getresponse()
        record["timings_ms"]["headers"] = _ms(started)
        received = Body(step["read_limit"])
        while True:
            if time.monotonic() > deadline:
                received.cut = f"the step's timeout ({step['timeout_s']:g} s) passed while reading the body"
                break
            chunk = response.read1(65536) if method != "HEAD" else b""
            if not chunk:
                break
            received.add(chunk)
        record["timings_ms"]["total"] = _ms(started)
        record["response"] = {
            "status": response.status, "reason": response.reason, "version": f"HTTP/{response.version / 10:.1f}",
            "headers": list(response.getheaders()),
            "body": received.record(files.with_suffix(".response.body"), evidence_bytes)}
        record["body"] = received.kept
    except (OSError, http.client.HTTPException) as e:
        record["timings_ms"]["total"] = _ms(started)
        reason = "timed out" if isinstance(e, socket.timeout) else f"{type(e).__name__}: {e}"
        record["error"] = f"no response: {reason}"
    finally:
        connection.close()
    return record


def header(record: dict, name: str) -> str | None:
    """A response header's value, every occurrence joined with ', ' (RFC 9110 §5.3); None if absent."""
    values = [v for k, v in (record["response"] or {}).get("headers", []) if k.lower() == name.lower()]
    return ", ".join(values) if values else None


def _ms(since: float) -> float:
    return round((time.perf_counter() - since) * 1000, 2)
