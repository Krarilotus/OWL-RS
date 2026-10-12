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
        self.assertEqual(plan.inputs, ["/data/dblp-kg-20261011.nt"])
        # The paper's answers are of another release of DBLP: the references decide here.
        self.assertEqual(plan.answers, {})
        known = sparql_plans.known_from_checks(ROOT / "benches/sparql/sparqloscope/dblp-checks.toml")
        self.assertEqual(known["003-join-2-large-large"], {"value": 51891252})
        self.assertNotIn("071-regex-prefix-1", known)  # disputed among the paper's engines

    def test_bsbm_runs_its_own_driver(self):
        plan = workloads.plans(ROOT, "bsbm", ["bsbm-bi-100m"], {"SPARQL_DATA": "/d"})[0]
        self.assertEqual(plan.inputs, ["/data/bsbm-100m.nt"])
        self.assertIn("{endpoint}", plan.external)
        self.assertEqual(plan.external[plan.external.index("--mix") + 1], "bi")
        unset = workloads.plans(ROOT, "bsbm", ["bsbm-bi-100m"], {})[0]
        self.assertIn("SPARQL_DATA", unset.unavailable)


class Rootfs(unittest.TestCase):
    def test_an_image_runs_unpacked_as_under_docker(self):
        from suitekit.runtime import RootfsRuntime, Spec
        with tempfile.TemporaryDirectory() as d:
            image = Path(d) / "vendor_store_1.0"
            (image / "rootfs").mkdir(parents=True)
            (image / "config.json").write_text(
                '[{"Config": {"Env": ["PATH=/usr/bin"], "Entrypoint": ["/entry.sh"], "WorkingDir": "/srv"}}]',
                encoding="utf-8")
            runtime = RootfsRuntime(Path(d), 18950, Path(d))
            self.assertTrue(runtime.image_present("vendor/store:1.0"))
            spec = Spec("s", "vendor/store:1.0", ["serve", "--port", "7"], {"K": "v"},
                        [Mount("/host/data", "/data"), Mount("/host/store", "/store", readonly=False)],
                        entrypoint=True, memory="12g")
            argv = runtime.argv(spec)
        self.assertEqual(argv[:7], ["systemd-run", "--user", "--scope", "-q", "-p", "MemoryMax=12g", "-p"])
        self.assertIn("--tmp-overlay", argv)
        self.assertEqual(argv[argv.index("--overlay-src") + 1], str(image / "rootfs"))
        joined = " ".join(argv)
        for part in ("--setenv PATH /usr/bin", "--setenv K v", "--ro-bind /host/data /data",
                     "--bind /host/store /store", "--chdir /srv", "-- /entry.sh serve --port 7"):
            self.assertIn(part, joined)


class QueryLimit(unittest.TestCase):
    def context(self, settings):
        return adapters.Context(None, ROOT, ROOT, ROOT, Mount("-", "/data"), [], 3600, settings, "t")

    def test_servers_stop_at_the_query_timeout(self):
        self.assertEqual(adapters.query_limit(self.context({"QUERY_TIMEOUT_S": "180"})), 180)
        self.assertEqual(adapters.query_limit(self.context({})), 3600)


if __name__ == "__main__":
    unittest.main()
