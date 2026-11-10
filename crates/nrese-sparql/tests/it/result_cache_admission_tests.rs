//! The result cache admits only published state (the lifecycle contract's S3; package 3.6,
//! `notes/2026-11-08-dict3-6-result-cache-design.md` §4, the token design check's §5 items 1,
//! 2, 3, 7, 8 and 9). The oracle is the cache's own state (its counters, its pins, whether it
//! offers a slot) and answers compared with an evaluation without the cache. Whether a
//! snapshot is published is the engine's `Snapshot::shared_state` (package 3.0's guards).

use std::sync::Arc;

use nrese_engine::{
    EncodedQuad, EncodedTriple, Engine, EngineConfig, InferredSubset, QuadPattern, ReadModel,
    Snapshot,
};
use nrese_rdf::{GraphName, Literal, Quad};
use nrese_sparql::{
    CachedOutput, GraphAccess, PinRequest, PinnedResult, QueryEvaluationError, QueryOptions,
    ReadView, ResultCache, ResultCacheStats, cached_output, evaluate_query, explain_query,
};
use nrese_sparql_syntax::SparqlParser;

use crate::native_differential_tests::{EX, ex, rows_up_to_equal_values};

const TSV: &str = "text/tab-separated-values";

/// Three joined patterns (their prefixes are offered), a group (a claimed part) and the
/// inferred statements (what masks change).
fn queries() -> [String; 3] {
    [
        format!("SELECT ?x ?w WHERE {{ ?x <{EX}p> ?y . ?y <{EX}p> ?z . ?z <{EX}q> ?w }}"),
        format!("SELECT ?y (COUNT(*) AS ?n) WHERE {{ ?x <{EX}p> ?y }} GROUP BY ?y"),
        format!("SELECT ?x ?o WHERE {{ ?x <{EX}s> ?o }}"),
    ]
}

fn quad(s: &str, p: &str, o: &str) -> Quad {
    Quad::new(ex(s), ex(p), ex(o), GraphName::DefaultGraph)
}

/// Two chains `?x p ?y p ?z q ?w`, a statement in each of the graphs `g/a/1` and `g/b/1`, and
/// two inferred statements `a s i1`, `b s i2`.
fn engine() -> Engine {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    let mut tx = engine.transaction();
    for (s, p, o) in [
        ("a", "p", "b"),
        ("b", "p", "c"),
        ("c", "q", "d"),
        ("x", "p", "y"),
        ("y", "p", "z"),
        ("z", "q", "w"),
    ] {
        tx.insert(quad(s, p, o).as_ref());
    }
    for (graph, object) in [("g/a/1", "one"), ("g/b/1", "two")] {
        let quad = Quad::new(
            ex("a"),
            ex("r"),
            Literal::new_simple_literal(object),
            GraphName::NamedNode(ex(graph)),
        );
        tx.insert(quad.as_ref());
    }
    let s = tx.intern(ex("s").as_ref().into());
    for (subject, object) in [("a", "i1"), ("b", "i2")] {
        let subject = tx.intern(ex(subject).as_ref().into());
        let object = tx.intern(ex(object).as_ref().into());
        tx.insert_inferred(EncodedTriple::new(subject, s, object));
    }
    tx.commit().unwrap();
    engine
}

fn parse(text: &str) -> nrese_sparql_syntax::Query {
    SparqlParser::new()
        .parse_query(text)
        .unwrap_or_else(|e| panic!("{e}: {text}"))
}

fn with_cache(cache: &Arc<ResultCache>) -> QueryOptions {
    QueryOptions {
        result_cache: Some(Arc::clone(cache)),
        ..QueryOptions::default()
    }
}

fn select(view: &impl ReadView, text: &str, options: &QueryOptions) -> Vec<String> {
    rows_up_to_equal_values(
        evaluate_query(view, &parse(text), options).unwrap(),
        false,
        false,
    )
}

/// `text`'s answer on `view` as TSV, written directly.
fn tsv(view: &impl ReadView, text: &str, options: &QueryOptions) -> Vec<u8> {
    let mut out = Vec::new();
    nrese_sparql::write_results(
        view,
        &parse(text),
        options,
        nrese_sparql::ResultsFormat::Tsv,
        None,
        &mut out,
    )
    .expect("written directly")
    .unwrap();
    out
}

