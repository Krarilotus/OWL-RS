//! The hand-over of the planner's reads ([`reads`]): what a BGP's planning reads for its
//! overlap counts serves the steps that read the same data, as a scan's table, as the check
//! in place of a probe, or (the intersection buffer) as the first two steps' result; never
//! read and paid again. Its answers are the steps' own: each case is compared with the
//! hand-over switched off ([`HANDOVER`]), where planning drops its reads as before.

use super::*;
use nrese_engine::{EncodedTriple, Engine, EngineConfig};
use nrese_exec::Envelope;
use nrese_rdf::{GraphName, NamedNode, Quad};
use nrese_sparql_syntax::SparqlParser;

/// A triple of `<http://e/name>`s.
type Triple = (String, &'static str, String);

fn e(name: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("http://e/{name}"))
}

/// The BGP the cases share: `a p ?x` and `b q ?x` (a counted pair on `?x`, the smallest
/// patterns), and `?x r ?y`.
const CORE: &str =
    "<http://e/a> <http://e/p> ?x . <http://e/b> <http://e/q> ?x . ?x <http://e/r> ?y";

/// `a p oI` for I below `n`, `b q oI` for I from `n / 2` to `3n / 2` (the pair shares
/// `n / 2` values), `c p wI` for I below `n` (none shared with `b q ?x`), `oI r yI` for I
/// below `2n`, and `k t oI` for every tenth I below `n`; in the default graph and, with
/// `named`, in `<http://e/g>` too. Written in `commits` commits, each kept a run of the
/// index (a policy that forms no window), `extra` among them; with `alias`, the last also
/// infers `alias owl:sameAs o60`, with equality on.
fn store(n: usize, commits: usize, named: bool, alias: bool, extra: &[Triple]) -> Engine {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        compaction: nrese_engine::CompactionPolicy {
            fanout: 0,
            ..nrese_engine::CompactionPolicy::default()
        },
        ..EngineConfig::default()
    })
    .unwrap();
    let mut triples: Vec<Triple> = extra.to_vec();
    for i in 0..n {
        triples.push(("a".into(), "p", format!("o{i}")));
        triples.push(("b".into(), "q", format!("o{}", n / 2 + i)));
        triples.push(("c".into(), "p", format!("w{i}")));
        if i % 10 == 0 {
            triples.push(("k".into(), "t", format!("o{i}")));
        }
    }
    for i in 0..2 * n {
        triples.push((format!("o{i}"), "r", format!("y{i}")));
    }
    for c in 0..commits {
        let mut tx = engine.transaction();
        for (s, p, o) in triples.iter().skip(c).step_by(commits) {
            let graphs: &[GraphName] = if named {
                &[GraphName::DefaultGraph, GraphName::NamedNode(e("g"))]
            } else {
                &[GraphName::DefaultGraph]
            };
            for graph in graphs {
                tx.insert(Quad::new(e(s), e(p), e(o), graph.clone()).as_ref());
            }
        }
        let same_as = (alias && c + 1 == commits).then(|| {
            let same_as = tx.intern(e("sameAs").as_ref().into());
            let subject = tx.intern(e("alias").as_ref().into());
            let object = tx.intern(e("o60").as_ref().into());
            tx.insert_inferred(EncodedTriple {
                subject,
                predicate: same_as,
                object,
            });
            same_as
        });
        tx.commit().unwrap();
        if same_as.is_some() {
            engine.set_equality(same_as);
        }
    }
    engine
}

/// `query`'s answers on `snapshot` with `options`, sorted, with the hand-over on or off.
fn answers(
    snapshot: &Snapshot,
    query: &str,
    options: &QueryOptions,
    handover: bool,
) -> Vec<String> {
    let query = SparqlParser::new().parse_query(query).unwrap();
    HANDOVER.set(handover);
    let results = crate::evaluate_query(snapshot, &query, options);
    HANDOVER.set(true);
    let mut rows: Vec<String> = match results.unwrap() {
        QueryResults::Solutions(solutions) => {
            solutions.map(|s| format!("{:?}", s.unwrap())).collect()
        }
        QueryResults::Boolean(value) => vec![value.to_string()],
        QueryResults::Graph(_) => unreachable!("a SELECT or ASK"),
    };
    rows.sort();
    rows
}

