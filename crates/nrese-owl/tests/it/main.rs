//! The reverse and forward OWL 2 RDF mappings: every construct read as the specification
//! says, malformed input diagnosed, and the round trip model → RDF → model the identity,
//! on a sample ontology and on random ones.

use std::collections::HashMap;

use nrese_owl::{
    Axiom, Characteristic, ClassExpr, DataRange, Diagnostic, EntityKind, ExprId, Make, ObjProp,
    Ontology, RangeId, Statement, Term, TermKind, Terms, Vocabulary, read, write,
};
use nrese_rdf::{BlankNode, Literal, NamedNode, Term as RdfTerm};
use nrese_rdf_io::{RdfFormat, RdfParser};

const EX: &str = "http://example.com/";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const PREFIXES: &str = "@prefix ex: <http://example.com/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
";

/// Terms by id, as a store would hold them.
#[derive(Default)]
struct Table {
    terms: Vec<RdfTerm>,
    ids: HashMap<RdfTerm, u64>,
    blanks: u64,
}

impl Table {
    fn id(&mut self, term: RdfTerm) -> u64 {
        if let Some(&id) = self.ids.get(&term) {
            return id;
        }
        let id = self.terms.len() as u64;
        self.terms.push(term.clone());
        self.ids.insert(term, id);
        id
    }

    fn iri_id(&mut self, iri: &str) -> u64 {
        self.id(NamedNode::new_unchecked(iri).into())
    }

    fn name(&self, id: Term) -> String {
        match &self.terms[id as usize] {
            RdfTerm::NamedNode(n) => {
                let iri = n.as_str();
                if let Some(local) = iri.strip_prefix(EX) {
                    format!("ex:{local}")
                } else if let Some(local) = iri.strip_prefix(XSD) {
                    format!("xsd:{local}")
                } else {
                    format!("<{iri}>")
                }
            }
            RdfTerm::BlankNode(_) => format!("_:b{id}"),
            RdfTerm::Literal(l) => format!("\"{}\"^^{}", l.value(), {
                let d = l.datatype().as_str().to_owned();
                d.strip_prefix(XSD)
                    .map_or(d.clone(), |l| format!("xsd:{l}"))
            }),
            other => other.to_string(),
        }
    }

    /// Interns every vocabulary term (writing needs them all).
    fn vocabulary(&mut self) -> Vocabulary {
        for (_, iri) in Vocabulary::iris() {
            self.iri_id(&iri);
        }
        Vocabulary::new(&|iri| self.iri(iri))
    }
}

impl Terms for Table {
    fn kind(&self, term: Term) -> TermKind {
        match &self.terms[term as usize] {
            RdfTerm::NamedNode(_) => TermKind::Iri,
            RdfTerm::BlankNode(_) => TermKind::Blank,
            _ => TermKind::Literal,
        }
    }

    fn lexical(&self, term: Term) -> Option<String> {
        match &self.terms[term as usize] {
            RdfTerm::Literal(l) => Some(l.value().to_owned()),
            _ => None,
        }
    }

    fn iri(&self, iri: &str) -> Option<Term> {
        self.ids
            .get(&RdfTerm::NamedNode(NamedNode::new_unchecked(iri)))
            .copied()
    }
}

