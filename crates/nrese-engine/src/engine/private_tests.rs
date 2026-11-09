//! Package 2b's guards for private transactions (`notes/2026-11-08-p2b-snapshot-switch-design.md`
//! §3: B7, B8, B9, B14, B15, B18 and the boundary inputs). Each states its input and what the
//! design's invariant (§2) says it must give.

use std::sync::Arc;

use nrese_exec::Budget;
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term, Triple};

use super::super::{Engine, EngineConfig, InferredSubset, ReadModel, Snapshot, TextScope};
use crate::error::EngineError;
use crate::{EncodedTriple, QuadPattern, SearchRefused, TermId, TextQuery, VectorQuery};

fn iri(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("http://example.com/{local}"))
}

fn literal(text: &str) -> Term {
    Literal::new_simple_literal(text).into()
}

fn quad(subject: &str, predicate: &str, object: Term) -> Quad {
    Quad::new(
        iri(subject),
        iri(predicate),
        object,
        GraphName::DefaultGraph,
    )
}

fn engine() -> Engine {
    Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .expect("engine")
}

/// An engine holding `ex:a ex:p "shared"`.
fn engine_with_one() -> Engine {
    let engine = engine();
    let mut tx = engine.transaction();
    tx.insert(quad("a", "p", literal("shared")).as_ref());
    tx.commit().expect("commit");
    engine
}

fn budget() -> Arc<Budget> {
    Arc::new(Budget::unlimited())
}

/// B7 and B9 (§3); the boundary inputs "interns nothing" and "an empty pending snapshot".
///
/// - Input: a private transaction over a committed snapshot. First its pending snapshot with no
///   change, then after it interns a term and a triple term of its own and asserts a statement
///   with the term.
/// - Expected: every pending snapshot, and every view made from one (restricted, `pending_over`,
///   with equality, as stored, canonical, within a reader's budget), is private. Each decodes
///   and looks up the transaction's terms through `Snapshot`'s methods. Each shares a dictionary
///   with the pending snapshot and none with the base. The base doesn't know the terms.
#[test]
fn a_private_pending_snapshot_and_its_views_read_the_transactions_terms() {
    let engine = engine_with_one();
    let base = engine.snapshot();
    let mut tx = engine.speculative_over(&base, budget()).unwrap();

    let empty = tx.pending_snapshot().unwrap();
    assert!(empty.is_private(), "private though it interned nothing");
    assert_eq!(empty.len_in(ReadModel::Asserted), 1, "the base's statement");
    assert!(!empty.shares_dictionary_with(&base));

    let term = literal("only here");
    let own = tx.try_intern(term.as_ref()).unwrap();
    assert_eq!(
        own.payload(),
        base.dictionary_len(),
        "the first own id is at the start"
    );
    let nested: Term = Triple::new(iri("a"), iri("p"), term.clone()).into();
    let triple = tx.try_intern(nested.as_ref()).unwrap();
    assert!(
        tx.try_insert(quad("a", "p", term.clone()).as_ref())
            .unwrap()
    );
    let added = quad("a", "p", term.clone());

    let pending = tx.pending_snapshot().unwrap();
    let view = base.with_inferred_subset(InferredSubset::Without(Vec::new()));
    let views: Vec<(&str, Snapshot)> = vec![
        ("pending", pending.clone()),
        (
            "restricted",
            pending.with_inferred_subset(InferredSubset::Without(Vec::new())),
        ),
        ("pending_over", tx.pending_over(&view, &[]).unwrap()),
        ("with_equality", pending.with_equality(None)),
        ("stored", pending.stored()),
        ("canonical", pending.with_canonical_equality()),
        ("reading_within", pending.reading_within(budget()).unwrap()),
    ];
    for (name, snapshot) in &views {
        assert!(snapshot.is_private(), "{name}: private");
        assert!(
            snapshot.shares_dictionary_with(&pending),
            "{name}: one vocabulary"
        );
        assert!(
            !snapshot.shares_dictionary_with(&base),
            "{name}: not the base's"
        );
        assert_eq!(snapshot.decode(own), Some(term.clone()), "{name}: decode");
        assert_eq!(snapshot.lookup(term.as_ref()), Some(own), "{name}: lookup");
        assert_eq!(
            snapshot.with_view(own, |view| view.str().map(str::to_owned)),
            Some(Some("only here".to_owned())),
            "{name}: with_view"
        );
        assert_eq!(
            snapshot.with_views(|views| views(own).and_then(|view| view.str().map(str::to_owned))),
            Some("only here".to_owned()),
            "{name}: with_views"
        );
        let shared = snapshot.lookup(iri("a").as_ref().into()).unwrap();
        let predicate = snapshot.lookup(iri("p").as_ref().into()).unwrap();
        assert_eq!(
            snapshot.triple_term(shared, predicate, own),
            Some(triple),
            "{name}: triple_term"
        );
        assert_eq!(
            snapshot.components(triple),
            Some([shared, predicate, own]),
            "{name}: components"
        );
        assert_eq!(
            snapshot.decode(triple),
            Some(nested.clone()),
            "{name}: decode a triple term"
        );
        let encoded = snapshot
            .lookup_quad(added.as_ref())
            .expect("the quad's ids");
        assert_eq!(
            snapshot.decode_quad(encoded),
            Some(added.clone()),
            "{name}: decode_quad"
        );
    }
    assert!(pending.contains(&pending.lookup_quad(added.as_ref()).unwrap()));
    assert_eq!(base.lookup(term.as_ref()), None, "the base doesn't know it");
    assert_eq!(
        tx.decode(own),
        Some(term.clone()),
        "the transaction decodes it too"
    );
    assert_eq!(tx.lookup(term.as_ref()), Some(own));
}

