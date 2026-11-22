//! A grouped aggregation's one-pass routes ([`aggregate::aggregate_in_one_pass`],
//! [`Context::numeric_pass`]) under memory limits. Spec: where the pass's reservation, its
//! numbers' charge or a term of its results does not fit, it releases everything it charged
//! and the per-column route ([`Context::aggregate_column`]) answers; the query is refused only
//! where that route does not fit either.
//!
//! The groups come from VALUES, as written. Sizes of the charges, from the queries below:
//! - 4,096 groups of one aggregate: the pass's states, 1,507,328 bytes
//!   ([`aggregate::pass_bytes`]), with about 131 kB held before them; then 4,096 numbers,
//!   466,944 bytes (`number_bytes`): the pass needs about 2.1 MB before its results' terms;
//! - 8,192 distinct values: their numbers, 933,888 bytes, with about 156 kB held before them.

use super::*;
use nrese_engine::{Engine, EngineConfig};
use nrese_exec::Envelope;
use nrese_rdf::Literal;
use nrese_rdf::vocab::xsd;
use nrese_sparql_syntax::{Query, SparqlParser};

const XSD_DOUBLE: &str = "<http://www.w3.org/2001/XMLSchema#double>";

/// `SELECT ?g (aggregates) … GROUP BY ?g` over VALUES rows `(g v)`.
fn grouped(values: &[(u32, u32)], aggregates: &str) -> String {
    let rows: String = values.iter().map(|(g, v)| format!("({g} {v}) ")).collect();
    format!("SELECT ?g {aggregates} WHERE {{ VALUES (?g ?v) {{ {rows}}} }} GROUP BY ?g")
}

/// 64 groups of 128 distinct integers: value `i` in group `i % 64`.
fn wide() -> Vec<(u32, u32)> {
    (0..64 * 128).map(|i| (i % 64, i)).collect()
}

/// 4,096 groups of one value, the group's own number.
fn single() -> Vec<(u32, u32)> {
    (0..4_096).map(|g| (g, g)).collect()
}

fn options(limit: Option<usize>) -> QueryOptions {
    QueryOptions {
        as_written: true,
        resources: crate::Resources {
            memory: Envelope::new(limit, None),
            ..crate::Resources::default()
        },
        ..QueryOptions::default()
    }
}

fn snapshot() -> Snapshot {
    let config = EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    };
    Engine::new(config).unwrap().snapshot()
}

fn integer_of(term: &Term) -> i64 {
    let Term::Literal(literal) = term else {
        panic!("{term} is no literal")
    };
    literal.value().parse().unwrap()
}

/// `text`'s rows through the public evaluator under `limit`: per group `?g`, `?t`.
fn answer(
    snapshot: &Snapshot,
    text: &str,
    limit: usize,
) -> Result<Vec<(i64, Option<Term>)>, String> {
    let query = SparqlParser::new().parse_query(text).unwrap();
    let rows = match crate::evaluate_query(snapshot, &query, &options(Some(limit))) {
        Ok(crate::QueryResults::Solutions(rows)) => rows,
        Ok(_) => panic!("not solutions"),
        Err(error) => return Err(format!("{error:?}")),
    };
    let mut out = Vec::new();
    for row in rows {
        let row = row.map_err(|error| format!("{error:?}"))?;
        out.push((integer_of(row.get("g").unwrap()), row.get("t").cloned()));
    }
    out.sort_by_key(|(g, _)| *g);
    Ok(out)
}

/// Spec: each query below is answered under a limit where the per-column route fits and the
/// pass does not (the pass's charge alone, see the module, exceeds the limit), with the rows
/// SPARQL gives (hand-computed). Each was refused with `MemoryLimit` before the fallback.
/// - `SUM(IF(?v = 0, "bad", ?v))` over [`wide`] at 1,060,000 bytes: its numbers (933,888 on
///   about 156 kB) need 1,089,632; group 0 holds the string and has no sum, group `g` sums
///   `128 g + 64 × (0 + … + 127)`.
/// - `SUM(?v + 0)` over [`single`] at 1,300,000 bytes (the states don't fit) and at 2,000,000
///   (the states fit, the numbers don't): group `g` sums to `g`.
/// - `SUM(xsd:double(?v))` over [`single`] at 3,000,000 bytes: states and numbers fit
///   (about 2.1 MB), the 4,096 new doubles (346 to 348 bytes each, 1.4 MB) don't: group `g`
///   sums to the double `g`.
#[test]
fn a_pass_that_does_not_fit_leaves_the_answer_to_the_per_column_route() {
    let snapshot = snapshot();
    let integer = |n: i64| -> Option<Term> {
        Some(Literal::new_typed_literal(n.to_string(), xsd::INTEGER).into())
    };
    let wide_sums: Vec<(i64, Option<Term>)> = (0..64)
        .map(|g| (g, (g > 0).then(|| 128 * g + 64 * 8_128).and_then(integer)))
        .collect();
    let single_sums: Vec<(i64, Option<Term>)> = (0..4_096).map(|g| (g, integer(g))).collect();
    // Group `g`'s sum is the double `g`.
    let doubles = |rows: &[(i64, Option<Term>)]| {
        rows.len() == 4_096
            && rows.iter().enumerate().all(|(g, (key, sum))| match sum {
                Some(Term::Literal(sum)) => {
                    *key == g as i64
                        && sum.datatype() == xsd::DOUBLE
                        && sum.value().parse::<f64>() == Ok(g as f64)
                }
                _ => false,
            })
    };
    type Case<'a> = (String, usize, &'a dyn Fn(&[(i64, Option<Term>)]) -> bool);
    let cases: [Case<'_>; 4] = [
        (
            grouped(&wide(), "(SUM(IF(?v = 0, \"bad\", ?v)) AS ?t)"),
            1_060_000,
            &|rows| rows == wide_sums,
        ),
        (
            grouped(&single(), "(SUM(?v + 0) AS ?t)"),
            1_300_000,
            &|rows| rows == single_sums,
        ),
        (
            grouped(&single(), "(SUM(?v + 0) AS ?t)"),
            2_000_000,
            &|rows| rows == single_sums,
        ),
        (
            grouped(&single(), &format!("(SUM({XSD_DOUBLE}(?v)) AS ?t)")),
            3_000_000,
            &doubles,
        ),
    ];
    let mut wrong = Vec::new();
    for (text, limit, right) in cases {
        let shape = &text[text.find("?g (").unwrap()..text.find(" WHERE").unwrap()];
        match answer(&snapshot, &text, limit) {
            Ok(rows) if right(&rows) => {}
            Ok(_) => wrong.push(format!("{shape} at {limit}: rows other than SPARQL's")),
            Err(error) => wrong.push(format!("{shape} at {limit}: {error}")),
        }
    }
    assert!(
        wrong.is_empty(),
        "expected GROUP BY answers:\n{}",
        wrong.join("\n")
    );
}

