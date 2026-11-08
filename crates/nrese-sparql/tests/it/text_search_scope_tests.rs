//! Full-text relevance under `SearchScope::Reader` (native/search.rs,
//! `Snapshot::text_search_in`): computed over the documents the reader can see, the string
//! literals that are objects of statements in the pattern's graphs, at the snapshot's
//! version, as the reader's access restricts them. Neither uncommitted literals, nor later
//! versions, nor graphs the reader may not read change a score or an order; a cached result
//! equals a fresh one. Under `SearchScope::Global` (the default) relevance stays the
//! dictionary's, as documented.
//!
//! Each oracle is an independent store holding only what the reader sees, or the same
//! store before the change: never the code under test's own corpus.

use std::sync::Arc;

use nrese_engine::{Engine, EngineConfig, Snapshot};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_sparql::{
    GraphAccess, QueryOptions, QueryResults, ResultCache, SearchScope, evaluate_query,
};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";
const PREFIXES: &str = "PREFIX bds: <http://www.bigdata.com/rdf/search#> \
                        PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

/// Two words of equal document frequency: `beta` in a shorter literal ranks first, until
/// `beta` grows common somewhere (then `alpha`, the rarer word, wins).
const RANKED: &str = "SELECT ?o ?score WHERE { ?o bds:search \"alpha beta\" ; bds:relevance ?score } \
                      ORDER BY DESC(?score) ?o";

/// [`RANKED`] under `GRAPH ?g`.
const NAMED: &str = "SELECT ?o ?score WHERE { GRAPH ?g { ?o bds:search \"alpha beta\" ; bds:relevance ?score } } \
                     ORDER BY DESC(?score) ?o";

/// [`RANKED`] in Jena's vocabulary: the matched literal and its score.
const JENA: &str = "PREFIX text: <http://jena.apache.org/text#> \
                    SELECT ?o ?score WHERE { (?s ?score ?o) text:query \"alpha beta\" } \
                    ORDER BY DESC(?score) ?o";

/// [`RANKED`] in GraphDB's legacy Lucene vocabulary: the resources and their score.
const LUC: &str = "PREFIX luc: <http://www.ontotext.com/owlim/lucene#> \
                   SELECT ?o ?score WHERE { ?o luc:anyIndex \"alpha beta\" ; luc:score ?score } \
                   ORDER BY DESC(?score) ?o";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

fn label_quad(subject: &str, text: &str, graph: Option<&str>) -> Quad {
    let graph: GraphName = match graph {
        Some(g) => ex(g).into(),
        None => GraphName::DefaultGraph,
    };
    Quad::new(
        ex(subject),
        NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label"),
        Term::from(Literal::new_simple_literal(text)),
        graph,
    )
}

/// The default graph: `alpha` and `beta` once each, and words of neither.
fn base(engine: &Engine) {
    let mut tx = engine.transaction();
    tx.insert(label_quad("a", "alpha one two", None).as_ref());
    tx.insert(label_quad("b", "beta", None).as_ref());
    for i in 0..8 {
        tx.insert(label_quad(&format!("f{i}"), &format!("filler {i}"), None).as_ref());
    }
    tx.commit().unwrap();
}

/// `beta` in many literals, in `graph`.
fn betas(tx: &mut nrese_engine::Transaction<'_>, graph: Option<&str>) {
    for i in 0..60 {
        tx.insert(label_quad(&format!("x{i}"), &format!("beta extra {i}"), graph).as_ref());
    }
}

fn reader() -> QueryOptions {
    QueryOptions {
        search_scope: SearchScope::Reader,
        ..QueryOptions::default()
    }
}

fn rows(snapshot: &Snapshot, query: &str, options: &QueryOptions) -> Vec<(String, String)> {
    let text = format!("{PREFIXES}{query}");
    let query = SparqlParser::new()
        .parse_query(&text)
        .unwrap_or_else(|e| panic!("{e}: {text}"));
    let QueryResults::Solutions(solutions) = evaluate_query(snapshot, &query, options).unwrap()
    else {
        panic!("solutions")
    };
    solutions
        .map(|solution| {
            let solution = solution.unwrap();
            let value = |name: &str| match solution.get(name) {
                Some(Term::Literal(l)) => l.value().to_owned(),
                other => format!("{other:?}"),
            };
            (value("o"), value("score"))
        })
        .collect()
}

/// Each row's failure, collected: every row reports, not only the first.
#[derive(Default)]
struct Rows(Vec<String>);

impl Rows {
    fn check<T: PartialEq + std::fmt::Debug>(&mut self, row: &str, got: T, want: T) {
        if got != want {
            self.0.push(format!("{row}: got {got:?}, want {want:?}"));
        }
    }

    fn done(self) {
        assert!(
            self.0.is_empty(),
            "{} rows failed:\n{}",
            self.0.len(),
            self.0.join("\n")
        );
    }
}

