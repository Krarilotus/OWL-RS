"""The answer recorder (suitekit/answers.py) on the cases of the answer replay of 10 October
2026: a COUNT typed xsd:int by one system, one deviating system among agreeing ones, an
unordered LIMIT, ordered and unordered answers, blank nodes, missing captures and an
adjudicated count; and the host registry (suitekit/hosts.py).

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from suitekit import answers, hosts  # noqa: E402

XSD = "http://www.w3.org/2001/XMLSchema#"


def lit(value, datatype=None) -> dict:
    return {"type": "literal", "value": str(value)} | ({"datatype": XSD + datatype} if datatype else {})


def iri(name) -> dict:
    return {"type": "uri", "value": f"http://example.org/{name}"}


def select(variables, *rows) -> str:
    return json.dumps({"head": {"vars": variables},
                       "results": {"bindings": [dict(zip(variables, row)) for row in rows]}})


def parse(*args) -> answers.Answer:
    return answers.parse(select(*args))


class Compare(unittest.TestCase):
    def test_an_int_count_matches_by_value_and_names_the_datatype(self):
        reference = parse(["n"], [lit(300, "integer")])
        qlever = parse(["n"], [lit(300, "int")])
        verdict = answers.compare(qlever, reference)
        self.assertEqual(verdict["verdict"], "ok")
        self.assertTrue(verdict["value_match"])
        self.assertFalse(verdict["strict_match"])
        self.assertIn("xsd:int for xsd:integer", verdict["note"])

    def test_values_compare_within_a_primitive_type_only(self):
        integer = parse(["n"], [lit(1, "integer")])
        self.assertTrue(answers.compare(parse(["n"], [lit("1.0", "decimal")]), integer)["value_match"])
        self.assertFalse(answers.compare(parse(["n"], [lit("1.0E0", "double")]), integer)["value_match"])
        self.assertFalse(answers.compare(parse(["n"], [lit(2, "int")]), integer)["value_match"])
        self.assertTrue(answers.compare(parse(["s"], [lit("a", "string")]), parse(["s"], [lit("a")]))["value_match"])

    def test_a_bag_ignores_order_a_sequence_does_not(self):
        a, b = [iri("a")], [iri("b")]
        self.assertTrue(answers.compare(parse(["x"], b, a), parse(["x"], a, b))["strict_match"])
        verdict = answers.compare(parse(["x"], b, a), parse(["x"], a, b), "sequence")
        self.assertEqual(verdict["verdict"], "wrong")
        self.assertIn("another order", verdict["note"])

    def test_an_unordered_limit_accepts_any_legal_subset(self):
        full = parse(["x"], [iri("a")], [iri("b")], [iri("c")])
        reference = parse(["x"], [iri("a")], [iri("b")])
        legal = answers.compare(parse(["x"], [iri("b")], [iri("c")]), reference, "limit", 2, full)
        self.assertEqual(legal["verdict"], "ok")
        foreign = answers.compare(parse(["x"], [iri("a")], [iri("d")]), reference, "limit", 2, full)
        self.assertEqual(foreign["verdict"], "wrong")
        short = answers.compare(parse(["x"], [iri("a")]), reference, "limit", 2, full)
        self.assertEqual(short["verdict"], "wrong")
        self.assertEqual(answers.compare(reference, reference, "limit", 2, None)["verdict"], "incomplete")

    def test_blank_nodes_are_left_to_adjudication(self):
        blank = parse(["x"], [{"type": "bnode", "value": "b0"}])
        self.assertEqual(answers.compare(blank, blank)["verdict"], "unqualified")

    def test_ask(self):
        yes, no = answers.parse('{"boolean": true}'), answers.parse('{"boolean": false}')
        self.assertTrue(answers.compare(yes, yes)["strict_match"])
        self.assertEqual(answers.compare(no, yes)["verdict"], "wrong")
        with self.assertRaises(ValueError):
            answers.parse('{"boolean": "true"}')

    def test_the_order_of_a_query(self):
        self.assertEqual(answers.order_of("SELECT * { ?s ?p ?o } ORDER BY ?s LIMIT 5"), ("sequence", None))
        self.assertEqual(answers.order_of("SELECT * { ?s ?p ?o }\nLIMIT 1000\n"), ("limit", 1000))
        self.assertEqual(answers.order_of("SELECT * { ?s ?p ?o } OFFSET 10 LIMIT 5"), ("limit", 5))
        self.assertEqual(answers.order_of("SELECT * { { SELECT ?s { ?s ?p ?o } LIMIT 5 } ?s ?q ?r }"), ("bag", None))
        self.assertEqual(answers.without_limit("SELECT * { ?s ?p ?o } LIMIT 5"), "SELECT * { ?s ?p ?o }\n")


class Recording(unittest.TestCase):
    def run_capture(self, bodies: dict[str, dict[str, str]], query: str, reference="jena", adjudicated=None,
                    expected=()) -> dict[str, dict]:
        """bodies: system -> query text -> response body; the verdict per system."""
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            path = directory / "q1.rq"
            path.write_text(query, encoding="utf-8")
            recorder = answers.Recorder(directory, reference, adjudicated)
            item = ("w", "t", "none", "q1.rq")
            for system in expected:
                recorder.expect(item, system)
            for system, by_text in bodies.items():
                def post(url, data, timeout, by_text=by_text):
                    body = by_text.get(data["query"].strip())
                    return (200, body) if body is not None else (500, "error")
                recorder.capture(post, "http://x", item, system, "off", 1, path, directory, 10)
            written = json.loads(recorder.write().read_text(encoding="utf-8"))
            return {v["system"]: v for v in written["verdicts"]}

    def test_one_deviating_system_does_not_disqualify_the_others(self):
        q = "SELECT ?n { ?s ?p ?n }"
        good, other = select(["n"], [lit(1, "integer")]), select(["n"], [lit(2, "integer")])
        verdicts = self.run_capture({"jena": {q: good}, "nrese": {q: good}, "qlever": {q: other}}, q)
        self.assertEqual(verdicts["nrese"]["verdict"], "ok")
        self.assertTrue(verdicts["nrese"]["strict_match"])
        self.assertEqual(verdicts["qlever"]["verdict"], "wrong")

    def test_the_reference_answers_the_query_without_its_limit_too(self):
        q, full = "SELECT ?x { ?x ?p ?o } LIMIT 2", "SELECT ?x { ?x ?p ?o }"
        rows = [[iri("a")], [iri("b")], [iri("c")]]
        bodies = {"jena": {q: select(["x"], *rows[:2]), full: select(["x"], *rows)},
                  "nrese": {q: select(["x"], *rows[1:])}}
        verdicts = self.run_capture(bodies, q)
        self.assertEqual(verdicts["nrese"]["verdict"], "ok")
        self.assertEqual(verdicts["nrese"]["order"], "limit")

    def test_missing_captures_and_a_failed_reference_are_incomplete(self):
        q = "SELECT ?x { ?x ?p ?o }"
        verdicts = self.run_capture({"jena": {}, "nrese": {q: select(["x"], [iri("a")])}}, q,
                                    expected=["jena", "nrese", "oxigraph"])
        self.assertEqual(verdicts["nrese"]["verdict"], "incomplete")
        self.assertEqual(verdicts["oxigraph"]["note"], "not captured")

    def test_a_reference_against_an_adjudicated_count_is_no_reference(self):
        q = "SELECT ?x { ?x ?p ?o }"
        one, two = select(["x"], [iri("a")]), select(["x"], [iri("a")], [iri("b")])
        verdicts = self.run_capture({"jena": {q: one}, "nrese": {q: one}, "qlever": {q: two}}, q,
                                    adjudicated={("w", "t", "q1"): "2"})
        # The count alone decides a differing count; an equal one leaves the bindings unchecked.
        self.assertEqual(verdicts["nrese"]["verdict"], "wrong")
        self.assertIn("known 2", verdicts["nrese"]["note"])
        self.assertEqual(verdicts["qlever"]["verdict"], "unqualified")

    def test_a_known_value_decides_where_no_reference_answered(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            query = directory / "count.rq"
            query.write_text("SELECT (COUNT(*) AS ?n) { ?a <http://example.org/follows>+ ?b }", encoding="utf-8")
            recorder = answers.Recorder(directory, "jena")
            item = ("fast", "q-paths~plain", "none", "count.rq")
            recorder.check(item, {"value": "90000000000"})
            replies = {"jena": (0, ""), "nrese": (200, select(["n"], [lit(90000000000, "integer")])),
                       "qlever": (200, select(["n"], [lit(89999999999, "int")]))}
            for system, reply in replies.items():
                recorder.capture(lambda url, data, timeout, reply=reply: reply, "http://x", item, system, "off", 1,
                                 query, directory, 0)
            verdicts = {v["system"]: v for v in recorder.verdicts()}
            self.assertEqual(verdicts["nrese"]["verdict"], "ok")
            self.assertEqual(verdicts["qlever"]["verdict"], "wrong")
            group = recorder.groups()[0]
            self.assertEqual((group["verdict"], group["wrong"], group["incomplete"]), ("wrong", ["qlever"], ["jena"]))
            self.assertEqual(group["reference"], "known answer")

    def test_the_next_reference_stands_in_and_confirms(self):
        q = "SELECT ?x { ?x ?p ?o }"
        good, bad = select(["x"], [iri("a")]), select(["x"], [iri("b")])
        # Jena timed out: Oxigraph is the reference, Virtuoso confirms it.
        verdicts = self.run_capture({"jena": {}, "oxigraph": {q: good}, "virtuoso": {q: good},
                                     "nrese": {q: good}, "qlever": {q: bad}}, q, reference="jena,oxigraph,virtuoso")
        self.assertEqual(verdicts["nrese"]["reference"], "oxigraph")
        self.assertEqual(verdicts["nrese"]["verdict"], "ok")
        self.assertEqual(verdicts["oxigraph"]["verdict"], "ok")
        self.assertIn("virtuoso", verdicts["oxigraph"]["note"])
        self.assertEqual(verdicts["qlever"]["verdict"], "wrong")
        self.assertEqual(verdicts["jena"]["verdict"], "incomplete")

    def test_disagreeing_references_leave_the_group_to_adjudication(self):
        q = "SELECT ?x { ?x ?p ?o }"
        a, b = select(["x"], [iri("a")]), select(["x"], [iri("b")])
        verdicts = self.run_capture({"jena": {q: a}, "oxigraph": {q: b}, "nrese": {q: a}}, q, reference="jena,oxigraph")
        self.assertEqual(verdicts["nrese"]["verdict"], "unqualified")
        self.assertIn("disagree", verdicts["nrese"]["note"])

    def test_an_unconfirmed_reference_is_only_the_reference(self):
        q = "SELECT ?x { ?x ?p ?o }"
        a = select(["x"], [iri("a")])
        verdicts = self.run_capture({"jena": {q: a}, "nrese": {q: a}}, q)
        self.assertEqual(verdicts["jena"]["verdict"], "reference")
        self.assertEqual(verdicts["nrese"]["verdict"], "ok")


class Groups(unittest.TestCase):
    def test_a_query_repeated_on_the_same_data_is_one_group_whose_verdict_names_whose(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            query = directory / "person.rq"
            query.write_text("SELECT ?a { ?p <http://example.org/age> ?a }", encoding="utf-8")
            recorder = answers.Recorder(directory, "jena")
            rows = {"jena": [lit(30, "integer")], "nrese": [lit(30, "integer")], "qlever": [lit(30, "int")],
                    "virtuoso": [lit(31, "integer")]}
            for tier in ("clients-lookups~plain", "clients-sweep~plain"):
                item = ("fast", tier, "none", "person.rq")
                for system, row in rows.items():
                    recorder.expect(item, system, ("/data/social.nt",))
                    recorder.capture(lambda url, data, timeout, row=row: (200, select(["a"], row)), "http://x",
                                     item, system, "off", 1, query, directory, 10)
                recorder.skip(item, "rdfox", "needs RDFox.lic", data=("/data/social.nt",))
            groups = recorder.groups()
            self.assertEqual(len(groups), 1)
            group = groups[0]
            self.assertEqual(len(group["items"]), 2)
            self.assertEqual(group["verdict"], "wrong")
            self.assertEqual(group["wrong"], ["virtuoso"])
            self.assertEqual(group["unsupported"], ["rdfox"])
            self.assertIn("xsd:int for xsd:integer", group["strict_differs"]["qlever"])
            self.assertIn("virtuoso", answers.table(groups))

    def test_captures_read_back_classify_the_same(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            query = directory / "q.rq"
            query.write_text("SELECT ?x { ?x ?p ?o } LIMIT 1", encoding="utf-8")
            recorder = answers.Recorder(directory, "jena,oxigraph")
            item = ("w", "t", "none", "q.rq")
            pool = select(["x"], [iri("a")], [iri("b")])
            bodies = {"jena": [select(["x"], [iri("a")]), pool], "oxigraph": [select(["x"], [iri("b")]), pool],
                      "nrese": [select(["x"], [iri("b")])]}
            for system, replies in bodies.items():
                logs = directory / system
                logs.mkdir()
                queue = list(replies)
                recorder.capture(lambda url, data, timeout, queue=queue: (200, queue.pop(0)), "http://x",
                                 item, system, "off", 1, query, logs, 10)
            written = recorder.write()
            again = answers.Recorder.load([written])
            self.assertEqual(again.verdicts(), recorder.verdicts())
            self.assertEqual({v["system"]: v["verdict"] for v in again.verdicts()},
                             {"jena": "ok", "oxigraph": "ok", "nrese": "ok"})

    def test_a_timeout_is_incomplete_an_unsupported_feature_unsupported(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            query = directory / "q.rq"
            query.write_text("SELECT ?x { ?x ?p ?o }", encoding="utf-8")
            recorder = answers.Recorder(directory, "jena")
            item = ("w", "t", "none", "q.rq")
            replies = {"jena": (200, select(["x"], [iri("a")])), "oxigraph": (0, ""),
                       "qlever": (400, "Feature not supported: SERVICE")}
            for system, reply in replies.items():
                recorder.capture(lambda url, data, timeout, reply=reply: reply, "http://x", item, system, "off", 1,
                                 query, directory, 0)
            verdicts = {v["system"]: v for v in recorder.verdicts()}
            self.assertEqual(verdicts["oxigraph"]["verdict"], "incomplete")
            self.assertIn("timeout", verdicts["oxigraph"]["note"])
            self.assertEqual(verdicts["qlever"]["verdict"], "unsupported")


class FastAnswers(unittest.TestCase):
    def test_a_cases_checks_are_known_answers_under_its_own_semantics(self):
        from suitekit.workloads import fast_answers
        case = {"semantics": "none", "check": {"q.count.value": "90000000000", "q.reach.value": "expect.people",
                                               "q.names.rows": 4, "recall": ">= 0.9", "q.x.operators": "has wcoj"}}
        with tempfile.TemporaryDirectory() as tmp:
            expect = Path(tmp) / "expect.json"
            expect.write_text('{"people": 300000}', encoding="utf-8")
            known = fast_answers(case, "plain", {"FAST_EXPECT": str(expect)})
        self.assertEqual(known, {"count": {"value": "90000000000"}, "reach": {"value": 300000}, "names": {"rows": 4}})
        self.assertNotIn("reach", fast_answers(case, "plain", {}))
        self.assertEqual(fast_answers({**case, "semantics": "owl2-rl"}, "plain", {}), {})
        self.assertIn("count", fast_answers({**case, "semantics": "owl2-rl"}, "closure", {}))


class Hosts(unittest.TestCase):
    REGISTRY = {
        "assets": {"root": "/assets"},
        "inputs": {"ore": {"path": "ore/files", "env": "NRESE_ORE_DIR"}},
        "host": {"office-pc": {"hostname": "sus-ai", "ssh": "Office-PC", "has": ["docker", "java"], "assets": True},
                 "phuoc-yu": {"hostname": "Phuoc-Yu", "has": [], "assets": True},
                 "khaleesi": {"has": [], "assets": False}},
    }

    def test_a_host_is_found_by_its_name(self):
        self.assertEqual(hosts.current(self.REGISTRY, "sus-ai")[0], "office-pc")
        self.assertEqual(hosts.current(self.REGISTRY, "phuoc-yu.local")[0], "phuoc-yu")
        self.assertIsNone(hosts.current(self.REGISTRY, "laptop"))

    def test_inputs_come_from_the_variable_else_the_assets(self):
        self.assertEqual(hosts.input_path("ore", self.REGISTRY, "sus-ai", {}), Path("/assets/ore/files"))
        self.assertEqual(hosts.input_path("ore", self.REGISTRY, "sus-ai", {"NRESE_ORE_DIR": "/x"}), Path("/x"))
        self.assertIsNone(hosts.input_path("ore", self.REGISTRY, "khaleesi", {}))
        self.assertIsNone(hosts.input_path("ore", self.REGISTRY, "laptop", {}))

    def test_a_host_without_docker_is_refused_an_unknown_one_is_not(self):
        self.assertEqual(hosts.lacking(["docker"], self.REGISTRY, "Phuoc-Yu"), ["docker"])
        self.assertEqual(hosts.lacking(["docker"], self.REGISTRY, "sus-ai"), [])
        self.assertEqual(hosts.lacking(["docker"], self.REGISTRY, "laptop"), [])

    def test_a_missing_image_resolves_to_its_verified_preserved_alias(self):
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "images.json").write_text(json.dumps({"images": [
                {"original_tags": ["nrese-bench/dl-reference:latest"], "loaded_id": "sha256:aa",
                 "preserved_tag": "nrese-preserved/dl-reference:kraris-aa"}]}), encoding="utf-8")
            registry = {**self.REGISTRY, "assets": {"root": tmp, "images": "images.json"}}
            local = {"nrese-preserved/dl-reference:kraris-aa": "sha256:aa"}
            self.assertEqual(hosts.image("nrese-bench/dl-reference", registry, "sus-ai", local.get),
                             "nrese-preserved/dl-reference:kraris-aa")
            # The tag itself when Docker has it; the tag when the alias's ID isn't the verified one.
            self.assertEqual(hosts.image("nrese-bench/dl-reference", registry, "sus-ai",
                                         {"nrese-bench/dl-reference": "sha256:bb"}.get), "nrese-bench/dl-reference")
            self.assertEqual(hosts.image("nrese-bench/dl-reference", registry, "sus-ai",
                                         {"nrese-preserved/dl-reference:kraris-aa": "sha256:cc"}.get),
                             "nrese-bench/dl-reference")
            self.assertEqual(hosts.image("nrese-bench/dl-reference", registry, "khaleesi", local.get),
                             "nrese-bench/dl-reference")

    def test_volumes_come_from_the_registry_or_their_variable(self):
        registry = {"volumes": {"fast": {"name": "nrese-fast-data", "env": "NRESE_FAST_VOLUME"}}}
        self.assertEqual(hosts.volume("fast", registry, {}), "nrese-fast-data")
        self.assertEqual(hosts.volume("fast", registry, {"NRESE_FAST_VOLUME": "mine"}), "mine")
        self.assertEqual(hosts.volume("data", environ={}), "nrese-bench-data")

    def test_a_hosts_configuration_is_the_one_in_force_when_a_run_started(self):
        registry = {"host": {"phuoc-yu": {"hostname": "Phuoc-Yu", "config": [
            {"since": "2026-10-12T13:00", "governor": "performance"}, {"since": "2026-10-09", "governor": "powersave"}]}}}
        self.assertEqual(hosts.configuration("phuoc-yu", "2026-10-12T12:14:30", registry)["governor"], "powersave")
        self.assertEqual(hosts.configuration("phuoc-yu", "2026-10-12T13:00:00+02:00", registry)["governor"], "performance")
        self.assertIsNone(hosts.configuration("phuoc-yu", "2026-10-01", registry))
        self.assertEqual(hosts.run_identity({"host": "Phuoc-Yu"}, "2026-10-12T10:51:30", registry),
                         {"host": "phuoc-yu", "config": "2026-10-09", "governor": None})
        self.assertEqual(hosts.run_identity({"host": "laptop"}, "2026-10-12", registry)["config"], "-")

    def test_the_repository_registry_reads(self):
        registry = hosts.load()
        self.assertIn("build", registry["host"]["office-pc"]["roles"])
        for entry in registry["inputs"].values():
            self.assertFalse(entry["path"].startswith("/"), "inputs are relative to assets.root")


if __name__ == "__main__":
    unittest.main()
