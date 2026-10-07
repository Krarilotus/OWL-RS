//! The result cache (`nrese_sparql::cache`): answers with the cache are the answers
//! without it, on random queries over random commits; parts are shared across queries,
//! renamed into each; transactions' pending states and older snapshots are never answered
//! from a newer one's parts; EXPLAIN shows the parts that came from the cache.

use std::sync::Arc;

use nrese_engine::{EncodedTriple, Engine, EngineConfig, ReadModel};
use nrese_rdf::{GraphName, Quad};
use nrese_sparql::{QueryOptions, QueryResults, ResultCache, evaluate_query, runs_natively};
use nrese_sparql_syntax::SparqlParser;

use crate::native_differential_tests::{
    EX, Rng, computed_query, ex, has_extremes, limited, random_dataset, random_query,
    rows_up_to_equal_values, set_query,
};

fn parse(text: &str) -> nrese_sparql_syntax::Query {
    SparqlParser::new()
        .parse_query(text)
        .unwrap_or_else(|e| panic!("{e}: {text}"))
}

fn with_cache(cache: &Arc<ResultCache>, model: ReadModel) -> QueryOptions {
    QueryOptions {
        read_model: model,
        result_cache: Some(Arc::clone(cache)),
        ..QueryOptions::default()
    }
}

fn rows_of(results: QueryResults<'_>, ordered: bool, text: &str) -> Vec<String> {
    rows_up_to_equal_values(results, ordered, has_extremes(text))
}

/// A commit of random changes: statements added (a third of the default graph's
/// inferred), statements removed.
fn random_commit(engine: &Engine, rng: &mut Rng) {
    let mut tx = engine.transaction();
    for quad in random_dataset(rng)
        .into_iter()
        .take(1 + rng.below(8) as usize)
    {
        if rng.below(3) == 0 {
            let triple = EncodedTriple::new(
                tx.intern(quad.subject.as_ref().into()),
                tx.intern(quad.predicate.as_ref().into()),
                tx.intern(quad.object.as_ref()),
            );
            tx.insert_inferred(triple);
        } else {
            tx.insert(quad.as_ref());
        }
    }
    let snapshot = engine.snapshot();
    let every = nrese_engine::QuadPattern {
        subject: None,
        predicate: None,
        object: None,
        graph: nrese_engine::GraphSelector::Any,
    };
    let present: Vec<Quad> = snapshot
        .quads_for_pattern_in(ReadModel::Asserted, &every)
        .filter_map(|quad| snapshot.decode_quad(quad))
        .collect();
    for _ in 0..rng.below(4) {
        if !present.is_empty() {
            let quad = &present[rng.below(present.len() as u64) as usize];
            tx.remove(quad.as_ref());
        }
    }
    tx.commit().unwrap();
}