/// The lists planning read and the column scans the steps made while `run` ran.
fn reads_of(run: impl FnOnce()) -> (usize, usize) {
    let (lists, scans) = (LIST_READS.get(), SCAN_READS.get());
    run();
    (LIST_READS.get() - lists, SCAN_READS.get() - scans)
}

/// A case's rows with the hand-over and without.
type Compared = (Vec<String>, Vec<String>);

/// `CORE`'s pattern, for [`Context::eval`].
fn core_pattern(extra: &str) -> GraphPattern {
    let text = format!("SELECT * WHERE {{ {CORE} {extra} }}");
    let Query::Select { pattern, .. } = SparqlParser::new().parse_query(&text).unwrap() else {
        unreachable!("a SELECT")
    };
    pattern
}

/// Spec §7 test 1: a pair that starts the order is decoded once, by planning, and its
/// steps take what was read; without the buffer each step takes its own list.
///
/// - Input: `CORE` over 100 values a pattern (the pair shares 50), evaluated with the
///   hand-over and without (as before it: planning's lists dropped, the steps scan); and
///   with the hand-over but no buffer (each of the pair's steps a scan of its pattern).
/// - Expected: two lists read either way; the steps scan only `?x r ?y` with the hand-over
///   (one column read), all three without (three). Red before the hand-over: 2 + 3.
#[test]
fn a_pair_that_starts_the_order_is_decoded_once() {
    let engine = store(100, 1, false, false, &[]);
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let query = format!("SELECT * WHERE {{ {CORE} }}");
    let mut found = Vec::new();
    for (handover, buffers, expected) in [
        (true, true, (2, 1)),
        (false, true, (2, 3)),
        (true, false, (2, 1)),
    ] {
        BUFFERS.set(buffers);
        let mut rows = Vec::new();
        let reads = reads_of(|| rows = answers(&snapshot, &query, &options, handover));
        BUFFERS.set(true);
        assert_eq!(rows.len(), 50, "hand-over {handover}, buffers {buffers}");
        found.push(((handover, buffers), reads, expected));
    }
    for (case, reads, expected) in found {
        assert_eq!(
            reads, expected,
            "(hand-over, buffers) {case:?}: (lists, column scans)"
        );
    }
}

