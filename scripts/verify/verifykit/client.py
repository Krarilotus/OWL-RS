"""One HTTP exchange, and its evidence: the request and the response as sent and received
(headers in order, every value), each body's SHA-256 and length over all of it with its first
bytes in a file of their own, and the times to connect, to the response's headers and to its last
byte. Times are recorded, never judged: /verify proves behaviour, not speed.

An exchange either completes or fails, before any expectation is read:
- **One deadline for all of it.** Resolving the host's name, connecting, sending, every read of
  the headers and of the body wait at most what is left of the step's time, so a peer that
  trickles its bytes can't hold it past its deadline (a socket timeout alone restarts with every
  byte), nor can a slow resolver or a name with many addresses.
- **Every byte the framing announced.** A body shorter than its Content-Length, a chunked body
  cut off, or one the deadline cut is an incomplete response: an error, never a body to check.
  The missing tail could hold the very text a `not_contains` excludes.
The evidence says both whether the saved prefix is all that was received (`saved_complete`) and
whether what was received is the whole message (`transport_complete`)."""
from __future__ import annotations

import base64
import hashlib
import http.client
import io
import json
import socket
import threading
import time
import urllib.parse
from pathlib import Path


class Deadline:
    """What is left of one operation's time, and what the operation is doing (`phase`)."""

    def __init__(self, seconds: float):
        self.seconds, self.end, self.phase = seconds, time.monotonic() + seconds, "connecting"

    def left(self) -> float:
        left = self.end - time.monotonic()
        if left <= 0:
            raise TimeoutError(self.passed())
        return left

    def passed(self) -> str:
        return f"the {self.seconds:g} s deadline passed while {self.phase}"


class _Reader(io.RawIOBase):
    """The socket's bytes, each read within the deadline."""

    def __init__(self, sock: socket.socket, deadline: Deadline):
        self.sock, self.deadline = sock, deadline

    def readable(self) -> bool:
        return True

    def readinto(self, buffer) -> int:
        self.sock.settimeout(self.deadline.left())
        try:
            return self.sock.recv_into(buffer)
        except TimeoutError:
            raise TimeoutError(self.deadline.passed()) from None


class _Socket:
    """The connection's socket as http.client uses it (sendall, makefile), every blocking call
    within the deadline; anything else is the socket's own. Closing it does nothing: http.client
    closes the connection's socket as soon as the server says it will close, before the body is
    read (a real socket file counts its readers; this one doesn't), so `transfer` closes the
    socket itself when the exchange is over."""

    def __init__(self, sock: socket.socket, deadline: Deadline):
        self._sock, self._deadline = sock, deadline

    def close(self) -> None:
        pass

    def sendall(self, data) -> None:
        self._sock.settimeout(self._deadline.left())
        try:
            self._sock.sendall(data)
        except TimeoutError:
            raise TimeoutError(self._deadline.passed()) from None

    def makefile(self, mode: str = "rb", *args, **kwargs):
        return io.BufferedReader(_Reader(self._sock, self._deadline))

    def __getattr__(self, name: str):
        return getattr(self._sock, name)


class Body:
    """A body read in full for the checks (up to its read limit), hashed and counted over every
    byte; `kept` is what the checks see."""

    def __init__(self, read_limit: int):
        self.digest, self.size, self.read_limit, self.parts = hashlib.sha256(), 0, read_limit, []

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
        saved = self.kept[:evidence_bytes]
        file.write_bytes(saved)
        return {"bytes": self.size, "sha256": self.digest.hexdigest(), "file": file.name,
                "saved_bytes": len(saved), "saved_complete": len(saved) == self.size,
                "beyond_read_limit": self.size > self.read_limit}


def connect(host: str, port: int, deadline: Deadline) -> socket.socket:
    """A connected socket within the deadline, name resolution included: getaddrinfo runs on a
    thread of its own (left behind, a daemon, if it outlasts the deadline: Python can't
    interrupt it), and each address in turn gets only what is left (not a timeout each, as
    socket.create_connection gives)."""
    deadline.phase = "resolving the host's name"
    found: dict = {}

    def resolve() -> None:
        try:
            found["addresses"] = socket.getaddrinfo(host, port, type=socket.SOCK_STREAM)
        except OSError as e:
            found["error"] = e
    resolver = threading.Thread(target=resolve, daemon=True)
    resolver.start()
    resolver.join(deadline.left())
    if resolver.is_alive():
        raise TimeoutError(deadline.passed())
    if "error" in found:
        raise found["error"]
    deadline.phase = "connecting"
    last: OSError = OSError(f"{host}: no address")
    for family, kind, protocol, _, address in found["addresses"]:
        sock = None
        try:
            # Inside the per-address handling: a family the host can't create (IPv6 off) is
            # this address's failure, and the next address is tried (Sol r3, finding 2).
            sock = socket.socket(family, kind, protocol)
            sock.settimeout(deadline.left())
            sock.connect(address)
            sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            return sock
        except TimeoutError:
            if sock is not None:
                sock.close()
            raise TimeoutError(deadline.passed()) from None
        except OSError as e:
            if sock is not None:
                sock.close()
            last = e
    raise last