/// `text`'s answer on `view` as TSV the way the store writes it: from the cache's bytes, or
/// written and offered to it.
fn tsv_through_the_cache(view: &impl ReadView, text: &str, options: &QueryOptions) -> Vec<u8> {
    match cached_output(view, &parse(text), options, TSV, "") {
        CachedOutput::Hit(bytes) => bytes.to_vec(),
        CachedOutput::Miss(slot) => {
            let written = tsv(view, text, options);
            slot.offer(written.clone(), std::time::Duration::from_millis(1));
            written
        }
        CachedOutput::Off => tsv(view, text, options),
    }
}

/// Runs every query on `published` with the cache, rows and bytes, twice: what the cache
/// holds before unpublished state is read.
fn warm(published: &Snapshot, options: &QueryOptions) {
    for _ in 0..2 {
        for text in queries() {
            select(published, &text, options);
            tsv_through_the_cache(published, &text, options);
        }
    }
}

/// What a request can leave in the cache: its counters (entries, bytes, admissions,
/// evictions, misses, hits) and its pins.
fn cache_state(cache: &ResultCache) -> (ResultCacheStats, Vec<PinnedResult>) {
    (cache.stats(), cache.pins())
}

/// `view`, unpublished, is answered as without the cache, through every entry point, and the
/// shared cache is left as `before`: no part, prefix or output entry, no claim, no eviction,
/// no pin. Its answer's bytes go through its own version's cache (package 3.0c), twice, and
/// are the bytes written without one.
fn answered_and_kept_off_the_shared_cache(
    name: &str,
    view: &Snapshot,
    cache: &ResultCache,
    options: &QueryOptions,
    before: &(ResultCacheStats, Vec<PinnedResult>),
) {
    assert!(view.shared_state().is_none(), "{name}: unpublished");
    let plain = QueryOptions::default();
    for text in queries() {
        assert_eq!(
            select(view, &text, options),
            select(view, &text, &plain),
            "{name}: {text}"
        );
        assert_eq!(
            tsv(view, &text, options),
            tsv(view, &text, &plain),
            "{name}"
        );
        explain_query(view, &parse(&text), options).unwrap();
        for _ in 0..2 {
            assert_eq!(
                tsv_through_the_cache(view, &text, options),
                tsv(view, &text, &plain),
                "{name}: its bytes, through its own version's cache"
            );
        }
    }
    assert_eq!(
        &cache_state(cache),
        before,
        "{name}: the cache is as it was"
    );
}

fn asserted(snapshot: &Snapshot) -> Vec<EncodedQuad> {
    snapshot
        .quads_for_pattern_in(ReadModel::Asserted, &QuadPattern::all())
        .collect()
}

fn inferred(snapshot: &Snapshot) -> Vec<EncodedQuad> {
    snapshot
        .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
        .collect()
}