/// Spec §7 test 2: the hand-over answers as the steps do without it, over the shapes that
/// read a BGP in other ways.
///
/// - Input: `CORE` (and the empty pair `c p ?x`, `b q ?x`) as a whole query, with a filter
///   on `?x`, ordered, as OPTIONAL's right side, under MINUS, EXISTS and NOT EXISTS, in a
///   subquery with COUNT; with a named graph beside it: FROM, FROM NAMED with GRAPH and the
///   union default graph; in a store of two runs (no lists); with the result cache, run
///   twice; with the buffer off and a filter that leaves one row after step 1, so step 2
///   probes the pattern its list checks in place of the probe.
/// - Expected: in every case the same rows with the hand-over as without; and at least one
///   case with rows (the comparison isn't vacuous).
#[test]
fn the_hand_over_answers_as_the_steps_do() {
    let core = |extra: &str| format!("SELECT * WHERE {{ {CORE} {extra} }}");
    let empty = "SELECT * WHERE { <http://e/c> <http://e/p> ?x . <http://e/b> <http://e/q> ?x . \
                 ?x <http://e/r> ?y }"
        .to_owned();
    let shapes: Vec<(&str, String)> = vec![
        ("whole", core("")),
        ("empty intersection", empty),
        ("filter on ?x", core("FILTER(?x != <http://e/o60>)")),
        (
            "ordered",
            format!("SELECT * WHERE {{ {CORE} }} ORDER BY DESC(?y)"),
        ),
        (
            "OPTIONAL's right side",
            format!("SELECT * WHERE {{ <http://e/k> <http://e/t> ?x OPTIONAL {{ {CORE} }} }}"),
        ),
        (
            "MINUS",
            format!("SELECT * WHERE {{ ?x <http://e/r> ?y MINUS {{ {CORE} }} }}"),
        ),
        (
            "EXISTS",
            format!("SELECT * WHERE {{ <http://e/k> <http://e/t> ?x FILTER EXISTS {{ {CORE} }} }}"),
        ),
        (
            "NOT EXISTS",
            format!(
                "SELECT * WHERE {{ <http://e/k> <http://e/t> ?x FILTER NOT EXISTS {{ {CORE} }} }}"
            ),
        ),
        (
            "subquery with COUNT",
            format!("SELECT ?n WHERE {{ {{ SELECT (COUNT(*) AS ?n) WHERE {{ {CORE} }} }} }}"),
        ),
        (
            "FROM",
            format!("SELECT * FROM <http://e/g> WHERE {{ {CORE} }}"),
        ),
        (
            "FROM NAMED with GRAPH",
            format!("SELECT * FROM NAMED <http://e/g> WHERE {{ GRAPH ?g {{ {CORE} }} }}"),
        ),
    ];
    // Each case's rows with the hand-over and without.
    let compare = |snapshot: &Snapshot, query: &str, options: &QueryOptions| {
        (
            answers(snapshot, query, options, true),
            answers(snapshot, query, options, false),
        )
    };
    let mut compared: Vec<(String, Compared)> = Vec::new();
    let mut failures = Vec::new();
    for (runs, named) in [(1, true), (2, false)] {
        let engine = store(100, runs, named, false, &[]);
        let snapshot = engine.snapshot();
        for (case, query) in &shapes {
            let options = QueryOptions::default();
            compared.push((
                format!("{case}, {runs} run(s)"),
                compare(&snapshot, query, &options),
            ));
        }
        let union = QueryOptions {
            union_default_graph: true,
            ..QueryOptions::default()
        };
        let case = format!("union default graph, {runs} run(s)");
        compared.push((case, compare(&snapshot, &core(""), &union)));
        // The cache filled by the first run of each, its prefixes taken by the second.
        let cached = || QueryOptions {
            result_cache: Some(Arc::new(crate::ResultCache::new(1 << 24).admitting_all())),
            ..QueryOptions::default()
        };
        for handover in [false, true] {
            let options = cached();
            let first = answers(&snapshot, &core(""), &options, handover);
            let second = answers(&snapshot, &core(""), &options, handover);
            if first != second
                || first != answers(&snapshot, &core(""), &QueryOptions::default(), false)
            {
                failures.push(format!("cached, hand-over {handover}, {runs} run(s)"));
            }
        }
        // One row after step 1 (a filter planned as keeping all), so step 2 probes.
        BUFFERS.set(false);
        let query = core("FILTER(COALESCE(?x = <http://e/o70>, false))");
        let case = format!("probe in place, {runs} run(s)");
        compared.push((case, compare(&snapshot, &query, &QueryOptions::default())));
        BUFFERS.set(true);
    }
    let mut with_rows = 0;
    for (case, (with, without)) in &compared {
        if with != without {
            failures.push(format!(
                "{case}: {} rows with, {} without",
                with.len(),
                without.len()
            ));
        }
        with_rows += usize::from(!with.is_empty());
    }
    assert!(failures.is_empty(), "{}", failures.join("; "));
    assert!(with_rows > 10, "{with_rows} cases with rows");
}