#[test]
fn answers_with_the_cache_equal_answers_without_it_over_random_commits() {
    let mut rng = Rng::seeded(20_261_006);
    let (mut checked, mut with_solutions) = (0, 0);
    let mut hits = 0;
    for dataset_case in 0..20 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in random_dataset(&mut rng) {
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        // A small budget, so that parts are evicted as well as hit.
        let cache = Arc::new(ResultCache::new(256 << 10).admitting_all());
        // A pool of queries, drawn from with repeats: the same queries, and queries
        // sharing groups (the generators reuse their shapes), meet the cache again.
        let pool: Vec<(String, bool)> = (0..24)
            .map(|i| match i % 3 {
                0 => random_query(&mut rng),
                1 => computed_query(&mut rng),
                _ => set_query(&mut rng),
            })
            .filter(|(text, order_dependent)| {
                // A set query whose answer depends on its rows' order (GROUP_CONCAT,
                // SAMPLE) may differ between evaluations, cached or not.
                !(*order_dependent && (text.contains("GROUP_CONCAT") || text.contains("SAMPLE")))
            })
            .collect();
        for round in 0..6 {
            if round > 0 {
                random_commit(&engine, &mut rng);
            }
            let snapshot = engine.snapshot();
            for query_case in 0..30 {
                let (text, ordered) = &pool[rng.below(pool.len() as u64) as usize];
                let query = parse(text);
                if !runs_natively(&query) {
                    continue;
                }
                let model = *rng.pick(&[
                    ReadModel::Materialised,
                    ReadModel::Asserted,
                    ReadModel::Inferred,
                ]);
                let without = QueryOptions {
                    read_model: model,
                    ..QueryOptions::default()
                };
                let before = cache.stats().hits;
                let cached = rows_of(
                    evaluate_query(&snapshot, &query, &with_cache(&cache, model)).unwrap(),
                    *ordered,
                    text,
                );
                hits += cache.stats().hits - before;
                let context = format!(
                    "dataset {dataset_case}, round {round}, query {query_case}, {model:?}: {text}"
                );
                if let Some(limit) = limited(text) {
                    // Any `limit` solutions will do: as many, each a solution.
                    let unlimited = parse(&text[..text.rfind(" LIMIT ").unwrap()]);
                    let mut all = rows_of(
                        evaluate_query(&snapshot, &unlimited, &without).unwrap(),
                        false,
                        text,
                    );
                    assert_eq!(cached.len(), all.len().min(limit), "{context}");
                    for row in &cached {
                        let at = all.iter().position(|r| r == row);
                        assert!(at.is_some(), "{row} is no solution: {context}");
                        all.remove(at.unwrap());
                    }
                } else {
                    let plain = rows_of(
                        evaluate_query(&snapshot, &query, &without).unwrap(),
                        *ordered,
                        text,
                    );
                    assert_eq!(cached, plain, "{context}");
                    with_solutions += usize::from(!plain.is_empty());
                }
                checked += 1;
            }
        }
        let stats = cache.stats();
        assert!(stats.bytes <= stats.capacity, "{stats:?}");
    }
    assert!(checked > 2_000, "{checked} queries checked");
    assert!(
        with_solutions * 4 > checked,
        "{with_solutions} of {checked}"
    );
    // Not vacuous: many queries meet parts computed before (775 hits over 3,600 queries
    // on 6 October; single patterns are scans, not parts).
    assert!(
        hits * 6 > checked as u64,
        "{hits} hits over {checked} queries"
    );
}

fn store_of(quads: &[(&str, &str, &str)]) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for (s, p, o) in quads {
        tx.insert(Quad::new(ex(s), ex(p), ex(o), GraphName::DefaultGraph).as_ref());
    }
    tx.commit().unwrap();
    engine
}

fn select(
    snapshot: &impl nrese_sparql::ReadView,
    text: &str,
    options: &QueryOptions,
) -> Vec<String> {
    rows_of(
        evaluate_query(snapshot, &parse(text), options).unwrap(),
        false,
        text,
    )
}

#[test]
fn computed_values_of_a_cached_part_are_this_querys_own() {
    let engine = store_of(&[("a", "p", "b"), ("c", "p", "d")]);
    let snapshot = engine.snapshot();
    let cache = Arc::new(ResultCache::new(1 << 20).admitting_all());
    let options = with_cache(&cache, ReadModel::Materialised);
    // The BIND's values are terms the store doesn't know: ids of the query's own.
    let part = format!("{{ ?s <{EX}p> ?o BIND(CONCAT(STR(?o), \"!\") AS ?v) }}");
    let first = format!("SELECT ?s ?v WHERE {part}");
    // Another query computes values of its own first, so its ids differ.
    let second = format!(
        "SELECT ?s ?v ?w WHERE {{ BIND(\"other\" AS ?w) {{ SELECT ?s ?v WHERE {part} }} }}"
    );
    let expected = select(&snapshot, &second, &QueryOptions::default());
    select(&snapshot, &first, &options);
    let hits = cache.stats().hits;
    assert_eq!(select(&snapshot, &second, &options), expected);
    assert!(cache.stats().hits > hits);
    assert!(
        expected.iter().any(|row| row.contains("b!")),
        "{expected:?}"
    );
}