/// Item 1: pending state of every kind, with no private id, read through a detached
/// `Snapshot` (the explicit gap: `ReadView for Snapshot` answers itself).
///
/// - Input: the cache warmed on a published snapshot; writer pending snapshots
///   (`pending_snapshot`, `pending_over`) after inserting and deleting statements of existing
///   ids, with no change, and after inserting then deleting one statement; a speculative
///   transaction's; a private transaction's (both builders).
/// - Expected: each answers as without the cache through every entry point and leaves the
///   shared cache exactly as it was; the published snapshot is answered from it afterwards.
#[test]
fn unpublished_snapshots_are_answered_and_leave_the_cache_as_it_was() {
    let engine = engine();
    let cache = Arc::new(ResultCache::new(1 << 20).admitting_all());
    let options = with_cache(&cache);
    let published = engine.snapshot();
    warm(&published, &options);
    let before = cache_state(&cache);

    let existing = asserted(&published);
    let first = existing[0];
    let moved = EncodedQuad::new(
        first.subject,
        first.predicate,
        existing[1].object,
        first.graph,
    );
    let view = published.with_inferred_subset(InferredSubset::Without(Vec::new()));
    let mut unpublished = Vec::new();
    {
        let mut tx = engine.transaction();
        assert!(tx.insert_encoded(moved), "existing ids only");
        assert!(tx.remove_encoded(existing[1]));
        unpublished.push(("writer, changed", tx.pending_snapshot()));
        unpublished.push(("writer, changed, over a view", tx.pending_over(&view, &[])));
    }
    {
        let tx = engine.transaction();
        unpublished.push(("writer, no change", tx.pending_snapshot()));
    }
    {
        let mut tx = engine.transaction();
        assert!(tx.insert_encoded(moved));
        assert!(tx.remove_encoded(moved));
        unpublished.push(("writer, INSERT then DELETE", tx.pending_snapshot()));
    }
    {
        let mut tx = engine.speculative();
        tx.insert(quad("c", "p", "x").as_ref());
        unpublished.push(("speculative", tx.pending_snapshot()));
    }
    {
        let budget = Arc::new(nrese_exec::Budget::unlimited());
        let mut tx = engine.speculative_over(&published, budget).unwrap();
        assert!(tx.try_insert(quad("w", "p", "new").as_ref()).unwrap());
        unpublished.push(("private", tx.pending_snapshot().unwrap()));
        unpublished.push(("private, over a view", tx.pending_over(&view, &[]).unwrap()));
    }
    for (name, snapshot) in &unpublished {
        answered_and_kept_off_the_shared_cache(name, snapshot, &cache, &options, &before);
    }

    let hits = cache.stats().hits;
    for text in queries() {
        assert_eq!(
            select(&published, &text, &options),
            select(&published, &text, &QueryOptions::default())
        );
    }
    assert!(cache.stats().hits > hits, "the published reads hit");
}

/// Item 2: no laundering through views; a published mask admits under its own identity.
///
/// - Input: a writer's pending snapshot through stored, equality, canonical, read-budget,
///   subset and `Only` views and a chain of them; then, from the published snapshot, a mask
///   hiding one inferred statement, warmed before and after its base.
/// - Expected: the unpublished views leave the cache as it was; the mask and its base each
///   get their own answer (one inferred statement against two) in both orders, and each is
///   admitted.
#[test]
fn views_admit_by_their_own_publication_under_their_own_identity() {
    let engine = engine();
    let cache = Arc::new(ResultCache::new(1 << 20).admitting_all());
    let options = with_cache(&cache);
    let published = engine.snapshot();
    warm(&published, &options);
    let before = cache_state(&cache);
    let mut tx = engine.transaction();
    tx.insert(quad("d", "p", "a").as_ref());
    let pending = tx.pending_snapshot();
    drop(tx);
    let budget = Arc::new(nrese_exec::Budget::unlimited());
    let views = [
        ("stored", pending.stored()),
        ("equality", pending.with_equality(None)),
        ("canonical", pending.with_canonical_equality()),
        ("read budget", pending.reading_within(budget).unwrap()),
        (
            "subset",
            pending.with_inferred_subset(InferredSubset::Without(Vec::new())),
        ),
        (
            "only",
            pending.with_inferred_subset(InferredSubset::Only(inferred(&pending))),
        ),
        (
            "chain",
            pending
                .stored()
                .with_canonical_equality()
                .with_inferred_subset(InferredSubset::Without(inferred(&pending)[..1].to_vec())),
        ),
    ];
    for (name, view) in &views {
        answered_and_kept_off_the_shared_cache(name, view, &cache, &options, &before);
    }

    let text = &queries()[2];
    for mask_first in [true, false] {
        let cache = Arc::new(ResultCache::new(1 << 20).admitting_all());
        let options = with_cache(&cache);
        let mask = published
            .with_inferred_subset(InferredSubset::Without(inferred(&published)[..1].to_vec()));
        assert!(
            mask.shared_state().is_some(),
            "a restriction of published state"
        );
        let order: [(&Snapshot, usize); 2] = match mask_first {
            true => [(&mask, 1), (&published, 2)],
            false => [(&published, 2), (&mask, 1)],
        };
        for _ in 0..2 {
            for (view, rows) in order {
                let stored = cache.stats().stored;
                let answer = select(view, text, &options);
                assert_eq!(answer.len(), rows, "mask first: {mask_first}");
                assert_eq!(answer, select(view, text, &QueryOptions::default()));
                assert_eq!(
                    tsv_through_the_cache(view, text, &options),
                    tsv(view, text, &QueryOptions::default())
                );
                assert!(cache.stats().stored >= stored);
            }
        }
        assert!(
            cache.stats().stored >= 2,
            "both admitted: {:?}",
            cache.stats()
        );
        assert!(cache.stats().hits > 0);
    }
}