/// Spec §7 test 2, canonical equality and late expansion: the hand-over answers as the
/// steps do where `sameAs` aliases are read.
///
/// - Input: the store of 100, with `alias owl:sameAs o60` inferred and equality on;
///   `CORE` with late expansion, early expansion and the canonical reading.
/// - Expected: the same rows with the hand-over as without, in each.
#[test]
fn the_hand_over_answers_as_the_steps_do_under_equality() {
    let engine = store(100, 1, false, true, &[]);
    let snapshot = engine.snapshot();
    let query = format!("SELECT * WHERE {{ {CORE} }}");
    for (case, options) in [
        ("late expansion", QueryOptions::default()),
        (
            "early expansion",
            QueryOptions {
                equality_early_expansion: true,
                ..QueryOptions::default()
            },
        ),
        (
            "canonical",
            QueryOptions {
                equality_canonical: true,
                ..QueryOptions::default()
            },
        ),
    ] {
        let without = answers(&snapshot, &query, &options, false);
        assert!(!without.is_empty(), "{case}: no rows, nothing tested");
        assert_eq!(
            answers(&snapshot, &query, &options, true),
            without,
            "{case}"
        );
    }
}

/// Spec §7 test 2, the probe in place: a step that would probe a pattern with a list checks
/// the running rows against the list, with no seek for that step.
///
/// - Input: `CORE` with a filter planned as keeping every row but leaving one after step 1,
///   the buffer off (so the pair's second step runs), under EXPLAIN ANALYZE.
/// - Expected: an index join "(read at planning)", without the probe's seek counts.
#[test]
fn a_step_that_would_probe_a_pattern_with_a_list_checks_the_list() {
    let engine = store(100, 1, false, false, &[]);
    let snapshot = engine.snapshot();
    let text = format!("SELECT * WHERE {{ {CORE} FILTER(COALESCE(?x = <http://e/o70>, false)) }}");
    let query = SparqlParser::new().parse_query(&text).unwrap();
    BUFFERS.set(false);
    let explained = explain(&snapshot, &query, &QueryOptions::default());
    BUFFERS.set(true);
    let (_, steps, rows, _, _) = explained.unwrap();
    assert_eq!(rows, 1);
    let checked: Vec<&PlanStep> = steps
        .iter()
        .filter(|s| s.operator == "index join" && s.detail.ends_with("(read at planning)"))
        .collect();
    assert_eq!(checked.len(), 1, "{steps:#?}");
    assert!(!checked[0].detail.contains("seeks"), "{steps:#?}");
}

/// The peak of `ctx`'s budget while it evaluates `pattern`, and its rows.
fn evaluated(ctx: &Context<'_>, pattern: &GraphPattern) -> (usize, Result<usize, ()>) {
    let answer = ctx.eval(pattern);
    let rows = match answer {
        Ok(solutions) => {
            let rows = solutions.table.len();
            ctx.consumed(solutions);
            Ok(rows)
        }
        Err(_) => Err(()),
    };
    (ctx.memory.peak(), rows)
}

