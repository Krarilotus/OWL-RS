"""The answer recorder (suitekit/answers.py) on the cases of the answer replay of 10 October
2026: a COUNT typed xsd:int by one system, one deviating system among agreeing ones, an
unordered LIMIT, ordered and unordered answers, blank nodes, missing captures and an
adjudicated count; and the host registry (suitekit/hosts.py).

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

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


FIXTURES = Path(__file__).resolve().parent.parent / "fixtures" / "answer-levels.json"


class Levels(unittest.TestCase):
    """The answer contract's shared cases (fixtures/answer-levels.json, the one set every
    comparator passes): R16 check-a2's q02/q13 witnesses and its 1/3 counterexample, the COUNT
    datatype, decimal division with and without its exact oracle, the known-value route."""

    def test_every_case_of_the_contract(self):
        cases = json.loads(FIXTURES.read_text(encoding="utf-8"))
        wrong = []
        for case in cases["compare"]:
            order, limit = answers.order_of(case["query"])
            got = answers.compare(answers.parse(json.dumps(case["answer"])), answers.parse(json.dumps(case["reference"])),
                                  order, limit, ops=answers.operators(case["query"]),
                                  oracle=answers.oracle_of(case["oracle"]) if "oracle" in case else None)
            expected = case["expected"]
            note = expected.get("note")
            if (got["verdict"], got["level"]) != (expected["verdict"], expected["level"]) \
                    or (note is not None and (note not in got["note"] if note else got["note"])):
                wrong.append(f"{case['name']}: {got['verdict']}/{got['level']} ({got['note']!r})")
        for case in cases["known"]:
            got = answers.same_value(case["term"], case["known"])
            if got != case["expected"]:
                wrong.append(f"{case['name']}: {got}")
        self.assertEqual(wrong, [], "SPEC answer contract (rule.owl-rs-answer-comparison)")


class QueryNames(unittest.TestCase):
    def test_a_recorded_query_file_is_its_results_item(self):
        """answers.json records a query by its file, the results by the file's stem (the driver's
        item): answers.query_id turns the one into the other, without a directory, one `.rq`
        and nothing else (Sol, PR #56 compare r1: geographica2's `…_1_1.0` lost its `.0`; fix
        check 1: `q1.rq.rq` lost both)."""
        dotted = "00-Synthetic_Selection_Intersects_1_1.0"
        self.assertEqual([answers.query_id(n) for n in (f"queries/{dotted}.rq", "queries\\q1.rq.rq", "q1.rq")],
                         [dotted, "q1.rq", "q1"], "SPEC the item is the query file's stem")


class OracleCompare(unittest.TestCase):
    """benches/oracle/compare.py (NRESE against Jena on the differential tests' queries) compares
    terms through the contract: its own integer folding and 18-digit decimal truncation agreed
    where the contract says cross-type and decimal division without an oracle."""

    def test_the_differential_comparison_classifies_as_the_contract_does(self):
        spec_ = importlib.util.spec_from_file_location(
            "oracle_compare", Path(__file__).resolve().parents[2] / "oracle" / "compare.py")
        oracle_compare = importlib.util.module_from_spec(spec_)
        spec_.loader.exec_module(oracle_compare)
        x = "http://www.w3.org/2001/XMLSchema#"
        cases = {  # name: (query, NRESE's rows, Jena's rows)
            "q1": ("SELECT ?n WHERE { ?s <http://e/p> ?n }", f'"7"^^<{x}int>', f'"7"^^<{x}integer>'),
            "q2": ("SELECT ?g (AVG(?a) AS ?m) WHERE { ?s <http://e/g> ?g ; <http://e/a> ?a } GROUP BY ?g",
                   f'<http://e/1896>\t"26.186885245901639344"^^<{x}decimal>',
                   f'<http://e/1896>\t"26.186885245901639344262295"^^<{x}decimal>'),
            "q3": ("SELECT ?n WHERE { ?s <http://e/p> ?n }", f'"7"^^<{x}integer>', f'"8"^^<{x}integer>'),
            # T8 Sol r1 S2: an AVG over doubles is compared by value; two decimal AVGs that can't
            # both be one quotient at their own digits are no division to leave to an oracle.
            "q4": ("SELECT ?g (AVG(?w) AS ?m) WHERE { ?s <http://e/g> ?g ; <http://e/w> ?w } GROUP BY ?g",
                   f'<http://e/1896>\t"3.25E0"^^<{x}double>', f'<http://e/1896>\t"2.25E0"^^<{x}double>'),
            "q5": ("SELECT ?g (AVG(?a) AS ?m) WHERE { ?s <http://e/g> ?g ; <http://e/a> ?a } GROUP BY ?g",
                   f'<http://e/1896>\t"3.25"^^<{x}decimal>', f'<http://e/1896>\t"2.25"^^<{x}decimal>'),
        }
        with tempfile.TemporaryDirectory() as tmp:
            where = Path(tmp) / "dump" / "suite" / "case"
            where.mkdir(parents=True)
            for name, (query, nrese, jena) in cases.items():
                header = "?g\t?m" if "\t" in nrese else "?n"
                (where / f"{name}.rq").write_text(query + "\n", encoding="utf-8")
                (where / f"{name}.nrese").write_text(f"{header}\n{nrese}\n", encoding="utf-8")
                (where / f"{name}.jena").write_text(f"{header}\n{jena}\n", encoding="utf-8")
            report = Path(tmp) / "report.md"
            with patch("sys.stdout"):
                code = oracle_compare.main([str(Path(tmp) / "dump"), "--report", str(report)])
            text = report.read_text(encoding="utf-8")
        self.assertEqual(code, 1, "q3's different value stays unexplained")
        self.assertIn("**cross-type** (1)", text, "SPEC answer contract: an xsd:int for an xsd:integer isn't agreement")
        self.assertIn("**division** (1)", text, "SPEC answer contract: no 18-digit truncation qualifies a division")
        unexplained = text.split("## Unexplained")[1]
        self.assertEqual([q for q in ("q3", "q4", "q5") if f"suite/case/{q}" not in unexplained], [],
                         "SPEC answer contract: a different value is unexplained, a division's result too")

    def test_every_shared_case_an_oracle_doesnt_decide(self):
        """T8 PR #56 R1: equal rows counted as an agreement before the contract saw them, so a COUNT
        in xsd:int agreed. Every shared case without an exact oracle (which this comparison has
        none of) goes through it: its agreements, deviations, divisions and differences are the
        contract's."""
        spec_ = importlib.util.spec_from_file_location(
            "oracle_compare", Path(__file__).resolve().parents[2] / "oracle" / "compare.py")
        oracle_compare = importlib.util.module_from_spec(spec_)
        spec_.loader.exec_module(oracle_compare)

        def nt(term: dict) -> str:
            if term["type"] == "uri":
                return f"<{term['value']}>"
            if term["type"] == "bnode":
                return f"_:{term['value']}"
            if "xml:lang" in term:
                return f'"{term["value"]}"@{term["xml:lang"]}'
            return f'"{term["value"]}"^^<{term["datatype"]}>' if term.get("datatype") else f'"{term["value"]}"'

        bucket = {"ok": "agree", "deviates": "cross-type", "unqualified": "division", "wrong": "a difference"}
        wrong = []
        for case in json.loads(FIXTURES.read_text(encoding="utf-8"))["compare"]:
            if "oracle" in case:
                continue
            names = case["answer"]["head"]["vars"]
            rows = lambda answer: "\n".join("\t".join(nt(b[v]) for v in names) for b in answer["results"]["bindings"])
            with tempfile.TemporaryDirectory() as tmp:
                where = Path(tmp) / "dump" / "suite" / "case"
                where.mkdir(parents=True)
                header = "\t".join(f"?{v}" for v in names)
                (where / "q1.rq").write_text(case["query"] + "\n", encoding="utf-8")
                (where / "q1.nrese").write_text(f"{header}\n{rows(case['answer'])}\n", encoding="utf-8")
                (where / "q1.jena").write_text(f"{header}\n{rows(case['reference'])}\n", encoding="utf-8")
                report = Path(tmp) / "report.md"
                with patch("sys.stdout"):
                    oracle_compare.main([str(Path(tmp) / "dump"), "--report", str(report)])
                text = report.read_text(encoding="utf-8")
            got = next((b for b in ("cross-type", "division", "blank-nodes") if f"**{b}** (1)" in text), None) \
                or ("agree" if "1 agree" in text.split("## Explained")[0] else "a difference")
            if got != bucket[case["expected"]["verdict"]]:
                wrong.append(f"{case['name']}: {got}")
        self.assertEqual(wrong, [], "SPEC answer contract: the oracle's verdicts are the contract's")


class SameAnswers(unittest.TestCase):
    """benches/sparql/same_answers.py (two runs' answers, system by system) compares through the
    contract with each query's operators (T8 Sol r1 S5): without them two conformant lengths of
    one AVG read as a wrong answer. Operators a run didn't record (answers.json from before T8)
    are unknown, not absent (Sol r2 S5): such a difference stays undecided."""

    def test_two_runs_compare_with_the_querys_operators(self):
        spec_ = importlib.util.spec_from_file_location(
            "same_answers", Path(__file__).resolve().parents[2] / "sparql" / "same_answers.py")
        same_answers = importlib.util.module_from_spec(spec_)
        spec_.loader.exec_module(same_answers)
        item = ["w", "t", "none", "q13.rq"]
        records = {"recorded": ({"items": [{"item": item, "operators": {"avg": "division"}}]}, "unqualified"),
                   "not recorded": ({"items": [{"item": item}]}, "unqualified"),
                   "no items": ({}, "unqualified"),
                   "recorded as none": ({"items": [{"item": item, "operators": {}}]}, "wrong")}
        got = {}
        for name, (record, _) in records.items():
            with tempfile.TemporaryDirectory() as tmp:
                runs = []
                for side, avg in (("a", "26.186885245901639344"), ("b", "26.186885245901639344262295")):
                    run = Path(tmp) / side
                    run.mkdir()
                    body = select(["games", "avg"], [iri("1896"), lit(avg, "decimal")])
                    (run / "q13.json").write_text(body, encoding="utf-8")
                    (run / "answers.json").write_text(json.dumps(record | {
                        "captures": [{"item": item, "system": "nrese", "cache": "off", "run": 1, "path": "q13.json",
                                      "sha256": hashlib.sha256(body.encode()).hexdigest()}]}), encoding="utf-8")
                    runs.append(str(run))
                with patch("sys.stdout", new_callable=io.StringIO) as out:
                    same_answers.main(["same_answers.py", *runs])
            line = next(l for l in out.getvalue().splitlines() if "q13.rq" in l)
            got[name] = line.split("q13.rq: ")[1].split()[0]
        self.assertEqual(got, {name: verdict for name, (_, verdict) in records.items()},
                         "SPEC answer contract: an AVG without its oracle is undecided, also where the operators "
                         "are unknown; known to have none, the decimals are data and differ")


class Compare(unittest.TestCase):
    def test_an_int_count_deviates_and_names_the_datatype(self):
        reference = parse(["n"], [lit(300, "integer")])
        qlever = parse(["n"], [lit(300, "int")])
        verdict = answers.compare(qlever, reference, ops=answers.operators("SELECT (COUNT(*) AS ?n) { ?s ?p ?o }"))
        self.assertEqual((verdict["verdict"], verdict["value_match"]), ("deviates", False))
        self.assertIn("COUNT not xsd:integer: ?n xsd:int", verdict["note"])

    def test_values_compare_within_one_datatype(self):
        integer = parse(["n"], [lit(1, "integer")])
        self.assertEqual(answers.compare(parse(["n"], [lit("01", "integer")]), integer)["level"], "value")
        self.assertEqual(answers.compare(parse(["n"], [lit("1.0", "decimal")]), integer)["verdict"], "deviates")
        self.assertEqual(answers.compare(parse(["n"], [lit("1.0E0", "double")]), integer)["verdict"], "deviates")
        self.assertEqual(answers.compare(parse(["n"], [lit(2, "int")]), integer)["verdict"], "wrong")
        self.assertTrue(answers.compare(parse(["s"], [lit("a", "string")]), parse(["s"], [lit("a")]))["value_match"])

    def test_the_operators_of_a_query(self):
        query = ('SELECT ?g (COUNT(DISTINCT ?a) AS ?n) (AVG(?age) AS ?avg) (?x / 2 AS ?half) (STR(<http://a/b>) AS ?s)'
                 ' WHERE { ?a <http://e/g> ?g }')
        self.assertEqual(answers.operators(query), {"n": "count", "avg": "division", "half": "division"})
        # T8 Sol r1 S3: only a COUNT's own result must be xsd:integer.
        self.assertEqual(answers.operators("SELECT (COUNT(*) / 2 AS ?half) (COUNT(?x) + 1 AS ?m) WHERE { }"),
                         {"half": "division"}, "SPEC answer contract: a COUNT divided is a division")

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
    def test_failed_measurement_is_retained_without_recapturing_or_expanding_limit(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            query = directory / "q.rq"
            text = "SELECT ?x { ?x ?p ?o } LIMIT 1"
            query.write_text(text, encoding="utf-8")
            recorder = answers.Recorder(directory, "jena")
            item = ("w", "t", "none", query.name)
            post = Mock(return_value=(200, select(["x"], [iri("a")])))
            recorder.capture(post, "http://fixture", item, "jena", "off", 1, query, directory, 600,
                             prior_error="timeout after 600 s")
            post.assert_not_called()
            self.assertEqual(recorder.queries[item], hashlib.sha256(text.encode()).hexdigest())
            self.assertEqual(recorder.orders[item], ("limit", 1))
            self.assertEqual(len(recorder.captures), 1)
            self.assertEqual(recorder.captures[0].run, 1)
            self.assertFalse(recorder.captures[0].unlimited)
            self.assertIsNone(recorder.captures[0].sha256)
            again = answers.Recorder.load([recorder.write()])
            self.assertEqual(again.verdicts(), recorder.verdicts())
            verdict = again.verdicts()[0]
            self.assertEqual(verdict["verdict"], "incomplete")
            self.assertIn("timeout after 600 s", verdict["note"])

    def test_old_success_does_not_hide_a_failed_later_invocation(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            query = directory / "q.rq"
            query.write_text("SELECT ?x { ?x ?p ?o }", encoding="utf-8")
            recorder = answers.Recorder(directory, "jena")
            item = ("w", "t", "none", query.name)
            post = Mock(return_value=(200, select(["x"], [iri("a")])))
            recorder.capture(post, "http://fixture", item, "jena", "off", 1, query, directory, 600)
            recorder.capture(post, "http://fixture", item, "jena", "off", 2, query, directory, 600,
                             prior_error="timeout")
            self.assertEqual(post.call_count, 1)
            self.assertEqual([c.run for c in recorder.captures], [1, 2])
            self.assertEqual(recorder.verdicts()[0]["verdict"], "incomplete")

    def test_driver_captures_successes_but_does_not_reexecute_failed_queries(self):
        from suitekit import driver
        from suitekit.workloads import Plan
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            queries = directory / "queries"
            queries.mkdir()
            for name in ("failed", "good"):
                (queries / f"{name}.rq").write_text("SELECT ?x { ?x ?p ?o }", encoding="utf-8")
            suite = driver.Suite.__new__(driver.Suite)
            suite.args = SimpleNamespace(dry_run=False, query_runs=1, query_timeout_s=600, order="fixed")
            suite.answers = answers.Recorder(directory, "jena")
            suite.harness = lambda: "fixture-harness"
            suite.emit = Mock()
            mix = {"queries": [{"id": "failed", "error": "timeout after 600 s"},
                               {"id": "good", "rows": 1, "first_ms": 1, "latencies_ms": [1]}]}
            suite.host_command = Mock(side_effect=lambda command, log:
                                      (directory / "queries-cache-off.json").write_text(json.dumps(mix)))
            post = Mock(return_value=(200, select(["x"], [iri("a")])))
            with patch.object(driver, "capped_post", post):
                suite.queries(SimpleNamespace(logs=directory), Plan("w", "t", queries=queries), "jena",
                              SimpleNamespace(answers_only=False), SimpleNamespace(query="http://fixture"),
                              {"regime": "none", "cache": "off"}, 1)
            self.assertEqual(post.call_count, 1)
            self.assertEqual(len(suite.answers.captures), 2)
            verdicts = {v["query"]: v for v in suite.answers.verdicts()}
            self.assertEqual(verdicts["failed.rq"]["verdict"], "incomplete")
            self.assertEqual(verdicts["good.rq"]["verdict"], "reference")
            self.assertTrue(any(call.kwargs.get("status") == "timeout" for call in suite.emit.call_args_list))

    def test_invalid_reports_are_preserved_and_never_launch_answer_recapture(self):
        from suitekit import driver
        from suitekit.workloads import Plan
        good = {"id": "q", "rows": 1, "first_ms": 1, "latencies_ms": [1]}
        for case, records in (("stale", [good]), ("missing", []), ("duplicate", [good, good]),
                              ("unfinished", [{**good, "latencies_ms": []}]), ("wrong-count", [good])):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as tmp:
                directory = Path(tmp)
                query_dir = directory / "queries"
                query_dir.mkdir()
                (query_dir / "q.rq").write_text("SELECT ?x { ?x ?p ?o }", encoding="utf-8")
                report = directory / "queries-cache-off.json"
                encoded = json.dumps({"queries": records})
                if case == "stale":
                    report.write_text(encoded, encoding="utf-8")
                suite = driver.Suite.__new__(driver.Suite)
                suite.args = SimpleNamespace(dry_run=False, query_runs=1, query_timeout_s=600, order="fixed")
                suite.answers = answers.Recorder(directory, "jena")
                suite.harness = lambda: "fixture-harness"
                suite.emit = Mock()
                suite.host_command = Mock(side_effect=lambda command, log: report.write_text(encoded, encoding="utf-8"))
                post = Mock(return_value=(200, select(["x"], [iri("a")])))
                with patch.object(driver, "capped_post", post):
                    suite.queries(SimpleNamespace(logs=directory),
                                  Plan("w", "t", queries=query_dir, expected={"q": 2},
                                       clients=(4,) if case != "wrong-count" else ()), "jena",
                                  SimpleNamespace(answers_only=False), SimpleNamespace(query="http://fixture"),
                                  {"regime": "none", "cache": "off"}, 1)
                self.assertEqual(post.call_count, int(case == "wrong-count"))
                self.assertEqual(suite.host_command.call_count, int(case != "stale"))
                self.assertEqual(report.read_text(encoding="utf-8"), encoded)
                self.assertEqual(len(suite.answers.captures), 1)
                if case == "wrong-count":
                    self.assertTrue(any(c.kwargs.get("status") == "wrong" for c in suite.emit.call_args_list))
                else:
                    self.assertEqual(suite.answers.verdicts()[0]["verdict"], "incomplete")

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
            # The same number in another datatype: deviates, named, not timed (the answer contract).
            self.assertEqual(group["deviates"], ["qlever"])
            self.assertIn("xsd:int for xsd:integer", group["systems"]["qlever"]["note"])
            self.assertIn("deviates: qlever", answers.table(groups))

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
