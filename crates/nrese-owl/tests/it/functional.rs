//! The functional-syntax reader (`nrese_owl::ofn`): every construct of the grammar read
//! into the model the RDF reader builds, unreadable text reported with its line and
//! column (and the rest still read), documents the writer makes read back to the same
//! model, and the model of a document going to triples and back.

use std::collections::BTreeMap;

use nrese_owl::{
    Axiom, Diagnostic, Intern, Ontology, Statement, Term, iri_text, literal_text, read,
    read_functional, write,
};
use nrese_rdf::{BlankNode, Literal, NamedNode, Term as RdfTerm};

use super::{Table, assert_round_trip, rendered};

impl Intern for Table {
    fn iri_id(&mut self, iri: &str) -> Term {
        Table::iri_id(self, iri)
    }

    fn literal_id(&mut self, lexical: &str, datatype: &str, language: Option<&str>) -> Term {
        let literal = match language {
            Some(tag) => Literal::new_language_tagged_literal(lexical, tag).unwrap_or_else(|_| {
                Literal::new_language_tagged_literal_unchecked(lexical, tag.to_lowercase())
            }),
            None => Literal::new_typed_literal(lexical, NamedNode::new_unchecked(datatype)),
        };
        self.id(literal.into())
    }

    fn blank_id(&mut self, label: &str, document: u32) -> Term {
        let label = if document == 0 {
            label.to_owned()
        } else {
            format!("{label}d{document}")
        };
        self.id(BlankNode::new_unchecked(label).into())
    }
}

/// A term as the functional syntax writes it.
pub fn ofn_name(table: &Table, t: Term) -> String {
    match &table.terms[t as usize] {
        RdfTerm::NamedNode(n) => iri_text(n.as_str()),
        RdfTerm::BlankNode(b) => format!("_:{}", b.as_str()),
        RdfTerm::Literal(l) => literal_text(l.value(), Some(l.datatype().as_str()), l.language()),
        other => panic!("not an OWL term: {other}"),
    }
}

/// The ontology written as a document and read back: the same model, no diagnostic of the
/// syntax.
pub fn assert_functional_round_trip(o: &Ontology, table: &mut Table, what: &str) {
    let text = o.functional_document(Some("http://example.com/o"), &|t| ofn_name(table, t));
    let (back, header) = read_functional(&text, table);
    let syntax: Vec<&Diagnostic> = back
        .diagnostics
        .iter()
        .filter(|d| matches!(d, Diagnostic::Syntax { .. }))
        .collect();
    assert!(syntax.is_empty(), "{what}: {syntax:?}\n{text}");
    assert_eq!(header.iri.as_deref(), Some("http://example.com/o"));
    let (before, after) = (rendered(o, table), rendered(&back, table));
    if before != after {
        let missing: Vec<&String> = before.iter().filter(|l| !after.contains(l)).collect();
        let extra: Vec<&String> = after.iter().filter(|l| !before.contains(l)).collect();
        panic!(
            "{what}: the functional round trip differs\nmissing: {missing:#?}\nextra: {extra:#?}\n{text}"
        );
    }
    assert_eq!(o.anonymous, back.anonymous, "{what}");
}

/// The model with each n-ary equivalence (classes, properties, individuals) as the
/// binary ones of its RDF form (the first operand with each other one), which is what
/// the triples read back give.
pub fn binary(o: &Ontology) -> Ontology {
    let mut out = o.clone();
    let mut axioms: Vec<Axiom> = Vec::new();
    for a in &o.axioms {
        match a {
            Axiom::EquivalentClasses(xs) if xs.len() > 2 => axioms.extend(
                xs[1..]
                    .iter()
                    .map(|&x| Axiom::EquivalentClasses(sorted(vec![xs[0], x]))),
            ),
            Axiom::EquivalentObjectProperties(xs) if xs.len() > 2 => axioms.extend(
                xs[1..]
                    .iter()
                    .map(|&x| Axiom::EquivalentObjectProperties(sorted(vec![xs[0], x]))),
            ),
            Axiom::EquivalentDataProperties(xs) if xs.len() > 2 => axioms.extend(
                xs[1..]
                    .iter()
                    .map(|&x| Axiom::EquivalentDataProperties(sorted(vec![xs[0], x]))),
            ),
            Axiom::SameIndividual(xs) if xs.len() > 2 => axioms.extend(
                xs[1..]
                    .iter()
                    .map(|&x| Axiom::SameIndividual(sorted(vec![xs[0], x]))),
            ),
            other => axioms.push(other.clone()),
        }
    }
    axioms.sort();
    axioms.dedup();
    out.sources = vec![Vec::new(); axioms.len()];
    out.axioms = axioms;
    out
}