/// Spec §7 test 3: a buffer the budget declines is no refusal; the reads are charged.
///
/// - Input: `CORE` over 1,000 values a pattern; planning's peak measured without a limit,
///   then planned within one byte less (the lists fit, the buffer, taken last, doesn't).
/// - Expected: the counts as without the limit, no buffer, and the request not marked
///   refused (an ordinary charge would mark it: `Budget::refused`).
#[test]
fn a_declined_buffer_marks_no_refusal() {
    let engine = store(1_000, 1, false, false, &[]);
    let snapshot = engine.snapshot();
    let planned = |limit: Option<usize>| {
        let options = QueryOptions {
            resources: crate::Resources {
                memory: Envelope::new(limit, None),
                ..crate::Resources::default()
            },
            ..QueryOptions::default()
        };
        let ctx = Context::new(&snapshot, &options, None, None);
        let scans: Vec<ScanPattern> = ["a p ?x", "b q ?x", "?x r ?y"]
            .iter()
            .map(|p| super::join_order_tests::scan(&ctx.snapshot, p))
            .collect();
        let counts: Vec<u64> = scans
            .iter()
            .map(|s| ctx.snapshot.estimate_in(ctx.model, &s.quad_pattern()))
            .collect();
        let (inputs, reads) = ctx.plan_inputs(&scans, &counts, &[], &mut Vec::new(), true);
        let overlaps: Vec<_> = inputs.into_iter().map(|input| input.overlaps).collect();
        let buffer = reads.has_buffer();
        drop(reads);
        (
            overlaps,
            buffer,
            ctx.memory.peak(),
            ctx.memory.refused() || ctx.budget.refused(),
        )
    };
    let (overlaps, buffer, peak, refused) = planned(None);
    assert!(
        buffer && !refused,
        "unlimited: buffer {buffer}, refused {refused}"
    );
    assert_eq!(overlaps[0], vec![(1, 500)], "{overlaps:?}");
    let (within, buffer, _, refused) = planned(Some(peak - 1));
    assert_eq!(within, overlaps);
    assert!(!buffer, "the buffer taken within {} bytes", peak - 1);
    assert!(!refused, "a declined buffer marked the request refused");
}

/// Review r1, F1: the candidate pairs are stored only as far as the optional part has room
/// for them; n patterns on one variable make n²/2 pairs, which no admission bounds before
/// they are sorted. Where the room is short, the BGP plans with its counts alone and the
/// request isn't marked refused.
///
/// - Input: `sI p oI` for I below 200, one commit; the 200 patterns `sI p ?x`, each one
///   match (19,900 candidate pairs, 24 bytes each), planned without a limit, then within
///   64 KiB: room for the lists the allowance admits (at most `MAX_LISTS` of one value each,
///   and their directory), not for the candidates.
/// - Expected: unlimited, overlaps counted; within 64 KiB, none for any pattern, and the
///   request not marked refused.
#[test]
fn candidate_pairs_stay_within_the_optional_budget() {
    let extra: Vec<Triple> = (0..200)
        .map(|i| (format!("s{i}"), "p", format!("o{i}")))
        .collect();
    let engine = store(1, 1, false, false, &extra);
    let snapshot = engine.snapshot();
    let planned = |limit: Option<usize>| {
        let options = QueryOptions {
            resources: crate::Resources {
                memory: Envelope::new(limit, None),
                ..crate::Resources::default()
            },
            ..QueryOptions::default()
        };
        let ctx = Context::new(&snapshot, &options, None, None);
        let scans: Vec<ScanPattern> = (0..200)
            .map(|i| super::join_order_tests::scan(&ctx.snapshot, &format!("s{i} p ?x")))
            .collect();
        let counts: Vec<u64> = scans
            .iter()
            .map(|s| ctx.snapshot.estimate_in(ctx.model, &s.quad_pattern()))
            .collect();
        let (inputs, reads) = ctx.plan_inputs(&scans, &counts, &[], &mut Vec::new(), true);
        drop(reads);
        let overlaps: Vec<_> = inputs.into_iter().map(|input| input.overlaps).collect();
        (overlaps, ctx.memory.refused() || ctx.budget.refused())
    };
    let (overlaps, _) = planned(None);
    assert!(
        overlaps.iter().any(|o| !o.is_empty()),
        "unlimited: no overlap"
    );
    let (within, refused) = planned(Some(64 << 10));
    assert!(
        within.iter().all(Vec::is_empty),
        "within 64 KiB: {within:?}"
    );
    assert!(
        !refused,
        "the candidates' refusal marked the request refused"
    );
}