/// Another writer's literals, not committed: the dictionary holds them, no snapshot does.
#[test]
fn uncommitted_literals_change_no_score() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let snapshot = engine.snapshot();
    let before = rows(&snapshot, RANKED, &reader());
    assert_eq!(before[0].0, "beta", "{before:?}");
    let mut tx = engine.transaction();
    betas(&mut tx, None);
    let mut check = Rows::default();
    check.check(
        "earlier snapshot",
        rows(&snapshot, RANKED, &reader()),
        before.clone(),
    );
    check.check(
        "new snapshot",
        rows(&engine.snapshot(), RANKED, &reader()),
        before.clone(),
    );
    drop(tx);
    check.check(
        "after abort",
        rows(&engine.snapshot(), RANKED, &reader()),
        before,
    );
    check.done();
}

#[test]
fn later_versions_change_no_score_of_an_earlier_one() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let snapshot = engine.snapshot();
    let before = rows(&snapshot, RANKED, &reader());
    let mut tx = engine.transaction();
    betas(&mut tx, None);
    tx.commit().unwrap();
    let mut check = Rows::default();
    check.check(
        "earlier snapshot",
        rows(&snapshot, RANKED, &reader()),
        before,
    );
    // The new version counts them: `beta` is common now, `alpha` ranks first (guard).
    let after = rows(&engine.snapshot(), RANKED, &reader());
    check.check("new version", after[0].0.as_str(), "alpha one two");
    check.done();
}

/// Literals deleted to unused: a store where they never were is the oracle.
#[test]
fn literals_no_statement_uses_count_for_nothing() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let before = rows(&engine.snapshot(), RANKED, &reader());
    let mut tx = engine.transaction();
    betas(&mut tx, None);
    tx.commit().unwrap();
    let mut tx = engine.transaction();
    for i in 0..60 {
        tx.remove(label_quad(&format!("x{i}"), &format!("beta extra {i}"), None).as_ref());
    }
    tx.commit().unwrap();
    assert_eq!(rows(&engine.snapshot(), RANKED, &reader()), before);
}

/// A reader who may not read graph `G` gets the scores and the order of a store without
/// `G`: in the default graph, in the merge of the graphs it may read, and under `GRAPH ?g`.
#[test]
fn an_unreadable_graph_changes_no_score() {
    let with_g = Engine::new(EngineConfig::default()).unwrap();
    base(&with_g);
    let mut tx = with_g.transaction();
    betas(&mut tx, Some("G"));
    tx.insert(label_quad("h", "alpha in h", Some("H")).as_ref());
    tx.commit().unwrap();
    let without_g = Engine::new(EngineConfig::default()).unwrap();
    base(&without_g);
    let mut tx = without_g.transaction();
    tx.insert(label_quad("h", "alpha in h", Some("H")).as_ref());
    tx.commit().unwrap();

    let restricted = |union: bool| QueryOptions {
        union_default_graph: union,
        access: Some(Arc::new(GraphAccess {
            default_graph: true,
            graphs: vec![format!("{EX}H")],
            ..GraphAccess::default()
        })),
        ..reader()
    };
    let everyone = |union: bool| QueryOptions {
        union_default_graph: union,
        ..reader()
    };
    let mut check = Rows::default();
    for union in [false, true] {
        for (name, query) in [
            ("default", RANKED),
            ("GRAPH ?g", NAMED),
            ("text:query", JENA),
            ("luc:anyIndex", LUC),
        ] {
            let got = rows(&with_g.snapshot(), query, &restricted(union));
            let absent = rows(&without_g.snapshot(), query, &everyone(union));
            assert!(!absent.is_empty() || query == NAMED, "oracle has rows");
            check.check(&format!("union {union}, {name}"), got, absent);
        }
    }
    // Who may read G sees it count (guard).
    let all = rows(&with_g.snapshot(), RANKED, &everyone(true));
    check.check("reader of G", all[0].0.as_str(), "alpha in h");
    check.done();
}

/// A reader's search under `GRAPH ?g` in reader scope binds only literals of the named
/// graphs it may read: its corpus is theirs, and returned data respects graph access
/// (SPARQL 1.1 §13.3). The global scope's rows are text_search_tests.rs's (#107).
#[test]
fn a_readers_graph_search_binds_only_graphs_it_may_read() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let mut tx = engine.transaction();
    betas(&mut tx, Some("G"));
    tx.insert(label_quad("h", "alpha in h", Some("H")).as_ref());
    tx.commit().unwrap();
    let options = QueryOptions {
        access: Some(Arc::new(GraphAccess {
            default_graph: true,
            graphs: vec![format!("{EX}H")],
            ..GraphAccess::default()
        })),
        ..reader()
    };
    let found: Vec<String> = rows(&engine.snapshot(), NAMED, &options)
        .into_iter()
        .map(|(text, _)| text)
        .collect();
    assert_eq!(found, ["alpha in h"]);
}

