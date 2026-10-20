"""The runner's self-test: every kind of check once where it must hold and once where it must
fail, each failure with the message that names what was expected and what came. A check that
passes vacuously (or never passes) shows here: disable any predicate and a wrong row passes.

CASES run against the server under test (`verify.py selftest`) or the suite's stand-in
(benches/suite/tests/test_verify.py). FAULTS run against a server of the self-test's own that
breaks HTTP on purpose: a body shorter than its Content-Length, a body still streaming at the
step's deadline, headers trickled past it. Each must fail as an incomplete exchange, before
its expectations are read, though each expectation would hold for the bytes that came."""
from __future__ import annotations

import contextlib
import json
import socket
import threading

QUERY = 'SELECT ?x { VALUES ?x { "a" "b" } }'
SPARQL_JSON = "application/sparql-results+json"


def _health(expect: dict) -> dict:
    return {"path": "/healthz", "expect": {"status": 200, **expect}}


def _query(expect: dict) -> dict:
    return {"path": "/dataset/query", "params": {"query": QUERY}, "headers": {"Accept": SPARQL_JSON},
            "expect": {"status": 200, **expect}}


VALUES = "$.results.bindings[*].x.value"
# (step name, the step, the message its failure must start with; None: it must pass)
CASES = [
    ("status", _health({}), None),
    ("status, wrong", {"path": "/healthz", "expect": {"status": 418}}, "status: expected 418, got 200"),
    ("header equals", _health({"headers": {"content-type": "application/json"}}), None),
    ("header equals, wrong", _health({"headers": {"content-type": "text/plain"}}),
     'header content-type: expected "text/plain", got "application/json"'),
    ("header present and absent", _health({"headers": {"content-type": {"present": True},
                                                       "x-verify-absent": {"present": False}}}), None),
    ("header present, wrong", _health({"headers": {"x-verify-absent": {"present": True}}}),
     "header x-verify-absent: expected present, absent"),
    ("header absent, wrong", _health({"headers": {"content-type": {"present": False}}}),
     'header content-type: expected absent, got "application/json"'),
    ("body equals", _health({"body": {"equals": '{"status":"ok"}'}}), None),
    ("body equals, wrong", _health({"body": {"equals": '{"status":"bad"}'}}),
     'body: expected "{"status":"bad"}", got "{"status":"ok"}"'),
    ("body contains", _query({"body": {"contains": "vars"}}), None),
    ("body contains, wrong", _query({"body": {"contains": "zzz-not-in-the-answer"}}),
     'body: expected to contain "zzz-not-in-the-answer", got '),
    ("body not_contains", _query({"body": {"not_contains": "zzz-not-in-the-answer"}}), None),
    ("body not_contains, wrong", _query({"body": {"not_contains": "vars"}}),
     'body: expected not to contain "vars", found it in '),
    ("body matches", _query({"body": {"matches": r'"vars":\s*\["x"\]'}}), None),
    ("body matches, wrong", _query({"body": {"matches": "^zzz"}}), "body: expected to match /^zzz/, got "),
    ("body length", _health({"body": {"length": 15}}), None),
    ("body length, wrong", _health({"body": {"length": 16}}), "body: expected length 16, got 15"),
    ("json equals", _query({"json": {"$.head.vars": ["x"]}}), None),
    ("json equals, wrong", _query({"json": {"$.head.vars": ["y"]}}), 'json $.head.vars: expected ["y"], got ["x"]'),
    ("json contains", _query({"json": {VALUES: {"contains": "a"}}}), None),
    ("json contains, wrong", _query({"json": {VALUES: {"contains": "c"}}}),
     f'json {VALUES}: expected to contain "c", got ["a", "b"]'),
    ("json not_contains", _query({"json": {VALUES: {"not_contains": "c"}}}), None),
    ("json not_contains, wrong", _query({"json": {VALUES: {"not_contains": "a"}}}),
     f'json {VALUES}: expected not to contain "a", found it in ["a", "b"]'),
    ("json matches", _query({"json": {"$.head.vars[0]": {"matches": "^x$"}}}), None),
    ("json matches, wrong", _query({"json": {"$.head.vars[0]": {"matches": "^y$"}}}),
     'json $.head.vars[0]: expected to match /^y$/, got "x"'),
    ("json length", _query({"json": {"$.head.vars": {"length": 1}}}), None),
    ("json length, wrong", _query({"json": {"$.head.vars": {"length": 2}}}),
     "json $.head.vars: expected length 2, got 1"),
    ("json present and absent", _query({"json": {"$.head": {"present": True}, "$.nope": {"present": False},
                                                 "$.results.bindings[*].x": {"present": True}}}), None),
    ("json present, wrong", _query({"json": {"$.nope": {"present": True}}}), "json $.nope: expected present, absent"),
    ("json absent, wrong", _query({"json": {"$.head": {"present": False}}}), "json $.head: expected absent, got "),
    ("json selection empty, wrong", _query({"json": {"$.results.bindings[*].nope": {"present": True}}}),
     "json $.results.bindings[*].nope: expected a nonempty selection, got []"),
    ("rows", _query({"rows": 2}), None),
    ("rows, wrong", _query({"rows": 3}), "rows: expected 3, got 2"),
]

