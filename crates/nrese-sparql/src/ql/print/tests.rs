//! The printer on small stores: what it writes, and what the printed query answers on
//! the data as asserted, without any reasoning (the differential test against NRESE with
//! reasoning is the store's).

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_sparql_syntax::SparqlParser;

use super::{PrintForm, Printed, print};
use crate::query::{QueryOptions, evaluate_query};
use crate::results::QueryResults;

const PREFIXES: &str = "@prefix : <http://e/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
";

/// A store holding `turtle` as asserted: nothing materialised.
fn engine(turtle: &str) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let text = format!("{PREFIXES}{turtle}");
    let mut tx = engine.transaction();
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_reader(text.as_bytes()) {
        tx.insert(quad.unwrap().as_ref());
    }
    tx.commit().unwrap();
    engine
}

fn printed(e: &Engine, query: &str, form: PrintForm) -> Printed {
    let query = SparqlParser::new()
        .parse_query(&format!("PREFIX : <http://e/> {query}"))
        .unwrap();
    print(&e.snapshot(), &query, form).unwrap()
}

/// The answers of `query` on the store as asserted, as sorted local names.
fn answers(e: &Engine, query: &str) -> Vec<String> {
    let query = SparqlParser::new()
        .parse_query(query)
        .unwrap_or_else(|error| panic!("{error}: {query}"));
    let snapshot = e.snapshot();
    let QueryResults::Solutions(solutions) =
        evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap()
    else {
        panic!("a SELECT");
    };
    let mut out: Vec<String> = solutions
        .map(|s| {
            s.unwrap()
                .iter()
                .map(|(_, t)| t.to_string().replace("http://e/", ""))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    out.sort();
    out
}

/// The printed query, in both forms, answers `expected` on the asserted data.
fn answers_as(turtle: &str, query: &str, expected: &[&str]) -> String {
    let e = engine(turtle);
    let mut text = String::new();
    for form in [PrintForm::Paths, PrintForm::Values] {
        let Printed::Query { text: q, .. } = printed(&e, query, form) else {
            panic!("{query}: {:?}", printed(&e, query, form));
        };
        assert_eq!(answers(&e, &q), expected, "{}:\n{q}", form.name());
        if form == PrintForm::Paths {
            text = q;
        }
    }
    text
}

#[test]
fn properties_print_as_paths_of_sub_properties_inverses_transitivity_and_chains() {
    let schema = ":headOf rdfs:subPropertyOf :worksFor .
        :worksFor rdfs:subPropertyOf :memberOf .
        :member owl:inverseOf :memberOf .
        :subOrganizationOf a owl:TransitiveProperty .
        :memberOf a owl:ObjectProperty .
        :inUniversity owl:propertyChainAxiom ( :memberOf :subOrganizationOf ) .";
    let data = ":ann :headOf :dept . :dept2 :member :bob .
        :dept :subOrganizationOf :school . :school :subOrganizationOf :uni .";
    let turtle = format!("{schema}\n{data}");
    let text = answers_as(
        &turtle,
        "SELECT ?x ?y WHERE { ?x :memberOf ?y }",
        &["<ann> <dept>", "<bob> <dept2>"],
    );
    assert!(text.contains("^(<http://e/member>)"), "{text}");
    answers_as(
        &turtle,
        "SELECT ?y WHERE { :dept :subOrganizationOf ?y }",
        &["<school>", "<uni>"],
    );
    answers_as(
        &turtle,
        "SELECT ?x ?u WHERE { ?x :inUniversity ?u }",
        &["<ann> <school>", "<ann> <uni>"],
    );
}

#[test]
fn classes_print_with_their_hierarchy_and_rl_rules_unfolded() {
    // LUBM's shape: a class defined by an intersection with a qualified existential.
    let turtle = ":Student owl:equivalentClass [ owl:intersectionOf ( :Person
            [ a owl:Restriction ; owl:onProperty :takesCourse ; owl:someValuesFrom :Course ] ) ] .
        :GraduateStudent rdfs:subClassOf :Student .
        :takesCourse rdfs:domain :Person .
        :UndergraduateCourse rdfs:subClassOf :Course .
        :sam :takesCourse :c1 . :c1 a :UndergraduateCourse .
        :gil a :GraduateStudent .
        :pat a :Person .";
    let text = answers_as(
        turtle,
        "SELECT ?x WHERE { ?x a :Student }",
        &["<gil>", "<sam>"],
    );
    assert!(
        text.contains(
            "rdf-syntax-ns#type> / ((((<http://www.w3.org/2000/01/rdf-schema#subClassOf>"
        ),
        "{text}"
    );
    // The path reaches an intersection's members: nothing below Person is enumerated.
    let text = answers_as(
        turtle,
        "SELECT ?x WHERE { ?x a :Person }",
        &["<gil>", "<pat>", "<sam>"],
    );
    assert!(
        text.contains("intersectionOf> / ((<http://www.w3.org/1999/02/22-rdf-syntax-ns#rest>)*"),
        "{text}"
    );
    assert!(!text.contains("type> <http://e/"), "{text}");
}

