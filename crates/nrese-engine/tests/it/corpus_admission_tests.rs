//! The corpus store admits only through the shared-state capability (dictionary lifecycle
//! 3.2; the contract's S3 and §5 row 24; the token design check's §5 items 1, 2, 3 and 9).
//! A snapshot without a token (a writer's pending state, a view that adds facts) leaves
//! the store as it found it: no store made, no entry, no key charge, no eviction, no
//! lineage step. Its searches still answer as an independent engine holding exactly its
//! statements. Lineage is recorded by a successful publication only.
//!
//! Observable: `text_corpus_stats` (started, kept, bytes, steps, walked, derived), and
//! scores against independent engines (`oracle`), never a flag alone.

use std::sync::Arc;

use nrese_engine::{
    CorpusTuning, EncodedTriple, Engine, EngineConfig, GraphSelector, InferredSubset, Snapshot,
    TermId, TextCorpusStats, TextScope,
};
use nrese_rdf::{Literal, NamedNode};

use crate::text_scope_tests::{MODEL, base, built, label, query, texts};

const DEFAULT: TextScope = TextScope::Graphs(GraphSelector::Exact(TermId::DEFAULT_GRAPH));

/// What the store holds, without the counters of builds (a search adds to those).
fn held(snapshot: &Snapshot) -> (bool, u64, u64, u64) {
    let TextCorpusStats {
        started,
        kept,
        bytes,
        steps,
        ..
    } = snapshot.text_corpus_stats();
    (started, kept, bytes, steps)
}

fn scored(snapshot: &Snapshot, scope: &TextScope, text: &str) -> Vec<(String, f64)> {
    let found = snapshot
        .text_search_in(MODEL, scope, &query(text), None)
        .unwrap();
    texts(snapshot, &found)
}

/// An engine holding `base`'s statements with `changes` applied: the oracle of a pending
/// state that applies them.
fn oracle(changes: impl FnOnce(&mut nrese_engine::Transaction<'_>)) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let mut tx = engine.transaction();
    changes(&mut tx);
    tx.commit().unwrap();
    engine
}

/// A writer's pending states, over the base and over a restricted view, with an insertion
/// and a deletion, an empty delta, and an insertion then deletion of one statement: each
/// searched in two scopes, each answer equal to its oracle's, and the store unchanged
/// (nothing kept, charged or recorded). Then the commit records one step. Red on the
/// parent: the pending builders recorded steps and the searches kept entries.
#[test]
fn pending_searches_leave_the_store_as_they_found_it() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let published = engine.snapshot();
    for scope in [&TextScope::EVERY, &DEFAULT] {
        scored(&published, scope, "alpha beta");
    }
    let before = held(&published);
    let changes = |tx: &mut nrese_engine::Transaction<'_>| {
        tx.insert(label("n", "alpha new beta", None).as_ref());
        tx.remove(label("b", "beta", None).as_ref());
    };
    let truth = oracle(changes).snapshot();
    let mut failed = Vec::new();

    // One writer at a time: each transaction is let go before the next takes the slot.
    {
        let empty = engine.transaction();
        let pending = empty.pending_snapshot();
        scored(&pending, &TextScope::EVERY, "alpha");
    }
    {
        let mut undone = engine.transaction();
        undone.insert(label("u", "alpha undone", None).as_ref());
        undone.remove(label("u", "alpha undone", None).as_ref());
        let pending = undone.pending_snapshot();
        scored(&pending, &TextScope::EVERY, "alpha");
    }
    if held(&published) != before {
        failed.push(format!(
            "empty and undone: store {:?} != {before:?}",
            held(&published)
        ));
    }

    let mut tx = engine.transaction();
    changes(&mut tx);
    let pending = tx.pending_snapshot();
    let view = published.with_inferred_subset(InferredSubset::Without(Vec::new()));
    let over = tx.pending_over(&view, &[]);
    for (name, snapshot) in [("pending", &pending), ("pending over a view", &over)] {
        for scope in [&TextScope::EVERY, &DEFAULT] {
            let (got, want) = (
                scored(snapshot, scope, "alpha beta"),
                scored(&truth, scope, "alpha beta"),
            );
            if got != want {
                failed.push(format!("{name}, {scope:?}: {got:?} != {want:?}"));
            }
        }
        if held(&published) != before {
            failed.push(format!(
                "{name}: store {:?} != {before:?}",
                held(&published)
            ));
        }
    }
    drop((pending, over, view));

    // The publication records its step.
    tx.commit().unwrap();
    let steps = engine.snapshot().text_corpus_stats().steps;
    if steps != before.3 + 1 {
        failed.push(format!(
            "steps after the commit: {steps}, before {}",
            before.3
        ));
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// A store whose budget holds exactly its two kept corpora: a pending state's search
/// claims nothing, so it evicts neither, and searching them again builds nothing. Red on
/// the parent: the pending search's claim evicted one.
#[test]
fn a_pending_search_evicts_nothing_under_pressure() {
    let warm = |engine: &Engine| {
        let snapshot = engine.snapshot();
        for scope in [&TextScope::EVERY, &DEFAULT] {
            scored(&snapshot, scope, "alpha beta");
        }
        snapshot
    };
    let sizing = Engine::new(EngineConfig::default()).unwrap();
    base(&sizing);
    let full = warm(&sizing).text_corpus_stats().bytes as usize;
    let engine = Engine::new(EngineConfig::default()).unwrap();
    engine.set_text_corpus_budget(Arc::new(nrese_exec::Budget::new(full)));
    base(&engine);
    let published = warm(&engine);
    let before = held(&published);
    assert_eq!(before.1, 2, "{before:?}");
    let mut tx = engine.transaction();
    tx.insert(label("n", "alpha new", None).as_ref());
    let pending = tx.pending_snapshot();
    scored(
        &pending,
        &TextScope::Graphs(GraphSelector::AnyNamed),
        "alpha",
    );
    let built_before = built(&published);
    for scope in [&TextScope::EVERY, &DEFAULT] {
        scored(&published, scope, "alpha beta");
    }
    assert_eq!(held(&published), before);
    assert_eq!(built(&published), built_before, "a kept corpus was evicted");
}

/// The first reader search an engine sees, on a pending state, makes no store of corpora.
/// Red on the parent.
#[test]
fn a_pending_search_makes_no_store() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let mut tx = engine.transaction();
    tx.insert(label("n", "alpha new", None).as_ref());
    let pending = tx.pending_snapshot();
    let truth = oracle(|tx| {
        tx.insert(label("n", "alpha new", None).as_ref());
    })
    .snapshot();
    assert_eq!(
        scored(&pending, &TextScope::EVERY, "alpha"),
        scored(&truth, &TextScope::EVERY, "alpha")
    );
    assert!(!engine.snapshot().text_corpus_stats().started);
}