impl Make for Table {
    fn blank(&mut self) -> Term {
        self.blanks += 1;
        self.id(BlankNode::new_unchecked(format!("w{}", self.blanks)).into())
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> Term {
        self.id(Literal::new_typed_literal(lexical, NamedNode::new_unchecked(datatype)).into())
    }
}

fn load(table: &mut Table, turtle: &str) -> Vec<Statement> {
    let text = format!("{PREFIXES}{turtle}");
    RdfParser::from_format(RdfFormat::Turtle)
        .for_slice(text.as_bytes())
        .map(|quad| {
            let quad = quad.unwrap_or_else(|e| panic!("{e}"));
            let s = table.id(quad.subject.into());
            let p = table.id(quad.predicate.into());
            let o = table.id(quad.object);
            Statement {
                triple: [s, p, o],
                graph: 0,
            }
        })
        .collect()
}

fn rendered(ontology: &Ontology, table: &Table) -> Vec<String> {
    let mut lines: Vec<String> = ontology
        .axioms
        .iter()
        .map(|a| ontology.functional(a, &|t| table.name(t)))
        .collect();
    lines.sort();
    lines
}

const SAMPLE: &str = r#"
ex:Person a owl:Class . ex:Student a owl:Class . ex:Course a owl:Class .
ex:takes a owl:ObjectProperty . ex:knows a owl:ObjectProperty .
ex:hasParent a owl:ObjectProperty . ex:hasChild a owl:ObjectProperty .
ex:hasGrandparent a owl:ObjectProperty .
ex:age a owl:DatatypeProperty , owl:FunctionalProperty .
ex:Student rdfs:subClassOf ex:Person ,
    [ a owl:Restriction ; owl:onProperty ex:takes ; owl:someValuesFrom ex:Course ] .
ex:Busy owl:equivalentClass [ a owl:Restriction ; owl:onProperty ex:takes ;
    owl:minQualifiedCardinality "3"^^xsd:nonNegativeInteger ; owl:onClass ex:Course ] .
ex:Single rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:knows ;
    owl:maxCardinality "1"^^xsd:nonNegativeInteger ] .
ex:Adult owl:equivalentClass [ a owl:Class ; owl:intersectionOf ( ex:Person
    [ a owl:Restriction ; owl:onProperty ex:age ; owl:someValuesFrom
        [ a rdfs:Datatype ; owl:onDatatype xsd:integer ;
          owl:withRestrictions ( [ xsd:minInclusive 18 ] ) ] ] ) ] .
ex:Teacher rdfs:subClassOf [ a owl:Restriction ;
    owl:onProperty [ owl:inverseOf ex:takes ] ; owl:allValuesFrom ex:Student ] .
ex:Color owl:equivalentClass [ a owl:Class ; owl:oneOf ( ex:red ex:green ) ] .
ex:NotPerson owl:equivalentClass [ a owl:Class ; owl:complementOf ex:Person ] .
ex:Narcissist rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:knows ; owl:hasSelf true ] .
ex:Either rdfs:subClassOf [ a owl:Class ; owl:unionOf ( ex:Student ex:Course ) ] .
ex:Aged rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:age ; owl:hasValue 42 ] .
[] a owl:AllDisjointClasses ; owl:members ( ex:Person ex:Course ex:Color ) .
ex:Animal owl:disjointUnionOf ( ex:Cat ex:Dog ) .
ex:hasParent owl:inverseOf ex:hasChild .
ex:hasGrandparent owl:propertyChainAxiom ( ex:hasParent ex:hasParent ) .
ex:takes rdfs:domain ex:Student ; rdfs:range ex:Course .
ex:age rdfs:range xsd:integer .
ex:knows a owl:SymmetricProperty .
ex:Person owl:hasKey ( ex:age ) .
ex:alice a ex:Student ; ex:takes ex:math ; ex:age 20 ; rdfs:label "Alice" ; owl:sameAs ex:ali .
ex:bob owl:differentFrom ex:alice .
[] a owl:NegativePropertyAssertion ; owl:sourceIndividual ex:bob ;
   owl:assertionProperty ex:takes ; owl:targetIndividual ex:math .
ex:Adult rdfs:comment "grown up" .
"#;

