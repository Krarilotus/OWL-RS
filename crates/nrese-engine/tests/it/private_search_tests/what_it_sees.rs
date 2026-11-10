//! What a private snapshot sees: its own and the base's documents, a restricted view, the
//! global entry points.

use super::*;

/// Attack items 1, 2, 3, 5, 6, 7, 8, 11: a private transaction's pending snapshot, the
/// base's corpora warm (so it derives) and cold (so it walks), answers every text and IRI
/// query in two models and three scopes as the oracle does. The store holds what it held,
/// and the published snapshot answers as before. Red before V3: every search refused.
#[test]
fn a_private_snapshot_searches_what_it_can_see() {
    let truth = oracle().snapshot();
    let want = answers(&truth);
    let mut failed = Vec::new();
    for warm in [true, false] {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        base(&engine);
        let published = engine.snapshot();
        let before = match warm {
            true => Some(answers(&published)),
            false => None,
        };
        let mut private = engine
            .speculative_over(&published, Arc::new(nrese_exec::Budget::unlimited()))
            .unwrap();
        let mut writer = engine.transaction();
        for quad in published_later() {
            writer.insert(quad.as_ref());
        }
        writer.commit().unwrap();
        // The store after the writer's publication (its lineage step): what the private
        // searches must leave it.
        let held_before = held(&published);
        private.try_intern(unused().as_ref()).unwrap();
        for quad in inserted() {
            assert!(private.try_insert(quad.as_ref()).unwrap(), "{quad}");
        }
        for quad in removed() {
            assert!(private.remove(quad.as_ref()), "{quad}");
        }
        let pending = private.pending_snapshot().unwrap();
        assert!(pending.is_private());
        let context = format!("warm {warm}");
        rows_equal(&answers(&pending), &want, &context, &mut failed);
        if held(&published) != held_before {
            failed.push(format!(
                "{context}: the store {held_before:?} -> {:?}",
                held(&published)
            ));
        }
        if let Some(before) = before {
            let after = answers(&published);
            rows_equal(&after, &before, "published after", &mut failed);
        }
    }
    assert!(
        failed.is_empty(),
        "{} rows failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

/// Attack item 4: on a private snapshot, the global entry points answer as the reader path
/// over every graph in the materialised model (DICT2 9 Nov 04:02, Q2). Red before V3:
/// refused.
#[test]
fn a_private_snapshots_global_search_is_its_reader_search() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let published = engine.snapshot();
    let mut private = engine
        .speculative_over(&published, Arc::new(nrese_exec::Budget::unlimited()))
        .unwrap();
    for quad in inserted() {
        private.try_insert(quad.as_ref()).unwrap();
    }
    for quad in removed() {
        private.remove(quad.as_ref());
    }
    let pending = private.pending_snapshot().unwrap();
    let mut failed = Vec::new();
    let model = ReadModel::Materialised;
    for (name, query) in queries() {
        let global = pending.text_search(&query).map(|f| decoded(&pending, &f));
        let reader = pending
            .text_search_in(model, &TextScope::EVERY, &query, None)
            .map(|f| decoded(&pending, &f));
        if global.as_ref().ok() != reader.as_ref().ok() || global.is_err() {
            failed.push(format!("text {name}: {global:?} != {reader:?}"));
        }
    }
    for (name, query) in iri_queries() {
        let global = pending.iri_search(&query).map(|f| decoded(&pending, &f));
        let reader = pending
            .iri_search_in(model, &TextScope::EVERY, &query, None)
            .map(|f| decoded(&pending, &f));
        if global.as_ref().ok() != reader.as_ref().ok() || global.is_err() {
            failed.push(format!("iri {name}: {global:?} != {reader:?}"));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// Attack items 2 and 13: a private snapshot over a restricted view (`pending_over`, an
/// inferred label hidden) answers as an engine without the hidden statement and with the
/// delta, whichever of the base and the view was searched first.
#[test]
fn a_private_snapshot_over_a_restricted_view_searches_what_it_can_see() {
    let hidden_text = "alpha hidden";
    let setup = || {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        base(&engine);
        let mut tx = engine.transaction();
        let triple = EncodedTriple {
            subject: tx.intern(ex("h").as_ref().into()),
            predicate: tx.intern(
                NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label")
                    .as_ref()
                    .into(),
            ),
            object: tx.intern(Literal::new_simple_literal(hidden_text).as_ref().into()),
        };
        assert!(tx.insert_inferred(triple));
        tx.commit().unwrap();
        (engine, triple.in_default_graph())
    };
    let want = answers(&oracle().snapshot());
    let mut failed = Vec::new();
    for view_first in [true, false] {
        let (engine, hidden) = setup();
        let published = engine.snapshot();
        let view = published.with_inferred_subset(InferredSubset::Without(vec![hidden]));
        match view_first {
            true => {
                answers(&view);
                answers(&published);
            }
            false => {
                answers(&published);
                answers(&view);
            }
        }
        let mut private = engine
            .speculative_over(&published, Arc::new(nrese_exec::Budget::unlimited()))
            .unwrap();
        for quad in inserted() {
            private.try_insert(quad.as_ref()).unwrap();
        }
        for quad in removed() {
            private.remove(quad.as_ref());
        }
        let over = private.pending_over(&view, &[hidden]).unwrap();
        let context = format!("view first {view_first}");
        rows_equal(&answers(&over), &want, &context, &mut failed);
    }
    assert!(
        failed.is_empty(),
        "{} rows failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}
