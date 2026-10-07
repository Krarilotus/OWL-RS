//! The reverse and forward OWL 2 RDF mappings: every construct read as the specification
//! says, malformed input diagnosed, and the round trip model → RDF → model the identity,
//! on a sample ontology and on random ones.

mod functional;

use std::collections::HashMap;

use nrese_owl::{
    Axiom, Diagnostic, Make, Ontology, Statement, Term, TermKind, Terms, Vocabulary, read, write,
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

    fn datatype(&self, term: Term) -> Option<String> {
        match &self.terms[term as usize] {
            RdfTerm::Literal(l) => Some(l.datatype().as_str().to_owned()),
            _ => None,
        }
    }

    fn language(&self, term: Term) -> Option<String> {
        match &self.terms[term as usize] {
            RdfTerm::Literal(l) => l.language().map(str::to_owned),
            _ => None,
        }
    }

    fn iri_text(&self, term: Term) -> Option<String> {
        match &self.terms[term as usize] {
            RdfTerm::NamedNode(n) => Some(n.as_str().to_owned()),
            _ => None,
        }
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

/// Every construct of the RDF mapping is read, each axiom with its source triples, and
/// written back to the same model.
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
    assert_round_trip(&ontology, &mut table);
}

/// What a datatype theory and key rules need reaches the model and the normalisation:
/// literals with their datatype and language tag, datatype and facet IRIs, anonymous
/// individuals, keys as DL-safe rules, datatype definitions (both still reported as
/// unsupported to engines that don't read them).
#[test]
fn literals_keys_and_definitions_reach_the_model() {
    let mut table = Table::default();
    let text = format!(
        "{PREFIXES}
ex:p a owl:DatatypeProperty . ex:q a owl:ObjectProperty . ex:C a owl:Class .
ex:C owl:hasKey ( ex:q ex:p ) .
ex:a ex:p \"1.50\"^^xsd:decimal , \"chat\"@fr .
_:b ex:p \"x\" .
ex:D a rdfs:Datatype ; owl:equivalentClass [ a rdfs:Datatype ; owl:onDatatype xsd:integer ;
    owl:withRestrictions ( [ xsd:maxInclusive 5 ] ) ] .
"
    );
    let statements = load(&mut table, &text);
    let o = read(&statements, &table);
    assert!(
        o.diagnostics.iter().all(|d| !d.is_fatal()),
        "{:?}",
        o.diagnostics
    );
    let lexicals: Vec<(String, Option<String>, Option<String>)> = {
        let mut v: Vec<_> = o
            .data
            .literals
            .values()
            .map(|l| (l.lexical.clone(), l.datatype.clone(), l.language.clone()))
            .collect();
        v.sort();
        v
    };
    assert!(lexicals.contains(&("1.50".into(), Some(format!("{XSD}decimal")), None)));
    assert!(
        lexicals
            .iter()
            .any(|(l, _, lang)| l == "chat" && lang.as_deref() == Some("fr"))
    );
    assert!(
        lexicals
            .iter()
            .any(|(l, d, _)| l == "5" && d.as_deref() == Some(&format!("{XSD}integer")[..]))
    );
    let iris: Vec<&String> = o.data.iris.values().collect();
    assert!(iris.contains(&&format!("{XSD}integer")));
    assert!(iris.contains(&&format!("{XSD}maxInclusive")));
    assert_eq!(o.anonymous.len(), 1);
    let n = nrese_owl::normalise(&o);
    assert_eq!(n.rules.len(), 1);
    let rule = &n.rules[0];
    assert_eq!(
        rule.head,
        vec![nrese_owl::HeadAtom::Equal(
            nrese_owl::Var::X,
            nrese_owl::Var::Y(0)
        )]
    );
    let q = table.iri(&format!("{EX}q")).unwrap();
    let p = table.iri(&format!("{EX}p")).unwrap();
    use nrese_owl::{BodyAtom, Var};
    for atom in [
        BodyAtom::Role(q, Var::X, Var::Y(1)),
        BodyAtom::Role(q, Var::Y(0), Var::Y(1)),
        BodyAtom::Data(p, Var::X, Var::V(0)),
        BodyAtom::Data(p, Var::Y(0), Var::V(0)),
    ] {
        assert!(rule.body.contains(&atom), "{atom:?} in {:?}", rule.body);
    }
    assert_eq!(n.definitions.len(), 1);
    let reasons: Vec<&str> = n.unsupported.iter().map(|(_, r)| *r).collect();
    assert!(reasons.contains(&nrese_owl::UNSUPPORTED_KEYS));
    assert!(reasons.contains(&nrese_owl::UNSUPPORTED_DATATYPE_DEFINITIONS));
}

/// `SubDataPropertyOf(p, owl:topDataProperty)` holds in every interpretation: no clause,
/// and not unsupported; any other axiom over the universal data property still is.
#[test]
fn a_data_property_below_the_top_one_is_a_tautology() {
    let mut table = Table::default();
    let text = format!(
        "{PREFIXES}
ex:p a owl:DatatypeProperty ; rdfs:subPropertyOf owl:topDataProperty .
"
    );
    let statements = load(&mut table, &text);
    let o = read(&statements, &table);
    assert_eq!(
        o.axioms
            .iter()
            .filter(|a| matches!(a, Axiom::SubDataPropertyOf(..)))
            .count(),
        1
    );
    let n = nrese_owl::normalise(&o);
    assert!(n.unsupported.is_empty(), "{:?}", n.unsupported);
    assert!(n.clauses.is_empty(), "{:?}", n.clauses);
    let text = format!(
        "{PREFIXES}
ex:p a owl:DatatypeProperty . owl:topDataProperty rdfs:subPropertyOf ex:p .
"
    );
    let mut table = Table::default();
    let statements = load(&mut table, &text);
    let n = nrese_owl::normalise(&read(&statements, &table));
    assert_eq!(n.unsupported.len(), 1);
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

/// Statements about the ontology (its header's annotations) are annotations, an anonymous
/// ontology's too, never assertions about an individual (the ontology node was read as an
/// individual: found by the functional-syntax reader's comparison, FS2RDF-ontology-
/// annotation-annotation-ar).
#[test]
fn statements_about_the_ontology_are_annotations() {
    let mut table = Table::default();
    let statements = load(
        &mut table,
        r#"
        [] a owl:Ontology ; ex:for ex:eqc .
        ex:o a owl:Ontology ; ex:about ex:thing ; ex:note "text" .
        "#,
    );
    let ontology = read(&statements, &table);
    assert!(ontology.axioms.is_empty(), "{:?}", ontology.axioms);
    // The two header typings and the three statements about the ontologies.
    assert_eq!(ontology.annotations, 5);
    assert!(
        ontology.diagnostics.is_empty(),
        "{:?}",
        ontology.diagnostics
    );
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
