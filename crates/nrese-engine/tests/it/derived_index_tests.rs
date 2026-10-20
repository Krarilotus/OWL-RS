//! Derived indexes kept on disk (`term/derived.rs`): the text indexes and the vector
//! index are written after a checkpoint and read at their first use after a reopen, then
//! extended with the terms interned since; a file that doesn't fit is ignored. And the
//! vector graph of a large space built on a thread while searches scan.

use nrese_engine::vector::{DATATYPE, Metric, lexical};
use nrese_engine::{
    DurabilityConfig, Engine, EngineConfig, SyncPolicy, TextQuery, VectorQuery, VectorStrategy,
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
        .unwrap()
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
fn text_query_errors_reach_snapshot_callers() {
    let engine = Engine::new(config()).unwrap();
    for populated in [false, true] {
        if populated {
            insert(
                &engine,
                "quick",
                Literal::new_simple_literal("quick brown fox"),
            );
        }
        let snapshot = engine.snapshot();
        for text in [
            "quick~nope",
            "quick~0.5",
            "quick~3",
            "quick~256",
            "quick~999999999999999999999",
            "quick~-1",
            "quick~1~2",
            "~1",
            "quick -fo*",
            "quick -fox~1",
            "quick -\"brown fox\"",
            "quick -fox?",
            "\"quick brown",
        ] {
            let query = TextQuery {
                text: text.to_owned(),
                all_words: false,
                prefix: false,
                stem: None,
            };
            let every = nrese_engine::TextScope::EVERY;
            let model = nrese_engine::ReadModel::Materialised;
            for result in [
                snapshot.text_search(&query),
                snapshot.iri_search_in(model, &every, &query, None),
            ] {
                assert!(
                    matches!(result, Err(nrese_engine::EngineError::InvalidTextQuery(_))),
                    "{text}: {result:?}"
                );
            }
        }
    }
    assert_eq!(
        search(&engine, "quick-brown"),
        search(&engine, "quick brown")
    );
    assert_eq!(search(&engine, "\"quick: brown! fox?\"").len(), 1);
    for text in ["quick~", "quick~0", "quick~1", "quick~2"] {
        assert_eq!(search(&engine, text).len(), 1, "{text}");
    }
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
        let (found, report) = engine
            .snapshot()
            .vector_search(&query, &|_| true)
            .expect("an unlimited search");
        assert_eq!(report.space, 50);
        engine.checkpoint().unwrap();
        assert!(dir.path().join("derived/text.index").exists());
        assert!(dir.path().join("derived/vectors.index").exists());
        (text, found)
    };
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(engine.stats().dictionary.derived_loaded, 0);
    assert_eq!(search(&engine, "bridge*"), before_text);
    let snapshot = engine.snapshot();
    let (found, report) = snapshot
        .vector_search(&query, &|_| true)
        .expect("an unlimited search");
    assert_eq!(report.space, 50, "the vectors read back");
    assert_eq!(found, before_vectors);
    assert_eq!(engine.stats().dictionary.derived_loaded, 2);
    // Terms interned since extend what was read.
    insert(&engine, "c", Literal::new_simple_literal("Bridge of Sighs"));
    assert_eq!(search(&engine, "bridge*").len(), 3);
    insert(&engine, "near", vector(&[1.0, 0.0001]));
    let (found, _) = engine
        .snapshot()
        .vector_search(&query, &|_| true)
        .expect("an unlimited search");
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