#[test]
fn every_construct_is_read() {
    let mut table = Table::default();
    let statements = load(&mut table, SAMPLE);
    let ontology = read(&statements, &table);
    let lines = rendered(&ontology, &table);
    let integer = |n: i64| format!("\"{n}\"^^xsd:integer");
    for expected in [
        "SubClassOf(ex:Student ex:Person)".to_owned(),
        "SubClassOf(ex:Student ObjectSomeValuesFrom(ex:takes ex:Course))".to_owned(),
        "EquivalentClasses(ObjectMinCardinality(3 ex:takes ex:Course) ex:Busy)".to_owned(),
        "SubClassOf(ex:Single ObjectMaxCardinality(1 ex:knows owl:Thing))".to_owned(),
        format!(
            "EquivalentClasses(ObjectIntersectionOf(DataSomeValuesFrom(ex:age DatatypeRestriction(xsd:integer xsd:minInclusive {})) ex:Person) ex:Adult)",
            integer(18)
        ),
        "SubClassOf(ex:Teacher ObjectAllValuesFrom(ObjectInverseOf(ex:takes) ex:Student))"
            .to_owned(),
        "EquivalentClasses(ObjectOneOf(ex:red ex:green) ex:Color)".to_owned(),
        "EquivalentClasses(ObjectComplementOf(ex:Person) ex:NotPerson)".to_owned(),
        "SubClassOf(ex:Narcissist ObjectHasSelf(ex:knows))".to_owned(),
        "SubClassOf(ex:Either ObjectUnionOf(ex:Course ex:Student))".to_owned(),
        format!("SubClassOf(ex:Aged DataHasValue(ex:age {}))", integer(42)),
        "DisjointClasses(ex:Color ex:Course ex:Person)".to_owned(),
        "DisjointUnion(ex:Animal ex:Cat ex:Dog)".to_owned(),
        "InverseObjectProperties(ex:hasParent ex:hasChild)".to_owned(),
        "SubObjectPropertyOf(ObjectPropertyChain(ex:hasParent ex:hasParent) ex:hasGrandparent)"
            .to_owned(),
        "ObjectPropertyDomain(ex:takes ex:Student)".to_owned(),
        "ObjectPropertyRange(ex:takes ex:Course)".to_owned(),
        "DataPropertyRange(ex:age xsd:integer)".to_owned(),
        "FunctionalDataProperty(ex:age)".to_owned(),
        "SymmetricObjectProperty(ex:knows)".to_owned(),
        "HasKey(ex:Person () (ex:age))".to_owned(),
        "ClassAssertion(ex:Student ex:alice)".to_owned(),
        "ObjectPropertyAssertion(ex:takes ex:alice ex:math)".to_owned(),
        format!("DataPropertyAssertion(ex:age ex:alice {})", integer(20)),
        "SameIndividual(ex:alice ex:ali)".to_owned(),
        "DifferentIndividuals(ex:alice ex:bob)".to_owned(),
        "NegativeObjectPropertyAssertion(ex:takes ex:bob ex:math)".to_owned(),
        "Declaration(DataProperty(ex:age))".to_owned(),
    ] {
        assert!(
            lines.contains(&expected),
            "missing {expected}\nread:\n{}",
            lines.join("\n")
        );
    }
    assert!(
        ontology.diagnostics.is_empty(),
        "{:?}",
        ontology.diagnostics
    );
    assert_eq!(ontology.annotations, 2, "the label and the comment");
    // Every axiom says where it came from; a restriction's triples with its axiom.
    let busy = ontology
        .axioms
        .iter()
        .position(|a| matches!(a, Axiom::EquivalentClasses(xs) if xs.len() == 2 && lines.iter().any(|_| true) && ontology.functional(a, &|t| table.name(t)).contains("Busy")))
        .unwrap();
    assert_eq!(
        ontology.sources[busy][0].triples.len(),
        1 + 4,
        "the axiom and the restriction's four"
    );
}