# (step name, the step, the message its failure must start with): against fault_server().
FAULTS = [
    ("truncated body", {"path": "/truncated", "timeout_s": 5, "expect": {"status": 200, "body": {"equals": "ok"}},
                        "capture": {"partial": {"body": True}}},
     "incomplete response: 2 of 100 body bytes, then the connection closed"),
    ("a value captured from it", {"path": "/healthz", "params": {"from": "${partial}"}, "expect": {"status": 200}},
     "not run: ${partial} not captured"),
    ("body past the deadline", {"path": "/stream", "timeout_s": 0.5,
                                "expect": {"status": 200, "body": {"not_contains": "ERROR"}}},
     "incomplete response: the 0.5 s deadline passed while reading the body"),
    ("headers past the deadline", {"path": "/trickle", "timeout_s": 0.5, "expect": {"status": 200}},
     "no response: the 0.5 s deadline passed while reading the response's headers"),
]


def scenario() -> dict:
    return {"name": "selftest", "description": "Every kind of check, held and broken: each wrong row must fail "
            "with its message, every other row must pass.", "step": [{"name": n, **s} for n, s, _ in CASES]}


def fault_scenario() -> dict:
    return {"name": "selftest-faults", "description": "Broken HTTP on purpose (the self-test's own server): each "
            "exchange must fail as incomplete, though its expectations would hold for the bytes that came.",
            "step": [{"name": n, **s} for n, s, _ in FAULTS]}


def judge(results: list[dict]) -> list[str]:
    """What the run got wrong about its own checks: a step that should pass and failed, a wrong
    one that passed, or one that failed without its message. Empty: the self-test passed."""
    problems = []
    by_name = {r["scenario"]: r for r in results}
    for scenario_name, table in (("selftest", CASES), ("selftest-faults", [(n, s, m) for n, s, m in FAULTS])):
        result = by_name.get(scenario_name, {})
        steps = {s["name"]: s for s in result.get("steps", [])}
        for name, _, message in table:
            step = steps.get(name)
            if step is None:
                problems.append(f"{name}: not run ({result.get('error') or 'no record'})")
            elif message is None and not step["ok"]:
                problems.append(f"{name}: should pass, failed: {'; '.join(step['failures'])}")
            elif message is not None and step["ok"]:
                problems.append(f"{name}: passed, though it is wrong on purpose")
            elif message is not None and not any(f.startswith(message) for f in step["failures"]):
                problems.append(f"{name}: failed without the message {message!r}: {'; '.join(step['failures'])}")
    return problems


def _json(status: str, body) -> bytes:
    data = json.dumps(body, separators=(",", ":")).encode()
    return (f"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {len(data)}\r\n"
            "Connection: close\r\n\r\n").encode() + data


# What the fault server answers: a path's parts, each sent after its pause (seconds).
FAULT_ROUTES = {
    "/truncated": [(0, b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 100\r\n\r\nok")],
    "/stream": [(0, b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 1000\r\n\r\nok")]
    + [(0.1, b"x")] * 30,
    "/trickle": [(0, b"HTTP/1.1 200 OK\r\n")] + [(0.15, f"X-Trickle-{i}: {i}\r\n".encode()) for i in range(20)]
    + [(0, b"Content-Length: 0\r\n\r\n")],
    "/healthz": [(0, _json("200 OK", {"status": "ok"}))],
    "/readyz": [(0, _json("200 OK", {"status": "ready"}))],
    "/version": [(0, _json("200 OK", {"service": "selftest-faults"}))],
    "/api/v1/repositories": [(0, _json("200 OK", []))],
}


@contextlib.contextmanager
def fault_server():
    """A loopback server answering FAULT_ROUTES, one thread per connection, gone when the block
    ends (its threads stop sending at once). Yields its base URL."""
    stop = threading.Event()
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen()
    listener.settimeout(0.1)
    threads = []

    def answer(conn: socket.socket) -> None:
        with conn:
            conn.settimeout(2)
            try:
                request = b""
                while b"\r\n\r\n" not in request and len(request) < 65536:
                    chunk = conn.recv(4096)
                    if not chunk:
                        return
                    request += chunk
                path = request.split(b" ", 2)[1].decode("latin-1").split("?")[0]
                for pause, data in FAULT_ROUTES.get(path, [(0, _json("404 Not Found", {"status": 404}))]):
                    if stop.wait(pause):
                        return
                    conn.sendall(data)
            except (OSError, IndexError):
                pass  # the client gave up (its deadline), or sent no request

    def serve() -> None:
        while not stop.is_set():
            try:
                conn, _ = listener.accept()
            except TimeoutError:
                continue
            except OSError:
                return
            thread = threading.Thread(target=answer, args=(conn,), daemon=True)
            threads.append(thread)
            thread.start()

    acceptor = threading.Thread(target=serve, daemon=True)
    acceptor.start()
    try:
        yield f"http://127.0.0.1:{listener.getsockname()[1]}"
    finally:
        stop.set()
        acceptor.join(5)
        listener.close()
        for thread in threads:
            thread.join(5)