/// The budget types' owner's review of the optional part, R2: a list building that the
/// optional part refused once gives no overlap at all, not the counts of the lists read
/// before the refusal; and the request isn't marked refused.
///
/// - Input: `a p ?x`, `b q ?x` and `c p ?x` over 100 values each (`a` and `b` share 50),
///   planned with counts of 150 each, so all three lists fit the allowance; read without a
///   limit and the buffer off (the reads' peak: the directory and three lists), then within
///   one byte less, so the last list read is refused.
/// - Expected: unlimited, `a p ?x` and `b q ?x` share 50; within the limit, no overlap for
///   any pattern, and the request not marked refused.
#[test]
fn a_refused_list_read_leaves_its_building_without_overlaps() {
    let engine = store(100, 1, false, false, &[]);
    let snapshot = engine.snapshot();
    let planned = |limit: Option<usize>| {
        let options = QueryOptions {
            resources: crate::Resources {
                memory: Envelope::new(limit, None),
                ..crate::Resources::default()
            },
            ..QueryOptions::default()
        };
        let ctx = Context::new(&snapshot, &options, None, None);
        let scans: Vec<ScanPattern> = ["a p ?x", "b q ?x", "c p ?x"]
            .iter()
            .map(|p| super::join_order_tests::scan(&ctx.snapshot, p))
            .collect();
        BUFFERS.set(false);
        let (inputs, reads) = ctx.plan_inputs(&scans, &[150; 3], &[], &mut Vec::new(), true);
        BUFFERS.set(true);
        drop(reads);
        let overlaps: Vec<_> = inputs.into_iter().map(|input| input.overlaps).collect();
        (overlaps, ctx.memory.peak(), ctx.memory.refused())
    };
    let (overlaps, peak, _) = planned(None);
    assert!(overlaps[0].contains(&(1, 50)), "{overlaps:?}");
    let (within, _, refused) = planned(Some(peak - 1));
    assert!(within.iter().all(Vec::is_empty), "{within:?}");
    assert!(
        !refused,
        "a refused optional read marked the request refused"
    );
}

/// Spec §7 test 3: the reads add to the peak at most the allowance, the buffer and their
/// directory (about 98 KiB), and the lists the steps take replace their scans.
///
/// - Input: `CORE` over 4,096 values a pattern (the pair's lists fill the allowance, and its
///   2,048 shared values the buffer's half), evaluated with the hand-over and without.
/// - Expected: the same rows; the peak with the hand-over at most the peak without it
///   (here every list serves a step).
#[test]
fn the_reads_replace_the_scans_they_serve() {
    let engine = store(4_096, 1, false, false, &[]);
    let snapshot = engine.snapshot();
    let pattern = core_pattern("");
    // Each query its own envelope, whose peak is measured.
    let run = |handover: bool| {
        let options = QueryOptions::default();
        let ctx = Context::new(&snapshot, &options, None, None);
        HANDOVER.set(handover);
        let evaluated = evaluated(&ctx, &pattern);
        HANDOVER.set(true);
        evaluated
    };
    let (without, rows) = run(false);
    let (with, rows_with) = run(true);
    assert_eq!(rows, Ok(2_048));
    assert_eq!(rows_with, rows);
    assert!(
        with <= without,
        "peak {with} with the hand-over, {without} without"
    );
}