#[test]
fn malformed_input_is_reported() {
    let mut table = Table::default();
    let statements = load(
        &mut table,
        r#"
        ex:p a owl:ObjectProperty . ex:r a owl:ObjectProperty .
        ex:A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:p ] .
        ex:B rdfs:subClassOf [ a owl:Class ; owl:unionOf _:l ] . _:l rdf:first ex:C .
        _:shared a owl:Restriction ; owl:onProperty ex:r ; owl:someValuesFrom ex:C .
        ex:E rdfs:subClassOf _:shared . ex:F rdfs:subClassOf _:shared .
        ex:t a owl:TransitiveProperty .
        ex:G rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:t ;
            owl:maxCardinality "1"^^xsd:nonNegativeInteger ] .
        ex:x ex:undeclared ex:y .
        ex:H owl:onProperty ex:p .
        "#,
    );
    let ontology = read(&statements, &table);
    let has = |test: &dyn Fn(&Diagnostic) -> bool| ontology.diagnostics.iter().any(test);
    assert!(
        has(
            &|d| matches!(d, Diagnostic::Malformed { what, .. } if what.contains("without a restriction property"))
        ),
        "{:?}",
        ontology.diagnostics
    );
    assert!(has(&|d| matches!(d, Diagnostic::BrokenList { .. })));
    assert!(has(&|d| matches!(d, Diagnostic::SharedBlankNode { .. })));
    assert!(has(&|d| matches!(
        d,
        Diagnostic::NonSimpleProperty {
            what: "a cardinality restriction",
            ..
        }
    )));
    assert!(has(&|d| matches!(d, Diagnostic::UndeclaredProperty { .. })));
    assert!(has(&|d| matches!(d, Diagnostic::NotOwl { .. })));
    // What is well-formed is still read: the shared restriction, the assertion.
    let lines = rendered(&ontology, &table);
    assert!(lines.contains(&"SubClassOf(ex:E ObjectSomeValuesFrom(ex:r ex:C))".to_owned()));
    assert!(lines.contains(&"ObjectPropertyAssertion(ex:undeclared ex:x ex:y)".to_owned()));
}

/// What the W3C OWL 2 DL tests showed read wrongly or dropped silently: a cardinality over
/// a data property where the source has no term for `rdfs:Literal` (DL-601: the axiom was
/// dropped without a diagnostic), RDF's `rdfs:Class`, `rdf:Property` and `rdf:List`
/// typing (read as class assertions: Restriction-005), and the built-in properties
/// (read as ordinary ones, by use).
#[test]
fn reserved_vocabulary_is_read_by_its_meaning() {
    let mut table = Table::default();
    let statements = load(
        &mut table,
        r#"
        ex:P a owl:DatatypeProperty .
        ex:C owl:equivalentClass [ a owl:Restriction ; owl:onProperty ex:P ;
            owl:maxCardinality "0"^^xsd:nonNegativeInteger ] .
        ex:D a rdfs:Class . ex:p a rdf:Property . _:cell a rdf:List .
        ex:x a [ a owl:Restriction , rdfs:Class ; owl:onProperty ex:q ;
            owl:allValuesFrom ex:D ] .
        ex:q a owl:ObjectProperty .
        ex:i a [ a owl:Restriction ; owl:onProperty owl:bottomDataProperty ;
            owl:someValuesFrom rdfs:Literal ] .
        "#,
    );
    let ontology = read(&statements, &table);
    let lines = rendered(&ontology, &table);
    for expected in [
        "EquivalentClasses(DataMaxCardinality(0 ex:P rdfs:Literal) ex:C)",
        "Declaration(Class(ex:D))",
        "ClassAssertion(ObjectAllValuesFrom(ex:q ex:D) ex:x)",
        "ClassAssertion(DataSomeValuesFrom(<http://www.w3.org/2002/07/owl#bottomDataProperty> rdfs:Literal) ex:i)",
    ] {
        assert!(
            lines.contains(&expected.to_owned()),
            "missing {expected}\nread:\n{}",
            lines.join("\n")
        );
    }
    assert!(
        !lines.iter().any(|l| l.contains("rdf-schema#Class>")
            || l.contains("#Property>")
            || l.contains("#List>")),
        "reserved classes read as classes:\n{}",
        lines.join("\n")
    );
    assert!(
        ontology.diagnostics.is_empty(),
        "{:?}",
        ontology.diagnostics
    );
    assert_eq!(
        ontology.builtin.bottom_data,
        table.iri("http://www.w3.org/2002/07/owl#bottomDataProperty")
    );
    assert_round_trip(&ontology, &mut table);
}