fn sorted<T: Ord>(mut v: Vec<T>) -> Vec<T> {
    v.sort();
    v.dedup();
    v
}

/// The model of a document to triples and back: the same (n-ary equivalences as their
/// binary RDF forms): the way a store takes a functional-syntax ontology.
pub fn assert_triples_round_trip(o: &Ontology, table: &mut Table) {
    assert_round_trip(&binary(o), table);
}

const EVERYTHING: &str = r#"Prefix(:=<http://example.com/>)
Prefix(ex:=<http://example.com/>)
# A comment; every construct of the grammar once.
Ontology(<http://example.com/o> <http://example.com/o/1>
Import(<http://example.com/other>)
Annotation(rdfs:label "an ontology"@en)
Declaration(Class(:A)) Declaration(Class(:B)) Declaration(Class(:C))
Declaration(ObjectProperty(:r)) Declaration(ObjectProperty(:s)) Declaration(ObjectProperty(:t)) Declaration(ObjectProperty(:u))
Declaration(DataProperty(:d)) Declaration(DataProperty(:e)) Declaration(Datatype(:D))
Declaration(NamedIndividual(:a)) Declaration(NamedIndividual(:b))
Declaration(Annotation(:note "declared") AnnotationProperty(:note))
SubClassOf(Annotation(:note "why") :A ObjectIntersectionOf(:B ObjectUnionOf(:C ObjectComplementOf(:A))))
EquivalentClasses(:A ObjectSomeValuesFrom(ObjectInverseOf(:r) owl:Thing) ObjectAllValuesFrom(:s owl:Nothing))
DisjointClasses(:A :B :C)
DisjointUnion(:A :C :B)
SubClassOf(:B ObjectOneOf(:a _:x))
SubClassOf(:B ObjectHasValue(:r :a))
SubClassOf(:B ObjectHasSelf(:r))
SubClassOf(:B ObjectMinCardinality(2 :r :C))
SubClassOf(:B ObjectMaxCardinality(1 :r))
SubClassOf(:B ObjectExactCardinality(3 :s :A))
SubClassOf(:C DataSomeValuesFrom(:d DataIntersectionOf(xsd:integer DatatypeRestriction(xsd:integer xsd:minInclusive "1"^^xsd:integer xsd:maxExclusive "9"^^xsd:integer))))
SubClassOf(:C DataAllValuesFrom(:d DataUnionOf(xsd:string DataComplementOf(rdfs:Literal))))
SubClassOf(:C DataHasValue(:e "q\"uote\\d"))
SubClassOf(:C DataMinCardinality(1 :d))
SubClassOf(:C DataMaxCardinality(2 :d DataOneOf("1"^^xsd:integer "x"@EN "y@de"^^rdf:PlainLiteral)))
SubClassOf(:C DataExactCardinality(1 :e xsd:boolean))
SubObjectPropertyOf(:r :s)
SubObjectPropertyOf(ObjectPropertyChain(:r ObjectInverseOf(:s)) :t)
EquivalentObjectProperties(:r :s)
DisjointObjectProperties(:r ObjectInverseOf(:s))
InverseObjectProperties(:u :t)
ObjectPropertyDomain(:r :A) ObjectPropertyRange(ObjectInverseOf(:r) :B)
FunctionalObjectProperty(:r) InverseFunctionalObjectProperty(:r) ReflexiveObjectProperty(:r)
IrreflexiveObjectProperty(:s) SymmetricObjectProperty(:r) AsymmetricObjectProperty(:s)
TransitiveObjectProperty(:t)
SubDataPropertyOf(:d :e) EquivalentDataProperties(:d :e) DisjointDataProperties(:d :e)
DataPropertyDomain(:d :A) DataPropertyRange(:d xsd:integer) FunctionalDataProperty(:d)
DatatypeDefinition(:D DatatypeRestriction(xsd:string xsd:maxLength "3"^^xsd:integer))
HasKey(:A (:r ObjectInverseOf(:s)) (:d))
HasKey(:B () (:e))
SameIndividual(:a :b) DifferentIndividuals(:a :b _:x)
ClassAssertion(:A :a)
ObjectPropertyAssertion(:r :a :b) ObjectPropertyAssertion(ObjectInverseOf(:r) :a _:x)
NegativeObjectPropertyAssertion(:s :a :b)
DataPropertyAssertion(:d :a "5"^^xsd:integer) NegativeDataPropertyAssertion(:d :b "6"^^<http://www.w3.org/2001/XMLSchema#integer>)
AnnotationAssertion(:note :a "a note")
AnnotationAssertion(Annotation(:note "nested") :note _:x <http://example.com/x>)
SubAnnotationPropertyOf(:note rdfs:comment) AnnotationPropertyDomain(:note :A)
AnnotationPropertyRange(:note xsd:string)
)
"#;

/// Every construct of the functional syntax, read as the RDF reader reads its RDF form.
#[test]
fn every_functional_construct_is_read() {
    let mut table = Table::default();
    let (o, header) = read_functional(EVERYTHING, &mut table);
    assert!(o.diagnostics.is_empty(), "{:?}", o.diagnostics);
    assert_eq!(header.iri.as_deref(), Some("http://example.com/o"));
    assert_eq!(header.version.as_deref(), Some("http://example.com/o/1"));
    assert_eq!(header.imports, vec!["http://example.com/other".to_owned()]);
    // One on the ontology, two on axioms (one in a declaration), one nested, and five
    // annotation axioms.
    assert_eq!(o.annotations, 9);
    let lines = rendered(&o, &table);
    let declarations = lines
        .iter()
        .filter(|l| l.starts_with("Declaration"))
        .count();
    assert_eq!(declarations, 13);
    assert_eq!(lines.len() - declarations, 47, "{}", lines.join("\n"));
    for expected in [
        "SubClassOf(ex:A ObjectIntersectionOf(ObjectUnionOf(ObjectComplementOf(ex:A) ex:C) ex:B))",
        "EquivalentClasses(ObjectAllValuesFrom(ex:s owl:Nothing) ObjectSomeValuesFrom(ObjectInverseOf(ex:r) owl:Thing) ex:A)",
        "DisjointUnion(ex:A ex:B ex:C)",
        "SubClassOf(ex:B ObjectMaxCardinality(1 ex:r owl:Thing))",
        "SubClassOf(ex:C DataMinCardinality(1 ex:d rdfs:Literal))",
        "SubObjectPropertyOf(ObjectPropertyChain(ex:r ObjectInverseOf(ex:s)) ex:t)",
        "InverseObjectProperties(ex:t ex:u)",
        "HasKey(ex:B () (ex:e))",
        "ObjectPropertyAssertion(ex:r ex:a ex:b)",
    ] {
        assert!(
            lines.contains(&expected.to_owned()),
            "missing {expected}\n{}",
            lines.join("\n")
        );
    }
    // The inverse's assertion is stored swapped, over the anonymous individual.
    let x = table.ids[&RdfTerm::BlankNode(BlankNode::new_unchecked("x"))];
    let r = table.iri_id("http://example.com/r");
    let a = table.iri_id("http://example.com/a");
    assert!(o.axioms.contains(&Axiom::ObjectPropertyAssertion(r, x, a)));
    assert!(o.anonymous.contains(&x));
    // Literals: the escapes, the language tags (in lower case), rdf:PlainLiteral's form.
    let literals: BTreeMap<String, (Option<String>, Option<String>)> = o
        .data
        .literals
        .values()
        .map(|l| (l.lexical.clone(), (l.datatype.clone(), l.language.clone())))
        .collect();
    assert_eq!(
        literals["q\"uote\\d"].0.as_deref(),
        Some("http://www.w3.org/2001/XMLSchema#string")
    );
    assert_eq!(literals["x"].1.as_deref(), Some("en"));
    assert_eq!(literals["y"].1.as_deref(), Some("de"));
    assert_functional_round_trip(&o, &mut table, "everything");
    assert_triples_round_trip(&o, &mut table);
}

/// What can't be read is a diagnostic at its line and column; the axiom it is in is left
/// out, and the rest is read.
#[test]
fn unreadable_functional_text_is_reported_where_it_is() {
    let text = "Prefix(:=<http://example.com/>)
Ontology(
SubClassOf(:A :B)
SubClassOf(:A nowhere:B)
Frobnicate(:A)
SubClassOf(:A :B :C)
ClassAssertion(ObjectSomeValuesFrom(:r) :a)
DLSafeRule(Body(ClassAtom(:A Variable(:x))) Head())
SubClassOf(:A DataSomeValuesFrom(:d :e xsd:integer))
SubClassOf(:A ObjectMinCardinality(-1 :r))
ClassAssertion(:C :c)
DataPropertyAssertion(:d :c \"open)
";
    let mut table = Table::default();
    let (o, _) = read_functional(text, &mut table);
    let positions: Vec<(u32, u32, &str)> = o
        .diagnostics
        .iter()
        .filter_map(|d| match *d {
            Diagnostic::Syntax { line, column, what } => Some((line, column, what)),
            _ => None,
        })
        .collect();
    let expected = [
        (4, 15, "a prefix no Prefix(…) declares"),
        (5, 1, "an axiom expected"),
        (6, 18, "')' expected"),
        (7, 39, "an IRI expected"),
        (8, 1, "a DL-safe rule (SWRL, not OWL 2)"),
        (
            9,
            40,
            "a data restriction over several properties (OWL 2's datatypes are unary)",
        ),
        (10, 36, "a cardinality that isn't a non-negative integer"),
        (12, 29, "a string without its closing quote"),
        (13, 1, "the ontology's ')' is missing"),
    ];
    assert_eq!(positions, expected);
    assert!(o.diagnostics.iter().all(Diagnostic::is_fatal));
    let lines = rendered(&o, &table);
    assert_eq!(
        lines,
        vec![
            "ClassAssertion(ex:C ex:c)".to_owned(),
            "SubClassOf(ex:A ex:B)".to_owned()
        ]
    );
}

/// Fuzzed ontologies of every profile (`nrese_owl::fuzz`) written as documents read back
/// to the same model, and from there to triples and back. `NRESE_FUZZ_CASES` widens it
/// (5,000 for the reader's gate).
#[test]
fn fuzzed_ontologies_round_trip_through_the_functional_syntax() {
    use nrese_owl::fuzz::{self, Name, Profile, Rng, Signature, Sizes};
    let cases = std::env::var("NRESE_FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300u64);
    let mut rng = Rng::new(0x2026_1005_0f17);
    for case in 0..cases {
        let mut table = Table::default();
        let mut intern = |name: &Name| match name {
            Name::Iri(iri) => table.iri_id(iri),
            Name::Integer(n) => nrese_owl::Make::literal(
                &mut table,
                &n.to_string(),
                "http://www.w3.org/2001/XMLSchema#integer",
            ),
        };
        let sig = Signature::new(Sizes::default(), &mut intern);
        let profile = match case % 4 {
            0 => Profile::el(),
            1 => Profile {
                data: false,
                ..Profile::sroiq()
            },
            2 => Profile {
                nominals: false,
                chains: false,
                ..Profile::sroiq()
            },
            _ => Profile::sroiq(),
        };
        let o = fuzz::ontology(&mut rng, &sig, profile);
        assert_functional_round_trip(&o, &mut table, &format!("case {case}"));
        let text = o.functional_document(None, &|t| ofn_name(&table, t));
        let (back, _) = read_functional(&text, &mut table);
        let vocabulary = table.vocabulary();
        let statements: Vec<Statement> = write(&back, &vocabulary, &mut table)
            .into_iter()
            .map(|triple| Statement { triple, graph: 0 })
            .collect();
        let again = read(&statements, &table);
        assert_eq!(
            rendered(&binary(&back), &table),
            rendered(&again, &table),
            "case {case}: document → model → triples → model"
        );
    }
}