/// Which route aggregates the groups: the general one, from one pass where it applies and
/// fits ([`Context::aggregate_columns`]), or an aggregate at a time alone
/// ([`Context::aggregate_column`]).
#[derive(Clone, Copy)]
enum Route {
    General,
    PerColumn,
}

#[derive(Debug, PartialEq, Eq)]
struct RouteCharges {
    used: usize,
    kept: usize,
    decoded_items: usize,
    after_decoder: usize,
}

fn release_decoder(ctx: &Context<'_>) -> usize {
    *ctx.decoded.borrow_mut() =
        decode_cache::DecodeCache::new(&ctx.budget, DECODE_CACHE_ENTRIES, 1);
    ctx.budget.used()
}

/// The GROUP BY under `pattern`: its input, keys and aggregates.
fn group_in(
    pattern: &GraphPattern,
) -> (
    &GraphPattern,
    &[Variable],
    &[(Variable, AggregateExpression)],
) {
    match pattern {
        GraphPattern::Group {
            inner,
            variables,
            aggregates,
        } => (inner, variables, aggregates),
        GraphPattern::Project { inner, .. } | GraphPattern::Extend { inner, .. } => group_in(inner),
        other => panic!("no GROUP BY in {other:?}"),
    }
}

/// `query`'s groups aggregated through `route` under `limit`, in a context of their own: per
/// group its key and values as text (sorted), or the refusal; and what the context holds
/// once the input is consumed, before reading the answer or clearing any cache.
fn aggregated(
    snapshot: &Snapshot,
    query: &Query,
    limit: Option<usize>,
    route: Route,
) -> (Result<Vec<String>, String>, RouteCharges) {
    let Query::Select { pattern, .. } = query else {
        panic!("a SELECT")
    };
    let (inner, variables, aggregates) = group_in(pattern);
    let options = options(limit);
    let ctx = Context::new(snapshot, &options, None, None);
    let refused = |error: NativeError| match QueryEvaluationError::from(error) {
        QueryEvaluationError::MemoryLimit(refused) => format!("{refused:?}"),
        other => panic!("{other:?}"),
    };
    let charges = || {
        let decoded_items = ctx.decoded.borrow().bytes_for_test();
        RouteCharges {
            used: ctx.budget.used(),
            kept: ctx.kept.borrow().bytes(),
            decoded_items,
            after_decoder: release_decoder(&ctx),
        }
    };
    let solutions = match ctx.eval(inner) {
        Ok(solutions) => solutions,
        Err(error) => return (Err(refused(error)), charges()),
    };
    let (groups, positions) = aggregate::groups_of(&solutions, variables).ok().unwrap();
    let columns = match route {
        Route::General => {
            ctx.aggregate_columns(&solutions, &groups.group_of, groups.len(), aggregates)
        }
        Route::PerColumn => {
            let mut members: Vec<Vec<usize>> = vec![Vec::new(); groups.len()];
            for (row, &group) in groups.group_of.iter().enumerate() {
                members[group as usize].push(row);
            }
            aggregates
                .iter()
                .map(|aggregate| ctx.aggregate_column(&solutions, &members, aggregate))
                .collect()
        }
    };
    let text = |id: u64| {
        decode(&ctx.snapshot, &ctx.computed.borrow(), id)
            .map_or("-".to_owned(), |term| term.to_string())
    };
    let answer = columns.map_err(refused).map(|columns| {
        let keys = aggregate::key_columns(&groups, &positions, None);
        let mut rows: Vec<String> = (0..groups.len())
            .map(|g| {
                let values = keys.iter().chain(&columns).map(|column| text(column[g]));
                values.collect::<Vec<_>>().join(" ")
            })
            .collect();
        rows.sort();
        rows
    });
    ctx.consumed(solutions);
    (answer, charges())
}

