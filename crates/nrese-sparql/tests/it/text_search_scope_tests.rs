//! Full-text relevance is computed over the documents the reader can see (native/search.rs,
//! `Snapshot::text_search_in`): the string literals that are objects of statements in the
//! pattern's graphs, at the snapshot's version, as the reader's access restricts them.
//! Neither uncommitted literals, nor later versions, nor graphs the reader may not read
//! change a score or an order; a cached result equals a fresh one.

use std::sync::Arc;

use nrese_engine::{Engine, EngineConfig, Snapshot};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_sparql::{GraphAccess, QueryOptions, QueryResults, ResultCache, evaluate_query};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";
const PREFIXES: &str = "PREFIX bds: <http://www.bigdata.com/rdf/search#> \
                        PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

/// Two words of equal document frequency: `beta` in a shorter literal ranks first, until
/// `beta` grows common somewhere (then `alpha`, the rarer word, wins).
const RANKED: &str = "SELECT ?o ?score WHERE { ?o bds:search \"alpha beta\" ; bds:relevance ?score } \
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

#[test]
fn uncommitted_literals_change_no_score() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let before = rows(&snapshot, RANKED, &options);
    assert_eq!(before[0].0, "beta", "{before:?}");
    // Another writer's literals, not committed: the dictionary holds them, no snapshot does.
    let mut tx = engine.transaction();
    betas(&mut tx, None);
    assert_eq!(rows(&snapshot, RANKED, &options), before);
    assert_eq!(rows(&engine.snapshot(), RANKED, &options), before);
    drop(tx);
    assert_eq!(rows(&engine.snapshot(), RANKED, &options), before);
}

#[test]
fn later_versions_change_no_score_of_an_earlier_one() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let before = rows(&snapshot, RANKED, &options);
    let mut tx = engine.transaction();
    betas(&mut tx, None);
    tx.commit().unwrap();
    assert_eq!(rows(&snapshot, RANKED, &options), before);
    // The new version counts them: `beta` is common now, `alpha` ranks first.
    let after = rows(&engine.snapshot(), RANKED, &options);
    assert_eq!(after[0].0, "alpha one two", "{after:?}");
}

#[test]
fn literals_no_statement_uses_count_for_nothing() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let options = QueryOptions::default();
    let before = rows(&engine.snapshot(), RANKED, &options);
    let mut tx = engine.transaction();
    betas(&mut tx, None);
    tx.commit().unwrap();
    let mut tx = engine.transaction();
    for i in 0..60 {
        tx.remove(label_quad(&format!("x{i}"), &format!("beta extra {i}"), None).as_ref());
    }
    tx.commit().unwrap();
    assert_eq!(rows(&engine.snapshot(), RANKED, &options), before);
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

    let reader = |union: bool| QueryOptions {
        union_default_graph: union,
        access: Some(Arc::new(GraphAccess {
            default_graph: true,
            graphs: vec![format!("{EX}H")],
            ..GraphAccess::default()
        })),
        ..QueryOptions::default()
    };
    let everyone = |union: bool| QueryOptions {
        union_default_graph: union,
        ..QueryOptions::default()
    };
    let named = "SELECT ?o ?score WHERE { GRAPH ?g { ?o bds:search \"alpha beta\" ; bds:relevance ?score } } \
                 ORDER BY DESC(?score) ?o";
    for union in [false, true] {
        for query in [RANKED, named] {
            let restricted = rows(&with_g.snapshot(), query, &reader(union));
            let absent = rows(&without_g.snapshot(), query, &everyone(union));
            assert!(!absent.is_empty() || query == named);
            assert_eq!(restricted, absent, "union {union}: {query}");
        }
    }
    // Who may read G sees it count.
    let all = rows(&with_g.snapshot(), RANKED, &everyone(true));
    assert_eq!(all[0].0, "alpha in h", "{all:?}");
}

/// Readers whose access is written differently but reads the same graphs rank among one
/// corpus, built once.
#[test]
fn readers_of_the_same_graphs_share_a_corpus() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let mut tx = engine.transaction();
    betas(&mut tx, Some("G"));
    tx.insert(label_quad("h", "alpha in h", Some("H")).as_ref());
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let reader = |access: GraphAccess| QueryOptions {
        union_default_graph: true,
        access: Some(Arc::new(access)),
        ..QueryOptions::default()
    };
    let by_name = reader(GraphAccess {
        default_graph: true,
        graphs: vec![format!("{EX}H")],
        ..GraphAccess::default()
    });
    let by_prefix = reader(GraphAccess {
        default_graph: true,
        prefixes: vec![EX.to_owned()],
        excluded: vec![format!("{EX}G")],
        ..GraphAccess::default()
    });
    let walked = snapshot.text_corpus_stats().walked;
    let first = rows(&snapshot, RANKED, &by_name);
    assert_eq!(snapshot.text_corpus_stats().walked, walked + 1);
    assert_eq!(rows(&snapshot, RANKED, &by_prefix), first);
    assert_eq!(snapshot.text_corpus_stats().walked, walked + 1);
}

/// A search alone in a pattern binds only literals the active graph shows: with equality
/// by representatives, a label every copy of which is asserted in a named graph alone isn't
/// in the default graph, though the inferred stack holds it over the representative (the
/// review's case of 18 October; engine-v2 returned no row).
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
    let found = rows(
        &snapshot,
        "SELECT ?o ?score WHERE { ?o bds:search \"secret\" ; bds:relevance ?score }",
        &QueryOptions::default(),
    );
    assert!(found.is_empty(), "{found:?}");
}

/// The result cache keys results by snapshot: a hit equals what the snapshot computes
/// afresh, whatever was interned since.
#[test]
fn a_cached_result_equals_a_fresh_one() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let snapshot = engine.snapshot();
    let cache = Arc::new(ResultCache::new(16 << 20).admitting_all());
    let cached = QueryOptions {
        result_cache: Some(Arc::clone(&cache)),
        ..QueryOptions::default()
    };
    let first = rows(&snapshot, RANKED, &cached);
    let mut tx = engine.transaction();
    betas(&mut tx, None);
    let hit = rows(&snapshot, RANKED, &cached);
    let fresh = rows(&snapshot, RANKED, &QueryOptions::default());
    assert_eq!(hit, first);
    assert_eq!(fresh, hit);
    drop(tx);
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
        ..QueryOptions::default()
    };
    let expected = rows(&plain.snapshot(), RANKED, &QueryOptions::default());
    assert_eq!(rows(&with_shapes.snapshot(), RANKED, &excluding), expected);
    assert_eq!(expected[0].0, "beta");
}
