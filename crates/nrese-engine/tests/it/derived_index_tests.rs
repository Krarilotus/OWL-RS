//! Derived indexes kept on disk (`term/derived.rs`): the text indexes and the vector
//! index are written after a checkpoint and read at their first use after a reopen, then
//! extended with the terms interned since; a file that doesn't fit is ignored. And the
//! vector graph of a large space built in the background while searches scan.

use nrese_engine::term::vectors::VectorBuild;
use nrese_engine::vector::{DATATYPE, Metric, lexical};
use nrese_engine::{
    DurabilityConfig, Engine, EngineConfig, SyncPolicy, TextQuery, VectorQuery, VectorSearchReport,
    VectorStrategy,
};
use nrese_rdf::{GraphNameRef, Literal, NamedNode, QuadRef};

fn config() -> EngineConfig {
    EngineConfig {
        background_maintenance: false,
        durability: DurabilityConfig {
            sync: SyncPolicy::EveryCommit,
            ..DurabilityConfig::default()
        },
        ..EngineConfig::default()
    }
}

fn insert(engine: &Engine, subject: &str, object: Literal) {
    let mut tx = engine.transaction();
    tx.insert(QuadRef::new(
        NamedNode::new_unchecked(format!("http://example.com/{subject}")).as_ref(),
        NamedNode::new_unchecked("http://example.com/p").as_ref(),
        object.as_ref(),
        GraphNameRef::DefaultGraph,
    ));
    tx.commit().unwrap();
}

fn search(engine: &Engine, text: &str) -> Vec<String> {
    let snapshot = engine.snapshot();
    let mut found: Vec<String> = snapshot
        .text_search(&TextQuery {
            text: text.to_owned(),
            all_words: false,
            prefix: false,
            stem: None,
        })
        .into_iter()
        .filter_map(|m| snapshot.decode(nrese_engine::TermId::from_raw(m.id)))
        .map(|t| t.to_string())
        .collect();
    found.sort();
    found
}

fn vector(values: &[f32]) -> Literal {
    Literal::new_typed_literal(lexical(values), NamedNode::new_unchecked(DATATYPE))
}

#[test]
fn text_and_vector_indexes_survive_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let query = VectorQuery {
        strategy: VectorStrategy::Approximate,
        metric: Metric::L2,
        ..VectorQuery::new(vec![1.0, 0.0], 3)
    };
    let (before_text, before_vectors) = {
        let engine = Engine::open(dir.path(), config()).unwrap();
        insert(&engine, "a", Literal::new_simple_literal("Tower Bridge"));
        insert(&engine, "b", Literal::new_simple_literal("London bridges"));
        for i in 0..50 {
            let angle = (i as f32 * 7.0).to_radians();
            insert(
                &engine,
                &format!("v{i}"),
                vector(&[angle.cos(), angle.sin()]),
            );
        }
        let text = search(&engine, "bridge*");
        assert_eq!(text.len(), 2);
        let snapshot = engine.snapshot();
        snapshot.prepare_vector_graph(&query);
        let (found, report) = snapshot.vector_search(&query, &|_| true);
        assert!(report.graph);
        engine.checkpoint().unwrap();
        assert!(dir.path().join("derived/text.index").exists());
        assert!(dir.path().join("derived/vectors.index").exists());
        (text, found)
    };
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(engine.stats().dictionary.derived_loaded, 0);
    assert_eq!(search(&engine, "bridge*"), before_text);
    let snapshot = engine.snapshot();
    let (found, report) = snapshot.vector_search(&query, &|_| true);
    assert!(
        report.graph && report.scanned == 0,
        "the graph read back: {report:?}"
    );
    assert_eq!(found, before_vectors);
    assert_eq!(engine.stats().dictionary.derived_loaded, 2);
    // Terms interned since extend what was read.
    insert(&engine, "c", Literal::new_simple_literal("Bridge of Sighs"));
    assert_eq!(search(&engine, "bridge*").len(), 3);
    insert(&engine, "near", vector(&[1.0, 0.0001]));
    let (found, _) = engine.snapshot().vector_search(&query, &|_| true);
    assert!(found[0].1 < 1e-6, "{found:?}");
}

#[test]
fn files_of_another_store_are_ignored() {
    let one = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(one.path(), config()).unwrap();
        insert(&engine, "a", Literal::new_simple_literal("Tower Bridge"));
        search(&engine, "tower");
        engine.checkpoint().unwrap();
    }
    let other = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(other.path(), config()).unwrap();
        insert(
            &engine,
            "x",
            Literal::new_simple_literal("Something else entirely"),
        );
        engine.checkpoint().unwrap();
    }
    std::fs::create_dir_all(other.path().join("derived")).unwrap();
    std::fs::copy(
        one.path().join("derived/text.index"),
        other.path().join("derived/text.index"),
    )
    .unwrap();
    let engine = Engine::open(other.path(), config()).unwrap();
    assert!(search(&engine, "tower").is_empty());
    assert_eq!(search(&engine, "else").len(), 1);
    assert_eq!(engine.stats().dictionary.derived_loaded, 0);
}

/// A space past the size built at once: the first search starts the graph's build in the
/// background and answers exactly; searches while it is pending start no other and stay
/// exact; once it has run, searches use the graph.
#[test]
fn large_graphs_are_built_in_the_background() {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    // The engine's builds are held here until the test runs them.
    let (held, builds) = std::sync::mpsc::channel::<VectorBuild>();
    engine.run_vector_builds_with(std::sync::Arc::new(move |build| {
        held.send(build)
            .map_err(|_| std::io::Error::other("the test has ended"))
    }));
    let mut tx = engine.transaction();
    for i in 0..60_000usize {
        let a = (i as f32 * 0.37).sin();
        let b = (i as f32 * 0.11).cos();
        tx.insert(QuadRef::new(
            NamedNode::new_unchecked(format!("http://example.com/{i}")).as_ref(),
            NamedNode::new_unchecked("http://example.com/p").as_ref(),
            vector(&[a, b, (i % 13) as f32]).as_ref(),
            GraphNameRef::DefaultGraph,
        ));
    }
    tx.commit().unwrap();
    let query = VectorQuery {
        metric: Metric::L2,
        ..VectorQuery::new(vec![0.3, 0.4, 5.0], 5)
    };
    let snapshot = engine.snapshot();
    let exact = VectorQuery {
        strategy: VectorStrategy::Exact,
        ..query.clone()
    };
    let (nearest, _) = snapshot.vector_search(&exact, &|_| true);
    let scanned = VectorSearchReport {
        space: 60_000,
        graph: false,
        scanned: 60_000,
    };
    // Pending: every search scans and answers exactly; the first started the one build.
    for search in ["first", "second"] {
        let (found, report) = snapshot.vector_search(&query, &|_| true);
        assert_eq!(report, scanned, "{search} search");
        assert_eq!(found, nearest, "{search} search");
    }
    let build = builds.try_recv().expect("the first search started a build");
    assert!(builds.try_recv().is_err(), "one build while it is pending");
    build.run();
    let (found, report) = snapshot.vector_search(&query, &|_| true);
    assert_eq!(
        report,
        VectorSearchReport {
            graph: true,
            scanned: 0,
            ..scanned
        }
    );
    let shared = found
        .iter()
        .filter(|(id, _)| nearest.iter().any(|(f, _)| f == id))
        .count();
    assert!(shared >= 4, "{shared} of the 5 nearest");
}
