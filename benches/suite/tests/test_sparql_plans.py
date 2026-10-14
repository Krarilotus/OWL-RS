"""The SPARQL group's plans: their known answers come from the published sources as written,
and the servers stop a query at the suite's query limit."""
from __future__ import annotations

import json
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
                        [Mount("/host/data", "/data"), Mount(str(Path(d) / "store"), "/store", readonly=False)],
                        entrypoint=True, memory="12g")
            argv = runtime.argv(spec)
            store = str(Path(d) / "store")
            # Docker creates a missing bind source; so does this runtime.
            self.assertTrue(Path(store).is_dir())
        self.assertEqual(argv[:4], ["systemd-run", "--user", "--scope", "-q"])
        self.assertTrue(argv[4].startswith("--unit=nrese-s-"))
        # systemd reads upper-case units only; at the cap the kernel kills the command, not systemd the scope.
        for part in ("MemoryMax=12G", "MemorySwapMax=0", "OOMPolicy=continue"):
            self.assertIn(part, argv)
        self.assertEqual(runtime.units["s"], argv[4].split("=", 1)[1])
        self.assertIn("--tmp-overlay", argv)
        self.assertEqual(argv[argv.index("--overlay-src") + 1], str(image / "rootfs"))
        joined = " ".join(argv)
        for part in ("--setenv PATH /usr/bin", "--setenv K v", "--ro-bind /host/data /data",
                     f"--bind {store} /store", "--chdir /srv", "-- /entry.sh serve --port 7"):
            self.assertIn(part, joined)


class RootfsLinks(unittest.TestCase):
    def test_mount_targets_follow_the_images_links(self):
        from suitekit.runtime import inside
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "opt/store/database").mkdir(parents=True)
            try:
                (root / "database").symlink_to("/opt/store/database")
                (root / "opt/data").symlink_to("store/database")
            except (OSError, NotImplementedError):
                self.skipTest("no symlinks here")
            self.assertEqual(inside(root, "/database"), "/opt/store/database")
            self.assertEqual(inside(root, "/opt/data/x"), "/opt/store/database/x")
            self.assertEqual(inside(root, "/index"), "/index")


class AnswerCap(unittest.TestCase):
    def test_an_answer_past_the_cap_is_never_held(self):
        import http.server
        import os
        import threading
        from suitekit import answers
        body = json.dumps({"head": {"vars": ["s"]},
                           "results": {"bindings": [{"s": {"type": "literal", "value": "x" * 100}}] * 50000}}).encode()

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                self.rfile.read(int(self.headers["Content-Length"]))
                self.send_response(200)
                self.send_header("Content-Type", "application/sparql-results+json")
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        url = f"http://127.0.0.1:{server.server_address[1]}/"
        old = os.environ.get("NRESE_ANSWER_CAP_MB")
        try:
            os.environ["NRESE_ANSWER_CAP_MB"] = "1"
            code, why = answers.capped_post(url, {"query": "SELECT * {}"}, timeout=10)
            self.assertEqual(code, -1)
            self.assertIn("cap of 1 MiB", why)
            with tempfile.TemporaryDirectory() as d:
                capture = answers.Recorder.post(answers.capped_post, url, ("w", "t", "none", "q.rq"), "s", "off", 1,
                                                "SELECT * {}", Path(d) / "q.json", 10)
                self.assertIsNone(capture.answer)
                self.assertIn("not compared", capture.error)
            os.environ["NRESE_ANSWER_CAP_MB"] = "16"
            code, text = answers.capped_post(url, {"query": "SELECT * {}"}, timeout=10)
            self.assertEqual((code, len(text)), (200, len(body)))
        finally:
            server.shutdown()
            server.server_close()
            if old is None:
                os.environ.pop("NRESE_ANSWER_CAP_MB", None)
            else:
                os.environ["NRESE_ANSWER_CAP_MB"] = old


class QueryLimit(unittest.TestCase):
    def context(self, settings):
        return adapters.Context(None, ROOT, ROOT, ROOT, Mount("-", "/data"), [], 3600, settings, "t")

    def test_servers_stop_at_the_query_timeout(self):
        self.assertEqual(adapters.query_limit(self.context({"QUERY_TIMEOUT_S": "180"})), 180)
        self.assertEqual(adapters.query_limit(self.context({})), 3600)



class ExternalQualification(unittest.TestCase):
    """Answers before time: an external driver's timings count only once its answers qualified."""

    def rows(self, status):
        return [{"task": "conformance", "item": "explore-qualification", "status": status, "note": "x"},
                {"task": "query", "item": "clients-1", "status": "ok", "ms": 12.0, "extra": "dropped"}]

    def test_timings_after_a_passed_qualification_stay(self):
        from suitekit.driver import external_rows
        rows = external_rows(self.rows("ok"), captured=False)
        self.assertEqual((rows[1]["status"], rows[1]["ms"]), ("ok", 12.0))
        self.assertNotIn("extra", rows[1])

    def test_unqualified_answers_get_no_time(self):
        from suitekit.driver import external_rows
        for status in ("failed", "wrong"):
            rows = external_rows(self.rows(status), captured=False)
            self.assertEqual((rows[1]["status"], rows[1]["ms"]), ("failed", ""))
            self.assertTrue(rows[1]["note"].startswith("answers unqualified"))
        # Without any qualification row either, unless the recorder captured the plan's queries.
        bare = [self.rows("ok")[1]]
        self.assertEqual(external_rows(bare, captured=False)[0]["status"], "failed")
        self.assertEqual(external_rows(bare, captured=True)[0]["status"], "ok")


class BsbmReferences(unittest.TestCase):
    """drive.py's verdict over two references' qualification answers."""

    def verdict(self, mine_agrees: dict[str, bool], references_agree=True, present=("virtuoso", "oxigraph")):
        import importlib.util
        import os
        from types import SimpleNamespace
        from unittest import mock
        spec = importlib.util.spec_from_file_location("bsbm_drive", ROOT / "benches/sparql/bsbm/drive.py")
        drive = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(drive)
        with tempfile.TemporaryDirectory() as d:
            qual = Path(d)
            for name in (*present, "nrese"):
                (qual / f"{name}-explore.qual").write_text("answers", encoding="utf-8")

            def same(args, theirs, mine, log, label):
                a, b = theirs.name.split("-")[0], mine.name.split("-")[0]
                if "nrese" in (a, b):
                    return mine_agrees[a if b == "nrese" else b]
                return references_agree

            args = SimpleNamespace(qualify=qual, system="nrese", mix="explore")
            with mock.patch.object(drive, "bibm", return_value=0), mock.patch.object(drive, "same", same), \
                    mock.patch.dict(os.environ, {"BSBM_REFERENCES": "virtuoso,oxigraph"}):
                return drive.qualify(args, qual, qual / "log")[0]

    def test_agreeing_with_both_references_is_ok(self):
        self.assertEqual(self.verdict({"virtuoso": True, "oxigraph": True}), "ok")

    def test_differing_from_a_reference_the_other_confirms_is_wrong(self):
        self.assertEqual(self.verdict({"virtuoso": False, "oxigraph": False}), "wrong")
        self.assertEqual(self.verdict({"virtuoso": True, "oxigraph": False}), "wrong")

    def test_disagreeing_references_leave_it_unqualified(self):
        self.assertEqual(self.verdict({"virtuoso": True, "oxigraph": False}, references_agree=False), "unqualified")

    def test_without_a_reference_the_answers_are_unchecked(self):
        self.assertEqual(self.verdict({}, present=()), "unchecked")


if __name__ == "__main__":
    unittest.main()
