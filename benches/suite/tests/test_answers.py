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
        self.assertEqual(verdict["verdict"], "match")
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
        self.assertEqual(verdict["verdict"], "mismatch")
        self.assertIn("another order", verdict["note"])

    def test_an_unordered_limit_accepts_any_legal_subset(self):
        full = parse(["x"], [iri("a")], [iri("b")], [iri("c")])
        reference = parse(["x"], [iri("a")], [iri("b")])
        legal = answers.compare(parse(["x"], [iri("b")], [iri("c")]), reference, "limit", 2, full)
        self.assertEqual(legal["verdict"], "match")
        foreign = answers.compare(parse(["x"], [iri("a")], [iri("d")]), reference, "limit", 2, full)
        self.assertEqual(foreign["verdict"], "mismatch")
        short = answers.compare(parse(["x"], [iri("a")]), reference, "limit", 2, full)
        self.assertEqual(short["verdict"], "mismatch")
        self.assertEqual(answers.compare(reference, reference, "limit", 2, None)["verdict"], "incomplete")

    def test_blank_nodes_are_left_to_adjudication(self):
        blank = parse(["x"], [{"type": "bnode", "value": "b0"}])
        self.assertEqual(answers.compare(blank, blank)["verdict"], "unqualified")

    def test_ask(self):
        yes, no = answers.parse('{"boolean": true}'), answers.parse('{"boolean": false}')
        self.assertTrue(answers.compare(yes, yes)["strict_match"])
        self.assertEqual(answers.compare(no, yes)["verdict"], "mismatch")
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
        self.assertEqual(verdicts["nrese"]["verdict"], "match")
        self.assertTrue(verdicts["nrese"]["strict_match"])
        self.assertEqual(verdicts["qlever"]["verdict"], "mismatch")

    def test_the_reference_answers_the_query_without_its_limit_too(self):
        q, full = "SELECT ?x { ?x ?p ?o } LIMIT 2", "SELECT ?x { ?x ?p ?o }"
        rows = [[iri("a")], [iri("b")], [iri("c")]]
        bodies = {"jena": {q: select(["x"], *rows[:2]), full: select(["x"], *rows)},
                  "nrese": {q: select(["x"], *rows[1:])}}
        verdicts = self.run_capture(bodies, q)
        self.assertEqual(verdicts["nrese"]["verdict"], "match")
        self.assertEqual(verdicts["nrese"]["order"], "limit")

    def test_missing_captures_and_a_failed_reference_are_incomplete(self):
        q = "SELECT ?x { ?x ?p ?o }"
        verdicts = self.run_capture({"jena": {}, "nrese": {q: select(["x"], [iri("a")])}}, q,
                                    expected=["jena", "nrese", "oxigraph"])
        self.assertEqual(verdicts["nrese"]["verdict"], "incomplete")
        self.assertEqual(verdicts["oxigraph"]["note"], "not captured")

    def test_a_reference_against_an_adjudicated_count_qualifies_nothing(self):
        q = "SELECT ?x { ?x ?p ?o }"
        body = select(["x"], [iri("a")])
        verdicts = self.run_capture({"jena": {q: body}, "nrese": {q: body}}, q, adjudicated={("w", "t", "q1"): "2"})
        self.assertEqual(verdicts["nrese"]["verdict"], "unqualified")
        self.assertIn("adjudicated", verdicts["nrese"]["note"])


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

    def test_the_repository_registry_reads(self):
        registry = hosts.load()
        self.assertIn("build", registry["host"]["office-pc"]["roles"])
        for entry in registry["inputs"].values():
            self.assertFalse(entry["path"].startswith("/"), "inputs are relative to assets.root")


if __name__ == "__main__":
    unittest.main()