/// Spec: the failed one-pass SUM leaves the per-column interning peak the same room
/// as the forced per-column route. Both give the double g for each group g.
#[test]
fn a_refused_pass_releases_decoded_inputs_before_the_per_column_route() {
    let snapshot = snapshot();
    let values: Vec<_> = (0..4_096)
        .flat_map(|g| [g % 512, 512 * (g / 512), 0, 0, 0].map(|v| (g, v)))
        .collect();
    let text = grouped(&values, &format!("(SUM({XSD_DOUBLE}(?v)) AS ?t)"));
    let query = SparqlParser::new().parse_query(&text).unwrap();
    let run = |limit, route| aggregated(&snapshot, &query, Some(limit), route);
    let (mut low, mut high) = (1_000_000, 4_500_000);
    assert!(run(low, Route::PerColumn).0.is_err());
    assert!(run(high, Route::PerColumn).0.is_ok());
    while high - low > 1 {
        let mid = (low + high) / 2;
        if run(mid, Route::PerColumn).0.is_ok() {
            high = mid;
        } else {
            low = mid;
        }
    }
    let mut expected: Vec<_> = (0..4_096)
        .map(|g| format!("{} {}", Literal::from(g as i64), Literal::from(g as f64)))
        .collect();
    expected.sort();
    let (serial, alone) = run(high, Route::PerColumn);
    let (general, after) = run(high, Route::General);
    assert_eq!(
        serial.ok().as_ref(),
        Some(&expected),
        "per-column SUM answers"
    );
    assert!(
        general.as_ref() == Ok(&expected),
        "fallback SUM answers at the per-column limit {high}: {:?}, charges {after:?}, per-column {alone:?}",
        general.err()
    );
    assert_eq!(
        after, alone,
        "fallback SUM leaves only the per-column charges"
    );
}

/// 20,480 rows in 4,096 groups over 8,192 distinct integers: value `i % 8192` in group
/// `i % 4096`. Enough rows that the per-column route aggregates in parallel, each worker
/// decoding into a cache of its own ([`Decoder`]); few enough distinct values that the
/// numeric pass decodes them into the query's ([`Context::term`]).
fn repeated() -> Vec<(u32, u32)> {
    (0..20_480).map(|i| (i % 4_096, i % 8_192)).collect()
}

/// The shapes the sweep runs: the three above, and others of either pass; over
/// [`repeated`], the pass leaves what it decoded in the query's cache where the per-column
/// route alone would not.
fn shapes() -> Vec<String> {
    let (wide, single, repeated) = (wide(), single(), repeated());
    vec![
        grouped(&repeated, &format!("(SUM({XSD_DOUBLE}(?v)) AS ?t)")),
        grouped(&repeated, "(AVG(?v * 2) AS ?t)"),
        grouped(
            &repeated,
            &format!("(SUM(IF(?v = 0, \"bad\", {XSD_DOUBLE}(?v))) AS ?t)"),
        ),
        grouped(&wide, "(SUM(IF(?v = 0, \"bad\", ?v)) AS ?t)"),
        grouped(&single, "(SUM(?v + 0) AS ?t)"),
        grouped(&single, &format!("(SUM({XSD_DOUBLE}(?v)) AS ?t)")),
        grouped(&single, "(AVG(?v * 2) AS ?t)"),
        grouped(&wide, "(AVG(?v) AS ?t)"),
        grouped(&wide, "(MIN(?v) AS ?a) (MAX(?v) AS ?b) (COUNT(?v) AS ?n)"),
        grouped(&single, "(COUNT(*) AS ?n)"),
        grouped(
            &wide,
            &format!(
                "(SUM(?v + 0) AS ?s) (AVG({XSD_DOUBLE}(?v)) AS ?a) (COUNT(*) AS ?n) (COUNT(?v) AS ?c)"
            ),
        ),
    ]
}

