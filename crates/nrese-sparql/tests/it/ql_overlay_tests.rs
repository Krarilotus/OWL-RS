//! Dictionary lifecycle V1, attack item 4 (`notes/2026-11-09-dict-v1-speculative-private-design.md`
//! §4; the caller trace's caller 1, item 5): QL across private vocabularies. Two private
//! transactions over one snapshot give their own terms the same numbers, so their schemas can
//! be equal by number and different in meaning. The QL rewriting must answer each from its own
//! schema, never one read for the other.
//!
//! The rewriting answers through existentials (tree witnesses: `ql_tests`); what a class
//! hierarchy alone entails is the materialisation's, so the schema here is an existential.

use std::sync::Arc;

use nrese_engine::{Engine, EngineConfig, Snapshot};
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

/// `class rdfs:subClassOf [ owl:onProperty :worksFor ; owl:someValuesFrom :Org ]` and
/// `:y a class`: the class, the restriction's blank node and `:y` new terms, in that order
/// (the same numbers in every vocabulary that adds them to one snapshot's).
fn schema(class: &str) -> String {
    format!(
        "{PREFIXES} <{class}> rdfs:subClassOf _:r .
         _:r a owl:Restriction ; owl:onProperty :worksFor ; owl:someValuesFrom :Org .
         :y a <{class}> ."
    )
}

/// `?x :worksFor ?o` through `ql`, on the pending snapshot of a private transaction that
/// states [`schema`] of `class`.
fn workers(engine: &Engine, ql: &Arc<QlRewriting>, class: &str) -> Vec<String> {
    answers(&private(engine, class), ql)
}

/// The pending snapshot of a private transaction over the latest that states [`schema`] of
/// `class`.
fn private(engine: &Engine, class: &str) -> Snapshot {
    let mut tx = engine.speculative(Arc::new(Budget::unlimited()));
    let text = schema(class);
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_reader(text.as_bytes()) {
        tx.try_insert(quad.unwrap().as_ref()).unwrap();
    }
    tx.pending_snapshot().unwrap()
}

/// `?x :worksFor ?o` on `snapshot` through `ql`, sorted.
fn answers(snapshot: &Snapshot, ql: &Arc<QlRewriting>) -> Vec<String> {
    let options = QueryOptions {
        ql: Some(Arc::clone(ql)),
        ..QueryOptions::default()
    };
    let query = SparqlParser::new()
        .parse_query("SELECT ?x WHERE { ?x <http://example.org/worksFor> ?o }")
        .unwrap();
    let QueryResults::Solutions(rows) = evaluate_query(snapshot, &query, &options).unwrap() else {
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

/// A publication after a private snapshot opened (review dict-v1c r2, R2): private
/// transactions take no writer slot, so a writer over the same base gives its new terms the
/// private snapshot's own numbers, and the store's entry its read compiles can be equal by
/// number to the private schema and differ in meaning.
///
/// - Input: a private transaction states [`schema`] of one class, its pending snapshot kept
///   and not read; a writer then commits [`schema`] of the other over the same base, and the
///   publication is read through the rewriting (its TBox in the store's entries); then the
///   private snapshot is read through it. Each way round: `:New` private and the reserved name
///   published, and the reverse.
/// - Expected: the private snapshot answers from its own schema: `:y` for `:New`, none for the
///   reserved name.
#[test]
fn a_private_schema_is_never_read_from_a_later_publication_equal_by_number() {
    let ordinary = format!("{EX}New");
    let reserved = format!("{OWL}NotAnOwlTerm");
    let y = vec![format!("<{EX}y>")];
    let mut failed = Vec::new();
    for (own, published) in [(&ordinary, &reserved), (&reserved, &ordinary)] {
        let engine = engine();
        let ql = Arc::new(QlRewriting::new(Closure { lists: true }));
        let pending = private(&engine, own);
        let mut writer = engine.transaction();
        let text = schema(published);
        for quad in RdfParser::from_format(RdfFormat::Turtle).for_reader(text.as_bytes()) {
            writer.insert(quad.unwrap().as_ref());
        }
        writer.commit().unwrap();
        let publication = answers(&engine.snapshot(), &ql);
        let got = answers(&pending, &ql);
        let want: &[String] = if own == &ordinary { &y } else { &[] };
        if got != want {
            failed.push(format!(
                "{own} private, {published} published (answered {publication:?}): {got:?}, \
                 expected {want:?}"
            ));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
