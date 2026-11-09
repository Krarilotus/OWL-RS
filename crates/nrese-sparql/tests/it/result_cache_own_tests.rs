//! An unpublished version's own result cache (the lifecycle contract's S3, "state owned by its
//! own version"; package 3.0c's slot; 3.6's design note §11, items O1–O3). A pending view a
//! session keeps is read again and again: its parts and its answer's bytes are kept in its
//! version, never in the store's cache, and every answer is the one an evaluation without a
//! cache gives. The oracle is the version's cache (`OwnState::get`), the store's cache's
//! counters, and answers compared with an evaluation without either.

use std::sync::Arc;

use nrese_engine::{Admission, Engine, EngineConfig, Snapshot, TermId};
use nrese_rdf::{GraphName, NamedNode, Quad};
use nrese_sparql::{
    CachedOutput, QueryOptions, ReadView, ResultCache, cached_output, evaluate_query,
};
use nrese_sparql_syntax::SparqlParser;

use crate::native_differential_tests::{EX, ex, rows_up_to_equal_values};

const TSV: &str = "text/tab-separated-values";
const SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";

fn parse(text: &str) -> nrese_sparql_syntax::Query {
    SparqlParser::new()
        .parse_query(text)
        .unwrap_or_else(|e| panic!("{e}: {text}"))
}

fn quad(s: &str, p: &str, o: &str) -> Quad {
    Quad::new(ex(s), ex(p), ex(o), GraphName::DefaultGraph)
}

/// Three joined patterns (their prefixes are offered) and a group (a claimed part).
fn queries() -> [String; 2] {
    [
        format!("SELECT ?x ?w WHERE {{ ?x <{EX}p> ?y . ?y <{EX}p> ?z . ?z <{EX}q> ?w }}"),
        format!("SELECT ?y (COUNT(*) AS ?n) WHERE {{ ?x <{EX}p> ?y }} GROUP BY ?y"),
    ]
}

/// Two chains `?x p ?y p ?z q ?w`, and `a` the same as `a2`.
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
    let same = Quad::new(
        ex("a"),
        NamedNode::new_unchecked(SAME_AS),
        ex("a2"),
        GraphName::DefaultGraph,
    );
    tx.insert(same.as_ref());
    tx.commit().unwrap();
    engine
}

/// A writer's pending state over `engine`: one more chain, which joins with the others.
fn pending(engine: &Engine) -> Snapshot {
    let mut tx = engine.transaction();
    tx.insert(quad("d", "p", "a").as_ref());
    tx.insert(quad("w", "q", "e").as_ref());
    tx.pending_snapshot()
}

fn select(view: &impl ReadView, text: &str, options: &QueryOptions) -> Vec<String> {
    rows_up_to_equal_values(
        evaluate_query(view, &parse(text), options).unwrap(),
        false,
        false,
    )
}

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
/// written and offered.
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

/// The result cache `view`'s version keeps, if it is unpublished and keeps one.
fn own_cache(view: &Snapshot) -> Option<Arc<ResultCache>> {
    match view.admission() {
        Admission::Own(own) => own.get::<ResultCache>(),
        Admission::Shared(_) => None,
    }
}

fn store_cache() -> (Arc<ResultCache>, QueryOptions) {
    let cache = Arc::new(ResultCache::new(1 << 20).admitting_all());
    let options = QueryOptions {
        result_cache: Some(Arc::clone(&cache)),
        ..QueryOptions::default()
    };
    (cache, options)
}

