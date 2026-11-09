//! Search over a private snapshot (dictionary lifecycle V3; the contract's §7, decision of
//! 10:23 item 1: "private views are scored over what they can see; committed global reads
//! are unchanged"). A private transaction's pending snapshot searches, text and IRI names,
//! as an independent engine holding exactly its statements would: its own new terms
//! (overlay ids) found, a base document it removes or hides not found or counted, a word
//! only it has found, terms it interned but doesn't use not found. The shared store of
//! corpora, and the published snapshot's answers, stay as they were.
//!
//! Oracle: an engine committed with the base's statements and the private delta
//! ([`oracle`]); answers compared as decoded texts or IRIs with their relevance.
//!
//! The shared fixture and helpers are here; the tests, by topic, in the modules below.

mod budgets;
mod equality;
mod parallel;
mod vectors;
mod what_it_sees;

use std::sync::Arc;

use nrese_engine::{
    CorpusTuning, EncodedTriple, Engine, EngineConfig, GraphSelector, InferredSubset, ReadModel,
    Snapshot, TermId, TextQuery, TextScope,
};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};

use crate::text_scope_tests::label;

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("http://example.com/{local}"))
}

/// `ex:holder ex:points ex:dormant`: `ex:dormant` an object only, in the base.
fn points() -> Quad {
    Quad::new(
        ex("holder"),
        ex("points"),
        ex("dormant"),
        GraphName::DefaultGraph,
    )
}

/// The base: labels in the default graph and in `ex:g`, `points`, and a literal interned,
/// used, then let go (`"gone word"`: the dictionary has it, no statement uses it).
fn base(engine: &Engine) {
    engine.set_corpus_tuning(CorpusTuning {
        documents_per_probe: 1,
        ..CorpusTuning::DEFAULT
    });
    let mut tx = engine.transaction();
    tx.insert(label("a", "alpha one two", None).as_ref());
    tx.insert(label("b", "beta", None).as_ref());
    for i in 0..8 {
        tx.insert(label(&format!("f{i}"), &format!("filler {i}"), None).as_ref());
    }
    tx.insert(label("g", "beta in g", Some("g")).as_ref());
    tx.insert(label("gone", "gone word", None).as_ref());
    tx.insert(points().as_ref());
    tx.commit().unwrap();
    let mut tx = engine.transaction();
    tx.remove(label("gone", "gone word", None).as_ref());
    tx.commit().unwrap();
}

/// The private delta: a new literal with a word the base lacks, on a new subject; a base
/// literal no statement used, used again; a base document removed; `ex:dormant` made a
/// subject; a new IRI subject in `ex:g`.
fn inserted() -> Vec<Quad> {
    vec![
        label("p", "alpha private zeta", None),
        label("q", "gone word", None),
        label("dormant", "dormant text", None),
        label("zebra", "stripes", Some("g")),
        label("p2", "late shared", None),
    ]
}

/// What a writer publishes after the private transaction opened (R3-F2): a literal the
/// private delta then uses too (its own there, found once), and one it doesn't (not found).
/// Their dictionary entries take numbers the overlay's own terms have.
fn published_later() -> Vec<Quad> {
    vec![
        label("w", "late shared", None),
        label("w", "writer only", None),
    ]
}

fn removed() -> Vec<Quad> {
    vec![label("b", "beta", None)]
}

/// Interned by the private transaction, used by none of its statements.
fn unused() -> Term {
    Literal::new_simple_literal("unused omega").into()
}

/// An engine holding the base's statements with the delta applied (not the let-go literal,
/// nor the unused one: no statement uses them).
fn oracle() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let mut tx = engine.transaction();
    for quad in inserted() {
        tx.insert(quad.as_ref());
    }
    for quad in removed() {
        tx.remove(quad.as_ref());
    }
    tx.commit().unwrap();
    engine
}

fn query(text: &str, prefix: bool, all_words: bool) -> TextQuery {
    TextQuery {
        text: text.to_owned(),
        all_words,
        prefix,
        stem: None,
    }
}

