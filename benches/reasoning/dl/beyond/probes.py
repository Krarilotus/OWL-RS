"""Minimal probes of how the references read DL-safe rules: one functional-syntax document
a line (`index<TAB>document`), each with the answer SWRL's semantics gives in a comment
list printed to stderr."""
import sys

P = ("Prefix(:=<http://example.org/scope#>) Prefix(owl:=<http://www.w3.org/2002/07/owl#>) "
     "Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>) ")
X = "Variable(<urn:swrl#x>)"
Y = "Variable(<urn:swrl#y>)"
probes = [
    ("empty head", f"ClassAssertion(:A :a) DLSafeRule(Body(ClassAtom(:A {X})) Head())", "i"),
    ("head x != x", f"ClassAssertion(:A :a) DLSafeRule(Body(ClassAtom(:A {X})) Head(DifferentIndividualsAtom({X} {X})))", "i"),
    ("head Nothing (control)", f"ClassAssertion(:A :a) DLSafeRule(Body(ClassAtom(:A {X})) Head(ClassAtom(owl:Nothing {X})))", "i"),
    ("declared individual only", f"Declaration(NamedIndividual(:a)) DLSafeRule(Body(SameIndividualAtom({X} {X})) Head(ClassAtom(owl:Nothing {X})))", "i"),
    ("body x = x only", f"ClassAssertion(:A :a) DLSafeRule(Body(SameIndividualAtom({X} {X})) Head(ClassAtom(owl:Nothing {X})))", "i"),
    ("inverse property atom", f"Declaration(ObjectProperty(:r)) ObjectPropertyAssertion(:r :a :b) DLSafeRule(Body(ObjectPropertyAtom(ObjectInverseOf(:r) {X} {Y})) Head(ClassAtom(owl:Nothing {X})))", "i"),
    ("body x = x, head x != x", f"ClassAssertion(:A :a) DLSafeRule(Body(SameIndividualAtom({X} {X})) Head(DifferentIndividualsAtom({X} {X})))", "i"),
    ("body x = y, head x != y", f"ClassAssertion(:A :a) DLSafeRule(Body(SameIndividualAtom({X} {Y})) Head(DifferentIndividualsAtom({X} {Y})))", "i"),
    ("anonymous elements not bound (control)", f"Declaration(ObjectProperty(:r)) ClassAssertion(owl:Thing :a) SubClassOf(owl:Thing ObjectSomeValuesFrom(:r :A)) DLSafeRule(Body(ClassAtom(:A {X})) Head(ClassAtom(owl:Nothing {X})))", "c"),
]
for k, (what, axioms, expected) in enumerate(probes):
    print(f"{k}\t{P}Ontology(<urn:probe:{k}> {axioms})")
    print(f"{k}\t{expected}\t{what}", file=sys.stderr)