/// Spec §7 test 3: a cached prefix replaces its steps, and their lists are released before
/// its rows are made.
///
/// - Input: `CORE`'s basic graph pattern over 1,000 values a pattern with the result cache,
///   evaluated as itself (a SELECT around it would be cached whole, above its joins): once
///   (its prefixes cached), then again in a new query; planning's peak measured apart.
/// - Expected: the second query's peak below planning's plus the cached rows' bytes: its
///   lists and buffer released before the rows were made (held, the peak is their sum).
#[test]
fn lists_inside_a_cached_prefix_are_released_before_its_rows_are_made() {
    let engine = store(1_000, 1, false, false, &[]);
    let snapshot = engine.snapshot();
    // One cache; each query its own envelope, whose peak is measured.
    let cache = Arc::new(crate::ResultCache::new(1 << 24).admitting_all());
    let options = || QueryOptions {
        result_cache: Some(Arc::clone(&cache)),
        ..QueryOptions::default()
    };
    let pattern = match core_pattern("") {
        GraphPattern::Project { inner, .. } => *inner,
        other => other,
    };
    assert!(matches!(pattern, GraphPattern::Bgp { .. }), "{pattern:?}");
    let first_options = options();
    let first = Context::new(&snapshot, &first_options, None, None);
    let (planning, rows) = {
        let Ok(answer) = first.eval(&pattern) else {
            panic!("the first query failed");
        };
        let bytes = answer.table.memory_bytes();
        first.consumed(answer);
        // Planning holds the lists and the buffer at once: measured as the overlap read.
        let plain = QueryOptions::default();
        let ctx = Context::new(&snapshot, &plain, None, None);
        let scans: Vec<ScanPattern> = ["a p ?x", "b q ?x", "?x r ?y"]
            .iter()
            .map(|p| super::join_order_tests::scan(&ctx.snapshot, p))
            .collect();
        let counts: Vec<u64> = scans
            .iter()
            .map(|s| ctx.snapshot.estimate_in(ctx.model, &s.quad_pattern()))
            .collect();
        let (_, reads) = ctx.plan_inputs(&scans, &counts, &[], &mut Vec::new(), true);
        assert!(reads.has_buffer());
        drop(reads);
        (ctx.memory.peak(), bytes)
    };
    let second_options = options();
    let second = Context::new(&snapshot, &second_options, None, None);
    let mut evaluation = (0, Err(()));
    let (lists, _) = reads_of(|| evaluation = evaluated(&second, &pattern));
    let (peak, answered) = evaluation;
    assert_eq!(answered, Ok(500));
    assert_eq!(
        lists, 2,
        "the second query's planning read no lists: nothing tested"
    );
    assert!(
        peak < planning + rows / 2,
        "peak {peak}: planning {planning}, cached rows {rows}"
    );
}

/// Spec §7 test 3: a BGP that ends on an error releases its reads: what stays charged
/// after it (the tables of D16-579's exits, as before) is no more than without the
/// hand-over.
///
/// - Input: `CORE` over 1,000 values a pattern and `?x r ?y` of 50,000 rows, within a limit
///   that holds planning's reads but not that pattern's scan.
/// - Expected: an error either way; the bytes still charged after it, with the hand-over,
///   at most those without.
#[test]
fn a_bgp_that_ends_on_an_error_releases_its_reads() {
    // A hundred more rows for each shared value: past the limit however the step joins.
    let more: Vec<Triple> = (0..50_000)
        .map(|i| (format!("o{}", 500 + i % 500), "r", format!("z{i}")))
        .collect();
    let engine = store(1_000, 1, false, false, &more);
    let snapshot = engine.snapshot();
    let options = QueryOptions {
        resources: crate::Resources {
            memory: Envelope::new(Some(256 << 10), None),
            ..crate::Resources::default()
        },
        ..QueryOptions::default()
    };
    let pattern = core_pattern("");
    let left = |handover: bool| {
        let ctx = Context::new(&snapshot, &options, None, None);
        HANDOVER.set(handover);
        let answer = ctx.eval(&pattern);
        HANDOVER.set(true);
        assert!(
            answer.is_err(),
            "hand-over {handover}: answered within the limit"
        );
        ctx.memory.used()
    };
    let (without, with) = (left(false), left(true));
    assert!(
        with <= without,
        "{with} bytes left with the hand-over, {without} without"
    );
}