#[test]
fn a_commit_or_a_pending_change_is_never_answered_from_older_parts() {
    let engine = store_of(&[("a", "p", "b")]);
    let cache = Arc::new(ResultCache::new(1 << 20).admitting_all());
    let options = with_cache(&cache, ReadModel::Materialised);
    let text = format!("SELECT ?s ?o WHERE {{ ?s <{EX}p> ?o }}");
    let old = engine.snapshot();
    assert_eq!(select(&old, &text, &options).len(), 1);
    // A transaction's pending state keeps its base's revision: it must not be taken for it.
    let mut tx = engine.transaction();
    tx.insert(Quad::new(ex("c"), ex("p"), ex("d"), GraphName::DefaultGraph).as_ref());
    assert_eq!(select(&tx, &text, &options).len(), 2);
    tx.commit().unwrap();
    let new = engine.snapshot();
    assert_eq!(select(&new, &text, &options).len(), 2);
    // The older snapshot, still held, keeps its own answer.
    assert_eq!(select(&old, &text, &options).len(), 1);
    // And the newer one is answered from the cache now.
    let hits = cache.stats().hits;
    assert_eq!(select(&new, &text, &options).len(), 2);
    assert!(cache.stats().hits > hits);
}

#[test]
fn volatile_parts_are_computed_every_time() {
    let engine = store_of(&[("a", "p", "b"), ("b", "q", "c")]);
    let snapshot = engine.snapshot();
    let cache = Arc::new(ResultCache::new(1 << 20).admitting_all());
    let options = with_cache(&cache, ReadModel::Materialised);
    let text = format!("SELECT ?s ?r WHERE {{ ?s <{EX}p> ?o . ?o <{EX}q> ?x BIND(RAND() AS ?r) }}");
    let first = select(&snapshot, &text, &options);
    let second = select(&snapshot, &text, &options);
    assert_ne!(first, second, "RAND() gives a new value per query");
    // The join below the BIND is cached and hit; the BIND is not.
    assert!(cache.stats().hits >= 1, "{:?}", cache.stats());
}

/// `query`'s answer on `view` written as TSV, as the store writes it.
fn tsv(
    view: &impl nrese_sparql::ReadView,
    query: &nrese_sparql_syntax::Query,
    options: &QueryOptions,
) -> Vec<u8> {
    let mut out = Vec::new();
    nrese_sparql::write_results(
        view,
        query,
        options,
        nrese_sparql::ResultsFormat::Tsv,
        None,
        &mut out,
    )
    .expect("written directly")
    .unwrap();
    out
}

const TSV: &str = "text/tab-separated-values";

#[test]
fn serialised_answers_are_kept_per_format_form_and_snapshot() {
    use nrese_sparql::{CachedOutput, cached_output};
    let engine = store_of(&[("a", "p", "b"), ("b", "q", "c")]);
    let cache = Arc::new(ResultCache::new(1 << 20).admitting_all());
    let options = with_cache(&cache, ReadModel::Materialised);
    let select = parse(&format!("SELECT ?s ?o WHERE {{ ?s <{EX}p> ?o }}"));
    let snapshot = engine.snapshot();
    let CachedOutput::Miss(slot) = cached_output(&snapshot, &select, &options, TSV, "") else {
        panic!("nothing cached yet")
    };
    let written = tsv(&snapshot, &select, &options);
    slot.offer(written.clone(), std::time::Duration::from_millis(5));
    let CachedOutput::Hit(bytes) = cached_output(&snapshot, &select, &options, TSV, "") else {
        panic!("the bytes just offered")
    };
    assert_eq!(&*bytes, written.as_slice());
    // Another format, another form or template over the same pattern: not these bytes.
    let json = "application/sparql-results+json";
    assert!(matches!(
        cached_output(&snapshot, &select, &options, json, ""),
        CachedOutput::Miss(_)
    ));
    for other in [
        format!("ASK {{ ?s <{EX}p> ?o }}"),
        format!("CONSTRUCT {{ ?s <{EX}r> ?o }} WHERE {{ ?s <{EX}p> ?o }}"),
        format!("CONSTRUCT {{ ?o <{EX}r> ?s }} WHERE {{ ?s <{EX}p> ?o }}"),
    ] {
        let other = parse(&other);
        let CachedOutput::Miss(slot) = cached_output(&snapshot, &other, &options, TSV, "") else {
            panic!("another query")
        };
        slot.offer(b"other".to_vec(), std::time::Duration::from_millis(5));
    }
    let CachedOutput::Hit(bytes) = cached_output(&snapshot, &select, &options, TSV, "") else {
        panic!("still cached")
    };
    assert_eq!(&*bytes, written.as_slice());
    // Another caller context (the store's reasoning mode, the status it reports), or the
    // QL rewriting on: not these bytes.
    assert!(matches!(
        cached_output(&snapshot, &select, &options, TSV, "owl2-dl"),
        CachedOutput::Miss(_)
    ));
    let rewriting = QueryOptions {
        ql: Some(Arc::new(nrese_sparql::ql::QlRewriting::new(
            nrese_sparql::ql::Closure::default(),
        ))),
        ..options.clone()
    };
    assert!(matches!(
        cached_output(&snapshot, &select, &rewriting, TSV, ""),
        CachedOutput::Miss(_)
    ));
    // Never answered from bytes: a volatile query, a pin, a transaction's pending state.
    let volatile = parse(&format!(
        "SELECT ?s ?r WHERE {{ ?s <{EX}p> ?o BIND(RAND() AS ?r) }}"
    ));
    assert!(matches!(
        cached_output(&snapshot, &volatile, &options, TSV, ""),
        CachedOutput::Off
    ));
    let pinning = QueryOptions {
        pin: Some(nrese_sparql::PinRequest {
            name: "p".into(),
            query: String::new(),
        }),
        ..options.clone()
    };
    assert!(matches!(
        cached_output(&snapshot, &select, &pinning, TSV, ""),
        CachedOutput::Off
    ));
    let mut tx = engine.transaction();
    tx.insert(Quad::new(ex("c"), ex("p"), ex("d"), GraphName::DefaultGraph).as_ref());
    assert!(matches!(
        cached_output(&tx, &select, &options, TSV, ""),
        CachedOutput::Off
    ));
    tx.commit().unwrap();
    // A commit: the older bytes don't answer the newer snapshot.
    assert!(matches!(
        cached_output(&engine.snapshot(), &select, &options, TSV, ""),
        CachedOutput::Miss(_)
    ));
}