#[test]
fn answers_through_existentials_print_with_the_tree_witnesses() {
    let turtle = ":Employee rdfs:subClassOf [ a owl:Restriction ;
            owl:onProperty :worksFor ; owl:someValuesFrom :Organisation ] .
        :Manager rdfs:subClassOf :Employee .
        :ann :worksFor :acme . :cat a :Manager .";
    answers_as(
        turtle,
        "SELECT ?x WHERE { ?x :worksFor [] }",
        &["<ann>", "<cat>"],
    );
    // Bags: the stated rows as they are, the rest once.
    let bag = format!("{turtle} :ann :worksFor :initech .");
    answers_as(
        &bag,
        "SELECT ?x WHERE { ?x :worksFor ?y }",
        &["<ann>", "<ann>", "<cat>"],
    );
}

#[test]
fn what_the_printer_cant_write_exactly_it_reports() {
    let not = |turtle: &str, query: &str, why: &str| {
        let e = engine(turtle);
        match printed(&e, query, PrintForm::Paths) {
            Printed::NotExpressible(reasons) => {
                assert!(
                    reasons.iter().any(|r| r.contains(why)),
                    "{why}: {reasons:?}"
                );
            }
            Printed::Query { text, .. } => panic!("printed, but shouldn't be:\n{text}"),
        }
    };
    not(
        ":p a owl:FunctionalProperty . :a :p :b .",
        "SELECT ?x WHERE { ?x :p ?y }",
        "equality",
    );
    not(
        ":a owl:sameAs :b . :a :p :c .",
        "SELECT ?x WHERE { ?x :p ?y }",
        "sameAs",
    );
    not(
        ":A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:allValuesFrom :B ] .",
        "SELECT ?x WHERE { ?x a :B }",
        "allValuesFrom",
    );
    not(
        ":p owl:propertyChainAxiom ( :q :p ) .",
        "SELECT ?x WHERE { ?x :p ?y }",
        "recursive",
    );
    not(
        ":p rdfs:subPropertyOf :q .",
        "SELECT ?x ?p WHERE { ?x ?p ?y }",
        "variable predicate",
    );
    // Where NRESE's own answers are sound-only (here a transitive property meets an
    // existential), the printed query gives the same answers and says so.
    let e = engine(
        ":p a owl:TransitiveProperty .
         :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :A ] .",
    );
    let Printed::Query { completeness, .. } =
        printed(&e, "SELECT ?x WHERE { ?x :p ?y }", PrintForm::Paths)
    else {
        panic!("printable: the same answers as NRESE's");
    };
    assert_eq!(completeness.as_str(), "sound-only");
    assert!(
        completeness
            .reasons
            .iter()
            .any(|r| r.text.contains("is transitive")),
        "{completeness:?}"
    );
    // A query the closure adds nothing to prints as it is.
    let e = engine(":a :p :b .");
    let Printed::Query { text: q, .. } =
        printed(&e, "SELECT ?x WHERE { ?x :p ?y }", PrintForm::Values)
    else {
        panic!("printable");
    };
    assert!(!q.contains("DISTINCT"), "{q}");
}

#[test]
fn negations_and_the_default_graph_alias_print_with_the_rewriting() {
    let turtle = ":Employee rdfs:subClassOf [ a owl:Restriction ;
            owl:onProperty :worksFor ; owl:someValuesFrom :Organisation ] .
        :Manager rdfs:subClassOf :Employee , :Person .
        :Visitor rdfs:subClassOf :Person .
        :cat a :Manager . :dan a :Visitor .";
    answers_as(
        turtle,
        "SELECT ?x WHERE { ?x a :Person FILTER NOT EXISTS { ?x :worksFor [] } }",
        &["<dan>"],
    );
    answers_as(
        turtle,
        "SELECT ?x WHERE { ?x a :Person MINUS { ?x :worksFor ?y } }",
        &["<dan>"],
    );
    answers_as(
        turtle,
        "SELECT ?x WHERE { GRAPH <urn:x-arq:DefaultGraph> { ?x :worksFor [] } }",
        &["<cat>"],
    );
}

/// The printer's budget: RL rules unfolded into each other can make the printed query
/// exponential (each class here has two rules over the one below: 2^18 copies of the
/// bottom); past its bound it says so instead of printing.
#[test]
fn the_printer_stops_at_its_size_bound() {
    let mut turtle = String::new();
    for i in 1..=18 {
        for p in ["p", "q"] {
            turtle.push_str(&format!(
                "[ owl:intersectionOf ( :C{} [ a owl:Restriction ; owl:onProperty :{p} ;
                    owl:someValuesFrom :X ] ) ] rdfs:subClassOf :C{i} .\n",
                i - 1
            ));
        }
    }
    let e = engine(&turtle);
    match printed(&e, "SELECT ?x WHERE { ?x a :C18 }", PrintForm::Paths) {
        Printed::NotExpressible(reasons) => {
            assert!(
                reasons.iter().any(|r| r.contains("class expansions")),
                "{reasons:?}"
            );
        }
        Printed::Query { text, .. } => panic!("printed {} bytes", text.len()),
    }
}
