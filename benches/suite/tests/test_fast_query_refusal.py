"""A query the engine refuses at its memory limit is no wrong answer: its run ends
`query-memory-limit`, which no case declares, so it fails wherever a wrong answer does (D16-596).

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches/suite"))
sys.path.insert(0, str(ROOT / "benches/fast"))

import abba  # noqa: E402
import fast  # noqa: E402
import release_gate as rg  # noqa: E402
from suitekit import cases  # noqa: E402

REFUSED = "query-memory-limit"
# The engine's refusals as a query's error, in its Display form (perf_lab `measure` stores
# `e.to_string()`): nrese-exec budget.rs `BudgetExceeded`, its own budget and the shared one;
# nrese-store error.rs `MemoryLimitWithoutAmounts` (both) and `ProcessMemoryLimit`.
REFUSALS = (
    "SPARQL evaluation error: query memory budget exceeded: 2990 MiB in use, 128 MiB more requested, limit 3072 MiB",
    "SPARQL evaluation error: the server's query memory is in use: 4498 MiB in use, 128 MiB more requested, "
    "limit 4608 MiB",
    "query memory budget exceeded: limit 3072 MiB (what the query held and asked for is not reported to a reader "
    "of some graphs)",
    "the server's query memory is in use: limit 4608 MiB (what the query held and asked for is not reported to a "
    "reader of some graphs)",
    "stopped at the process's memory limit (3072 MiB); nothing was applied",
)
# Errors that are no memory limit reached: the unread memory (budget.rs, `unread`: no limit known,
# nrese-store `is_memory_limit` false), perf_lab's own timeout, and a name that merely contains the words.
NOT_REFUSALS = (
    "SPARQL evaluation error: the process's memory couldn't be read to find its memory limit, so nothing more "
    "can be admitted; set the limit by hand, in bytes, or 0 for none",
    "timeout after 30 s",
    "SPARQL evaluation error: unknown function <urn:memorylimit>",
    "SPARQL evaluation error: unknown function <urn:memory-limit>",
)
CASE = {"name": "refusal", "data": "none", "args": [], "queries": "", "metric": "cold_ms", "cap_gb": 4,
        "workload": "fast", "tier": "refusal", "semantics": "none",
        "check": {"q.count.value": 5030, "q.probe.value": "true"}}


def measured_with(error: str | None, case: dict = CASE, probe: str = "true", reports: int | None = None,
                  exits: tuple[int, ...] = ()) -> dict:
    """The case measured by a perf_lab that answers `probe` and gives `count`'s error (None: its
    answer), exiting 0 as perf_lab does after a refused query (perf_lab/main.rs `measure`); only
    the first `reports` invocations write their report (None: all); invocation i exits `exits[i]`
    (0 past them)."""
    count = {"name": "count", "error": error} if error else {"name": "count", "first_ms": 3, "p50_ms": 2,
                                                             "value": 5030}
    with tempfile.TemporaryDirectory() as directory:
        scratch, calls = Path(directory), []

        def container(*args, **kwargs):
            calls.append(1)
            if reports is None or len(calls) <= reports:
                (scratch / f"{case['name']}.json").write_text(json.dumps({
                    "open_s": 1, "peak_mib": 100,
                    "queries": [{"name": "probe", "first_ms": 2, "p50_ms": 1, "value": probe}, count]}))
            code = exits[len(calls) - 1] if len(calls) <= len(exits) else 0
            return code, "", "Error: killed" if code else "", 1

        with patch.object(fast, "SCRATCH", scratch), patch.object(fast, "container", container):
            return fast.measure(case, case["name"], 1, {}, {}, "none")


def side(record: dict) -> dict:
    return rg.evidence([{"n": 1, "report": {"records": [record]}}], record["case"])


class QueryRefusal(unittest.TestCase):
    """Spec: D16-596 (master, 14 Nov 2026): a refused query is no wrong answer, and it must stay as
    visible as one: a failed run (fast.not_as_expected), a failed smoke check (abba.record_check),
    a failed candidate in the release gate (release_gate.state `failed`: gate() lists it, the
    release verdict is NOT PASSED), whatever the case declares. Review r1: F1 (a declared limit
    accepted it), F2 (it hid another failure), F3 (other errors taken for it), F4 (suite rows);
    review r2: G1 (another declared failure hid it), G2 (the gate called it a wrong answer)."""

    def test_a_refused_query_is_its_own_status_never_a_wrong_answer(self):
        self.assertEqual(measured_with(None)["status"], "ok")  # the same case, answered: right
        for error in REFUSALS:
            with self.subTest(error=error[:50]):
                record = measured_with(error)
                self.assertEqual(record["status"], REFUSED)
                # Stopped at a limit of the case (fast.py LIMITS), the product's: no harness failure.
                self.assertEqual(record["verdict"], {"verdict": "incomplete", "whose": "product"})
                self.assertTrue(any(note.startswith("count: refused at its memory limit") for note in record["notes"]),
                                record["notes"])

    def test_it_fails_the_run_the_smoke_check_and_the_gate_whatever_the_case_declares(self):
        catalog = cases.load()
        self.assertNotIn(REFUSED, cases.OUTCOMES)  # no case can declare it
        for name in ("support-ladder-256", "support-ladder-256-capped", "geo4-relations"):
            declared = next(c for c in catalog if c["name"] == name)
            with self.subTest(case=name, outcome=declared.get("outcome", "ok")):
                record = measured_with(REFUSALS[0], {**CASE, "name": name, "outcome": declared.get("outcome", "ok")})
                self.assertEqual(fast.not_as_expected([record]), [record])
                self.assertNotEqual(abba.record_check(catalog, record), [])
                self.assertEqual(rg.state(side(record)), "failed")
                self.assertEqual(record["status"], REFUSED)

    def test_another_failure_decides_the_status_and_the_refusal_still_fails_the_run(self):
        # Review r2 G1: the refusal can't hide behind another failure that the case declares
        # (geo4-relations declares `wrong`; support-ladder-256 a process stopped at its limit).
        catalog = cases.load()
        runs = [(f"another query wrong, {outcome} declared", "wrong",
                 measured_with(REFUSALS[0], {**CASE, "name": name, "outcome": outcome}, probe="false"))
                for name, outcome in (("support-ladder-256-capped", "ok"), ("support-ladder-256", "memory-limit"),
                                      ("geo4-relations", "wrong"))]
        runs.append(("a later invocation killed at the cap, memory-limit declared", "memory-limit",
                     measured_with(REFUSALS[0], {**CASE, "name": "support-ladder-256", "repeat": 2,
                                                 "outcome": "memory-limit"}, exits=(0, 137))))
        runs.append(("a later invocation without its report, memory-limit declared", "failed",
                     measured_with(REFUSALS[0], {**CASE, "name": "support-ladder-256", "repeat": 2,
                                                 "outcome": "memory-limit"}, reports=1)))
        for what, status, record in runs:
            with self.subTest(what):
                self.assertEqual(fast.not_as_expected([record]), [record])
                self.assertNotEqual(abba.record_check(catalog, record), [])
                self.assertEqual(rg.state(side(record)), "failed")
                self.assertEqual(record["status"], status)
                self.assertEqual(list(record.get("refused", {})), ["count"])

    def test_the_gate_says_a_refusing_side_refused_not_that_its_answer_is_wrong(self):
        # Review r2 G2: a baseline that refused a query, otherwise right.
        why = rg.why_not(side(measured_with(REFUSALS[0])), "base")
        self.assertNotIn("wrong", why)
        self.assertIn("count refused at its memory limit", why)
        # Its other query's wrong answer is still called wrong.
        failed = "; ".join(side(measured_with(REFUSALS[0], probe="false"))["failed_checks"])
        self.assertIn("q.probe.value", failed)
        self.assertNotIn("q.count.value", failed)

    def test_only_the_engine_s_memory_refusal_is_one(self):
        for error in NOT_REFUSALS:
            with self.subTest(error=error[:50]):
                self.assertNotIn(measured_with(error)["status"], ("ok", "memory-limit", REFUSED))

    def test_the_suite_s_rows_call_it_failed_not_wrong(self):
        record = measured_with(REFUSALS[0])
        report = {"started": "2026-11-14T13:00:00", "runtime": "native", "machine": {"host": "h"},
                  "git": {"commit": "0123456789abcdef"}, "records": [record]}
        rows = fast.suite_rows(report, [CASE])
        self.assertEqual([r.status for r in rows], ["failed"])
        self.assertIn("count refused at its memory limit", rows[0].note)


if __name__ == "__main__":
    unittest.main()