/// B8 (§3): two private transactions over one snapshot, each interning a different term.
///
/// - Expected: both terms get the same number. Their snapshots don't share a dictionary either
///   way, and each decodes its own term at that number.
#[test]
fn two_private_transactions_give_their_own_terms_the_same_number() {
    let engine = engine_with_one();
    let base = engine.snapshot();
    let (first, second) = (
        engine.speculative_over(&base, budget()).unwrap(),
        engine.speculative_over(&base, budget()).unwrap(),
    );
    let one = first.try_intern(literal("first's").as_ref()).unwrap();
    let other = second.try_intern(literal("second's").as_ref()).unwrap();
    assert_eq!(one, other, "the same number");
    let (a, b) = (
        first.pending_snapshot().unwrap(),
        second.pending_snapshot().unwrap(),
    );
    assert!(!a.shares_dictionary_with(&b));
    assert!(!b.shares_dictionary_with(&a));
    assert_eq!(a.decode(one), Some(literal("first's")));
    assert_eq!(b.decode(other), Some(literal("second's")));
}

/// B9 at the edge (§3): a term the dictionary gets after the base was taken.
///
/// - Input: a snapshot; then a writer commits `ex:b ex:p "published later"` (two new terms);
///   then a private transaction over the earlier snapshot interns `"published later"`.
/// - Expected: the transaction gives it an id of its own, the first from the start it opened
///   at, not the dictionary's (which comes after `ex:b`'s). Its pending snapshot decodes that
///   id to the term.
#[test]
fn a_term_published_after_the_base_is_the_private_transactions_own() {
    let engine = engine_with_one();
    let base = engine.snapshot();
    let mut writer = engine.transaction();
    writer.insert(quad("b", "p", literal("published later")).as_ref());
    writer.commit().expect("commit");
    let shared = engine
        .snapshot()
        .lookup(literal("published later").as_ref())
        .unwrap();
    assert_eq!(shared.payload(), base.dictionary_len() + 1, "after ex:b's");

    let tx = engine.speculative_over(&base, budget()).unwrap();
    let later = tx.try_intern(literal("published later").as_ref()).unwrap();
    assert_eq!(
        later.payload(),
        base.dictionary_len(),
        "its own, the first from its start"
    );
    let pending = tx.pending_snapshot().unwrap();
    assert_eq!(pending.decode(later), Some(literal("published later")));
    assert_eq!(
        pending.lookup(literal("published later").as_ref()),
        Some(later)
    );
}