/// Item 3: two readers' access over one snapshot, warmed in both orders.
///
/// - Input: one published snapshot; a reader who may read the graphs under `g/a/` and an
///   unrestricted one; the named-graph query and the join, twice each, restricted first and
///   unrestricted first.
/// - Expected: every answer, rows and bytes, is the reader's own as without the cache (`one`
///   alone for the restricted reader; `one` and `two` for the other).
#[test]
fn two_readers_access_over_one_snapshot_keep_their_own_answers_in_both_orders() {
    let engine = engine();
    let snapshot = engine.snapshot();
    let restricted = |cache: Option<&Arc<ResultCache>>| QueryOptions {
        access: Some(Arc::new(GraphAccess {
            prefixes: vec![format!("{EX}g/a/")],
            default_graph: true,
            ..GraphAccess::default()
        })),
        result_cache: cache.map(Arc::clone),
        ..QueryOptions::default()
    };
    let everyone = |cache: Option<&Arc<ResultCache>>| QueryOptions {
        result_cache: cache.map(Arc::clone),
        ..QueryOptions::default()
    };
    let named = format!("SELECT ?g ?o WHERE {{ GRAPH ?g {{ ?x <{EX}r> ?o }} }}");
    let texts = [named.clone(), queries()[0].clone()];
    assert_eq!(select(&snapshot, &named, &restricted(None)).len(), 1);
    assert_eq!(select(&snapshot, &named, &everyone(None)).len(), 2);
    for restricted_first in [true, false] {
        let cache = Arc::new(ResultCache::new(1 << 20).admitting_all());
        let readers = match restricted_first {
            true => [restricted(Some(&cache)), everyone(Some(&cache))],
            false => [everyone(Some(&cache)), restricted(Some(&cache))],
        };
        let plain = match restricted_first {
            true => [restricted(None), everyone(None)],
            false => [everyone(None), restricted(None)],
        };
        for _ in 0..2 {
            for (options, plain) in readers.iter().zip(&plain) {
                for text in &texts {
                    assert_eq!(
                        select(&snapshot, text, options),
                        select(&snapshot, text, plain),
                        "restricted first: {restricted_first}: {text}"
                    );
                    assert_eq!(
                        tsv_through_the_cache(&snapshot, text, options),
                        tsv(&snapshot, text, plain),
                        "restricted first: {restricted_first}: {text}"
                    );
                }
            }
        }
        assert!(cache.stats().hits > 0, "{:?}", cache.stats());
    }
}

/// Item 7: an output slot taken before a commit and offered after it.
///
/// - Input: a slot for the join's bytes on revision 1; a commit changing the answer; the
///   newer snapshot's bytes written and offered; then the old slot's.
/// - Expected: the newer snapshot is answered with its own bytes; the older one is never
///   answered from the newer's, nor its late offer kept for the newer.
#[test]
fn an_output_slot_offered_after_a_commit_answers_no_other_revision() {
    let engine = engine();
    let cache = Arc::new(ResultCache::new(1 << 20).admitting_all());
    let options = with_cache(&cache);
    let text = &queries()[0];
    let old = engine.snapshot();
    let CachedOutput::Miss(old_slot) = cached_output(&old, &parse(text), &options, TSV, "") else {
        panic!("nothing cached yet");
    };
    let old_bytes = tsv(&old, text, &QueryOptions::default());
    let mut tx = engine.transaction();
    tx.insert(quad("c", "q", "e").as_ref());
    tx.commit().unwrap();
    let new = engine.snapshot();
    let new_bytes = tsv(&new, text, &QueryOptions::default());
    assert_ne!(old_bytes, new_bytes);
    assert_eq!(tsv_through_the_cache(&new, text, &options), new_bytes);
    old_slot.offer(old_bytes.clone(), std::time::Duration::from_millis(1));
    assert_eq!(tsv_through_the_cache(&new, text, &options), new_bytes);
    assert!(matches!(
        cached_output(&old, &parse(text), &options, TSV, ""),
        CachedOutput::Miss(_)
    ));
}