/// The queries: words shared and own, an unused term's, a let-go one's, an exclusion, a
/// phrase across shared and own words, a prefix, a fuzzy match, all words.
fn queries() -> Vec<(&'static str, TextQuery)> {
    vec![
        ("alpha", query("alpha", false, false)),
        ("beta", query("beta", false, false)),
        ("zeta", query("zeta", false, false)),
        ("omega", query("omega", false, false)),
        ("gone", query("gone", false, false)),
        ("stripes", query("stripes", false, false)),
        ("alpha -zeta", query("alpha -zeta", false, false)),
        ("phrase", query("\"alpha private\"", false, false)),
        ("prefix", query("alp", true, false)),
        ("fuzzy", query("zeto~1", false, false)),
        ("all words", query("alpha zeta", false, true)),
        ("dormant", query("dormant", false, false)),
        ("late", query("late", false, false)),
        ("writer", query("writer", false, false)),
    ]
}

fn iri_queries() -> Vec<(&'static str, TextQuery)> {
    vec![
        ("zebra", query("zebra", false, false)),
        ("dormant", query("dormant", false, false)),
        ("holder", query("holder", false, false)),
        ("p", query("p", false, false)),
        ("prefix", query("zeb", true, false)),
    ]
}

/// The scopes, by each snapshot's own ids: every graph, the default graph, `ex:g`.
fn scopes(snapshot: &Snapshot) -> Vec<(&'static str, TextScope)> {
    let g = snapshot.lookup(ex("g").as_ref().into()).expect("ex:g");
    vec![
        ("every", TextScope::EVERY),
        (
            "default",
            TextScope::Graphs(GraphSelector::Exact(TermId::DEFAULT_GRAPH)),
        ),
        ("g", TextScope::Graphs(GraphSelector::Exact(g))),
    ]
}

/// Matches decoded: a literal's value or an IRI, with its relevance.
fn decoded(snapshot: &Snapshot, found: &[nrese_engine::TextMatch]) -> Vec<(String, f64)> {
    found
        .iter()
        .map(|m| match snapshot.decode(TermId::from_raw(m.id)) {
            Some(Term::Literal(l)) => (l.value().to_owned(), m.relevance),
            Some(Term::NamedNode(n)) => (n.as_str().to_owned(), m.relevance),
            other => panic!("{other:?}"),
        })
        .collect()
}

/// A named answer: a search's decoded matches, or its refusal.
type Row = (String, Result<Vec<(String, f64)>, String>);
type Answers = Vec<Row>;

/// Every answer of `snapshot` (text and IRI, each model, scope and query), by name.
fn answers(snapshot: &Snapshot) -> Answers {
    let mut out = Vec::new();
    for model in [ReadModel::Materialised, ReadModel::Asserted] {
        for (scope_name, scope) in scopes(snapshot) {
            for (iris, queries) in [(false, queries()), (true, iri_queries())] {
                for (name, query) in queries {
                    let found = match iris {
                        false => snapshot.text_search_in(model, &scope, &query, None),
                        true => snapshot.iri_search_in(model, &scope, &query, None),
                    };
                    let key = format!("{model:?} {scope_name} iris {iris} {name}");
                    out.push((
                        key,
                        found
                            .map(|found| decoded(snapshot, &found))
                            .map_err(|e| format!("{e:?}")),
                    ));
                }
            }
        }
    }
    out
}

/// What the store of corpora holds, without the counters of builds.
fn held(snapshot: &Snapshot) -> (bool, u64, u64, u64) {
    let stats = snapshot.text_corpus_stats();
    (stats.started, stats.kept, stats.bytes, stats.steps)
}

fn rows_equal(got: &[Row], want: &[Row], context: &str, failed: &mut Vec<String>) {
    assert_eq!(got.len(), want.len());
    for ((name, got), (_, want)) in got.iter().zip(want) {
        if got != want {
            failed.push(format!("{context}: {name}: {got:?} != {want:?}"));
        }
    }
}