/// A view that adds inferred facts is unpublished: it keeps nothing, and answers with its
/// facts. A restriction (a subset) is eligible under its own content: it keeps its own
/// entry, beside the base's. Red on the parent: the additive view kept one.
#[test]
fn an_adding_view_keeps_nothing_and_a_restriction_keeps_its_own() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    // Graph g's literal, also as an inferred default-graph statement: existing ids only.
    let published = engine.snapshot();
    let id = |term: nrese_rdf::TermRef<'_>| published.lookup(term).unwrap();
    let label_of = NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label");
    let added = EncodedTriple {
        subject: id(NamedNode::new_unchecked("http://example.com/g")
            .as_ref()
            .into()),
        predicate: id(label_of.as_ref().into()),
        object: id(Literal::new_simple_literal("beta in g").as_ref().into()),
    };
    scored(&published, &DEFAULT, "beta");
    let before = held(&published);
    let adding = published.with_inferred_added(&[added.in_default_graph()]);
    let found = scored(&adding, &DEFAULT, "beta");
    assert!(
        found.iter().any(|(text, _)| text == "beta in g"),
        "{found:?}"
    );
    assert_eq!(held(&published), before, "an adding view kept an entry");
    let restriction = published.with_inferred_subset(InferredSubset::Without(Vec::new()));
    scored(&restriction, &DEFAULT, "beta");
    let after = held(&published);
    assert_eq!(after.1, before.1 + 1, "{after:?}");
}