/// B14 (§3): what reads a snapshot's published state, given a private one.
///
/// - Input: a durable engine and a private transaction's pending snapshot.
/// - Expected: it has no shared dictionary; a checkpoint of it is refused and writes no file;
///   characteristic sets built from it are its own and write no statistics file.
#[test]
fn a_private_snapshot_has_no_published_state() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(
        dir.path(),
        EngineConfig {
            background_maintenance: false,
            ..EngineConfig::default()
        },
    )
    .unwrap();
    let mut tx = engine.transaction();
    tx.insert(quad("a", "p", literal("shared")).as_ref());
    tx.commit().unwrap();
    let mut private = engine
        .speculative_over(&engine.snapshot(), budget())
        .unwrap();
    private
        .try_insert(quad("a", "p", literal("own")).as_ref())
        .unwrap();
    let pending = private.pending_snapshot().unwrap();
    assert!(pending.shared_dictionary().is_none());

    let before = files(dir.path());
    let out = tempfile::tempdir().unwrap();
    let refused = crate::durability::checkpoint::write(out.path(), &pending, 0, None);
    assert!(
        matches!(refused, Err(EngineError::Configuration(_))),
        "{refused:?}"
    );
    assert!(files(out.path()).is_empty(), "no checkpoint written");
    let _ = pending.characteristic_sets_in(ReadModel::Asserted);
    assert_eq!(files(dir.path()), before, "no statistics file written");
}