/// O1: a kept pending view read again and again.
///
/// - Input: the store's cache; one writer's pending snapshot, kept; the join and the group,
///   their rows and their bytes, three times.
/// - Expected: every answer is the one without a cache; the version keeps a cache that holds
///   parts and the answers' bytes and is hit on the repeats; the store's cache is untouched.
#[test]
fn a_kept_pending_view_is_answered_from_its_own_versions_cache() {
    let engine = engine();
    let (cache, options) = store_cache();
    let view = pending(&engine);
    let plain = QueryOptions::default();
    let before = cache.stats();
    for round in 0..3 {
        for text in queries() {
            assert_eq!(
                select(&view, &text, &options),
                select(&view, &text, &plain),
                "round {round}: {text}"
            );
            assert_eq!(
                tsv_through_the_cache(&view, &text, &options),
                tsv(&view, &text, &plain),
                "round {round}: {text}"
            );
        }
    }
    let own = own_cache(&view).expect("the pending version keeps a result cache");
    let stats = own.stats();
    assert!(
        stats.stored > 0 && stats.answers > 0,
        "parts and answers kept: {stats:?}"
    );
    assert!(stats.hits > 0, "the repeats hit: {stats:?}");
    assert_eq!(cache.stats(), before, "the store's cache is untouched");
}

/// O2: equality modes of one version (3.0c: every view of one version reaches its slot).
///
/// - Input: one pending version read with `owl:sameAs` expansion and read as stored, in both
///   orders, twice each; `a sameAs a2`, so the expanded answer has `a2`'s rows too.
/// - Expected: each view's answer is its own, as without a cache, and they differ; the
///   version's cache holds entries.
#[test]
fn equality_modes_of_one_version_keep_their_own_answers() {
    let engine = engine();
    let text = format!("SELECT ?x ?y WHERE {{ ?x <{EX}p> ?y }}");
    for expanded_first in [true, false] {
        let (_cache, options) = store_cache();
        let view = pending(&engine);
        let same_as: TermId = view
            .lookup(NamedNode::new_unchecked(SAME_AS).as_ref().into())
            .expect("owl:sameAs is stored");
        let expanded = view.with_equality(Some(same_as));
        let stored = view.stored();
        let plain = QueryOptions::default();
        let (expected_expanded, expected_stored) = (
            select(&expanded, &text, &plain),
            select(&stored, &text, &plain),
        );
        assert_ne!(
            expected_expanded, expected_stored,
            "the expansion shows a2's rows"
        );
        let order: [(&Snapshot, &Vec<String>); 2] = match expanded_first {
            true => [(&expanded, &expected_expanded), (&stored, &expected_stored)],
            false => [(&stored, &expected_stored), (&expanded, &expected_expanded)],
        };
        for _ in 0..2 {
            for (view, expected) in order {
                assert_eq!(
                    &select(view, &text, &options),
                    expected,
                    "expanded first: {expanded_first}"
                );
                let bytes = tsv_through_the_cache(view, &text, &options);
                assert_eq!(
                    bytes,
                    tsv(view, &text, &plain),
                    "expanded first: {expanded_first}"
                );
            }
        }
        let own = own_cache(&view).expect("the version keeps a result cache");
        assert!(own.stats().stored >= 2, "{:?}", own.stats());
    }
}

/// O3: two pending versions of one transaction keep caches of their own.
///
/// - Input: one transaction's pending state taken twice (each call makes a version); the join
///   on the first, then on the second.
/// - Expected: each is answered as without a cache; the second's version doesn't find the
///   first's entries (its first read hits nothing).
#[test]
fn two_pending_versions_keep_caches_of_their_own() {
    let engine = engine();
    let (_cache, options) = store_cache();
    let mut tx = engine.transaction();
    tx.insert(quad("d", "p", "a").as_ref());
    let (first, second) = (tx.pending_snapshot(), tx.pending_snapshot());
    let text = &queries()[0];
    let plain = QueryOptions::default();
    for _ in 0..2 {
        assert_eq!(select(&first, text, &options), select(&first, text, &plain));
    }
    assert_eq!(
        select(&second, text, &options),
        select(&second, text, &plain)
    );
    let (one, two) = (
        own_cache(&first).expect("the first keeps one"),
        own_cache(&second).expect("the second keeps one"),
    );
    assert!(!Arc::ptr_eq(&one, &two));
    assert!(one.stats().hits > 0, "the first's repeat hit");
    assert_eq!(
        two.stats().hits,
        0,
        "the second found nothing of the first's"
    );
}
