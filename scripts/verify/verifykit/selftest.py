"""The runner's self-test: expectations that are wrong on purpose, one per kind of check, must
each fail with the message that names what was expected and what came; a control step must pass.
If a check could pass vacuously (or never pass), this is where it shows. The same table runs
against a real server (`verify.py selftest`) and against a stand-in in the suite's tests
(benches/suite/tests/test_verify.py)."""
from __future__ import annotations

QUERY = 'SELECT ?x { VALUES ?x { "a" "b" } }'
SPARQL_JSON = "application/sparql-results+json"


def _query(expect: dict) -> dict:
    return {"path": "/dataset/query", "params": {"query": QUERY}, "headers": {"Accept": SPARQL_JSON},
            "expect": {"status": 200, **expect}}


# (step name, the step, the message its failure must carry; None: it must pass)
CASES = [
    ("control: the server answers", {"path": "/healthz", "expect": {"status": 200}}, None),
    ("control: the query's two rows", _query({"rows": 2, "json": {"$.head.vars": ["x"]}}), None),
    ("wrong status", {"path": "/healthz", "expect": {"status": 418}}, "status: expected 418, got 200"),
    ("wrong header", {"path": "/healthz", "expect": {"status": 200, "headers": {"x-verify-absent": {"present": True}}}},
     "header x-verify-absent: expected present, absent"),
    ("wrong body", _query({"body": {"contains": "zzz-not-in-the-answer"}}),
     'body: expected to contain "zzz-not-in-the-answer", got '),
    ("wrong json", _query({"json": {"$.head.vars": ["y"]}}), 'json $.head.vars: expected ["y"], got ["x"]'),
    ("wrong rows", _query({"rows": 3}), "rows: expected 3, got 2"),
]


def scenario() -> dict:
    return {"name": "selftest", "description": "Wrong expectations on purpose: each must fail with its message.",
            "step": [{"name": name, **step} for name, step, _ in CASES]}


def judge(result: dict) -> list[str]:
    """What the run got wrong about its own checks: a control step that failed, a wrong one that
    passed, or one that failed without its message. Empty: the self-test passed."""
    problems = []
    steps = {s["name"]: s for s in result.get("steps", [])}
    for name, _, message in CASES:
        step = steps.get(name)
        if step is None:
            problems.append(f"{name}: not run ({result.get('error') or 'no record'})")
        elif message is None and not step["ok"]:
            problems.append(f"{name}: a control step failed: {'; '.join(step['failures'])}")
        elif message is not None and step["ok"]:
            problems.append(f"{name}: passed, though its expectation is wrong")
        elif message is not None and not any(f.startswith(message) for f in step["failures"]):
            problems.append(f"{name}: failed without the message {message!r}: {'; '.join(step['failures'])}")
    return problems