fn files(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// B15 (§3; the search owner's one test): searches on a private snapshot.
///
/// - Input: an engine with a label and a vector literal, its text and vector indexes not yet
///   built; a private transaction's pending snapshot.
/// - Expected: the six searches and the two corpus helpers return `SearchRefused::Private`;
///   preparing a vector graph does nothing. Afterwards the dictionary's text coverage and
///   vector bytes are as before. The published snapshot still searches.
#[test]
fn a_private_snapshot_refuses_every_search_and_leaves_the_shared_indexes() {
    let engine = engine();
    let vector: Term = Literal::new_typed_literal(
        nrese_vector::lexical(&[1.0, 0.0]),
        NamedNode::new_unchecked(nrese_vector::DATATYPE),
    )
    .into();
    let mut tx = engine.transaction();
    tx.insert(quad("a", "label", literal("alpha beta")).as_ref());
    tx.insert(quad("a", "vector", vector).as_ref());
    tx.commit().unwrap();
    let base = engine.snapshot();
    let private = engine.speculative_over(&base, budget()).unwrap();
    let pending = private.pending_snapshot().unwrap();
    let stats = || engine.inner.shared.dictionary.stats();
    let before = stats();

    let text = TextQuery {
        text: "alpha".to_owned(),
        all_words: false,
        prefix: false,
        stem: None,
    };
    let near = VectorQuery::new(vec![1.0, 0.0], 1);
    let unlimited = budget();
    let every = TextScope::EVERY;
    let model = ReadModel::Materialised;
    let refused = |result: Result<(), SearchRefused>| matches!(result, Err(SearchRefused::Private));
    assert!(refused(pending.text_search(&text).map(drop)), "text_search");
    assert!(refused(pending.iri_search(&text).map(drop)), "iri_search");
    assert!(
        refused(
            pending
                .vector_search(&near, &|_| true, &unlimited)
                .map(drop)
        ),
        "vector_search"
    );
    assert!(
        refused(pending.text_search_in(model, &every, &text, None).map(drop)),
        "text_search_in"
    );
    assert!(
        refused(pending.iri_search_in(model, &every, &text, None).map(drop)),
        "iri_search_in"
    );
    assert!(
        refused(
            pending
                .vector_search_in(model, &every, &near, &|_| true, &unlimited)
                .map(drop)
        ),
        "vector_search_in"
    );
    assert!(refused(
        pending.text_corpus_members(model, &every, false).map(drop)
    ));
    assert!(refused(
        pending.vector_corpus_members(model, &every).map(drop)
    ));
    pending.prepare_vector_graph(&near);
    let after = stats();
    assert_eq!(after.text_covered, before.text_covered, "text index cover");
    assert_eq!(
        after.vector_bytes, before.vector_bytes,
        "vector graph state"
    );
    assert_eq!(after.index_bytes, before.index_bytes, "the indexes' bytes");

    assert_eq!(
        base.text_search(&text).unwrap().len(),
        1,
        "the published snapshot searches"
    );
}

/// B18 (§3): corpus lineage, re-specified by dictionary 3.2 against the contract's S3 and §5
/// row 24 (lineage only at publication; DUR's condition 2).
///
/// - Input: an engine whose reader-scoped search keeps a corpus. A write transaction's
///   commit (the control), then a speculative writer's pending snapshot, then a private
///   transaction's pending snapshot and its pending view.
/// - Expected: the commit records a lineage step; neither pending state records one.
#[test]
fn a_private_transaction_records_no_corpus_lineage() {
    let engine = engine();
    let mut tx = engine.transaction();
    tx.insert(quad("a", "label", literal("alpha beta")).as_ref());
    tx.commit().unwrap();
    let base = engine.snapshot();
    let text = TextQuery {
        text: "alpha".to_owned(),
        all_words: false,
        prefix: false,
        stem: None,
    };
    base.text_search_in(ReadModel::Materialised, &TextScope::EVERY, &text, None)
        .unwrap();
    let steps = || engine.snapshot().text_corpus_stats().steps;
    let start = steps();

    // The control: a publication records its step.
    let mut published = engine.transaction();
    published.insert(quad("p", "label", literal("epsilon")).as_ref());
    published.commit().unwrap();
    let after_writer = steps();
    assert_eq!(
        after_writer,
        start + 1,
        "the measure sees a publication's step"
    );

    let mut writer = engine.speculative();
    writer.insert(quad("b", "label", literal("gamma")).as_ref());
    let _pending_writer = writer.pending_snapshot();
    assert_eq!(
        steps(),
        after_writer,
        "no lineage step for a writer's pending state"
    );

    let mut private = engine.speculative_over(&base, budget()).unwrap();
    private
        .try_insert(quad("c", "label", literal("delta")).as_ref())
        .unwrap();
    let _pending = private.pending_snapshot().unwrap();
    let view = base.with_inferred_subset(InferredSubset::Without(Vec::new()));
    let _over = private.pending_over(&view, &[]).unwrap();
    assert_eq!(
        steps(),
        after_writer,
        "no lineage step for a private transaction"
    );
}

/// Boundary inputs (§3): a snapshot at dictionary length 0, and a term only the overlay has
/// removed.
///
/// - Expected: over an empty engine's snapshot the first own id is 0. Removing a quad of a
///   term only the transaction has works once and then reports nothing.
#[test]
fn private_transactions_at_the_edges() {
    let empty = engine();
    let base = empty.snapshot();
    assert_eq!(base.dictionary_len(), 0);
    let mut tx = empty.speculative_over(&base, budget()).unwrap();
    let own = tx.try_intern(literal("first").as_ref()).unwrap();
    assert_eq!(own.payload(), 0);
    let statement = quad("a", "p", literal("first"));
    assert!(tx.try_insert(statement.as_ref()).unwrap());
    assert!(tx.remove(statement.as_ref()), "removed once");
    assert!(!tx.remove(statement.as_ref()), "then nothing to remove");
    assert_eq!(tx.len(), 0);
}

/// `speculative_over`'s refusals (the master, 17:50: the accepted change to its signature).
/// A private transaction over a private snapshot would read that snapshot's own ids, past its
/// start, as dictionary ids; one over another engine's snapshot would read another dictionary.
///
/// - Input: a private transaction's pending snapshot, with a term of its own; a published
///   snapshot of another engine.
/// - Expected: `speculative_over` either is refused with `EngineError::Configuration`; the
///   engine's own published snapshot is not.
#[test]
fn speculative_over_refuses_a_private_snapshot_and_another_engines() {
    let engine = engine_with_one();
    let base = engine.snapshot();
    let mut tx = engine.speculative_over(&base, budget()).unwrap();
    tx.try_insert(quad("a", "p", literal("own")).as_ref())
        .unwrap();
    let private = tx.pending_snapshot().unwrap();
    assert!(
        matches!(
            engine.speculative_over(&private, budget()),
            Err(EngineError::Configuration(_))
        ),
        "over a private snapshot"
    );
    let other = engine_with_one();
    assert!(
        matches!(
            engine.speculative_over(&other.snapshot(), budget()),
            Err(EngineError::Configuration(_))
        ),
        "over another engine's snapshot"
    );
    assert!(
        engine.speculative_over(&base, budget()).is_ok(),
        "its own published one"
    );
}

/// The private transaction's charges (§2 point 5): what its vocabulary holds is charged to
/// its budget for as long as a pending snapshot holds a view of it, and released after.
///
/// - Input: a private transaction interning 100 terms, two pending snapshots, a term decoded
///   from one after the transaction drops.
/// - Expected: charged while a snapshot lives; the decoded term is the interned one; nothing
///   charged once the snapshots go.
#[test]
fn a_private_transactions_vocabulary_is_charged_until_its_last_snapshot_goes() {
    let engine = engine_with_one();
    let base = engine.snapshot();
    let charged = budget();
    let tx = engine
        .speculative_over(&base, Arc::clone(&charged))
        .unwrap();
    let ids: Vec<TermId> = (0..100)
        .map(|n| {
            tx.try_intern(literal(&format!("term {n}")).as_ref())
                .unwrap()
        })
        .collect();
    let first = tx.pending_snapshot().unwrap();
    let second = tx.pending_snapshot().unwrap();
    drop(tx);
    assert!(charged.used() > 0, "charged while snapshots hold it");
    assert_eq!(second.decode(ids[42]), Some(literal("term 42")));
    drop(first);
    assert!(charged.used() > 0, "one snapshot still holds it");
    drop(second);
    assert_eq!(charged.used(), 0, "released with the last holder");
}

/// An engine with equality off, `ex:c owl:sameAs ex:b` inferred: (it, `owl:sameAs`, `ex:b`).
fn engine_with_a_class() -> (Engine, TermId, TermId) {
    let engine = engine();
    let mut tx = engine.transaction();
    tx.insert(quad("b", "p", literal("v")).as_ref());
    let same_as = tx.intern(
        NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs")
            .as_ref()
            .into(),
    );
    let b = tx.lookup(iri("b").as_ref().into()).unwrap();
    let c = tx.intern(iri("c").as_ref().into());
    assert!(tx.insert_inferred(EncodedTriple::new(c, same_as, b)));
    tx.commit().unwrap();
    (engine, same_as, b)
}

/// Sol-loop r1 S1 (8 Nov): a private snapshot's equality classes are its own (§2 point 3: no
/// private id reaches shared state), whichever is read first.
///
/// - Input: an engine with equality off and a published class {`ex:b`, `ex:c`}; a private
///   transaction that infers `ex:x owl:sameAs ex:b`, `ex:x` its own term. Its pending snapshot
///   and the published snapshot, each read with equality on, in both orders.
/// - Expected: privately `ex:x` is in `ex:b`'s class; the published classes don't know it,
///   whether they were read before or after the private ones.
#[test]
fn a_private_snapshots_equality_classes_stay_its_own() {
    // The private classes first.
    let (engine, same_as, b) = engine_with_a_class();
    let base = engine.snapshot();
    let mut tx = engine.speculative_over(&base, budget()).unwrap();
    let x = tx.try_intern(iri("x").as_ref().into()).unwrap();
    assert!(tx.insert_inferred(EncodedTriple::new(x, same_as, b)));
    let private = tx.pending_snapshot().unwrap();
    let classes = private
        .with_equality(Some(same_as))
        .equality_classes()
        .unwrap();
    assert_eq!(
        classes.representative(x),
        classes.representative(b),
        "privately one class"
    );
    let published = base
        .with_equality(Some(same_as))
        .equality_classes()
        .unwrap();
    assert_ne!(
        published.representative(x),
        published.representative(b),
        "the published classes, read after, don't know the private alias"
    );

    // The published classes first.
    let (engine, same_as, b) = engine_with_a_class();
    let base = engine.snapshot();
    let published = base
        .with_equality(Some(same_as))
        .equality_classes()
        .unwrap();
    let mut tx = engine.speculative_over(&base, budget()).unwrap();
    let x = tx.try_intern(iri("x").as_ref().into()).unwrap();
    assert_ne!(published.representative(x), published.representative(b));
    assert!(tx.insert_inferred(EncodedTriple::new(x, same_as, b)));
    let private = tx.pending_snapshot().unwrap();
    let classes = private
        .with_equality(Some(same_as))
        .equality_classes()
        .unwrap();
    assert_eq!(
        classes.representative(x),
        classes.representative(b),
        "privately one class, the published classes read before"
    );
}

/// `?s ex:p ?o` read with equality on, expanded (`canonical` false) or canonical: each
/// answer's subject and object, decoded (an id the snapshot can't decode as `?`), sorted.
fn equality_answers(
    snapshot: &Snapshot,
    same_as: TermId,
    canonical: bool,
) -> Vec<(String, String)> {
    let expanded = snapshot.with_equality(Some(same_as));
    let view = match canonical {
        true => expanded.with_canonical_equality(),
        false => expanded,
    };
    let pattern = QuadPattern {
        predicate: view.lookup(iri("p").as_ref().into()),
        ..QuadPattern::all()
    };
    let text = |id| {
        view.decode(id)
            .map_or("?".to_owned(), |term| term.to_string())
    };
    let mut answers: Vec<(String, String)> = view
        .quads_for_pattern_in(ReadModel::Materialised, &pattern)
        .map(|quad| (text(quad.subject), text(quad.object)))
        .collect();
    answers.sort();
    answers
}

fn pairs(rows: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = rows
        .iter()
        .map(|&(s, o)| (format!("<http://example.com/{s}>"), format!("\"{o}\"")))
        .collect();
    rows.sort();
    rows
}

/// Review P2B-F1 (8 Nov): a real equality class with a private member, its answers read
/// canonically and expanded, in both orders.
///
/// - Input: the published class {`ex:b`, `ex:c`} (`ex:b` its representative, the smallest id)
///   with `ex:b ex:p "v"`. A private transaction stores what the closure over representatives
///   holds for its own `ex:x` joining that class (the module doc of `equality`): `ex:x
///   owl:sameAs ex:b`, and `ex:b ex:p "w"` for its asserted `ex:x ex:p "w"`. Its pending
///   snapshot and the published one read with equality on, canonically and expanded: the
///   private first, then on a fresh engine the published first.
/// - Expected, either order: canonically, privately `"v"` and `"w"` on `ex:b`, published
///   `"v"` on `ex:b`. Expanded, privately both on each of `ex:b`, `ex:c` and `ex:x`; published
///   `"v"` on `ex:b` and `ex:c` alone.
#[test]
fn equality_reads_keep_a_private_class_member_private() {
    for private_first in [true, false] {
        let (engine, same_as, b) = engine_with_a_class();
        let base = engine.snapshot();
        let read = |snapshot: &Snapshot| {
            [false, true].map(|canonical| equality_answers(snapshot, same_as, canonical))
        };
        let published_before = (!private_first).then(|| read(&base));
        let mut tx = engine.speculative_over(&base, budget()).unwrap();
        let x = tx.try_intern(iri("x").as_ref().into()).unwrap();
        let p = base.lookup(iri("p").as_ref().into()).unwrap();
        let w = tx.try_intern(literal("w").as_ref()).unwrap();
        assert!(tx.insert_inferred(EncodedTriple::new(x, same_as, b)));
        assert!(tx.insert_inferred(EncodedTriple::new(b, p, w)));
        assert!(
            tx.try_insert(quad("x", "p", literal("w")).as_ref())
                .unwrap()
        );
        let [private_expanded, private_canonical] = read(&tx.pending_snapshot().unwrap());
        let [published_expanded, published_canonical] =
            published_before.unwrap_or_else(|| read(&base));
        let order = if private_first {
            "private first"
        } else {
            "published first"
        };
        assert_eq!(
            private_canonical,
            pairs(&[("b", "v"), ("b", "w")]),
            "{order}: privately, canonical"
        );
        assert_eq!(
            private_expanded,
            pairs(&[
                ("b", "v"),
                ("b", "w"),
                ("c", "v"),
                ("c", "w"),
                ("x", "v"),
                ("x", "w")
            ]),
            "{order}: privately, expanded"
        );
        assert_eq!(
            published_canonical,
            pairs(&[("b", "v")]),
            "{order}: published, canonical"
        );
        assert_eq!(
            published_expanded,
            pairs(&[("b", "v"), ("c", "v")]),
            "{order}: published, expanded"
        );
    }
}

/// Review P2B-F1 (8 Nov): `pending_over`'s version keeps its equality classes apart from its
/// view's, in both orders.
///
/// - Input: the published class {`ex:b`, `ex:c`}; a restricted view of the base; a private
///   transaction's `pending_over` that view, hiding the inferred `ex:c owl:sameAs ex:b`. The
///   private result's classes and the view's, both read with equality on, the private one
///   first, then on a fresh engine the view's first.
/// - Expected: privately `ex:c` is outside `ex:b`'s class; in the view it is inside,
///   whichever is read first.
#[test]
fn a_pending_view_hiding_an_equality_edge_keeps_its_classes_apart() {
    for private_first in [true, false] {
        let (engine, same_as, b) = engine_with_a_class();
        let base = engine.snapshot();
        let c = base.lookup(iri("c").as_ref().into()).unwrap();
        let edge = EncodedTriple::new(c, same_as, b).in_default_graph();
        let view = base.with_inferred_subset(InferredSubset::Without(Vec::new()));
        let classes_of = |snapshot: &Snapshot| {
            snapshot
                .with_equality(Some(same_as))
                .equality_classes()
                .unwrap()
        };
        let view_before = (!private_first).then(|| classes_of(&view));
        let tx = engine.speculative_over(&base, budget()).unwrap();
        let private = classes_of(&tx.pending_over(&view, &[edge]).unwrap());
        let viewed = view_before.unwrap_or_else(|| classes_of(&view));
        let order = if private_first {
            "private first"
        } else {
            "view first"
        };
        assert_ne!(
            private.representative(c),
            private.representative(b),
            "{order}: the hidden edge joins nothing privately"
        );
        assert_eq!(
            viewed.representative(c),
            viewed.representative(b),
            "{order}: the view keeps its class"
        );
    }
}