/// Item 8: two independent engines with equal revisions and ids share one cache.
///
/// - Input: two engines whose statements have the same shape and ids but different terms;
///   the join and the group on each, twice, through one cache.
/// - Expected: each engine's answers, rows and bytes, are its own.
#[test]
fn independent_engines_sharing_a_cache_keep_their_own_answers() {
    let engine_of = |object: &str| {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        tx.insert(quad("a", "p", "b").as_ref());
        tx.insert(quad("b", "p", "c").as_ref());
        tx.insert(quad("c", "q", object).as_ref());
        tx.commit().unwrap();
        engine
    };
    let (one, two) = (engine_of("one"), engine_of("two"));
    let (first, second) = (one.snapshot(), two.snapshot());
    assert_eq!(first.revision(), second.revision());
    let cache = Arc::new(ResultCache::new(1 << 20).admitting_all());
    let options = with_cache(&cache);
    for _ in 0..2 {
        for snapshot in [&first, &second] {
            for text in &queries()[..2] {
                assert_eq!(
                    select(snapshot, text, &options),
                    select(snapshot, text, &QueryOptions::default())
                );
                assert_eq!(
                    tsv_through_the_cache(snapshot, text, &options),
                    tsv(snapshot, text, &QueryOptions::default())
                );
            }
        }
    }
    assert!(cache.stats().hits > 0);
}

/// Item 9: a pin asked for on unpublished state.
///
/// - Input: the join pinned as `n` on the published snapshot; then the group, pinned as `n`,
///   on a writer's pending snapshot.
/// - Expected: the pending request fails as it does on a transaction (the cache is off for it)
///   and `n` still pins the join, held at the published revision.
#[test]
fn a_pin_on_unpublished_state_fails_and_leaves_the_pins_as_they_were() {
    let engine = engine();
    let cache = Arc::new(ResultCache::new(1 << 20).admitting_all());
    let [join, group, _] = queries();
    let pinning = |query: &str| QueryOptions {
        pin: Some(PinRequest {
            name: "n".into(),
            query: query.to_owned(),
        }),
        ..with_cache(&cache)
    };
    let published = engine.snapshot();
    select(&published, &join, &pinning(&join));
    let pins = cache.pins();
    assert_eq!(pins.len(), 1);
    assert_eq!((pins[0].query.as_str(), pins[0].rows), (join.as_str(), 2));
    let mut tx = engine.transaction();
    tx.insert(quad("d", "p", "a").as_ref());
    let pending = tx.pending_snapshot();
    drop(tx);
    let refused = evaluate_query(&pending, &parse(&group), &pinning(&group)).err();
    assert!(
        matches!(&refused, Some(QueryEvaluationError::Argument(message))
            if message.contains("nothing can be pinned")),
        "{refused:?}"
    );
    assert_eq!(cache.pins(), pins, "the published pin stays");
}