/// An expression the reader can't take leaves its axiom out with a diagnostic, never
/// silently (a qualified cardinality without its class was dropped without one).
#[test]
fn unreadable_expressions_are_reported() {
    let mut table = Table::default();
    let statements = load(
        &mut table,
        r#"
        ex:p a owl:ObjectProperty .
        ex:A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:p ;
            owl:maxQualifiedCardinality "1"^^xsd:nonNegativeInteger ] .
        "#,
    );
    let ontology = read(&statements, &table);
    assert!(
        !ontology
            .axioms
            .iter()
            .any(|a| matches!(a, Axiom::SubClassOf(..))),
        "{:?}",
        ontology.axioms
    );
    assert!(
        ontology.diagnostics.iter().any(|d| d.is_fatal()),
        "{:?}",
        ontology.diagnostics
    );
}

/// Horn axioms give Horn clauses (found by the U1 bound on OWL2Bench, 4 October 2026): a
/// union on the left (`A ⊔ B ⊑ C` was `⊤ → C ∨ Q`) and an existential over a non-simple
/// property on the left (`A ⊓ ∃R.D ⊑ B` was `A → B ∨ Q`, Q the start of R's automaton).
#[test]
fn horn_axioms_stay_horn() {
    let mut table = Table::default();
    let statements = load(
        &mut table,
        r#"
        ex:worksFor a owl:ObjectProperty , owl:TransitiveProperty .
        ex:partOf a owl:ObjectProperty . ex:partOf rdfs:subPropertyOf ex:worksFor .
        [ a owl:Class ; owl:unionOf ( ex:Man ex:Woman ) ] rdfs:subClassOf ex:Person .
        [ a owl:Class ; owl:intersectionOf ( ex:Person
            [ a owl:Restriction ; owl:onProperty ex:worksFor ;
              owl:someValuesFrom ex:Organization ] ) ] rdfs:subClassOf ex:Employee .
        [ a owl:Restriction ; owl:onProperty ex:worksFor ; owl:someValuesFrom ex:Org ]
            rdfs:subClassOf ex:Member .
        ex:Boss rdfs:subClassOf [ a owl:Class ; owl:intersectionOf ( ex:Person
            [ a owl:Restriction ; owl:onProperty ex:worksFor ;
              owl:allValuesFrom ex:Organization ] ) ] .
        "#,
    );
    let ontology = read(&statements, &table);
    assert!(
        ontology.diagnostics.is_empty(),
        "{:?}",
        ontology.diagnostics
    );
    let normalised = nrese_owl::normalise(&ontology);
    let disjunctive: Vec<_> = normalised
        .clauses
        .iter()
        .filter(|c| !c.flags.horn)
        .collect();
    assert!(disjunctive.is_empty(), "{disjunctive:#?}");
    assert!(normalised.unsupported.is_empty());
}

/// Model → RDF → model is the identity on `ontology`.
fn assert_round_trip(ontology: &Ontology, table: &mut Table) {
    let vocabulary = table.vocabulary();
    let triples = write(ontology, &vocabulary, table);
    let statements: Vec<Statement> = triples
        .into_iter()
        .map(|triple| Statement { triple, graph: 0 })
        .collect();
    let back = read(&statements, table);
    // Random ontologies may break OWL 2 DL's restriction to simple properties: reported
    // on both sides, not a matter of the mapping.
    assert!(
        back.diagnostics
            .iter()
            .all(|d| !d.is_fatal() || matches!(d, Diagnostic::NonSimpleProperty { .. })),
        "{:?}",
        back.diagnostics
    );
    let (before, after) = (rendered(ontology, table), rendered(&back, table));
    if before != after {
        let missing: Vec<&String> = before.iter().filter(|l| !after.contains(l)).collect();
        let extra: Vec<&String> = after.iter().filter(|l| !before.contains(l)).collect();
        panic!("round trip differs\nmissing: {missing:#?}\nextra: {extra:#?}");
    }
}