def transfer(method: str, url: str, headers: list[tuple[str, str]], payload: bytes | None,
             timeout_s: float, read_limit: int) -> dict:
    """One request and its whole response within `timeout_s`. The result: `response` (status,
    reason, version, headers; None if they didn't all arrive), `body` (a Body), `declared`
    (the Content-Length, if any), `timings_ms`, and `error`: None only for a complete response."""
    parts = urllib.parse.urlsplit(url)
    target = parts.path + (f"?{parts.query}" if parts.query else "")
    deadline = Deadline(timeout_s)
    out = {"response": None, "body": Body(read_limit), "declared": None, "timings_ms": {}, "error": None}
    connection = http.client.HTTPConnection(parts.hostname, parts.port)
    raw = None
    started = time.perf_counter()
    try:
        raw = connect(parts.hostname, parts.port or 80, deadline)
        connection.sock = _Socket(raw, deadline)
        out["timings_ms"]["connect"] = _ms(started)
        deadline.phase = "sending the request"
        connection.putrequest(method, target, skip_accept_encoding=True)
        for name, value in headers:
            connection.putheader(name, value)
        if payload is not None:
            connection.putheader("Content-Length", str(len(payload)))
        connection.endheaders(payload)
        deadline.phase = "reading the response's headers"
        response = connection.getresponse()
        out["timings_ms"]["headers"] = _ms(started)
        out["response"] = {"status": response.status, "reason": response.reason,
                           "version": f"HTTP/{response.version / 10:.1f}", "headers": list(response.getheaders())}
        out["declared"] = response.length  # None: chunked, or until the connection closes
        deadline.phase = "reading the body"
        while chunk := response.read1(65536):
            out["body"].add(chunk)
        if response.length:  # what Content-Length announced and the connection never sent
            raise http.client.IncompleteRead(b"", response.length)
    except (OSError, http.client.HTTPException) as e:
        prefix = "no response" if out["response"] is None else "incomplete response"
        if isinstance(e, http.client.IncompleteRead):
            reason = (f"{out['body'].size} of {out['declared']} body bytes, then the connection closed"
                      if out["declared"] else f"the chunked body ended early after {out['body'].size} bytes")
        elif isinstance(e, TimeoutError):
            reason = deadline.passed() + (f", after {out['body'].size} body bytes" if out["response"] else "")
        else:
            reason = f"{type(e).__name__}: {e}"
        out["error"] = f"{prefix}: {reason}"
    finally:
        out["timings_ms"]["total"] = _ms(started)
        connection.close()
        if raw is not None:
            raw.close()
    return out


def get(url: str, timeout_s: float, read_limit: int = 1 << 20) -> dict:
    """A GET within `timeout_s` all told (the doctor's readings, the readiness probe)."""
    return transfer("GET", url, [], None, timeout_s, read_limit)


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
    (None if its headers didn't all arrive), `timings_ms` and `error` (None only for a complete
    response); `body` holds the response's kept bytes for the checks and is left out of the
    evidence."""
    method, url, headers, payload = request_of(step, base)
    files.parent.mkdir(parents=True, exist_ok=True)
    sent = None
    if payload is not None:
        sent = Body(len(payload))
        sent.add(payload)
    done = transfer(method, url, headers, payload, step["timeout_s"], step["read_limit"])
    record = {"request": {"method": method, "url": url, "headers": headers,
                          "body": sent.record(files.with_suffix(".request.body"), evidence_bytes) if sent else None},
              "response": done["response"], "timings_ms": done["timings_ms"], "error": done["error"],
              "body": done["body"].kept}
    if done["response"] is not None:
        record["response"]["body"] = {**done["body"].record(files.with_suffix(".response.body"), evidence_bytes),
                                      "declared_bytes": done["declared"],
                                      "transport_complete": done["error"] is None}
    return record


def header(record: dict, name: str) -> str | None:
    """A response header's value, every occurrence joined with ', ' (RFC 9110 §5.3); None if absent."""
    values = [v for k, v in (record["response"] or {}).get("headers", []) if k.lower() == name.lower()]
    return ", ".join(values) if values else None


def _ms(since: float) -> float:
    return round((time.perf_counter() - since) * 1000, 2)