/// Pending searches derive from the base's kept corpus where its runs are the pending
/// version's prefix: the derived counter rises, the walked one doesn't. The derivation
/// covers a new literal (an id past the base's dictionary) and the last use of one going.
/// Members and scores equal the oracle's. A guard on the parent (it derived through the
/// pending step); red by mutation.
#[test]
fn a_pending_search_derives_exactly_from_the_base() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    engine.set_corpus_tuning(CorpusTuning {
        documents_per_probe: 1,
        ..CorpusTuning::DEFAULT
    });
    base(&engine);
    let published = engine.snapshot();
    scored(&published, &TextScope::EVERY, "alpha beta");
    let changes = |tx: &mut nrese_engine::Transaction<'_>| {
        tx.insert(label("n", "alpha fresh", None).as_ref());
        tx.remove(label("b", "beta", None).as_ref());
    };
    let truth = oracle(changes).snapshot();
    let mut tx = engine.transaction();
    changes(&mut tx);
    let pending = tx.pending_snapshot();
    let (walked, derived) = built(&published);
    let mut failed = Vec::new();
    for text in ["alpha", "beta", "alpha beta", "fresh"] {
        let (got, want) = (
            scored(&pending, &TextScope::EVERY, text),
            scored(&truth, &TextScope::EVERY, text),
        );
        if got != want {
            failed.push(format!("{text}: {got:?} != {want:?}"));
        }
    }
    let (walked_after, derived_after) = built(&published);
    if walked_after != walked || derived_after <= derived {
        failed.push(format!(
            "walked {walked} -> {walked_after}, derived {derived} -> {derived_after}"
        ));
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// A restricted view's kept entry (its mask run) is never the base of a pending state over
/// the unrestricted base, and the base's entry serves a pending state over the view only
/// through every extra run: both answer as their oracles, in both warming orders. Text and
/// IRI scores against independent engines.
#[test]
fn views_and_pending_states_answer_in_both_warming_orders() {
    let hidden_text = "alpha hidden";
    let setup = || {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        engine.set_corpus_tuning(CorpusTuning {
            documents_per_probe: 1,
            ..CorpusTuning::DEFAULT
        });
        base(&engine);
        let mut tx = engine.transaction();
        let subject = tx.intern(
            NamedNode::new_unchecked("http://example.com/h")
                .as_ref()
                .into(),
        );
        let predicate = tx.intern(
            NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label")
                .as_ref()
                .into(),
        );
        let object = tx.intern(Literal::new_simple_literal(hidden_text).as_ref().into());
        let inferred = EncodedTriple {
            subject,
            predicate,
            object,
        };
        assert!(tx.insert_inferred(inferred));
        tx.commit().unwrap();
        (engine, inferred.in_default_graph())
    };
    // Oracles: the base with the inferred statement, and without it.
    let with_hidden = {
        let (engine, _) = setup();
        engine
    };
    let without_hidden = oracle(|_| {});
    let changes = |tx: &mut nrese_engine::Transaction<'_>| {
        tx.insert(label("n", "alpha new", None).as_ref());
    };
    let pending_truth = {
        let (engine, _) = setup();
        let mut tx = engine.transaction();
        changes(&mut tx);
        tx.commit().unwrap();
        engine
    };
    let over_truth = oracle(changes);
    let mut failed = Vec::new();
    for view_first in [true, false] {
        let (engine, hidden) = setup();
        let published = engine.snapshot();
        let view = published.with_inferred_subset(InferredSubset::Without(vec![hidden]));
        let order: [(&str, &Snapshot); 2] = match view_first {
            true => [("view", &view), ("base", &published)],
            false => [("base", &published), ("view", &view)],
        };
        for (name, snapshot) in order {
            let truth = match name {
                "view" => without_hidden.snapshot(),
                _ => with_hidden.snapshot(),
            };
            let (got, want) = (
                scored(snapshot, &TextScope::EVERY, "alpha"),
                scored(&truth, &TextScope::EVERY, "alpha"),
            );
            if got != want {
                failed.push(format!("{name} first {view_first}: {got:?} != {want:?}"));
            }
        }
        let mut tx = engine.transaction();
        changes(&mut tx);
        let pending = tx.pending_snapshot();
        let over = tx.pending_over(&view, &[]);
        for (name, snapshot, truth) in [
            ("pending over the base", &pending, pending_truth.snapshot()),
            ("pending over the view", &over, over_truth.snapshot()),
        ] {
            let (got, want) = (
                scored(snapshot, &TextScope::EVERY, "alpha"),
                scored(&truth, &TextScope::EVERY, "alpha"),
            );
            if got != want {
                failed.push(format!(
                    "{name}, view first {view_first}: {got:?} != {want:?}"
                ));
            }
            let iris = |s: &Snapshot| {
                s.iri_search_in(MODEL, &TextScope::EVERY, &query("n"), None)
                    .unwrap()
                    .len()
            };
            if iris(snapshot) != iris(&truth) {
                failed.push(format!("{name} IRIs, view first {view_first}"));
            }
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// A vector search's hits decoded (lexical form, distance) with its report's space.
fn nearest(snapshot: &Snapshot, query: &nrese_engine::VectorQuery) -> (Vec<(String, f32)>, usize) {
    let budget = Arc::new(nrese_exec::Budget::unlimited());
    let (hits, report) = snapshot
        .vector_search_in(MODEL, &TextScope::EVERY, query, &|_| true, &budget)
        .unwrap();
    let hits = hits
        .iter()
        .map(|&(id, distance)| match snapshot.decode(id) {
            Some(nrese_rdf::Term::Literal(l)) => (l.value().to_owned(), distance),
            other => panic!("{other:?}"),
        })
        .collect();
    (hits, report.space)
}

/// Attack items 2, 3 and 7 (the design note's list), guards: a writer's pending state's
/// derived views (as stored, canonical, restricted) and a pending state searched after its
/// transaction was dropped and another commit published keep nothing, and answer text,
/// IRI and vector searches as an engine holding their statements; vectors with the store
/// warm in both orders (the published base first, the pending state first).
#[test]
fn pending_views_and_abandoned_pending_states_keep_nothing() {
    let vectors = |tx: &mut nrese_engine::Transaction<'_>| {
        tx.insert(label("v", "[1, 0]", None).as_ref());
        tx.insert(label("w", "[0.6, 0.8]", None).as_ref());
    };
    let changes = |tx: &mut nrese_engine::Transaction<'_>| {
        tx.insert(label("n", "alpha new", None).as_ref());
        tx.insert(label("x", "[0, 1]", None).as_ref());
        tx.remove(label("w", "[0.6, 0.8]", None).as_ref());
    };
    let truth = {
        let engine = oracle(vectors);
        let mut tx = engine.transaction();
        changes(&mut tx);
        tx.commit().unwrap();
        engine.snapshot()
    };
    let near = nrese_engine::VectorQuery::new(vec![1.0, 1.0], 3);
    let mut failed = Vec::new();
    for base_first in [true, false] {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        base(&engine);
        let mut tx = engine.transaction();
        vectors(&mut tx);
        tx.commit().unwrap();
        let published = engine.snapshot();
        let mut tx = engine.transaction();
        changes(&mut tx);
        let pending = tx.pending_snapshot();
        let views = [
            ("pending", pending.clone()),
            ("stored", pending.stored()),
            ("canonical", pending.with_canonical_equality()),
            (
                "restricted",
                pending.with_inferred_subset(InferredSubset::Without(Vec::new())),
            ),
        ];
        let warm = || {
            scored(&published, &TextScope::EVERY, "alpha");
            published
                .iri_search_in(MODEL, &TextScope::EVERY, &query("n"), None)
                .unwrap();
            nearest(&published, &near);
        };
        if base_first {
            warm();
        }
        let before = held(&published);
        for (name, view) in &views {
            let context = format!("{name}, base first {base_first}");
            let (got, want) = (
                scored(view, &TextScope::EVERY, "alpha"),
                scored(&truth, &TextScope::EVERY, "alpha"),
            );
            if got != want {
                failed.push(format!("{context}: text {got:?} != {want:?}"));
            }
            let iris = |s: &Snapshot| {
                s.iri_search_in(MODEL, &TextScope::EVERY, &query("n"), None)
                    .unwrap()
                    .len()
            };
            if iris(view) != iris(&truth) {
                failed.push(format!("{context}: IRIs"));
            }
            let (got, want) = (nearest(view, &near), nearest(&truth, &near));
            if got != want {
                failed.push(format!("{context}: vectors {got:?} != {want:?}"));
            }
            if held(&published) != before {
                failed.push(format!(
                    "{context}: store {before:?} -> {:?}",
                    held(&published)
                ));
            }
        }
        if !base_first {
            warm();
        }
        // The transaction dropped, another commit published: the pending state, searched
        // again, still keeps nothing and answers with its own statements.
        drop(tx);
        let mut other = engine.transaction();
        other.insert(label("o", "alpha other", None).as_ref());
        other.commit().unwrap();
        let before = held(&engine.snapshot());
        let (got, want) = (
            scored(&pending, &TextScope::EVERY, "alpha"),
            scored(&truth, &TextScope::EVERY, "alpha"),
        );
        if got != want {
            failed.push(format!(
                "abandoned, base first {base_first}: {got:?} != {want:?}"
            ));
        }
        if nearest(&pending, &near) != nearest(&truth, &near) {
            failed.push(format!("abandoned vectors, base first {base_first}"));
        }
        if held(&engine.snapshot()) != before {
            failed.push(format!(
                "abandoned, base first {base_first}: the store changed"
            ));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