#[test]
fn the_sample_round_trips() {
    let mut table = Table::default();
    let statements = load(&mut table, SAMPLE);
    let ontology = read(&statements, &table);
    assert_round_trip(&ontology, &mut table);
}

/// The terms random ontologies are built from.
struct Names {
    classes: Vec<Term>,
    props: Vec<Term>,
    data: Term,
    individuals: Vec<Term>,
    integer: Term,
    facet: Term,
    literals: Vec<Term>,
}

/// Random ontologies built in the model, written and read back.
#[test]
fn random_ontologies_round_trip() {
    let cases = std::env::var("NRESE_FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let mut state = 0x0EEF_2026_1003u64;
    let mut next = move |n: u64| {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) % n.max(1)
    };
    for _ in 0..cases {
        let mut table = Table::default();
        let mut o = Ontology::default();
        let classes: Vec<Term> = (0..4).map(|i| table.iri_id(&format!("{EX}C{i}"))).collect();
        let props: Vec<Term> = (0..3).map(|i| table.iri_id(&format!("{EX}p{i}"))).collect();
        let data = table.iri_id(&format!("{EX}d"));
        let individuals: Vec<Term> = (0..3).map(|i| table.iri_id(&format!("{EX}a{i}"))).collect();
        let integer = table.iri_id(&format!("{XSD}integer"));
        let min_inclusive = table.iri_id(&format!("{XSD}minInclusive"));
        let literals: Vec<Term> = (0..3)
            .map(|n| {
                table.id(Literal::new_typed_literal(
                    n.to_string(),
                    NamedNode::new_unchecked(format!("{XSD}integer")),
                )
                .into())
            })
            .collect();
        let mut axioms: Vec<Axiom> = Vec::new();
        for &c in &classes {
            axioms.push(Axiom::Declaration(EntityKind::Class, c));
        }
        for &p in &props {
            axioms.push(Axiom::Declaration(EntityKind::ObjectProperty, p));
        }
        axioms.push(Axiom::Declaration(EntityKind::DataProperty, data));
        let property = |next: &mut dyn FnMut(u64) -> u64| {
            let p = props[next(props.len() as u64) as usize];
            if next(4) == 0 {
                ObjProp::Inverse(p)
            } else {
                ObjProp::Named(p)
            }
        };
        fn range(
            o: &mut Ontology,
            next: &mut dyn FnMut(u64) -> u64,
            integer: Term,
            facet: Term,
            literals: &[Term],
        ) -> RangeId {
            let r = match next(3) {
                0 => DataRange::Datatype(integer),
                1 => DataRange::Restriction(integer, vec![(facet, literals[next(3) as usize])]),
                _ => DataRange::OneOf(vec![literals[0], literals[1 + next(2) as usize]]),
            };
            RangeId(o.ranges.intern(r))
        }
        fn class(
            o: &mut Ontology,
            next: &mut dyn FnMut(u64) -> u64,
            depth: u32,
            names: &Names,
        ) -> ExprId {
            let Names {
                classes,
                props,
                data,
                individuals,
                integer,
                facet,
                literals,
            } = names;
            let property = |next: &mut dyn FnMut(u64) -> u64| {
                let p = props[next(props.len() as u64) as usize];
                if next(4) == 0 {
                    ObjProp::Inverse(p)
                } else {
                    ObjProp::Named(p)
                }
            };
            let pick = if depth == 0 { 0 } else { next(15) };
            let e = match pick {
                0 | 1 => ClassExpr::Class(classes[next(classes.len() as u64) as usize]),
                2 => {
                    let (a, b) = (
                        class(o, next, depth - 1, names),
                        class(o, next, depth - 1, names),
                    );
                    let mut v = vec![a, b];
                    v.sort();
                    v.dedup();
                    ClassExpr::And(v)
                }
                3 => {
                    let (a, b) = (
                        class(o, next, depth - 1, names),
                        class(o, next, depth - 1, names),
                    );
                    let mut v = vec![a, b];
                    v.sort();
                    v.dedup();
                    ClassExpr::Or(v)
                }
                4 => ClassExpr::Not(class(o, next, depth - 1, names)),
                5 => ClassExpr::Some(property(next), class(o, next, depth - 1, names)),
                6 => ClassExpr::All(property(next), class(o, next, depth - 1, names)),
                7 => ClassExpr::HasValue(property(next), individuals[next(3) as usize]),
                8 => ClassExpr::Min(
                    next(4) as u32,
                    property(next),
                    class(o, next, depth - 1, names),
                ),
                9 => ClassExpr::Max(
                    next(4) as u32,
                    property(next),
                    class(o, next, depth - 1, names),
                ),
                10 => ClassExpr::Exact(
                    1 + next(3) as u32,
                    property(next),
                    class(o, next, depth - 1, names),
                ),
                11 => ClassExpr::OneOf(vec![individuals[0], individuals[1 + next(2) as usize]]),
                12 => ClassExpr::DataSome(*data, range(o, next, *integer, *facet, literals)),
                13 => ClassExpr::DataHasValue(*data, literals[next(3) as usize]),
                _ => ClassExpr::Thing,
            };
            ExprId(o.classes.intern(e))
        }
        let names = Names {
            classes: classes.clone(),
            props: props.clone(),
            data,
            individuals: individuals.clone(),
            integer,
            facet: min_inclusive,
            literals: literals.clone(),
        };
        for _ in 0..12 {
            let axiom = match next(12) {
                0..=2 => {
                    let (a, b) = (
                        class(&mut o, &mut next, 2, &names),
                        class(&mut o, &mut next, 2, &names),
                    );
                    Axiom::SubClassOf(a, b)
                }
                3 => {
                    let (a, b) = (
                        class(&mut o, &mut next, 2, &names),
                        class(&mut o, &mut next, 2, &names),
                    );
                    if a == b {
                        continue;
                    }
                    let mut v = vec![a, b];
                    v.sort();
                    Axiom::EquivalentClasses(v)
                }
                4 => {
                    let mut v: Vec<ExprId> = (0..2 + next(2))
                        .map(|_| class(&mut o, &mut next, 1, &names))
                        .collect();
                    v.sort();
                    v.dedup();
                    if v.len() < 2 {
                        continue;
                    }
                    Axiom::DisjointClasses(v)
                }
                5 => {
                    let chain: Vec<ObjProp> =
                        (0..1 + next(2)).map(|_| property(&mut next)).collect();
                    Axiom::SubObjectPropertyOf(chain, ObjProp::Named(props[next(3) as usize]))
                }
                6 => Axiom::ObjectPropertyDomain(
                    property(&mut next),
                    class(&mut o, &mut next, 1, &names),
                ),
                7 => Axiom::ObjectCharacteristic(
                    [
                        Characteristic::Functional,
                        Characteristic::Symmetric,
                        Characteristic::Reflexive,
                    ][next(3) as usize],
                    property(&mut next),
                ),
                8 => Axiom::ClassAssertion(
                    class(&mut o, &mut next, 2, &names),
                    individuals[next(3) as usize],
                ),
                9 => Axiom::ObjectPropertyAssertion(
                    props[next(3) as usize],
                    individuals[next(3) as usize],
                    individuals[next(3) as usize],
                ),
                10 => Axiom::DataPropertyAssertion(
                    data,
                    individuals[next(3) as usize],
                    literals[next(3) as usize],
                ),
                _ => Axiom::DataPropertyRange(
                    data,
                    range(&mut o, &mut next, integer, min_inclusive, &literals),
                ),
            };
            axioms.push(axiom);
        }
        axioms.sort();
        axioms.dedup();
        o.sources = vec![Vec::new(); axioms.len()];
        o.axioms = axioms;
        assert_round_trip(&o, &mut table);
    }
}

