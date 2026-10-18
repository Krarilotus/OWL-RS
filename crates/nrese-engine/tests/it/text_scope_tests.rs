//! Full-text corpora (`Snapshot::text_search_in`): a search ranks among the literals its
//! snapshot's statements in its scope hold, so neither an open transaction, nor a later
//! version, nor another scope changes its scores; a replica answers as its primary does.
//! The corpus is built once per version, model and scope, then kept (performance.md §0).

use nrese_engine::{
    DurabilityConfig, Engine, EngineConfig, GraphSelector, ReadModel, Snapshot, SyncPolicy, TermId,
    TextMatch, TextQuery, TextScope,
};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};

fn label(subject: &str, text: &str, graph: Option<&str>) -> Quad {
    Quad::new(
        NamedNode::new_unchecked(format!("http://example.com/{subject}")),
        NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label"),
        Literal::new_simple_literal(text),
        match graph {
            Some(g) => NamedNode::new_unchecked(format!("http://example.com/{g}")).into(),
            None => GraphName::DefaultGraph,
        },
    )
}

fn query(text: &str) -> TextQuery {
    TextQuery {
        text: text.to_owned(),
        all_words: false,
        prefix: false,
        stem: None,
    }
}

/// The matches as texts with their relevance (ids differ between engines).
fn texts(snapshot: &Snapshot, found: &[TextMatch]) -> Vec<(String, f64)> {
    found
        .iter()
        .map(|m| match snapshot.decode(TermId::from_raw(m.id)) {
            Some(Term::Literal(l)) => (l.value().to_owned(), m.relevance),
            other => panic!("{other:?}"),
        })
        .collect()
}

fn base(engine: &Engine) {
    let mut tx = engine.transaction();
    tx.insert(label("a", "alpha one two", None).as_ref());
    tx.insert(label("b", "beta", None).as_ref());
    for i in 0..8 {
        tx.insert(label(&format!("f{i}"), &format!("filler {i}"), None).as_ref());
    }
    tx.insert(label("g", "beta in g", Some("g")).as_ref());
    tx.commit().unwrap();
}

/// Guard (performance.md §0): one walk per version, model and scope, then the kept corpus;
/// a search changes no counter of another scope's, and an open transaction builds nothing.
#[test]
fn corpora_are_built_once_per_version_and_scope() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let snapshot = engine.snapshot();
    let default = TextScope::Graphs(GraphSelector::Exact(TermId::DEFAULT_GRAPH));
    let model = ReadModel::Materialised;
    let builds = snapshot.text_corpus_builds();
    let every = snapshot.text_search(&query("alpha beta"));
    assert_eq!(snapshot.text_corpus_builds(), builds + 1);
    assert_eq!(snapshot.text_search(&query("beta")).len(), 2);
    assert_eq!(snapshot.text_corpus_builds(), builds + 1);
    let in_default = snapshot.text_search_in(model, &default, &query("alpha beta"));
    assert_eq!(snapshot.text_corpus_builds(), builds + 2);
    assert_eq!(in_default.len(), 2);
    assert_eq!(every.len(), 3);
    // Graph g's literal counts in every graph, not in the default graph's.
    assert_ne!(
        texts(&snapshot, &every)[..2],
        texts(&snapshot, &in_default)[..]
    );

    let mut tx = engine.transaction();
    for i in 0..60 {
        tx.insert(label(&format!("x{i}"), &format!("beta extra {i}"), None).as_ref());
    }
    assert_eq!(snapshot.text_search(&query("alpha beta")), every);
    assert_eq!(
        snapshot.text_search_in(model, &default, &query("alpha beta")),
        in_default
    );
    assert_eq!(snapshot.text_corpus_builds(), builds + 2);
    tx.commit().unwrap();
    // The old version keeps its corpus; the new one builds its own.
    assert_eq!(snapshot.text_search(&query("alpha beta")), every);
    assert_eq!(snapshot.text_corpus_builds(), builds + 2);
    let later = engine.snapshot();
    assert_eq!(later.text_search(&query("alpha beta")).len(), 63);
    assert_eq!(later.text_corpus_builds(), builds + 3);
    // Autocompletion's other index (IRIs by local name, among the subjects) has its own.
    let names = later.iri_search_in(model, &TextScope::EVERY, &query("x1"));
    assert_eq!(names.len(), 1);
    assert_eq!(later.text_corpus_builds(), builds + 4);
}

/// The merge of graphs and every named graph: their literals, each once.
#[test]
fn scopes_of_several_graphs() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let mut tx = engine.transaction();
    tx.insert(label("h", "beta in h", Some("h")).as_ref());
    tx.insert(label("h2", "beta in g", Some("h")).as_ref());
    tx.insert(label("d", "beta in h", None).as_ref());
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let id = |g: &str| {
        snapshot
            .lookup(
                NamedNode::new_unchecked(format!("http://example.com/{g}"))
                    .as_ref()
                    .into(),
            )
            .unwrap()
    };
    let found = |scope: &TextScope| {
        let mut found: Vec<String> = texts(
            &snapshot,
            &snapshot.text_search_in(ReadModel::Materialised, scope, &query("beta")),
        )
        .into_iter()
        .map(|(text, _)| text)
        .collect();
        found.sort();
        found
    };
    let mut g_h = vec![id("g"), id("h")];
    g_h.sort_unstable();
    assert_eq!(found(&TextScope::Merge(g_h)), ["beta in g", "beta in h"]);
    assert_eq!(
        found(&TextScope::Graphs(GraphSelector::AnyNamed)),
        ["beta in g", "beta in h"]
    );
    assert_eq!(
        found(&TextScope::Merge(vec![TermId::DEFAULT_GRAPH])),
        ["beta", "beta in h"]
    );
    assert_eq!(found(&TextScope::EVERY), ["beta", "beta in g", "beta in h"]);
}

/// A replica answers as its primary, whatever the primary's open transactions interned.
#[test]
fn a_replica_scores_as_its_primary() {
    let config = || EngineConfig {
        background_maintenance: false,
        durability: DurabilityConfig {
            sync: SyncPolicy::EveryCommit,
            ..DurabilityConfig::default()
        },
        ..EngineConfig::default()
    };
    let (primary_dir, replica_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let primary = Engine::open(primary_dir.path(), config()).unwrap();
    base(&primary);
    primary.write_image(replica_dir.path()).unwrap();
    let replica = Engine::open(replica_dir.path(), config()).unwrap();
    let mut tx = primary.transaction();
    for i in 0..60 {
        tx.insert(label(&format!("x{i}"), &format!("beta extra {i}"), None).as_ref());
    }
    let (p, r) = (primary.snapshot(), replica.snapshot());
    let q = query("alpha beta");
    assert_eq!(p.text_search(&q), r.text_search(&q));
    assert_eq!(texts(&p, &p.text_search(&q))[0].0, "beta");
    drop(tx);
}
