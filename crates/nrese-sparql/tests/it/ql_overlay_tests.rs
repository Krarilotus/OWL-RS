//! Dictionary lifecycle V1, attack item 4 (`notes/2026-11-09-dict-v1-speculative-private-design.md`
//! §4; the caller trace's caller 1, item 5): QL across private vocabularies. Two private
//! transactions over one snapshot give their own terms the same numbers, so their schemas can
//! be equal by number and different in meaning. The QL rewriting must answer each from its own
//! schema, never one read for the other.
//!
//! The rewriting answers through existentials (tree witnesses: `ql_tests`); what a class
//! hierarchy alone entails is the materialisation's, so the schema here is an existential.

use std::sync::Arc;

use nrese_engine::{Engine, EngineConfig};
use nrese_exec::Budget;
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_sparql::ql::{Closure, QlRewriting};
use nrese_sparql::{QueryOptions, QueryResults, evaluate_query};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.org/";
const OWL: &str = "http://www.w3.org/2002/07/owl#";
const PREFIXES: &str = "@prefix : <http://example.org/> .
    @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
    @prefix owl: <http://www.w3.org/2002/07/owl#> .";

/// A published property `:worksFor` and class `:Org`, and a published `owl:Thing` member.
fn engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let text =
        format!("{PREFIXES} :worksFor a owl:ObjectProperty . :Org a owl:Class . :z a owl:Thing .");
    let mut tx = engine.transaction();
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_reader(text.as_bytes()) {
        tx.insert(quad.unwrap().as_ref());
    }
    tx.commit().unwrap();
    engine
}

/// `?x :worksFor ?o` through `ql`, on the pending snapshot of a private transaction that
/// states `class rdfs:subClassOf [ owl:onProperty :worksFor ; owl:someValuesFrom :Org ]` and
/// `:y a class`: the class, the restriction's blank node and `:y` its own new terms, in that
/// order (the same numbers in every such transaction over one snapshot).
fn workers(engine: &Engine, ql: &Arc<QlRewriting>, class: &str) -> Vec<String> {
    let mut tx = engine.speculative(Arc::new(Budget::unlimited()));
    let text = format!(
        "{PREFIXES} <{class}> rdfs:subClassOf _:r .
         _:r a owl:Restriction ; owl:onProperty :worksFor ; owl:someValuesFrom :Org .
         :y a <{class}> ."
    );
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_reader(text.as_bytes()) {
        tx.try_insert(quad.unwrap().as_ref()).unwrap();
    }
    let pending = tx.pending_snapshot().unwrap();
    let options = QueryOptions {
        ql: Some(Arc::clone(ql)),
        ..QueryOptions::default()
    };
    let query = SparqlParser::new()
        .parse_query("SELECT ?x WHERE { ?x <http://example.org/worksFor> ?o }")
        .unwrap();
    let QueryResults::Solutions(rows) = evaluate_query(&pending, &query, &options).unwrap() else {
        panic!("solutions expected");
    };
    let mut got: Vec<String> = rows
        .map(|row| row.unwrap().get("x").unwrap().to_string())
        .collect();
    got.sort();
    got
}

/// - Input: two private transactions over one snapshot, read through one QL rewriting. One
///   makes a new ordinary class `:New` a subclass of "works for an organisation" with `:y` a
///   member; the other does the same with a new name in the reserved OWL namespace, which the
///   OWL reading leaves out of the schema (an entity of a reserved vocabulary). Their new terms
///   take the same numbers, so their statements are equal by number. Each order of the two.
/// - Expected: `:y` works for something through `:New` only: the reserved name's transaction
///   answers none, whichever read first, and the other `:y`.
#[test]
fn private_vocabularies_equal_by_number_keep_their_own_ql_schema() {
    let engine = engine();
    let ordinary = format!("{EX}New");
    let reserved = format!("{OWL}NotAnOwlTerm");
    let y = vec![format!("<{EX}y>")];
    let mut failed = Vec::new();
    for order in [[&ordinary, &reserved], [&reserved, &ordinary]] {
        let ql = Arc::new(QlRewriting::new(Closure { lists: true }));
        for class in order {
            let got = workers(&engine, &ql, class);
            let want: &[String] = if class == &ordinary { &y } else { &[] };
            if got != want {
                failed.push(format!(
                    "{class}, read {}: {got:?}, expected {want:?}",
                    if class == order[0] { "first" } else { "second" }
                ));
            }
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