/// The SROIQ(D) fuzzer (`nrese_owl::fuzz`, work package 2.5): what it generates is OWL 2
/// DL (no diagnostic at all once written and read back: the global restrictions hold),
/// round-trips, and normalises with a regular RBox; its transformations keep that.
#[test]
fn fuzzed_ontologies_are_owl2_dl() {
    use nrese_owl::fuzz::{self, Name, Profile, Rng, Signature, Sizes};
    let cases = std::env::var("NRESE_FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300u64);
    let mut rng = Rng::new(0x2026_1003_0250);
    for case in 0..cases {
        let mut table = Table::default();
        let mut intern = |name: &Name| match name {
            Name::Iri(iri) => table.iri_id(iri),
            Name::Integer(n) => table.literal(&n.to_string(), &format!("{XSD}integer")),
        };
        let sig = Signature::new(Sizes::default(), &mut intern);
        let profile = if case % 3 == 0 {
            Profile::el()
        } else {
            Profile::sroiq()
        };
        let o = fuzz::ontology(&mut rng, &sig, profile);
        let check = |o: &Ontology, table: &mut Table, what: &str| {
            let vocabulary = table.vocabulary();
            let statements: Vec<Statement> = write(o, &vocabulary, table)
                .into_iter()
                .map(|triple| Statement { triple, graph: 0 })
                .collect();
            let back = read(&statements, table);
            let names = |t: Term| table.name(t);
            let axioms: Vec<String> = o.axioms.iter().map(|a| o.functional(a, &names)).collect();
            assert!(
                back.diagnostics.is_empty(),
                "case {case} ({what}): {:?}\n{}",
                back.diagnostics,
                axioms.join("\n")
            );
            let normalised = nrese_owl::normalise(&back);
            assert!(
                normalised.unsupported.is_empty(),
                "case {case} ({what}): {:?}\n{}",
                normalised.unsupported,
                axioms.join("\n")
            );
        };
        check(&o, &mut table, "generated");
        assert_round_trip(&o, &mut table);
        // Renamed by swapping two classes and two individuals: still DL, and renaming back
        // gives the ontology again.
        let (c0, c1, a0, a1) = (
            sig.classes[0],
            sig.classes[1],
            sig.individuals[0],
            sig.individuals[1],
        );
        let swap = |t: Term| match t {
            t if t == c0 => c1,
            t if t == c1 => c0,
            t if t == a0 => a1,
            t if t == a1 => a0,
            t => t,
        };
        let renamed = fuzz::rename(&o, &swap);
        check(&renamed, &mut table, "renamed");
        let back = fuzz::rename(&renamed, &swap);
        let render = |o: &Ontology, table: &Table| {
            let mut v: Vec<String> = o
                .axioms
                .iter()
                .map(|a| o.functional(a, &|t| table.name(t)))
                .collect();
            v.sort();
            v
        };
        assert_eq!(render(&back, &table), render(&o, &table), "case {case}");
        let shuffled = fuzz::shuffle(&o, &mut rng);
        assert_eq!(render(&shuffled, &table), render(&o, &table), "case {case}");
        let redundant = fuzz::add_redundant(&o, &mut rng, &sig, 3, profile.el);
        check(&redundant, &mut table, "redundant");
        let mut fresh = 0;
        let defined = fuzz::define_fresh(&o, 2, &mut || {
            fresh += 1;
            table.iri_id(&format!("{}F{fresh}", fuzz::FUZZ))
        });
        check(&defined, &mut table, "defined");
    }
}
