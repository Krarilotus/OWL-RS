"""The SPARQL group's plans: their known answers come from the published sources as written,
and the servers stop a query at the suite's query limit."""
from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches/suite"))

from suitekit import adapters, sparql_plans, workloads  # noqa: E402
from suitekit.runtime import Mount  # noqa: E402


class KnownAnswers(unittest.TestCase):
    def test_checks_table(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "checks.toml"
            path.write_text('[check]\n"q.001-a.value" = 1996\n"q.081-b.rows" = 10\n"q.058-c.value" = \'x y\'\n',
                            encoding="utf-8")
            self.assertEqual(sparql_plans.known_from_checks(path),
                             {"001-a": {"value": 1996}, "081-b": {"rows": 10}, "058-c": {"value": "x y"}})

    def test_sp2bench_published_sizes(self):
        known = sparql_plans.known_from_table(ROOT / "benches/sparql/sp2bench/published.tsv", "25m")
        self.assertEqual(known["q2"], {"rows": 1876999})
        self.assertNotIn("q4", known)  # the paper has no size for Q4 at 25 M
        self.assertNotIn("q12a", known)  # ASK answers are left to the references

    def test_plans_registered_and_named_after_their_cases(self):
        plan = workloads.plans(ROOT, "sparqloscope", None, {})[0]
        self.assertEqual(plan.tier, "sparqloscope-dblp")
        self.assertEqual(len(list(plan.queries.glob("*.rq"))), 105)
        self.assertEqual(plan.answers["003-join-2-large-large"], {"value": 51891252})
        # Disputed in the paper's results: no known answer, the references decide.
        self.assertNotIn("071-regex-prefix-1", plan.answers)


class QueryLimit(unittest.TestCase):
    def context(self, settings):
        return adapters.Context(None, ROOT, ROOT, ROOT, Mount("-", "/data"), [], 3600, settings, "t")

    def test_servers_stop_at_the_query_timeout(self):
        self.assertEqual(adapters.query_limit(self.context({"QUERY_TIMEOUT_S": "180"})), 180)
        self.assertEqual(adapters.query_limit(self.context({})), 3600)


if __name__ == "__main__":
    unittest.main()