#[test]
fn serialised_answers_equal_fresh_ones_over_random_commits() {
    use nrese_sparql::{CachedOutput, cached_output};
    let mut rng = Rng::seeded(20_261_007);
    let (mut checked, mut from_bytes) = (0, 0);
    for _ in 0..12 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in random_dataset(&mut rng) {
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let cache = Arc::new(ResultCache::new(256 << 10).admitting_all());
        let pool: Vec<(String, bool)> = (0..16)
            .map(|i| match i % 2 {
                0 => random_query(&mut rng),
                _ => computed_query(&mut rng),
            })
            .filter(|(text, _)| limited(text).is_none())
            .collect();
        for round in 0..5 {
            if round > 0 {
                random_commit(&engine, &mut rng);
            }
            let snapshot = engine.snapshot();
            for _ in 0..20 {
                let (text, ordered) = &pool[rng.below(pool.len() as u64) as usize];
                let query = parse(text);
                if !runs_natively(&query) {
                    continue;
                }
                let model = *rng.pick(&[ReadModel::Materialised, ReadModel::Asserted]);
                let options = with_cache(&cache, model);
                let fresh = tsv(
                    &snapshot,
                    &query,
                    &QueryOptions {
                        read_model: model,
                        ..QueryOptions::default()
                    },
                );
                let answer = match cached_output(&snapshot, &query, &options, TSV, "") {
                    CachedOutput::Hit(bytes) => {
                        from_bytes += 1;
                        bytes.to_vec()
                    }
                    CachedOutput::Miss(slot) => {
                        let written = tsv(&snapshot, &query, &options);
                        slot.offer(written.clone(), std::time::Duration::from_millis(1));
                        written
                    }
                    CachedOutput::Off => continue,
                };
                // Rows in any order unless ordered; equal values may be written alike.
                let lines = |bytes: &[u8]| {
                    let text = String::from_utf8(bytes.to_vec()).unwrap();
                    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
                    if !ordered {
                        lines[1..].sort();
                    }
                    lines
                };
                assert_eq!(
                    lines(&answer),
                    lines(&fresh),
                    "round {round}, {model:?}: {text}"
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 800, "{checked}");
    // 278 of 1,200 on 7 October: the pool's repeats on an unchanged snapshot and model.
    assert!(
        from_bytes * 6 > checked,
        "{from_bytes} of {checked} from bytes"
    );
}
