"""The Datalog kit's translations (benches/reasoning/datalog/program.py): one program, read
once, written for NRESE (Notation3 over triples), Soufflé, Nemo and VLog. A translation
that drops or weakens a construct would make the engines compute different things, so what
an engine can't express is refused, never approximated. The answers themselves are checked
at run time (`datalog.py run --qualify`) against Soufflé's published counts.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

KIT = Path(__file__).resolve().parents[2] / "reasoning" / "datalog"
sys.path.insert(0, str(KIT))

import program as P  # noqa: E402

TYPE = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>"


def flowlog(name: str) -> P.Program:
    return P.parse((KIT / "programs" / "flowlog" / f"{name}.dl").read_text(encoding="utf-8"))


class FlowLogPrograms(unittest.TestCase):
    def test_transitive_closure_in_every_language(self):
        tc = flowlog("tc")
        self.assertEqual(tc.inputs, {"Arc": "Arc.csv"})
        self.assertEqual(tc.derived, ["Tc"])
        self.assertIn("{ ?x <r:Arc> ?y } => { ?x <r:Tc> ?y } .", P.to_n3(tc))
        self.assertIn("{ ?z <r:Arc> ?y . ?x <r:Tc> ?z } => { ?x <r:Tc> ?y } .", P.to_n3(tc))
        self.assertIn("Tc(?x, ?y) :- Arc(?z, ?y), Tc(?x, ?z) .", P.to_nemo(tc))
        self.assertIn('@import Arc :- csv{resource="Arc.csv", format=(int, int)} .', P.to_nemo(tc))
        self.assertIn(".printsize Tc", P.to_souffle(tc))
        rules, conf = P.to_vlog(tc, "/data")
        self.assertIn("Tc(Vx,Vy) :- Arc(Vz,Vy),Tc(Vx,Vz)", rules)
        self.assertIn("EDB0_param1=Arc", conf)

    def test_inequality_is_notEqualTo_and_refused_by_vlog(self):
        sg = flowlog("sg")
        self.assertIn("?x log:notEqualTo ?y", P.to_n3(sg))
        self.assertIn("?x != ?y", P.to_nemo(sg))
        with self.assertRaises(P.NotExpressible):
            P.to_vlog(sg, "/data")

    def test_higher_arity_and_negation_are_not_expressible_in_triples(self):
        with self.assertRaisesRegex(P.NotExpressible, "arity 3"):
            P.to_n3(flowlog("galen"))
        doop = flowlog("batik")
        self.assertTrue(any(isinstance(b, P.Atom) and b.negated for r in doop.rules for b in r.body))
        self.assertIn("!MethodImplemented(", P.to_souffle(doop))
        self.assertIn("~MethodImplemented(", P.to_nemo(doop))
        with self.assertRaises(P.NotExpressible):
            P.to_n3(doop)

    def test_a_constant_binding_becomes_the_constants_iri(self):
        program = P.parse(".in\n.decl E(x: number, y: number)\n.input E.csv\n.printsize\n.decl R(x: number)\n"
                          ".rule\nR(t) :- E(t, s), t = 7.\n")
        self.assertIn("{ <n:7> <r:E> ?s } => { <n:7> " + TYPE + " <r:R> } .", P.to_n3(program))

    def test_rdfox_runs_the_same_triples_as_nrese(self):
        rules = P.to_rdfox(flowlog("sg"))
        self.assertIn("[?x, <r:Sg>, ?y] :- [?a, <r:Arc>, ?x], [?a, <r:Arc>, ?y], FILTER(?x != ?y) .", rules)
        with self.assertRaises(P.NotExpressible):
            P.to_rdfox(flowlog("galen"))

    def test_input_rows_become_triples(self):
        self.assertEqual(list(P.ntriples("Arc", [["1", "2"]], 2)), ["<n:1> <r:Arc> <n:2> .\n"])
        self.assertEqual(list(P.ntriples("Source", [["5"]], 1)), [f"<n:5> {TYPE} <r:Source> .\n"])


TRIPLES = """
RP0(A) :- TE(A,rdf:type,<http://e/Person>)
RP1(A,B) :- TE(A,<http://e/knows>,B)
RP2(A) :- TE(A,rdf:type,<http://e/Nothing>)
RP0(X) :- RP1(X,Y)
RP1(X,Z) :- RP1(X,Y),RP1(Y,Z)
RP2(<http://e/contradiction>) :- RP1(X,X)
"""


class TriplePrograms(unittest.TestCase):
    def setUp(self):
        self.program = P.parse_triples(TRIPLES)

    def test_relations_are_their_triple_patterns(self):
        self.assertEqual(self.program.patterns["RP1"], ("prop", "<http://e/knows>"))
        n3 = P.to_n3(self.program)
        self.assertIn(f"{{ ?X <http://e/knows> ?Y }} => {{ ?X {TYPE} <http://e/Person> }} .", n3)
        self.assertIn(f"=> {{ <http://e/contradiction> {TYPE} <http://e/Nothing> }} .", n3)
        self.assertEqual(P.count_query(self.program, "RP1"),
                         "SELECT (COUNT(*) AS ?n) WHERE { ?a <http://e/knows> ?b }")
        self.assertIn(f"[<http://e/contradiction>, {TYPE}, <http://e/Nothing>] :- [?X, <http://e/knows>, ?X] .",
                      P.to_rdfox(self.program))

    def test_the_other_engines_bind_each_relation_to_its_pattern(self):
        self.assertIn(f'RP0(a) :- TE(a, "{TYPE}", "<http://e/Person>").', P.to_souffle(self.program))
        self.assertIn('RP2("<http://e/contradiction>") :- RP1(X, X).', P.to_souffle(self.program))
        self.assertIn(f"RP1(?a, ?b) :- TE(?a, <http://e/knows>, ?b) .", P.to_nemo(self.program))
        rules, conf = P.to_vlog(self.program, "/data")
        self.assertIn("RP0(A) :- TE(A,rdf:type,<http://e/Person>)", rules)
        self.assertIn("RP2(<http://e/contradiction>) :- RP1(VX,VX)", rules)
        self.assertIn("EDB0_param0=/data/trident", conf)

    def test_a_relation_without_a_pattern_is_refused(self):
        with self.assertRaisesRegex(ValueError, "without a triple pattern"):
            P.parse_triples("RP0(A) :- TE(A,rdf:type,<http://e/C>)\nRP0(X) :- RP9(X)\n")

    def test_ntriples_terms_keep_a_literal_with_spaces(self):
        self.assertEqual(P.ntriples_terms('<http://e/a> <http://e/p> "a b ." .\n'),
                         ("<http://e/a>", "<http://e/p>", '"a b ."'))


if __name__ == "__main__":
    unittest.main()