/// The setting chooses the corpus: on one store, global relevance counts the graph the
/// reader may not read (the documented global behaviour), reader relevance doesn't.
#[test]
fn the_setting_chooses_what_relevance_counts() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let mut tx = engine.transaction();
    betas(&mut tx, Some("G"));
    tx.commit().unwrap();
    let restricted = |scope: SearchScope| QueryOptions {
        search_scope: scope,
        access: Some(Arc::new(GraphAccess {
            default_graph: true,
            ..GraphAccess::default()
        })),
        ..QueryOptions::default()
    };
    let first = |scope| {
        rows(&engine.snapshot(), RANKED, &restricted(scope))[0]
            .0
            .clone()
    };
    let mut check = Rows::default();
    check.check(
        "global (guard)",
        first(SearchScope::Global),
        "alpha one two".to_owned(),
    );
    check.check("reader", first(SearchScope::Reader), "beta".to_owned());
    check.done();
}

/// A search alone in a pattern binds only literals the active graph shows: with equality
/// by representatives, a label every copy of which is asserted in a named graph alone isn't
/// in the default graph, though the inferred stack holds it over the representative (the
/// review's case of 18 October). Both scopes.
#[test]
fn a_search_alone_binds_only_what_the_active_graph_shows() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let iri = |s: &str| NamedNode::new_unchecked(format!("{EX}{s}"));
    let representative = tx.intern(iri("Representative").as_ref().into());
    let alias = tx.intern(iri("Alias").as_ref().into());
    let same_as = tx.intern(
        NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs")
            .as_ref()
            .into(),
    );
    for name in ["Representative", "Alias"] {
        tx.insert(label_quad(name, "secret alpha", Some("g")).as_ref());
    }
    let label = tx.intern(
        NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label")
            .as_ref()
            .into(),
    );
    let text = tx.intern(Literal::new_simple_literal("secret alpha").as_ref().into());
    for (subject, predicate, object) in [
        (alias, same_as, representative),
        (representative, label, text),
    ] {
        tx.insert_inferred(nrese_engine::EncodedTriple {
            subject,
            predicate,
            object,
        });
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot().with_equality(Some(same_as));
    let mut check = Rows::default();
    for scope in [SearchScope::Global, SearchScope::Reader] {
        let found = rows(
            &snapshot,
            "SELECT ?o ?score WHERE { ?o bds:search \"secret\" ; bds:relevance ?score }",
            &QueryOptions {
                search_scope: scope,
                ..QueryOptions::default()
            },
        );
        check.check(scope.name(), found, Vec::new());
    }
    check.done();
}

/// The result cache keys results by snapshot and scope: a hit equals what the snapshot
/// computes afresh, whatever was interned since, and a reader's search never hits a global
/// result of the same query (nor the reverse).
#[test]
fn a_cached_result_equals_a_fresh_one() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let mut tx = engine.transaction();
    betas(&mut tx, Some("G"));
    tx.commit().unwrap();
    let oracle = Engine::new(EngineConfig::default()).unwrap();
    base(&oracle);
    let snapshot = engine.snapshot();
    let cache = Arc::new(ResultCache::new(16 << 20).admitting_all());
    let options = |scope: SearchScope, cached: bool| QueryOptions {
        search_scope: scope,
        result_cache: cached.then(|| Arc::clone(&cache)),
        access: Some(Arc::new(GraphAccess {
            default_graph: true,
            ..GraphAccess::default()
        })),
        ..QueryOptions::default()
    };
    let mut check = Rows::default();
    let global = rows(&snapshot, RANKED, &options(SearchScope::Global, true));
    let reader = rows(&snapshot, RANKED, &options(SearchScope::Reader, true));
    let mut tx = engine.transaction();
    betas(&mut tx, None);
    check.check(
        "reader after global",
        reader.clone(),
        rows(&oracle.snapshot(), RANKED, &self::reader()),
    );
    check.check(
        "global hit",
        rows(&snapshot, RANKED, &options(SearchScope::Global, true)),
        rows(&snapshot, RANKED, &options(SearchScope::Global, false)),
    );
    check.check(
        "global hit (first)",
        global.clone(),
        rows(&snapshot, RANKED, &options(SearchScope::Global, false)),
    );
    check.check(
        "reader hit",
        rows(&snapshot, RANKED, &options(SearchScope::Reader, true)),
        reader,
    );
    drop(tx);
    check.done();
}

/// A default graph merged from every graph but some (SHACL's data graph without its shapes,
/// `set_default_graph_excluding`) ranks among the literals of the graphs it reads alone: the
/// left-out graph's many `beta`s change no score, as if it weren't there.
#[test]
fn a_merge_of_every_graph_but_some_ranks_without_them() {
    let (plain, with_shapes) = (Engine::new(EngineConfig::default()).unwrap(), {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        base(&engine);
        let mut tx = engine.transaction();
        betas(&mut tx, Some("shapes"));
        tx.commit().unwrap();
        engine
    });
    base(&plain);
    let mut dataset = nrese_sparql::QueryDatasetSpecification::new();
    dataset.set_default_graph_excluding(vec![ex("shapes").into()]);
    let excluding = QueryOptions {
        dataset: Some(dataset),
        ..reader()
    };
    let expected = rows(&plain.snapshot(), RANKED, &reader());
    assert_eq!(expected[0].0, "beta", "oracle");
    assert_eq!(rows(&with_shapes.snapshot(), RANKED, &excluding), expected);
}
