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

/// Attack item 9: a private delta that changes `owl:sameAs` statements (one class split,
/// one joined) under readers of equality classes, expanded and canonical, with the base's
/// IRI corpus of `ex:g` warm: each IRI search equals an engine holding the same statements.
/// The mutant that skips the `sameAs` check is to be red here (DICT2 9 Nov 04:02, Q1 b).
#[test]
fn a_private_same_as_change_searches_its_own_classes() {
    let same_as_iri = NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs");
    // Interned in this order, so the first of each class is its representative.
    let names = ["rep", "alias", "other", "joined"];
    let setup = |split: bool, join: bool| {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        engine.set_corpus_tuning(CorpusTuning {
            documents_per_probe: 1,
            ..CorpusTuning::DEFAULT
        });
        let mut tx = engine.transaction();
        let ids: Vec<TermId> = names
            .iter()
            .map(|name| tx.intern(ex(name).as_ref().into()))
            .collect();
        let same_as = tx.intern(same_as_iri.as_ref().into());
        let edge = |alias: usize, rep: usize| EncodedTriple {
            subject: ids[alias],
            predicate: same_as,
            object: ids[rep],
        };
        if !split {
            assert!(tx.insert_inferred(edge(1, 0)));
        }
        if join {
            assert!(tx.insert_inferred(edge(3, 2)));
        }
        for name in ["rep", "other"] {
            tx.insert(Quad::new(ex(name), ex("p"), ex("o"), ex("g")).as_ref());
        }
        tx.insert(Quad::new(ex("filler"), ex("p"), ex("o"), ex("g")).as_ref());
        tx.commit().unwrap();
        engine.set_equality(Some(same_as));
        (engine, ids, same_as)
    };
    let mut failed = Vec::new();
    for canonical in [false, true] {
        let read = |snapshot: &Snapshot| match canonical {
            true => snapshot.with_canonical_equality(),
            false => snapshot.clone(),
        };
        let search = |snapshot: &Snapshot| {
            let g = snapshot.lookup(ex("g").as_ref().into()).unwrap();
            let scope = TextScope::Graphs(GraphSelector::Exact(g));
            let mut found = snapshot
                .iri_search_in(
                    ReadModel::Materialised,
                    &scope,
                    &query("rep alias other joined filler", false, false),
                    None,
                )
                .map(|found| decoded(snapshot, &found))
                .map_err(|e| format!("{e:?}"));
            if let Ok(found) = &mut found {
                found.sort_by(|a, b| a.0.cmp(&b.0));
            }
            found
        };
        let (truth, _, _) = setup(true, true);
        let want = search(&read(&truth.snapshot()));
        let (engine, ids, same_as) = setup(false, false);
        let published = engine.snapshot();
        // Warm: the base's corpus kept, so the private state could derive from it.
        search(&read(&published)).expect("the published search");
        let mut private = engine
            .speculative_over(&published, Arc::new(nrese_exec::Budget::unlimited()))
            .unwrap();
        assert!(private.remove_inferred(EncodedTriple {
            subject: ids[1],
            predicate: same_as,
            object: ids[0],
        }));
        assert!(private.insert_inferred(EncodedTriple {
            subject: ids[3],
            predicate: same_as,
            object: ids[2],
        }));
        let got = search(&read(&private.pending_snapshot().unwrap()));
        if got != want || got.is_err() {
            failed.push(format!("canonical {canonical}: {got:?} != {want:?}"));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// Attack item 9, DICT2's condition (b) (sol-loop r1 F1): a private removal of a `sameAs`
/// statement written representative first (`ex:rep owl:sameAs ex:alias`, `ex:rep` the
/// smaller id), asserted or inferred, under expanded and canonical readers, the base's IRI
/// corpus of the default graph warm. Its only IRI subject is `ex:rep`, so a derivation
/// probing the touched terms alone would keep `ex:alias` (no longer in a class, no longer
/// a subject): the private state walks instead. Each search equals an engine without the
/// statement. Red by mutation (the `sameAs` check skipped).
#[test]
fn a_private_removal_of_a_same_as_written_representative_first_drops_the_alias() {
    let same_as_iri = NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs");
    let setup = |edge: bool, inferred: bool| {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        engine.set_corpus_tuning(CorpusTuning {
            documents_per_probe: 1,
            ..CorpusTuning::DEFAULT
        });
        let mut tx = engine.transaction();
        // The representative interned first: the smaller id.
        let rep = tx.intern(ex("rep").as_ref().into());
        let alias = tx.intern(ex("alias").as_ref().into());
        let same_as = tx.intern(same_as_iri.as_ref().into());
        for subject in ["rep", "filler0", "filler1", "filler2"] {
            tx.insert(Quad::new(ex(subject), ex("p"), ex("o"), GraphName::DefaultGraph).as_ref());
        }
        let triple = EncodedTriple {
            subject: rep,
            predicate: same_as,
            object: alias,
        };
        if edge {
            match inferred {
                true => assert!(tx.insert_inferred(triple)),
                false => assert!(tx.insert_encoded(triple.in_default_graph())),
            }
        }
        tx.commit().unwrap();
        engine.set_equality(Some(same_as));
        (engine, triple)
    };
    let search = |snapshot: &Snapshot| {
        let scope = TextScope::Graphs(GraphSelector::Exact(TermId::DEFAULT_GRAPH));
        let mut found = snapshot
            .iri_search_in(
                ReadModel::Materialised,
                &scope,
                &query("rep alias filler0", false, false),
                None,
            )
            .map(|found| decoded(snapshot, &found))
            .map_err(|e| format!("{e:?}"));
        if let Ok(found) = &mut found {
            found.sort_by(|a, b| a.0.cmp(&b.0));
        }
        found
    };
    let mut failed = Vec::new();
    for inferred in [false, true] {
        for canonical in [false, true] {
            let read = |snapshot: &Snapshot| match canonical {
                true => snapshot.with_canonical_equality(),
                false => snapshot.clone(),
            };
            let (truth, _) = setup(false, inferred);
            let want = search(&read(&truth.snapshot()));
            let (engine, triple) = setup(true, inferred);
            let published = engine.snapshot();
            search(&read(&published)).expect("the published search");
            let mut private = engine
                .speculative_over(&published, Arc::new(nrese_exec::Budget::unlimited()))
                .unwrap();
            match inferred {
                true => assert!(private.remove_inferred(triple)),
                false => assert!(private.remove_encoded(triple.in_default_graph())),
            }
            let got = search(&read(&private.pending_snapshot().unwrap()));
            if got != want || got.is_err() {
                failed.push(format!(
                    "inferred {inferred}, canonical {canonical}: {got:?} != {want:?}"
                ));
            }
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// Sol-loop r1 B1: a private search's own index is charged as an upper bound of what it
/// allocates. One own literal of 10,000 distinct words (about 60 KB of text; its index
/// holds 10,000 words with their postings, well past 256 KiB) within a 256 KiB request is
/// refused; within 64 MiB it is found. Red before the fix: the old estimate (three bytes a
/// text byte) charged about 180 KB and answered.
#[test]
fn a_private_search_charges_its_own_index_before_it_builds_it() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let published = engine.snapshot();
    let mut private = engine
        .speculative_over(&published, Arc::new(nrese_exec::Budget::unlimited()))
        .unwrap();
    let text: Vec<String> = (0..10_000).map(|i| format!("w{i:04}")).collect();
    let text = format!("alpha {}", text.join(" "));
    private
        .try_insert(label("big", &text, None).as_ref())
        .unwrap();
    let pending = private.pending_snapshot().unwrap();
    let alpha = query("alpha", false, false);
    let search = |limit: usize| {
        let budget = Arc::new(nrese_exec::Budget::new(limit));
        pending.text_search_in(
            ReadModel::Materialised,
            &TextScope::EVERY,
            &alpha,
            Some(&budget),
        )
    };
    let small = search(256 << 10);
    assert!(small.is_err(), "within 256 KiB: {small:?}");
    let found = search(64 << 20).unwrap();
    assert!(
        decoded(&pending, &found).iter().any(|(t, _)| *t == text),
        "within 64 MiB"
    );
}

/// Sol-loop r1 B3: a private snapshot that reads within a request's budget (as a SPARQL
/// query's does), with equality expansion and a cold vector corpus, searched by the global
/// vector entry point within that same budget, answers: the search charges the budget
/// through the snapshot's own reads, not through a bridge that holds all its room. Red
/// before the fix: the bridge reserved the remainder and the expansion's reads were
/// refused.
#[test]
fn a_private_global_vector_search_within_the_readers_budget_answers() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let rep = tx.intern(ex("rep").as_ref().into());
    let alias = tx.intern(ex("alias").as_ref().into());
    let same_as = tx.intern(
        NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs")
            .as_ref()
            .into(),
    );
    assert!(tx.insert_inferred(EncodedTriple {
        subject: alias,
        predicate: same_as,
        object: rep,
    }));
    tx.insert(label("rep", "[1, 0]", None).as_ref());
    tx.commit().unwrap();
    engine.set_equality(Some(same_as));
    let published = engine.snapshot();
    let mut private = engine
        .speculative_over(&published, Arc::new(nrese_exec::Budget::unlimited()))
        .unwrap();
    private
        .try_insert(label("own", "[0, 1]", None).as_ref())
        .unwrap();
    let budget = Arc::new(nrese_exec::Budget::new(64 << 20));
    let reader = private
        .pending_snapshot()
        .unwrap()
        .reading_within(Arc::clone(&budget))
        .unwrap();
    let near = nrese_engine::VectorQuery::new(vec![0.0, 1.0], 2);
    let found = reader.vector_search(&near, &|_| true, &budget);
    assert!(found.is_ok(), "{:?}", found.as_ref().err());
    let (hits, _) = found.unwrap();
    assert_eq!(hits.len(), 2);
    assert!(reader.read_refused().is_none());
}

/// A vector search's answer decoded: each hit's lexical form and distance, and the
/// report's space and scan.
type VectorAnswer = Result<(Vec<(String, f32)>, usize, usize), String>;

fn vector_answer(
    snapshot: &Snapshot,
    query: &nrese_engine::VectorQuery,
    accept: &(dyn Fn(TermId) -> bool + Sync),
) -> VectorAnswer {
    let budget = Arc::new(nrese_exec::Budget::unlimited());
    let (hits, report) = snapshot
        .vector_search_in(
            ReadModel::Materialised,
            &TextScope::EVERY,
            query,
            accept,
            &budget,
        )
        .map_err(|e| format!("{e:?}"))?;
    let hits = hits
        .iter()
        .map(|&(id, distance)| match snapshot.decode(id) {
            Some(Term::Literal(l)) => (l.value().to_owned(), distance),
            other => panic!("{other:?}"),
        })
        .collect();
    Ok((hits, report.space, report.scanned))
}

/// Attack item 10: vectors. The base holds `[1, 0]` and `[0.6, 0.8]`; the private delta
/// adds `[0, 1]` (its own, at the same distance from `[1, 1]` as the base's `[1, 0]`), a
/// vector of a dimension no base vector has, and removes `[0.6, 0.8]`. Each search, exact,
/// equals an engine holding the same statements: hits by distance then id across shared
/// and own vectors (k on whole tie groups), `accept` refusing the own one, a dimension only
/// the delta has, and the report's space and scan: exactly shared plus own, which the
/// SPARQL caller widens k by (SRCH 9 Nov).
#[test]
fn a_private_snapshot_searches_its_vectors_and_the_bases() {
    let base = |engine: &Engine| {
        let mut tx = engine.transaction();
        tx.insert(label("a", "[1, 0]", None).as_ref());
        tx.insert(label("c", "[0.6, 0.8]", None).as_ref());
        tx.insert(label("a", "alpha", None).as_ref());
        tx.commit().unwrap();
    };
    let inserted = || vec![label("b", "[0, 1]", None), label("d", "[0, 0, 1]", None)];
    let removed = || vec![label("c", "[0.6, 0.8]", None)];
    let truth = {
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
        engine.snapshot()
    };
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
    let own_of = |snapshot: &Snapshot| {
        let own: Term = Literal::new_typed_literal(
            "[0, 1]",
            NamedNode::new_unchecked(nrese_engine::vector::DATATYPE),
        )
        .into();
        snapshot.lookup(own.as_ref())
    };
    let queries = [
        // k on whole tie groups (SRCH 9 Nov): which members of a group a cut keeps
        // follows the order offers arrive in, which an independent engine needn't share.
        ("tie", nrese_engine::VectorQuery::new(vec![1.0, 1.0], 2)),
        (
            "near own",
            nrese_engine::VectorQuery::new(vec![0.0, 1.0], 3),
        ),
        (
            "own dimension",
            nrese_engine::VectorQuery::new(vec![0.0, 0.0, 1.0], 2),
        ),
        (
            "no dimension",
            nrese_engine::VectorQuery::new(vec![1.0; 4], 2),
        ),
    ];
    let mut failed = Vec::new();
    for (name, query) in &queries {
        for refuse_own in [false, true] {
            let (got, want) = match refuse_own {
                false => (
                    vector_answer(&pending, query, &|_| true),
                    vector_answer(&truth, query, &|_| true),
                ),
                true => {
                    let (mine, theirs) = (own_of(&pending), own_of(&truth));
                    (
                        vector_answer(&pending, query, &|id| Some(id) != mine),
                        vector_answer(&truth, query, &|id| Some(id) != theirs),
                    )
                }
            };
            if got != want || got.is_err() {
                failed.push(format!(
                    "{name}, refuse own {refuse_own}: {got:?} != {want:?}"
                ));
            }
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// Attack item 12: a private search within a budget too small for what it builds is
/// refused, never answered in part; within enough, it answers as the oracle does. Text, IRI
/// and vectors, the corpora cold.
#[test]
fn a_private_search_without_room_is_refused() {
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
    let truth = oracle().snapshot();
    let model = ReadModel::Materialised;
    let text = query("alpha zeta", false, false);
    let tiny = Arc::new(nrese_exec::Budget::new(64));
    let mut failed = Vec::new();
    if pending
        .text_search_in(model, &TextScope::EVERY, &text, Some(&tiny))
        .is_ok()
    {
        failed.push("text within 64 bytes answered".to_owned());
    }
    if pending
        .iri_search_in(
            model,
            &TextScope::EVERY,
            &query("zebra", false, false),
            Some(&tiny),
        )
        .is_ok()
    {
        failed.push("IRI within 64 bytes answered".to_owned());
    }
    let near = nrese_engine::VectorQuery::new(vec![1.0, 0.0], 2);
    if pending
        .vector_search_in(model, &TextScope::EVERY, &near, &|_| true, &tiny)
        .is_ok()
    {
        failed.push("vectors within 64 bytes answered".to_owned());
    }
    let enough = Arc::new(nrese_exec::Budget::new(64 << 20));
    let got = pending
        .text_search_in(model, &TextScope::EVERY, &text, Some(&enough))
        .map(|found| decoded(&pending, &found));
    let want = truth
        .text_search_in(model, &TextScope::EVERY, &text, None)
        .map(|found| decoded(&truth, &found));
    if got.as_ref().ok() != want.as_ref().ok() || got.is_err() {
        failed.push(format!("within enough: {got:?} != {want:?}"));
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