/// Spec: under every limit of a sweep from 200 kB to 6 MB, the general route answers as the
/// per-column route does under the same limit; where the per-column route is refused, the
/// general one is refused too or answers as the per-column route does without a limit.
#[test]
fn a_pass_is_refused_only_where_the_per_column_route_is() {
    let snapshot = snapshot();
    let mut wrong = Vec::new();
    for text in shapes() {
        let query = SparqlParser::new().parse_query(&text).unwrap();
        let shape = &text[text.find("?g (").unwrap()..text.find(" WHERE").unwrap()];
        let (unlimited, _) = aggregated(&snapshot, &query, None, Route::PerColumn);
        let unlimited = unlimited.expect("no limit");
        for limit in (2..=60).map(|k| k * 100_000) {
            let (general, _) = aggregated(&snapshot, &query, Some(limit), Route::General);
            let (per_column, _) = aggregated(&snapshot, &query, Some(limit), Route::PerColumn);
            match (general, per_column) {
                (Ok(general), Ok(per_column)) if general != per_column => {
                    wrong.push(format!("{shape} at {limit}: the routes answer differently"));
                }
                (Ok(general), Err(_)) if general != unlimited => {
                    wrong.push(format!("{shape} at {limit}: a wrong answer"));
                }
                (Err(refused), Ok(_)) => wrong.push(format!(
                    "{shape} at {limit}: refused ({refused}) where the per-column route answers"
                )),
                _ => {}
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Spec: a pass that falls back leaves the query holding what the per-column route alone
/// leaves (its results' terms), nothing of its own and nothing twice: with the answer the
/// per-column route gives, the budget reads the same afterwards. Over [`wide`] the pass
/// declines late, at the string, once its numbers were found and charged (933,888 bytes);
/// the other three are the limits of the test above where a charge of the pass is refused.
#[test]
fn a_pass_that_falls_back_leaves_nothing_charged() {
    let snapshot = snapshot();
    let cases = [
        (
            grouped(&wide(), "(SUM(IF(?v = 0, \"bad\", ?v)) AS ?t)"),
            4_000_000,
        ),
        (grouped(&single(), "(SUM(?v + 0) AS ?t)"), 1_300_000),
        (grouped(&single(), "(SUM(?v + 0) AS ?t)"), 2_000_000),
        (
            grouped(&single(), &format!("(SUM({XSD_DOUBLE}(?v)) AS ?t)")),
            3_000_000,
        ),
    ];
    let mut wrong = Vec::new();
    for (text, limit) in cases {
        let query = SparqlParser::new().parse_query(&text).unwrap();
        let shape = &text[text.find("?g (").unwrap()..text.find(" WHERE").unwrap()];
        let (general, after) = aggregated(&snapshot, &query, Some(limit), Route::General);
        let (per_column, alone) = aggregated(&snapshot, &query, Some(limit), Route::PerColumn);
        let per_column = per_column.expect("the per-column route fits");
        match general {
            Err(refused) => wrong.push(format!("{shape} at {limit}: refused ({refused})")),
            Ok(general) if general != per_column => {
                wrong.push(format!("{shape} at {limit}: the routes answer differently"));
            }
            Ok(_) if after.after_decoder != alone.after_decoder || after.kept != alone.kept => wrong.push(format!(
                "{shape} at {limit}: charges {after:?} afterwards, the per-column route {alone:?}"
            )),
            Ok(_) => {}
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// 16 subjects with `k`, each with 512 rows (`has`); row `i` has `g` `i % 1,024` and `v`
/// `i`: the join is 8,192 rows in 1,024 groups of `?g`, eight values each, from the 16 rows
/// of its smallest pattern, which a streamed GROUP BY takes in one morsel however it sizes
/// them ([`Context::group_streamed`]: the first has 16 rows).
fn seeded() -> Snapshot {
    let config = EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    };
    let engine = Engine::new(config).unwrap();
    let node = |name: String| nrese_rdf::NamedNode::new_unchecked(format!("urn:{name}"));
    let integer = |n: u32| Literal::new_typed_literal(n.to_string(), xsd::INTEGER);
    let mut tx = engine.transaction();
    let mut insert = |s: String, p: &str, o: Term| {
        let quad = nrese_rdf::Quad::new(
            node(s),
            node(p.to_owned()),
            o,
            nrese_rdf::GraphName::DefaultGraph,
        );
        tx.insert(quad.as_ref());
    };
    for c in 0..16u32 {
        insert(format!("c{c}"), "k", integer(c).into());
        for j in 0..512 {
            let i = c * 512 + j;
            insert(format!("c{c}"), "has", node(format!("r{i}")).into());
            insert(format!("r{i}"), "g", integer(i % 1_024).into());
            insert(format!("r{i}"), "v", integer(i).into());
        }
    }
    tx.commit().unwrap();
    engine.snapshot()
}

#[derive(Clone, Copy)]
enum RefuseDirect {
    AfterGrouping,
    CacheGrowth,
    CacheEviction,
}

thread_local! {
    static REFUSE_DIRECT: Cell<Option<RefuseDirect>> = const { Cell::new(None) };
}

/// Trigger the real, typed budget refusal at a repeatable point of the normal direct plan.
/// The streamed plan never enters these hooks.
fn refuse_direct(ctx: &Context<'_>) -> NativeError {
    let refused = ctx.budget.charge(ctx.budget.remaining() + 1).unwrap_err();
    QueryEvaluationError::MemoryLimit(refused).into()
}

const FIRST_CACHED: u64 = u64::MAX - 17;
const ADDED_CACHED: u64 = u64::MAX - 18;

fn seed_cache(ctx: &Context<'_>) {
    let term = Some(Literal::from("x".repeat(8_192)).into());
    ctx.decoded.borrow_mut().keep(FIRST_CACHED, &term);
}

fn evict_cache(ctx: &Context<'_>) {
    assert!(
        ctx.decoded.borrow().get(FIRST_CACHED).is_some(),
        "live decoder entry before eviction"
    );
    let mut holds_room = Reservation::new(&*ctx.budget);
    holds_room.charge(ctx.budget.remaining() - 1).unwrap();
    ctx.decoded.borrow_mut().keep(ADDED_CACHED, &None);
    assert!(
        ctx.decoded.borrow().get(FIRST_CACHED).is_none(),
        "decoder entry evicted"
    );
}

fn plan_count(ctx: &Context<'_>) {
    let mut counts = ctx.distinct_counts.borrow_mut();
    let before = counts.charged();
    for id in 0..64 {
        let pattern = nrese_engine::QuadPattern {
            subject: Some(TermId::from_raw(u64::MAX - 64 - id)),
            ..nrese_engine::QuadPattern::all()
        };
        counts
            .get_or_insert_with((pattern, Permutation::Spog), || Some(16))
            .unwrap();
        if counts.charged() > before {
            return;
        }
    }
    panic!("the direct plan did not grow its distinct-count cache");
}

pub(super) fn direct_input(ctx: &Context<'_>) -> NativeResult<()> {
    if matches!(REFUSE_DIRECT.get(), Some(RefuseDirect::CacheEviction)) {
        REFUSE_DIRECT.set(None);
        evict_cache(ctx);
        return Err(refuse_direct(ctx));
    }
    Ok(())
}

pub(super) fn direct_output(
    ctx: &Context<'_>,
    solutions: NativeResult<Solutions>,
) -> NativeResult<Solutions> {
    let solutions = solutions?;
    let Some(refusal) = REFUSE_DIRECT.take() else {
        return Ok(solutions);
    };
    if matches!(refusal, RefuseDirect::AfterGrouping) {
        let held = counters(ctx);
        assert!(
            held[1] > 0 && held[2] > 0,
            "the direct plan kept terms and numbers"
        );
    }
    if matches!(refusal, RefuseDirect::CacheGrowth) {
        plan_count(ctx);
        let term = Some(Literal::from("y".repeat(8_192)).into());
        ctx.decoded.borrow_mut().keep(ADDED_CACHED, &term);
    }
    ctx.consumed(solutions);
    Err(refuse_direct(ctx))
}

/// The direct plan and retry both use normal planning. Force a refusal after the direct
/// group has produced terms and kept its inner point, or after its input has been charged.
fn streamed_retry(refusal: RefuseDirect, message: &str) {
    let snapshot = seeded();
    let text = format!(
        "SELECT ?g (AVG({XSD_DOUBLE}(?v)) AS ?a) WHERE {{ ?c <urn:k> ?k . \
         ?c <urn:has> ?r . ?r <urn:g> ?g . ?r <urn:v> ?v }} GROUP BY ?g"
    );
    let Query::Select { pattern, .. } = SparqlParser::new().parse_query(&text).unwrap() else {
        panic!("a SELECT")
    };
    let (inner, keys, aggregates) = group_in(&pattern);
    let plan = stream::Plan::of(inner, aggregates, false).expect("streamable group");
    let options = QueryOptions {
        as_written: false,
        ..options(Some(12_000_000))
    };
    let run = |retry| {
        let ctx = Context::new(&snapshot, &options, None, None);
        let mut live = Reservation::new(&*ctx.budget);
        live.charge(65_536).unwrap();
        if matches!(refusal, RefuseDirect::CacheEviction) {
            seed_cache(&ctx);
        }
        let (result, reached_refusal) = if retry {
            REFUSE_DIRECT.set(Some(refusal));
            let result = ctx.group(inner, keys, aggregates, None);
            (result, REFUSE_DIRECT.take().is_none())
        } else {
            match refusal {
                RefuseDirect::AfterGrouping => {}
                RefuseDirect::CacheGrowth => plan_count(&ctx),
                RefuseDirect::CacheEviction => evict_cache(&ctx),
            }
            (ctx.group_streamed(&plan, keys, aggregates), true)
        };
        let solutions = result.ok().expect("the adaptive route answers");
        let mut rows: Vec<_> = (0..solutions.table.len())
            .map(|row| {
                let term = |c| {
                    decode(
                        &ctx.snapshot,
                        &ctx.computed.borrow(),
                        solutions.table.get(row, c),
                    )
                };
                let group = integer_of(&term(0).unwrap());
                let Some(Term::Literal(avg)) = term(1) else {
                    panic!("group {group} has no average");
                };
                (group, avg.value().parse::<f64>().unwrap())
            })
            .collect();
        rows.sort_by_key(|(key, _)| *key);
        ctx.consumed(solutions);
        let [kept, terms, numbers, used, decoded] = counters(&ctx);
        let planned = ctx.distinct_counts.borrow().charged();
        // Sample both live caches, then let the decoder independently give back its room.
        let remaining = release_decoder(&ctx);
        let attributed = (kept + planned) as i64;
        (
            rows,
            [kept, terms, numbers],
            [
                used as i64 - attributed,
                decoded as i64,
                remaining as i64 - attributed,
            ],
            reached_refusal,
        )
    };
    let expected: Vec<_> = (0..1_024).map(|g| (g, g as f64 + 3_584.0)).collect();
    let alone = run(false);
    let retried = run(true);
    assert_eq!(alone.0, expected, "adaptive streaming answers AVG");
    assert_eq!(
        retried.0, expected,
        "retried adaptive streaming answers AVG"
    );
    assert!(retried.3, "the same direct plan reached the refused charge");
    let [alone_used, alone_items, alone_after] = alone.2;
    let [retry_used, retry_items, retry_after] = retried.2;
    assert!(
        alone_used >= alone_items + alone_after,
        "adaptive streaming keeps the decoder charged"
    );
    assert_eq!(retry_after, alone_after, "{message}");
    assert!(retry_used >= retry_items + alone_after, "{message}");
    assert_eq!(retried.1, alone.1, "{message}");
}

#[test]
fn a_streamed_retry_holds_nothing_of_the_refused_direct_attempt() {
    streamed_retry(
        RefuseDirect::AfterGrouping,
        "the retry keeps no terms or numbers from the direct attempt",
    );
}

#[test]
fn a_streamed_retry_leaves_live_cache_growth_charged() {
    streamed_retry(
        RefuseDirect::CacheGrowth,
        "the retry leaves the live planner cache charged",
    );
}

#[test]
fn a_streamed_retry_releases_abandoned_tables_after_cache_eviction() {
    streamed_retry(
        RefuseDirect::CacheEviction,
        "the retry releases the abandoned input in full after cache eviction",
    );
}

/// 20,000 subjects, each with `p0` its number modulo 100 and `p1` its number modulo 2,000:
/// their join is 20,000 rows in 2,000 groups of `?y`, both patterns as large, so the join
/// builds a hash table ([`Context::join`]).
fn joined() -> Snapshot {
    let config = EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    };
    let engine = Engine::new(config).unwrap();
    let node = |name: &str| nrese_rdf::NamedNode::new_unchecked(format!("urn:{name}"));
    let mut tx = engine.transaction();
    for i in 0..20_000u32 {
        for (p, o) in [("p0", i % 100), ("p1", i % 2_000)] {
            let o = Literal::new_typed_literal(o.to_string(), xsd::INTEGER);
            let quad = nrese_rdf::Quad::new(
                node(&format!("s{i}")),
                node(p),
                o,
                nrese_rdf::GraphName::DefaultGraph,
            );
            tx.insert(quad.as_ref());
        }
    }
    tx.commit().unwrap();
    engine.snapshot()
}

/// Spec: a GROUP BY whose direct attempt is refused by its join's row limit, on the join's
/// own error path, holds after its streamed retry ([`Context::group`]) what the same plan
/// streamed alone holds in a context of its own ([`Context::group_streamed`]): the refused
/// join's hash table and inputs are let go before the retry. `AVG(?x * 2)` over [`joined`],
/// from 2.3 MB to 4 MB in steps of 100 kB; at each limit where the direct attempt is refused
/// past its rows and the streamed route answers, the retry answers its rows and holds its
/// bytes (the live decoder and the planner's counts aside). At least one limit compares.
#[test]
fn a_streamed_retry_after_a_refused_join_holds_what_the_streamed_route_holds() {
    let snapshot = joined();
    let text = "SELECT ?y (AVG(?x * 2) AS ?a) \
                WHERE { ?s <urn:p0> ?x . ?s <urn:p1> ?y } GROUP BY ?y";
    let Query::Select { pattern, .. } = SparqlParser::new().parse_query(text).unwrap() else {
        panic!("a SELECT")
    };
    let (inner, keys, aggregates) = group_in(&pattern);
    let plan = stream::Plan::of(inner, aggregates, false).expect("streamable group");
    let refused = |error: NativeError| match QueryEvaluationError::from(error) {
        QueryEvaluationError::MemoryLimit(refused) => refused,
        other => panic!("{other:?}"),
    };
    let run = |limit: usize, retry: bool| {
        let options = QueryOptions {
            as_written: false,
            ..options(Some(limit))
        };
        let ctx = Context::new(&snapshot, &options, None, None);
        let result = if retry {
            ctx.group(inner, keys, aggregates, None)
        } else {
            ctx.group_streamed(&plan, keys, aggregates)
        };
        let rows = result
            .map_err(|error| format!("{:?}", refused(error)))
            .map(|solutions| {
                let text = |row: usize, c: usize| {
                    decode(
                        &ctx.snapshot,
                        &ctx.computed.borrow(),
                        solutions.table.get(row, c),
                    )
                    .map_or("-".to_owned(), |term| term.to_string())
                };
                let mut rows: Vec<String> = (0..solutions.table.len())
                    .map(|row| format!("{} {}", text(row, 0), text(row, 1)))
                    .collect();
                rows.sort();
                ctx.consumed(solutions);
                rows
            });
        let kept = ctx.kept.borrow().bytes();
        let planned = ctx.distinct_counts.borrow().charged();
        let held = release_decoder(&ctx) as i64 - planned as i64;
        (rows, kept, held)
    };
    let direct_refused_past_rows = |limit: usize| {
        let options = QueryOptions {
            as_written: false,
            ..options(Some(limit))
        };
        let ctx = Context::new(&snapshot, &options, None, None);
        match ctx.group_direct(inner, keys, aggregates, None) {
            Ok(solutions) => {
                ctx.consumed(solutions);
                false
            }
            Err(error) => refused(error).rows.is_some(),
        }
    };
    let (mut wrong, mut compared) = (Vec::new(), 0);
    for limit in (23..=40).map(|k| k * 100_000) {
        let (alone, alone_kept, alone_held) = run(limit, false);
        if alone.is_err() || !direct_refused_past_rows(limit) {
            continue;
        }
        compared += 1;
        let (retried, retry_kept, retry_held) = run(limit, true);
        match retried {
            Err(refused) => wrong.push(format!(
                "{limit}: refused ({refused}) where the streamed route answers"
            )),
            Ok(rows) if Ok(&rows) != alone.as_ref() => {
                wrong.push(format!("{limit}: the routes answer differently"));
            }
            Ok(_) if (retry_kept, retry_held) != (alone_kept, alone_held) => wrong.push(format!(
                "{limit}: bytes held after the retry {retry_held} (kept {retry_kept}), the \
                 streamed route {alone_held} (kept {alone_kept})"
            )),
            Ok(_) => {}
        }
    }
    assert!(
        compared > 0,
        "no limit refused the direct attempt at its join"
    );
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Spec: the ledger of the table charges made since a save point ([`Context::save`]) gives
/// back only what it recorded. A table charged before a point and consumed while the point
/// is open (as where an attempt returns without closing its point) breaks that: the query
/// fails with an internal error ([`Context::check`]), and restoring the point releases
/// nothing beyond the table's own charge. The budget keeps every other live charge, here a
/// reservation of 65,536 bytes; nested in an outer point too.
#[test]
fn a_table_consumed_inside_a_later_point_fails_the_query_and_keeps_live_charges() {
    let snapshot = snapshot();
    let options = options(Some(1 << 20));
    for nested in [false, true] {
        let ctx = Context::new(&snapshot, &options, None, None);
        let mut live = Reservation::new(&*ctx.budget);
        live.charge(65_536).unwrap();
        let outer = nested.then(|| ctx.save());
        let rows: Vec<[u64; 1]> = (0..512).map(|i| [i]).collect();
        let table = ctx
            .produced(Solutions {
                vars: vec![Variable::new_unchecked("x")],
                table: IdTable::from_rows(1, rows.iter().map(|row| &row[..])),
                ordered: false,
            })
            .ok()
            .unwrap();
        let bytes = table.table.memory_bytes();
        let before = ctx.budget.used();
        let point = ctx.save();
        ctx.consumed(table);
        ctx.restore(point);
        assert_eq!(
            ctx.budget.used(),
            before - bytes,
            "nested {nested}: the budget keeps every live charge"
        );
        assert!(
            matches!(
                ctx.check().map_err(QueryEvaluationError::from),
                Err(QueryEvaluationError::Unexpected(_))
            ),
            "nested {nested}: the unbalanced ledger fails the query"
        );
        if let Some(outer) = outer {
            ctx.restore(outer);
        }
        assert!(
            ctx.budget.used() >= 65_536,
            "nested {nested}: the live reservation stays"
        );
    }
}

/// What the context keeps to the query's end, by counter: its charge, its computed terms,
/// its remembered numbers (since any open save point too), and its live budget and cache.
fn counters(ctx: &Context<'_>) -> [usize; 5] {
    let since = ctx.since.borrow();
    let remembered = ctx
        .numbers
        .borrow()
        .values()
        .map(|m| m.len())
        .sum::<usize>()
        + since
            .iter()
            .flat_map(|since| since.numbers.values())
            .map(|m| m.len())
            .sum::<usize>();
    [
        ctx.kept.borrow().bytes(),
        ctx.computed.borrow().len(),
        remembered,
        ctx.budget.used(),
        ctx.decoded.borrow().bytes_for_test(),
    ]
}

/// Spec: a restored save point ([`Context::save`]) leaves the context's own counters as they
/// were at the point: its charge, its computed terms, its remembered numbers and the
/// budget's use. Before the point, a pass's numbers and a term, which stay; in it, two
/// passes (each in a point of its own, kept into this one: numbers, and new terms) and a
/// term by itself, all let go.
#[test]
fn a_restored_save_point_leaves_the_context_as_it_was() {
    let snapshot = snapshot();
    let options = options(None);
    let ctx = Context::new(&snapshot, &options, None, None);
    let aggregate = |text: &str| {
        let query = SparqlParser::new().parse_query(text).unwrap();
        let Query::Select { pattern, .. } = &query else {
            panic!("a SELECT")
        };
        let (inner, variables, aggregates) = group_in(pattern);
        let solutions = ctx.eval(inner).ok().expect("no limit");
        let (groups, _) = aggregate::groups_of(&solutions, variables).ok().unwrap();
        let columns = ctx.aggregate_columns(&solutions, &groups.group_of, groups.len(), aggregates);
        assert!(columns.is_ok(), "no limit");
        ctx.consumed(solutions);
    };
    let decimal =
        |lexical: &str| -> Term { Literal::new_typed_literal(lexical, xsd::DECIMAL).into() };
    aggregate(&grouped(&wide(), "(SUM(?v + 0) AS ?t)"));
    // Terms kept from before the point, one past a power of two: a map's room rounds up to
    // a power of two of buckets.
    for i in 0..1_025 {
        let text = format!("a term made before the point, number {i}");
        assert!(ctx.id(&Literal::new_simple_literal(text).into()).is_ok());
    }
    let older: Term = Literal::from("a term made before the point, number 512").into();
    let older_id = ctx.id(&older).ok().unwrap();
    // Start with an empty decoder so everything it adds is speculative.
    release_decoder(&ctx);
    let before = counters(&ctx);
    assert!(before[1] >= 1_025, "the terms are computed: {before:?}");
    let point = ctx.save();
    aggregate(&grouped(&wide(), "(SUM(?v * 3) AS ?t)"));
    aggregate(&grouped(
        &single(),
        &format!("(SUM({XSD_DOUBLE}(?v)) AS ?t)"),
    ));
    assert!(ctx.id(&decimal("1.5")).is_ok());
    let within = counters(&ctx);
    assert!(
        within[1] > before[1] && within[2] > before[2],
        "the attempt made terms and numbers: {before:?}, then {within:?}"
    );
    ctx.restore(point);
    let after = counters(&ctx);
    assert_eq!(
        after[..3],
        before[..3],
        "[kept bytes, computed terms, remembered numbers]"
    );
    assert_eq!(after[4], 0, "no speculative decode bytes survive");
    assert_eq!(after[3], before[3], "live non-cache charges");
    assert_eq!(ctx.term(older_id), Some(older.clone()));
    assert_eq!(ctx.id(&older).ok(), Some(older_id));
    // What stays allocated is what the charge of the terms kept covers: two slots of the
    // list and two of the map per term ([`computed_bytes`]; a small table has three).
    let (terms, ids) = (ctx.computed.borrow(), ctx.computed_ids.borrow());
    assert!(
        terms.capacity() <= 2 * terms.len() && ids.capacity() <= 2 * ids.len().max(2),
        "{} terms in a list of {} and a map of {}",
        terms.len(),
        terms.capacity(),
        ids.capacity()
    );
}

/// A triple term new to the store: `<urn:s> <urn:p> object`.
fn triple(object: Term) -> Term {
    nrese_rdf::Triple::new(
        nrese_rdf::NamedNode::new_unchecked("urn:s"),
        nrese_rdf::NamedNode::new_unchecked("urn:p"),
        object,
    )
    .into()
}

/// Spec: rollback removes a computed ID's first-use alias registered since the mark
/// while the older term and pre-existing aliases keep their identity and charge.
#[test]
fn a_restore_releases_a_new_alias_for_an_older_computed_term() {
    let snapshot = snapshot();
    let ctx = Context::new(&snapshot, &options(None), None, None);
    let term = triple(Literal::from("interned before the point").into());
    let retained = triple(Literal::from("aliased before the point").into());
    let id = ctx.id(&term).ok().unwrap();
    let retained_id = ctx.id(&retained).ok().unwrap();
    let alias = internal::class_constant(id);
    let retained_alias = internal::class_constant(retained_id);
    let evaluate = |alias: &nrese_rdf::NamedNode| {
        ctx.evaluator
            .eval(&Expression::NamedNode(alias.clone()), &|_| None)
    };
    ctx.register_alias(retained_alias.as_str(), retained_id);
    let before = counters(&ctx);
    let point = ctx.save();
    let values = HashMap::from([(Variable::new_unchecked("v"), term.clone())]);
    assert!(substitute::Values::new(&ctx, &values).is_ok());
    assert_eq!(evaluate(&alias), Some(term.clone()));
    ctx.restore(point);
    assert_eq!(
        evaluate(&alias),
        None,
        "the older term's first-use alias is gone"
    );
    assert_eq!(
        ctx.lookup_const(alias.as_ref().into()),
        None,
        "the context alias is gone"
    );
    assert_eq!(evaluate(&retained_alias), Some(retained));
    assert_eq!(counters(&ctx), before, "only pre-existing charges stay");
    assert_eq!(ctx.term(id), Some(term.clone()));
    assert_eq!(ctx.id(&term).ok(), Some(id), "the older term keeps its ID");
}

/// Spec: a restore lets go of the aliases of the terms it lets go, with their charge
/// ([`Context::save`]). A substituted triple term's alias is named by its id; an id made
/// again after the restore, under the same alias, stands for its new term in expressions.
#[test]
fn a_restore_lets_go_of_the_aliases_of_the_terms_it_lets_go() {
    let snapshot = snapshot();
    let options = options(None);
    let ctx = Context::new(&snapshot, &options, None, None);
    let named = |iri: &str| -> Term { nrese_rdf::NamedNode::new_unchecked(iri).into() };
    let alias_of = |alias: &nrese_rdf::NamedNode| {
        ctx.evaluator
            .eval(&Expression::NamedNode(alias.clone()), &|_| None)
    };
    let before = (counters(&ctx), ctx.aliases.borrow().clone());
    let point = ctx.save();
    let id = ctx.id(&triple(named("urn:attempt"))).ok().unwrap();
    let alias = internal::class_constant(id);
    ctx.register_alias(alias.as_str(), id);
    assert_eq!(alias_of(&alias), Some(triple(named("urn:attempt"))));
    ctx.restore(point);
    assert_eq!(alias_of(&alias), None, "the alias after the restore");
    assert_eq!(
        (ctx.aliases.borrow().capacity(), ctx.evaluator.alias_room()),
        (0, 0),
        "no alias is left, nor room for one"
    );
    assert_eq!(
        (counters(&ctx), ctx.aliases.borrow().clone()),
        before,
        "[kept bytes, computed terms, remembered numbers, budget used], aliases"
    );
    let again = ctx.id(&triple(named("urn:retry"))).ok().unwrap();
    assert_eq!(again, id, "the witness needs the id made again");
    ctx.register_alias(alias.as_str(), again);
    assert_eq!(alias_of(&alias), Some(triple(named("urn:retry"))));
}

/// Spec: an alias refused since a save point is let go with it: the query is no longer
/// refused for it after a restore ([`Context::check`]). The triple is stored, so its alias
/// alone is charged: its copied payload, more than the limit.
#[test]
fn a_restore_lets_go_of_an_alias_refused_since_the_point() {
    let payload = "x".repeat(65_536);
    let term = triple(Literal::from(payload.as_str()).into());
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    let mut tx = engine.transaction();
    let subject = nrese_rdf::NamedNode::new_unchecked("urn:r");
    let predicate = nrese_rdf::NamedNode::new_unchecked("urn:p");
    tx.insert(
        nrese_rdf::Quad::new(
            subject,
            predicate,
            term.clone(),
            nrese_rdf::GraphName::DefaultGraph,
        )
        .as_ref(),
    );
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let options = options(Some(payload.len() - 1));
    let ctx = Context::new(&snapshot, &options, None, None);
    assert!(ctx.check().is_ok(), "the reader fits before the alias");
    let point = ctx.save();
    let id = ctx.id(&term).ok().unwrap();
    assert_eq!(computed_index(id), None, "a stored term");
    ctx.register_alias(internal::class_constant(id).as_str(), id);
    assert!(ctx.check().is_err(), "the alias does not fit");
    ctx.restore(point);
    assert!(ctx.check().is_ok(), "refused within the point only");
}

/// Spec: an expression's numbers past the memo's limit ([`NUMBER_MEMO_ENTRIES`]) are let go
/// with their charge when a pass's point is kept, inside outer points (a streamed GROUP BY's
/// direct attempt, and a GROUP BY's attempt within it) as outside them, counting what every
/// point holds for the expression: a pass holds no more numbers within attempts than
/// without them. The outer attempt holds half the limit from an earlier pass; the pass
/// finds one number more than the other half, charged as [`Context::numeric_pass`] charges
/// them.
#[test]
fn a_point_kept_inside_another_lets_go_of_numbers_past_the_limit() {
    let snapshot = snapshot();
    let options = options(None);
    let ctx = Context::new(&snapshot, &options, None, None);
    let v = Variable::new_unchecked("v");
    let remember = |layer: usize, ids: std::ops::Range<usize>| {
        let mut since = ctx.since.borrow_mut();
        let layer = &mut since[layer];
        assert!(layer.held.charge(ids.len() * number_bytes()).is_ok());
        layer
            .numbers
            .entry((Expression::Variable(v.clone()), v.clone()))
            .or_default()
            .extend(ids.map(|id| (id as u64, Number::Error)));
    };
    let outer = ctx.save();
    let half = NUMBER_MEMO_ENTRIES / 2;
    remember(0, 0..half);
    let before = counters(&ctx);
    let inner = ctx.save();
    let pass = ctx.save();
    remember(2, half..NUMBER_MEMO_ENTRIES + 1);
    ctx.keep(pass);
    assert_eq!(
        counters(&ctx),
        before,
        "[kept bytes, computed terms, remembered numbers, budget used] once the pass's point is kept"
    );
    ctx.keep(inner);
    ctx.keep(outer);
}