/// Spec §7 test 4: no lists under a LIMIT (EXPLAIN ANALYZE too), with index joins forced,
/// or in the plan without running; at most [`reads::MAX_LISTS`] in one BGP.
///
/// - Input: `CORE` over 100 values a pattern: with LIMIT 5, evaluated and explained; with
///   index joins forced; planned without running (`plan`); explained in full. A BGP of 40
///   patterns `s{i} p ?x` that match nothing (every count 0, so every pair fits).
/// - Expected: no list read in the first four; exactly two explained in full (the BGP's
///   own planning; the estimates read none); [`reads::MAX_LISTS`] for the 40.
#[test]
fn lists_are_read_only_where_a_step_can_take_them() {
    let engine = store(100, 1, false, false, &[]);
    let snapshot = engine.snapshot();
    let parse = |text: &str| SparqlParser::new().parse_query(text).unwrap();
    let limited = parse(&format!("SELECT * WHERE {{ {CORE} }} LIMIT 5"));
    let whole = parse(&format!("SELECT * WHERE {{ {CORE} }}"));
    let default = QueryOptions::default();
    let forced = QueryOptions {
        index_joins: Some(true),
        ..QueryOptions::default()
    };
    let cases: [(&str, &dyn Fn(), usize); 5] = [
        (
            "LIMIT",
            &|| drop(crate::evaluate_query(&snapshot, &limited, &default).unwrap()),
            0,
        ),
        (
            "LIMIT, explained",
            &|| drop(explain(&snapshot, &limited, &default).unwrap()),
            0,
        ),
        (
            "index joins forced",
            &|| drop(crate::evaluate_query(&snapshot, &whole, &forced).unwrap()),
            0,
        ),
        (
            "plan without running",
            &|| drop(plan(&snapshot, &whole, &default).unwrap()),
            0,
        ),
        (
            "explained",
            &|| drop(explain(&snapshot, &whole, &default).unwrap()),
            2,
        ),
    ];
    for (case, run, expected) in cases {
        let (lists, _) = reads_of(run);
        assert_eq!(lists, expected, "{case}");
    }
    // Known terms, never together: each pattern `s{i} p ?x` matches nothing.
    let known: Vec<Triple> = (0..40)
        .map(|i| (format!("s{i}"), "u", "v".to_owned()))
        .collect();
    let engine = store(100, 1, false, false, &known);
    let snapshot = engine.snapshot();
    let patterns: Vec<String> = (0..40)
        .map(|i| format!("<http://e/s{i}> <http://e/p> ?x"))
        .collect();
    let many = parse(&format!("SELECT * WHERE {{ {} }}", patterns.join(" . ")));
    let (lists, _) = reads_of(|| drop(crate::evaluate_query(&snapshot, &many, &default).unwrap()));
    assert_eq!(lists, reads::MAX_LISTS);
}

/// Spec §7 test 5: EXPLAIN ANALYZE shows the steps that took what planning read, with
/// their rows.
///
/// - Input: `CORE` over 100 values a pattern, explained.
/// - Expected: a scan of `a p ?x` or `b q ?x` with 100 rows and a join with 50, both
///   "(read at planning)"; then `?x r ?y`'s step as before.
#[test]
fn explain_shows_the_steps_read_at_planning() {
    let engine = store(100, 1, false, false, &[]);
    let snapshot = engine.snapshot();
    let query = SparqlParser::new()
        .parse_query(&format!("SELECT * WHERE {{ {CORE} }}"))
        .unwrap();
    let (_, steps, rows, _, _) = explain(&snapshot, &query, &QueryOptions::default()).unwrap();
    assert_eq!(rows, 50);
    let read: Vec<(&str, u64)> = steps
        .iter()
        .filter(|s| s.detail.ends_with("(read at planning)"))
        .map(|s| (s.operator.as_str(), s.rows))
        .collect();
    assert_eq!(read, [("scan", 100), ("join", 50)], "{steps:#?}");
    assert!(
        steps
            .iter()
            .any(|s| s.detail.contains("<http://e/r>") && !s.detail.contains("planning")),
        "{steps:#?}"
    );
}
