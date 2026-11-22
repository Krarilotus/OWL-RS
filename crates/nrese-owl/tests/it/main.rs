//! The reverse and forward OWL 2 RDF mappings: every construct read as the specification
//! says, malformed input diagnosed, and the round trip model → RDF → model the identity,
//! on a sample ontology and on random ones.

mod functional;
mod list_cells;
mod order;
mod premise;
mod ql_status;
mod typing;

use std::collections::HashMap;

use nrese_owl::mapping::ClassExpressionMarkers;
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

/// The class-expression markers a caller looks up by their own IRIs are the mapping's, those
/// of the whole vocabulary, whichever of its terms the source has
/// (`ClassExpressionMarkers::lookup` and `::of`: one definition, I24c-2).
#[test]
fn class_expression_markers_looked_up_alone_are_the_vocabularys() {
    let mut every = Table::default();
    let vocabulary = every.vocabulary();
    let markers = ClassExpressionMarkers::lookup(&|iri| every.iri(iri));
    assert_eq!(markers, ClassExpressionMarkers::of(&vocabulary));
    assert!(
        markers
            .types
            .iter()
            .chain(&markers.set_operators)
            .all(Option::is_some)
    );
    // A source with every other term of the vocabulary, under other ids.
    let mut some = Table::default();
    some.iri_id("http://example.com/first");
    for (_, iri) in Vocabulary::iris().into_iter().step_by(2) {
        some.iri_id(&iri);
    }
    let vocabulary = Vocabulary::new(&|iri| some.iri(iri));
    assert_eq!(
        ClassExpressionMarkers::lookup(&|iri| some.iri(iri)),
        ClassExpressionMarkers::of(&vocabulary)
    );
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

/// The ontology of statements in several graphs is that of their union ([`read`]): every
/// statement of a restriction, a list and a datatype restriction also in a second graph,
/// the same statement twice, is read as from one graph: the same axioms, no diagnostic.
#[test]
fn a_structure_repeated_in_a_second_graph_is_read_as_in_one() {
    let mut table = Table::default();
    let once = load(
        &mut table,
        "ex:C a owl:Class . ex:D a owl:Class . ex:A a owl:Class . ex:p a owl:ObjectProperty .
         ex:d a owl:DatatypeProperty .
         [ a owl:Restriction ; owl:onProperty ex:p ; owl:someValuesFrom ex:D ] rdfs:subClassOf ex:C .
         ex:A owl:equivalentClass [ a owl:Class ; owl:intersectionOf ( ex:C ex:D ) ] .
         ex:d rdfs:range [ a rdfs:Datatype ; owl:onDatatype xsd:integer ;
             owl:withRestrictions ( [ xsd:minInclusive 1 ] ) ] .",
    );
    let twice: Vec<Statement> = once
        .iter()
        .flat_map(|s| {
            [0, 1].map(|graph| Statement {
                triple: s.triple,
                graph,
            })
        })
        .collect();
    let (one, two) = (read(&once, &table), read(&twice, &table));
    let fatal = |o: &Ontology| -> Vec<String> {
        let fatal = o.diagnostics.iter().filter(|d| d.is_fatal());
        fatal.map(|d| format!("{d:?}")).collect()
    };
    assert!(fatal(&one).is_empty(), "{:?}", one.diagnostics);
    assert_eq!(rendered(&two, &table), rendered(&one, &table));
    assert_eq!(fatal(&two), fatal(&one), "{:?}", two.diagnostics);
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
    let reasons: Vec<nrese_owl::Unsupported> = n.unsupported.iter().map(|&(_, r)| r).collect();
    assert!(reasons.contains(&nrese_owl::Unsupported::Keys));
    assert!(reasons.contains(&nrese_owl::Unsupported::DatatypeDefinitions));
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

/// OWL 2 RL's rules read only max cardinalities (cls-maxc1/2, cls-maxqc1-4), and only as
/// the RDF mapping writes them: a max or max-qualified one spelled otherwise is marked
/// (`Ontology::noncanonical_cardinality`, which the RL route and the rewriting's status read).
/// The mapping's spelling isn't, and neither is an exact or min one, whose spelling no rule
/// reads, so plain `owl:cardinality 1` gives no `Unread` hazard.
#[test]
fn only_a_max_cardinality_the_rules_dont_read_is_marked() {
    for (restriction, marked) in [
        ("owl:maxCardinality 1", true),
        ("owl:maxCardinality \"01\"^^xsd:nonNegativeInteger", true),
        ("owl:maxQualifiedCardinality 1 ; owl:onClass ex:D", true),
        ("owl:maxCardinality \"1\"^^xsd:nonNegativeInteger", false),
        ("owl:cardinality 1", false),
        ("owl:minCardinality 1", false),
    ] {
        let mut table = Table::default();
        let statements = load(
            &mut table,
            &format!(
                "ex:p a owl:ObjectProperty . ex:D a owl:Class .
                 ex:C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:p ; {restriction} ] ."
            ),
        );
        let ontology = read(&statements, &table);
        assert_eq!(
            ontology.noncanonical_cardinality.is_some(),
            marked,
            "{restriction}"
        );
    }
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

/// An n-ary structure without its members, and a negative assertion without its target
/// (review A-1103), are reported, never dropped silently; what is well-formed beside them
/// is read.
#[test]
fn malformed_n_ary_structures_are_reported() {
    let mut table = Table::default();
    let statements = load(
        &mut table,
        r#"
        ex:p a owl:ObjectProperty .
        [] a owl:AllDisjointClasses .
        [] a owl:AllDisjointProperties .
        [] a owl:AllDifferent .
        [] a owl:NegativePropertyAssertion ; owl:sourceIndividual ex:a ;
           owl:assertionProperty ex:p .
        ex:A rdfs:subClassOf ex:B .
        "#,
    );
    let ontology = read(&statements, &table);
    let reported = |what: &str| {
        (ontology.diagnostics.iter())
            .filter(|d| matches!(d, Diagnostic::Malformed { what: w, .. } if *w == what))
            .count()
    };
    assert_eq!(
        [
            reported("an n-ary axiom without its members"),
            reported("a negative property assertion without its target"),
        ],
        [3, 1],
        "{:?}",
        ontology.diagnostics
    );
    assert!(rendered(&ontology, &table).contains(&"SubClassOf(ex:A ex:B)".to_owned()));
}

/// Spec (*Structural Specification* §9.5: a key is on all its property expressions): a key one
/// of whose members can't be read is no key, and is reported malformed. Read on its other
/// members it would be a stronger key than the input states, identifying more individuals; with
/// none read, an empty key. The member an inverse given twice, a blank node without
/// `owl:inverseOf`, and every member unreadable; each case's kept keys printed.
#[test]
fn a_key_with_a_member_that_cannot_be_read_is_no_key() {
    let what = "a key with a member that can't be read";
    let mut wrong = Vec::new();
    for members in [
        "ex:p [ owl:inverseOf ex:q, ex:r ]",
        "ex:p [ ]",
        "[ owl:inverseOf ex:q, ex:r ]",
    ] {
        let mut table = Table::default();
        let statements = load(
            &mut table,
            &format!(
                "ex:p a owl:ObjectProperty . ex:q a owl:ObjectProperty . ex:r a owl:ObjectProperty .
                ex:C a owl:Class ; owl:hasKey ( {members} ) ."
            ),
        );
        let ontology = read(&statements, &table);
        let keys: Vec<String> = (rendered(&ontology, &table).into_iter())
            .filter(|axiom| axiom.starts_with("HasKey("))
            .collect();
        let reported = (ontology.diagnostics.iter())
            .any(|d| matches!(d, Diagnostic::Malformed { what: w, .. } if *w == what));
        eprintln!("KEY ( {members} ): kept {keys:?}; the key reported malformed: {reported}");
        if !keys.is_empty() || !reported {
            wrong.push(format!("( {members} ): kept {keys:?}, reported {reported}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
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

/// An IRI of the reserved vocabulary as a class, property or individual is OWL 2 Full
/// (Structural Specification §2.4): the axioms that use it are left out, each reserved name
/// they use reported (`rdf:type`, `rdfs:Class`, `owl:Class`, `rdf:first`) (the W3C tests WebOnt-I5.3-014, WebOnt-Class-001, WebOnt-I5.5-001 were answered as if
/// they were fresh names).
#[test]
fn reserved_vocabulary_as_an_entity_is_left_out() {
    let mut table = Table::default();
    let statements = load(
        &mut table,
        r#"
        rdf:type rdfs:domain ex:Y .
        rdfs:Class owl:equivalentClass owl:Class .
        rdf:first a owl:FunctionalProperty .
        ex:x a ex:Y .
        "#,
    );
    let ontology = read(&statements, &table);
    let lines = rendered(&ontology, &table);
    assert!(
        lines.iter().all(|l| !l.contains("www.w3.org")),
        "kept:\n{}",
        lines.join("\n")
    );
    assert!(lines.contains(&"ClassAssertion(ex:Y ex:x)".to_owned()));
    let reserved = ontology
        .diagnostics
        .iter()
        .filter(|d| matches!(d, Diagnostic::Reserved { .. }))
        .count();
    assert_eq!(reserved, 4, "{:?}", ontology.diagnostics);
    assert!(
        ontology
            .diagnostics
            .iter()
            .filter(|d| matches!(d, Diagnostic::Reserved { .. }))
            .all(Diagnostic::is_fatal)
    );
}

/// Spec (Structural Specification §5.8, declarations; §5.5: of the reserved vocabulary only
/// names other than the nine built-in annotation properties are no annotation properties): a
/// declaration of each kind of entity is read as one, through its `rdf:type` statement, and so
/// is `rdfs:label` declared an annotation property.
#[test]
fn a_declaration_of_each_kind_is_read_through_its_typing() {
    let mut table = Table::default();
    let statements = load(
        &mut table,
        r#"
        ex:C a owl:Class . ex:K a rdfs:Class . ex:o a owl:ObjectProperty .
        ex:d a owl:DatatypeProperty . ex:n a owl:AnnotationProperty . ex:t a rdfs:Datatype .
        ex:i a owl:NamedIndividual . rdfs:label a owl:AnnotationProperty .
        "#,
    );
    let lines = rendered(&read(&statements, &table), &table);
    let missing: Vec<&str> = [
        "Declaration(Class(ex:C))",
        "Declaration(Class(ex:K))",
        "Declaration(ObjectProperty(ex:o))",
        "Declaration(DataProperty(ex:d))",
        "Declaration(AnnotationProperty(ex:n))",
        "Declaration(Datatype(ex:t))",
        "Declaration(NamedIndividual(ex:i))",
        "Declaration(AnnotationProperty(<http://www.w3.org/2000/01/rdf-schema#label>))",
    ]
    .into_iter()
    .filter(|d| !lines.contains(&(*d).to_owned()))
    .collect();
    assert!(
        missing.is_empty(),
        "missing {missing:?}\nread:\n{}",
        lines.join("\n")
    );
}

/// Spec (Structural Specification §2.4 allows `owl:Thing`, `owl:Nothing` and the four top and
/// bottom properties as entities; §5.5 lets of the reserved vocabulary only the nine built-in
/// annotation properties be annotation properties; the RDF mapping's Table 6 declares one by
/// `owl:AnnotationProperty` or `owl:OntologyProperty`): each of the six, declared an
/// annotation property in either spelling, is read as no annotation property, where `ex:n`
/// so declared is one.
#[test]
fn the_six_entities_of_the_reserved_vocabulary_declared_annotation_properties_are_none() {
    let six = [
        "Thing",
        "Nothing",
        "topObjectProperty",
        "bottomObjectProperty",
        "topDataProperty",
        "bottomDataProperty",
    ];
    let declared = |text: &str, name: &str| {
        let mut table = Table::default();
        let lines = rendered(&read(&load(&mut table, text), &table), &table);
        lines.contains(&format!("Declaration(AnnotationProperty({name}))"))
    };
    let mut kept = Vec::new();
    for spelling in ["owl:AnnotationProperty", "owl:OntologyProperty"] {
        assert!(
            declared(&format!("ex:n a {spelling} ."), "ex:n"),
            "setup: {spelling}"
        );
        for name in six {
            let iri = format!("<http://www.w3.org/2002/07/owl#{name}>");
            if declared(&format!("owl:{name} a {spelling} ."), &iri) {
                kept.push(format!("{spelling}: {iri}"));
            }
        }
    }
    assert!(kept.is_empty(), "{kept:#?}");
}

/// Spec (Structural Specification §2.4 gives `owl:Thing`, `owl:Nothing` and the four top and
/// bottom properties a class's or a property's role only; §5.6.1: no IRI of the reserved
/// vocabulary is an individual): each of the six where an individual goes (an assertion's
/// individual or value, an equality, a difference, a nominal, a `hasValue`, a negative
/// assertion, a `NamedIndividual` declaration) is reported reserved and its axiom left out;
/// as a class or a property it is read, unreported.
#[test]
fn the_six_entities_of_the_reserved_vocabulary_as_individuals_are_left_out() {
    let reserved = |ontology: &Ontology| {
        (ontology.diagnostics.iter()).any(|d| matches!(d, Diagnostic::Reserved { .. }))
    };
    let places = [
        "ex:p a owl:ObjectProperty . ex:a ex:p owl:Thing .",
        "owl:Nothing a ex:C .",
        "ex:p a owl:ObjectProperty . owl:topObjectProperty ex:p ex:b .",
        "ex:d a owl:DatatypeProperty . owl:topDataProperty ex:d \"v\" .",
        "ex:a owl:sameAs owl:bottomObjectProperty .",
        "ex:a owl:differentFrom owl:bottomDataProperty .",
        "ex:C owl:equivalentClass [ owl:oneOf ( owl:Thing ) ] .",
        "ex:p a owl:ObjectProperty . \
         ex:C owl:equivalentClass [ a owl:Restriction ; owl:onProperty ex:p ; owl:hasValue owl:Nothing ] .",
        "ex:p a owl:ObjectProperty . [] a owl:NegativePropertyAssertion ; \
         owl:sourceIndividual ex:a ; owl:assertionProperty ex:p ; owl:targetIndividual owl:Thing .",
        "owl:Nothing a owl:NamedIndividual .",
    ];
    let mut wrong = Vec::new();
    for data in places {
        let mut table = Table::default();
        let ontology = read(&load(&mut table, data), &table);
        let lines = rendered(&ontology, &table);
        if !reserved(&ontology) || lines.iter().any(|l| l.contains("www.w3.org")) {
            wrong.push(format!("{data}: {lines:?} {:?}", ontology.diagnostics));
        }
    }
    let mut table = Table::default();
    let legal = "ex:C rdfs:subClassOf owl:Thing . owl:Nothing rdfs:subClassOf ex:C . ex:a a owl:Thing . \
        ex:p a owl:ObjectProperty ; rdfs:subPropertyOf owl:topObjectProperty . \
        ex:a owl:topObjectProperty ex:b . ex:a owl:bottomDataProperty \"v\" .";
    let ontology = read(&load(&mut table, legal), &table);
    if reserved(&ontology) || ontology.axioms.len() < 6 {
        let lines = rendered(&ontology, &table);
        wrong.push(format!("legal: {lines:?} {:?}", ontology.diagnostics));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A disjointness that repeats an operand says that operand is empty (an individual
/// different from itself: a contradiction), as OWL 2's RDF-Based Semantics reads it; the
/// operands were deduplicated and the axiom said nothing (WebOnt-disjointWith-010,
/// rdfbased-sem-eqdis-different-irrflxv).
#[test]
fn a_disjointness_repeating_an_operand_empties_it() {
    let mut table = Table::default();
    let statements = load(
        &mut table,
        r#"
        ex:C owl:disjointWith ex:C .
        ex:p a owl:ObjectProperty ; owl:propertyDisjointWith ex:p .
        ex:a owl:differentFrom ex:a .
        "#,
    );
    let ontology = read(&statements, &table);
    let lines = rendered(&ontology, &table);
    for expected in [
        "SubClassOf(ex:C owl:Nothing)",
        "SubClassOf(ObjectSomeValuesFrom(ex:p owl:Thing) owl:Nothing)",
        "ClassAssertion(owl:Nothing ex:a)",
    ] {
        assert!(
            lines.contains(&expected.to_owned()),
            "missing {expected}\nread:\n{}",
            lines.join("\n")
        );
    }
    assert_eq!(ontology.diagnostics, vec![Diagnostic::RepeatedOperand]);
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

/// OWL 2's built-in annotation properties are declared implicitly (Structural Specification
/// 5.5 and 5.8, Table 5; 3.6): undeclared, a domain, a range and a super-property of one
/// are annotation axioms (Mapping to RDF 3.2.5, Table 16), and a property under one matches
/// no pattern there. Each is read as the property's annotation-property axiom, and no
/// logical axiom names it.
#[test]
fn the_built_in_annotation_properties_schema_is_annotations() {
    let builtins = [
        "rdfs:label",
        "rdfs:comment",
        "rdfs:seeAlso",
        "rdfs:isDefinedBy",
        "owl:versionInfo",
        "owl:deprecated",
        "owl:priorVersion",
        "owl:backwardCompatibleWith",
        "owl:incompatibleWith",
    ];
    let mut wrong = Vec::new();
    for a in builtins {
        let iri = a
            .replace("rdfs:", "http://www.w3.org/2000/01/rdf-schema#")
            .replace("owl:", "http://www.w3.org/2002/07/owl#");
        for form in [
            format!("{a} rdfs:domain ex:C . ex:x {a} ex:v ."),
            format!("{a} rdfs:range ex:C . ex:x {a} ex:v ."),
            format!("{a} rdfs:subPropertyOf ex:sup . ex:sup a owl:AnnotationProperty ."),
            format!("ex:p rdfs:subPropertyOf {a} . ex:x ex:p ex:v ."),
        ] {
            let mut table = Table::default();
            let statements = load(&mut table, &form);
            let ontology = read(&statements, &table);
            let lines = rendered(&ontology, &table);
            let named = lines.iter().any(|l| l.contains(&iri));
            if ontology.annotation_schema != [table.iri(&iri).unwrap()] || named {
                wrong.push(format!(
                    "{form}: {:?}, {lines:?}",
                    ontology.annotation_schema
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// OWL 1's `owl:OntologyProperty` typing declares an annotation property (*Mapping to RDF
/// Graphs* §3.1.2, Table 6, which replaces it with `owl:AnnotationProperty`): its domain,
/// range and sub-properties are annotation axioms (Table 16), as a declared annotation
/// property's are, also where the name is typed an object property too: no axiom names it
/// but its declarations.
#[test]
fn an_ontology_property_typing_declares_an_annotation_property() {
    let note = "http://example.com/note";
    let mut wrong = Vec::new();
    for typing in [
        "owl:OntologyProperty",
        "owl:OntologyProperty, owl:ObjectProperty",
    ] {
        for form in [
            "ex:note rdfs:domain ex:C . ex:x ex:note ex:v .",
            "ex:note rdfs:range ex:C . ex:x ex:note ex:v .",
            "ex:note rdfs:subPropertyOf ex:sup . ex:sup a owl:AnnotationProperty .",
            "ex:p rdfs:subPropertyOf ex:note . ex:x ex:p ex:v .",
        ] {
            let data = format!("ex:note a {typing} . {form}");
            let mut table = Table::default();
            let statements = load(&mut table, &data);
            let ontology = read(&statements, &table);
            let lines = rendered(&ontology, &table);
            // As the lines name it (`Table::name`).
            let named = lines
                .iter()
                .filter(|l| l.contains("ex:note"))
                .any(|l| !l.starts_with("Declaration("));
            if ontology.annotation_schema != [table.iri(note).unwrap()] || named {
                wrong.push(format!(
                    "{data}: {:?}, {lines:?}",
                    ontology.annotation_schema
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The built-in annotation properties are known to each read by the terms it reads with:
/// one the terms gain after an earlier read (`rdfs:comment`, which the first read's terms
/// don't have) is an annotation property to the later read, as `rdfs:label` is to both.
#[test]
fn a_built_in_annotation_first_met_in_a_later_read_is_known() {
    let mut table = Table::default();
    let first = load(&mut table, r#"ex:a ex:p ex:b ; rdfs:label "a" ."#);
    let before = read(&first, &table);
    assert_eq!(before.annotations, 1, "{:?}", before.axioms);
    let later = load(
        &mut table,
        r#"ex:a ex:p ex:b ; rdfs:label "a" ; rdfs:comment ex:c ."#,
    );
    let after = read(&later, &table);
    assert_eq!(after.annotations, 2, "{:?}", after.axioms);
    assert_eq!(
        after.axioms, before.axioms,
        "rdfs:comment read as a property"
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
    // Ontologies may break OWL 2 DL's global restrictions (simple properties, named
    // individuals in nominals): reported on both sides, not a matter of the mapping.
    let restriction = |d: &Diagnostic| {
        matches!(
            d,
            Diagnostic::NonSimpleProperty { .. } | Diagnostic::AnonymousIndividual { .. }
        )
    };
    assert!(
        back.diagnostics
            .iter()
            .all(|d| !d.is_fatal() || restriction(d)),
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

/// Spec (*OWL 2 Structural Specification* §9.1.4: a disjoint union is of a class; *Mapping to
/// RDF* Table 16, `CE(*:x)`): `owl:disjointUnionOf` on a blank node, a class expression or
/// not, is malformed on that node and keeps no axiom; on a named class it is read. The kept
/// disjoint unions and the subjects' reports printed.
#[test]
fn a_disjoint_union_on_a_blank_node_is_malformed() {
    let mut table = Table::default();
    let statements = load(
        &mut table,
        r#"
        _:u owl:disjointUnionOf ( ex:A ex:B ) .
        [ owl:unionOf ( ex:C ex:D ) ] owl:disjointUnionOf ( ex:A ex:B ) .
        ex:E owl:disjointUnionOf ( ex:A ex:B ) .
        "#,
    );
    let ontology = read(&statements, &table);
    let union_of = table.iri_id("http://www.w3.org/2002/07/owl#disjointUnionOf");
    let reported: Vec<bool> = (statements.iter())
        .filter(|s| s.triple[1] == union_of && table.kind(s.triple[0]) == TermKind::Blank)
        .map(|s| {
            (ontology.diagnostics.iter())
                .any(|d| matches!(d, Diagnostic::Malformed { node, .. } if *node == s.triple[0]))
        })
        .collect();
    let lines = rendered(&ontology, &table);
    let kept: Vec<&str> = (lines.iter().map(String::as_str))
        .filter(|l| l.starts_with("DisjointUnion"))
        .collect();
    eprintln!("DISJOINT UNION kept {kept:?}; the blank subjects reported {reported:?}");
    assert_eq!(
        (kept, reported),
        (vec!["DisjointUnion(ex:E ex:A ex:B)"], vec![true, true]),
        "{:?}",
        ontology.diagnostics
    );
}

/// A graph read alone: its axioms but declarations, the terms its malformed reports are on
/// (as [`Table::name`] writes them), and every diagnostic.
fn kept_and_malformed(turtle: &str) -> (Vec<String>, Vec<String>, Vec<Diagnostic>) {
    let mut table = Table::default();
    let statements = load(&mut table, turtle);
    let ontology = read(&statements, &table);
    let kept = (rendered(&ontology, &table).into_iter())
        .filter(|l| !l.starts_with("Declaration"))
        .collect();
    let malformed = (ontology.diagnostics.iter())
        .filter_map(|d| match d {
            Diagnostic::Malformed { node, .. } => Some(table.name(*node)),
            _ => None,
        })
        .collect();
    (kept, malformed, ontology.diagnostics)
}

/// Each case is malformed: only the given number of axioms kept from it (none but where it
/// holds another axiom too), and a malformed report on `node` (`None`: on some node); each
/// case's reading printed. `what` names the kind in the printout.
fn assert_malformed(what: &str, cases: &[(&str, Option<&str>, usize)]) {
    let mut wrong = Vec::new();
    for &(turtle, node, axioms) in cases {
        let (kept, malformed, diagnostics) = kept_and_malformed(turtle);
        eprintln!("{what}: {turtle:?}: kept {kept:?}; malformed on {malformed:?}");
        let reported = node.map_or(!malformed.is_empty(), |n| malformed.iter().any(|m| m == n));
        if kept.len() != axioms || !reported {
            wrong.push(format!("{turtle}: kept {kept:?}; {diagnostics:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (*Mapping to RDF* Tables 12 and 13: a blank node of a class expression or a data
/// range has one form, its operator or its restriction's one filler or cardinality): a node
/// with two is no structure, as a structure property given twice is not: malformed, and no
/// axiom is read from it. Two set operators, a set operator with `owl:onProperty`, a
/// restriction with two fillers, one with a filler and a cardinality, a data range with two
/// forms.
#[test]
fn a_node_with_more_than_one_form_is_no_structure() {
    assert_malformed(
        "TWO FORMS",
        &[
            (
                "ex:E owl:equivalentClass [ owl:intersectionOf ( ex:A ex:B ) ; \
                 owl:unionOf ( ex:C ex:D ) ] .",
                None,
                0,
            ),
            (
                "ex:p a owl:ObjectProperty . ex:E owl:equivalentClass [ owl:unionOf ( ex:A ex:B ) ; \
                 owl:onProperty ex:p ; owl:someValuesFrom ex:C ] .",
                None,
                0,
            ),
            (
                "ex:p a owl:ObjectProperty . ex:E rdfs:subClassOf [ a owl:Restriction ; \
                 owl:onProperty ex:p ; owl:someValuesFrom ex:A ; owl:allValuesFrom ex:B ] .",
                None,
                0,
            ),
            (
                "ex:p a owl:ObjectProperty . ex:E rdfs:subClassOf [ a owl:Restriction ; \
                 owl:onProperty ex:p ; owl:someValuesFrom ex:A ; \
                 owl:maxCardinality \"1\"^^xsd:nonNegativeInteger ] .",
                None,
                0,
            ),
            (
                "ex:d a owl:DatatypeProperty . ex:d rdfs:range [ a rdfs:Datatype ; \
                 owl:intersectionOf ( xsd:integer xsd:decimal ) ; \
                 owl:datatypeComplementOf xsd:string ] .",
                None,
                0,
            ),
        ],
    );
}

/// Spec (*Mapping to RDF* Tables 13 and 16: a class `owl:oneOf`'s members, `owl:AllDifferent`'s,
/// `owl:sameAs`'s and `owl:differentFrom`'s object, an object `owl:hasValue`'s value and a negative
/// object assertion's source and target are individuals, `*:x` or `_:x`): a literal there is
/// malformed on the literal, and the axiom is left out. `C ≡ {"1"}` among them, and `a = "1"`
/// with `"1"` and `a` all different, which read with the literal as an individual has no model.
#[test]
fn a_literal_where_an_individual_belongs_is_malformed() {
    let one = Some("\"1\"^^xsd:string");
    assert_malformed(
        "LITERAL INDIVIDUAL",
        &[
            ("ex:C owl:equivalentClass [ owl:oneOf ( \"1\" ) ] .", one, 0),
            (
                "[] a owl:AllDifferent ; owl:members ( \"1\" ex:a ) .",
                one,
                0,
            ),
            ("ex:a owl:sameAs \"1\" .", one, 0),
            (
                "ex:a owl:sameAs \"1\" . [] a owl:AllDifferent ; owl:members ( \"1\" ex:a ) .",
                one,
                0,
            ),
            ("ex:a owl:differentFrom \"1\" .", one, 0),
            (
                "ex:p a owl:ObjectProperty . ex:C rdfs:subClassOf [ a owl:Restriction ; \
                 owl:onProperty ex:p ; owl:hasValue \"1\" ] .",
                one,
                0,
            ),
            (
                "ex:p a owl:ObjectProperty . [] a owl:NegativePropertyAssertion ; \
                 owl:sourceIndividual ex:a ; owl:assertionProperty ex:p ; \
                 owl:targetIndividual \"1\" .",
                one,
                0,
            ),
            (
                "ex:p a owl:ObjectProperty . [] a owl:NegativePropertyAssertion ; \
                 owl:sourceIndividual \"1\" ; owl:assertionProperty ex:p ; \
                 owl:targetIndividual ex:b .",
                one,
                0,
            ),
        ],
    );
}

/// Spec (*Mapping to RDF* Table 16, `owl:disjointUnionOf` with `n ≥ 2`; *Structural
/// Specification* §9.1.4, `disjointClassExpressions`, two at least): a disjoint union of no
/// class, read as its class equal to an empty union, says the class is empty; of one, that it
/// is that one. Each is malformed on its class, and no axiom is read.
#[test]
fn a_disjoint_union_of_fewer_than_two_classes_is_malformed() {
    assert_malformed(
        "SHORT DISJOINT UNION",
        &[
            ("ex:C owl:disjointUnionOf () .", Some("ex:C"), 0),
            ("ex:C owl:disjointUnionOf ( ex:A ) .", Some("ex:C"), 0),
        ],
    );
}

/// Spec (*Mapping to RDF* Table 17, §3.2.2: an `owl:Axiom` or `owl:Annotation` node annotates
/// its main triple, `s p o` from its `owl:annotatedSource`, `owl:annotatedProperty` and
/// `owl:annotatedTarget`, which the graph contains): one whose main triple is absent annotates
/// nothing, and states no axiom: malformed, not taken as annotations without a word.
#[test]
fn a_reified_axiom_without_its_main_triple_is_malformed() {
    assert_malformed(
        "NO MAIN TRIPLE",
        &[
            (
                "[] a owl:Axiom ; owl:annotatedSource ex:A ; \
                 owl:annotatedProperty rdfs:subClassOf ; owl:annotatedTarget ex:B ; \
                 rdfs:comment \"why\" .",
                None,
                0,
            ),
            (
                "ex:A rdfs:subClassOf ex:B . _:x a owl:Axiom ; owl:annotatedSource ex:A ; \
                 owl:annotatedProperty rdfs:subClassOf ; owl:annotatedTarget ex:B ; \
                 rdfs:comment \"why\" . [] a owl:Annotation ; owl:annotatedSource _:x ; \
                 owl:annotatedProperty rdfs:comment ; owl:annotatedTarget \"not given\" ; \
                 rdfs:comment \"how\" .",
                None,
                1,
            ),
        ],
    );
}

/// The valid neighbours of the four forms above get no report, and keep their axioms: a named
/// class with two forms (OWL 1's two axioms on it, or two equivalences); a restriction with a
/// qualified cardinality and its `owl:onClass`; a datatype restriction with its facets; a class
/// `owl:oneOf` of individuals; a data `owl:hasValue` with a literal; a data `owl:oneOf` of
/// literals; a literal on a declared data property, and on an undeclared one (read as data); a
/// disjoint union of two; an annotated axiom and an annotated annotation whose main triples are
/// present. Each case's reading printed.
#[test]
fn the_valid_neighbours_of_the_unchecked_forms_get_no_report() {
    let mut wrong = Vec::new();
    for (turtle, axioms) in [
        (
            "ex:E owl:intersectionOf ( ex:A ex:B ) . ex:E owl:unionOf ( ex:C ex:D ) .",
            &[
                "EquivalentClasses(ObjectIntersectionOf(ex:A ex:B) ex:E)",
                "EquivalentClasses(ObjectUnionOf(ex:C ex:D) ex:E)",
            ][..],
        ),
        (
            "ex:E owl:equivalentClass [ owl:intersectionOf ( ex:A ex:B ) ] , \
             [ owl:unionOf ( ex:C ex:D ) ] .",
            &[
                "EquivalentClasses(ObjectIntersectionOf(ex:A ex:B) ex:E)",
                "EquivalentClasses(ObjectUnionOf(ex:C ex:D) ex:E)",
            ],
        ),
        (
            "ex:p a owl:ObjectProperty . ex:E rdfs:subClassOf [ a owl:Restriction ; \
             owl:onProperty ex:p ; owl:maxQualifiedCardinality \"1\"^^xsd:nonNegativeInteger ; \
             owl:onClass ex:A ] .",
            &["SubClassOf(ex:E ObjectMaxCardinality(1 ex:p ex:A))"],
        ),
        (
            "ex:d a owl:DatatypeProperty . ex:d rdfs:range [ a rdfs:Datatype ; \
             owl:onDatatype xsd:integer ; \
             owl:withRestrictions ( [ xsd:minInclusive \"1\"^^xsd:integer ] ) ] .",
            &[
                "DataPropertyRange(ex:d DatatypeRestriction(xsd:integer xsd:minInclusive \"1\"^^xsd:integer))",
            ],
        ),
        (
            "ex:C owl:equivalentClass [ owl:oneOf ( ex:a ex:b ) ] .",
            &["EquivalentClasses(ObjectOneOf(ex:a ex:b) ex:C)"],
        ),
        (
            "ex:d a owl:DatatypeProperty . ex:C rdfs:subClassOf [ a owl:Restriction ; \
             owl:onProperty ex:d ; owl:hasValue \"1\" ] .",
            &["SubClassOf(ex:C DataHasValue(ex:d \"1\"^^xsd:string))"],
        ),
        (
            "ex:d a owl:DatatypeProperty . ex:d rdfs:range [ a rdfs:Datatype ; \
             owl:oneOf ( \"1\" \"2\" ) ] .",
            &["DataPropertyRange(ex:d DataOneOf(\"1\"^^xsd:string \"2\"^^xsd:string))"],
        ),
        (
            "ex:d a owl:DatatypeProperty . ex:a ex:d \"1\" .",
            &["DataPropertyAssertion(ex:d ex:a \"1\"^^xsd:string)"],
        ),
        (
            "ex:a ex:q \"1\" .",
            &["DataPropertyAssertion(ex:q ex:a \"1\"^^xsd:string)"],
        ),
        (
            "ex:C owl:disjointUnionOf ( ex:A ex:B ) .",
            &["DisjointUnion(ex:C ex:A ex:B)"],
        ),
        (
            "ex:A rdfs:subClassOf ex:B . _:x a owl:Axiom ; owl:annotatedSource ex:A ; \
             owl:annotatedProperty rdfs:subClassOf ; owl:annotatedTarget ex:B ; \
             rdfs:comment \"why\" . _:x rdfs:comment \"given\" . [] a owl:Annotation ; \
             owl:annotatedSource _:x ; owl:annotatedProperty rdfs:comment ; \
             owl:annotatedTarget \"given\" ; rdfs:comment \"how\" .",
            &["SubClassOf(ex:A ex:B)"],
        ),
    ] {
        let (kept, _, diagnostics) = kept_and_malformed(turtle);
        eprintln!("NEIGHBOUR {turtle:?}: kept {kept:?}; {diagnostics:?}");
        if kept != axioms || !diagnostics.is_empty() {
            wrong.push(format!("{turtle}: kept {kept:?}; {diagnostics:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (*Mapping to RDF* Tables 12 and 13: a blank node is one class expression or one data
/// range, by one form): a node that is both, `owl:complementOf` and `owl:datatypeComplementOf`
/// on one node typed `owl:Class` and `rdfs:Datatype`, is no structure: malformed, and no axiom is
/// read from it, whether a class axiom, a data axiom or both use it.
#[test]
fn a_node_with_a_class_and_a_data_range_form_is_no_structure() {
    let node = "_:n a owl:Class , rdfs:Datatype ; owl:complementOf ex:A ; \
                owl:datatypeComplementOf xsd:integer . ex:d a owl:DatatypeProperty . ";
    let (class, range) = ("ex:E rdfs:subClassOf _:n .", "ex:d rdfs:range _:n .");
    assert_malformed(
        "CLASS AND RANGE",
        &[
            (&format!("{node}{class} {range}"), None, 0),
            (&format!("{node}{class}"), None, 0),
            (&format!("{node}{range}"), None, 0),
        ],
    );
}

/// Spec (*Mapping to RDF* Table 16: `*:x op:y *:z`, an object property assertion, takes an
/// individual `*:z`): a literal object on a declared object property is malformed on the literal,
/// and no assertion is read; its neighbours, a declared data property and an undeclared one with a
/// literal, are data assertions (above).
#[test]
fn a_literal_on_a_declared_object_property_is_malformed() {
    assert_malformed(
        "LITERAL ON AN OBJECT PROPERTY",
        &[(
            "ex:p a owl:ObjectProperty . ex:a ex:p \"1\" .",
            Some("\"1\"^^xsd:string"),
            0,
        )],
    );
}

/// Spec (*Mapping to RDF* Tables 16 and 17: an n-ary axiom's RDF form is its main triple and
/// its list): each axiom a disjointness with a repeated operand is read as (the n-ary axiom
/// over the rest, the class equal to a lone operand, the repeated operand empty:
/// `Disjoint::axioms`) comes from that whole form, so its sources, which a refusal's
/// justification names, include the list's `rdf:first` and `rdf:rest` statements. An
/// `owl:AllDisjointClasses` and an `owl:disjointUnionOf`, each repeating `ex:A`. Each axiom's
/// source predicates printed.
#[test]
fn each_axiom_of_a_repeated_disjointness_has_its_list_among_its_sources() {
    let mut wrong = Vec::new();
    for turtle in [
        "[] a owl:AllDisjointClasses ; owl:members ( ex:A ex:A ex:B ) .",
        "ex:C owl:disjointUnionOf ( ex:A ex:A ) .",
    ] {
        let mut table = Table::default();
        let ontology = read(&load(&mut table, turtle), &table);
        for (axiom, sources) in ontology.axioms.iter().zip(&ontology.sources) {
            let line = ontology.functional(axiom, &|t| table.name(t));
            let predicates: Vec<String> = (sources.iter().flat_map(|s| &s.triples))
                .map(|t| table.name(t[1]))
                .collect();
            eprintln!("LIST SOURCES {turtle}: {line}: {predicates:?}");
            let has = |local: &str| {
                predicates
                    .iter()
                    .any(|p| p.ends_with(&format!("#{local}>")))
            };
            if !has("first") || !has("rest") {
                wrong.push(format!("{turtle}: {line}: {predicates:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