/// Sol-loop round 1, S1: canonical answers read a view of the snapshot with an identity of
/// its own; their bytes are never another reading's.
///
/// - Input: `Alias owl:sameAs Rep` inferred, the store reading equality classes, and
///   `Rep p o`; `?x p o` with early expansion, answered canonically (`Rep` alone) and expanded
///   (`Rep` and `Alias`), through one cache, canonical first and expanded first, twice each.
/// - Expected: every answer's bytes are its own reading's, as written without a cache, and
///   the two readings' differ.
#[test]
fn canonical_and_expanded_answers_keep_their_own_bytes_in_both_orders() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let rep = tx.intern(ex("Rep").as_ref().into());
    let alias = tx.intern(ex("Alias").as_ref().into());
    let same_as = nrese_rdf::NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs");
    let same_as = tx.intern(same_as.as_ref().into());
    tx.insert(quad("Rep", "p", "o").as_ref());
    tx.insert_inferred(EncodedTriple::new(alias, same_as, rep));
    tx.commit().unwrap();
    engine.set_equality(Some(same_as));
    let snapshot = engine.snapshot();
    let text = format!("SELECT ?x WHERE {{ ?x <{EX}p> <{EX}o> }}");
    let reading = |canonical: bool, cache: Option<&Arc<ResultCache>>| QueryOptions {
        equality_early_expansion: true,
        equality_canonical: canonical,
        result_cache: cache.map(Arc::clone),
        ..QueryOptions::default()
    };
    let (canonical, expanded) = (
        tsv(&snapshot, &text, &reading(true, None)),
        tsv(&snapshot, &text, &reading(false, None)),
    );
    assert_ne!(canonical, expanded, "the expansion answers Alias too");
    for canonical_first in [true, false] {
        let cache = Arc::new(ResultCache::new(1 << 20).admitting_all());
        let order = match canonical_first {
            true => [(true, &canonical), (false, &expanded)],
            false => [(false, &expanded), (true, &canonical)],
        };
        for _ in 0..2 {
            for (mode, expected) in order {
                assert_eq!(
                    &tsv_through_the_cache(&snapshot, &text, &reading(mode, Some(&cache))),
                    expected,
                    "canonical first: {canonical_first}, canonical: {mode}"
                );
            }
        }
        assert!(cache.stats().hits > 0, "{:?}", cache.stats());
    }
}

/// Sol-loop round 2, S2: a canonical answer whose view the budgets refuse is never another
/// reading's cached bytes.
///
/// - Input: `Alias owl:sameAs Rep` inferred, the store reading equality classes, `Rep p o`
///   and a statement in a graph naming `Alias`; the store's canonical-view budget and the
///   request's envelope 1 byte; `?x p o` answered expanded and offered to the cache, then
///   asked canonically.
/// - Expected: the canonical request finds no cached bytes (canonical answers there fail
///   with their memory limit; they don't read the expanded answer).
#[test]
fn a_refused_canonical_view_is_never_answered_from_the_expanded_bytes() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let rep = tx.intern(ex("Rep").as_ref().into());
    let alias = tx.intern(ex("Alias").as_ref().into());
    let same_as = nrese_rdf::NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs");
    let same_as = tx.intern(same_as.as_ref().into());
    tx.insert(quad("Rep", "p", "o").as_ref());
    let named = Quad::new(
        ex("Alias"),
        ex("in"),
        ex("z"),
        GraphName::NamedNode(ex("g")),
    );
    tx.insert(named.as_ref());
    tx.insert_inferred(EncodedTriple::new(alias, same_as, rep));
    tx.commit().unwrap();
    engine.set_equality(Some(same_as));
    engine.set_canonical_view_budget(Arc::new(nrese_exec::Budget::new(1)));
    let snapshot = engine.snapshot();
    let text = format!("SELECT ?x WHERE {{ ?x <{EX}p> <{EX}o> }}");
    let cache = Arc::new(ResultCache::new(1 << 20).admitting_all());
    let expanded = QueryOptions {
        equality_early_expansion: true,
        result_cache: Some(Arc::clone(&cache)),
        ..QueryOptions::default()
    };
    tsv_through_the_cache(&snapshot, &text, &expanded);
    assert!(matches!(
        cached_output(&snapshot, &parse(&text), &expanded, TSV, ""),
        CachedOutput::Hit(_)
    ));
    let canonical = QueryOptions {
        equality_canonical: true,
        resources: nrese_sparql::Resources {
            memory: nrese_exec::Envelope::new(Some(1), None),
            ..nrese_sparql::Resources::default()
        },
        ..expanded
    };
    assert!(
        !matches!(
            cached_output(&snapshot, &parse(&text), &canonical, TSV, ""),
            CachedOutput::Hit(_)
        ),
        "a canonical request read the expanded answer's bytes"
    );
}
