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
    let cases: [(String, usize, &dyn Fn(&[(i64, Option<Term>)]) -> bool); 4] = [
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
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Which route aggregates the groups: the general one, from one pass where it applies and
/// fits ([`Context::aggregate_columns`]), or an aggregate at a time alone
/// ([`Context::aggregate_column`]).
#[derive(Clone, Copy)]
enum Route {
    General,
    PerColumn,
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
/// once the input is consumed and its cache of decoded terms let go (a cache is never
/// needed for an answer, and holds what room its last term found).
fn aggregated(
    snapshot: &Snapshot,
    query: &Query,
    limit: Option<usize>,
    route: Route,
) -> (Result<Vec<String>, String>, usize) {
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
    let solutions = match ctx.eval(inner) {
        Ok(solutions) => solutions,
        Err(error) => return (Err(refused(error)), ctx.budget.used()),
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
    let text = |id: u64| ctx.term(id).map_or("-".to_owned(), |term| term.to_string());
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
    *ctx.decoded.borrow_mut() =
        decode_cache::DecodeCache::new(&ctx.budget, DECODE_CACHE_ENTRIES, 1);
    (answer, ctx.budget.used())
}

/// The shapes the sweep runs: the three above, and others of either pass.
fn shapes() -> Vec<String> {
    let (wide, single) = (wide(), single());
    vec![
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

/// Spec: under every limit of a sweep from 200 kB to 4.5 MB, the general route answers as the
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
        for limit in (2..=45).map(|k| k * 100_000) {
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
            Ok(_) if after != alone => wrong.push(format!(
                "{shape} at {limit}: {after} bytes held afterwards, the per-column route {alone}"
            )),
            Ok(_) => {}
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// 20,000 subjects, each with `p0` its number modulo 100 and `p1` its number modulo 2,000:
/// their join is 20,000 rows in 2,000 groups of `?y`.
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

/// `query` under `options` in a context of its own: its rows as text (sorted), or the
/// refusal; and what the context holds once the rows are let go and its cache of decoded
/// terms too.
fn evaluated(
    snapshot: &Snapshot,
    query: &Query,
    options: &QueryOptions,
) -> (Result<Vec<String>, String>, usize) {
    let Query::Select { pattern, .. } = query else {
        panic!("a SELECT")
    };
    let ctx = Context::new(snapshot, options, None, None);
    let answer = match ctx.eval(pattern) {
        Ok(solutions) => {
            let table = &solutions.table;
            let text = |id: u64| ctx.term(id).map_or("-".to_owned(), |term| term.to_string());
            let mut rows: Vec<String> = (0..table.len())
                .map(|r| {
                    let row = (0..table.width()).map(|c| text(table.get(r, c)));
                    row.collect::<Vec<_>>().join(" ")
                })
                .collect();
            rows.sort();
            ctx.consumed(solutions);
            Ok(rows)
        }
        Err(error) => match QueryEvaluationError::from(error) {
            QueryEvaluationError::MemoryLimit(refused) => Err(format!("{refused:?}")),
            other => panic!("{other:?}"),
        },
    };
    *ctx.decoded.borrow_mut() =
        decode_cache::DecodeCache::new(&ctx.budget, DECODE_CACHE_ENTRIES, 1);
    (answer, ctx.budget.used())
}

/// Spec: a GROUP BY refused directly and answered in morsels instead ([`Context::group`])
/// is answered from what the query held before the direct attempt: with the morsels' answer,
/// the budget then reads what the streamed route alone leaves (its results' terms), not
/// that and what the direct attempt kept (its numbers and terms). Over limits from 200 kB
/// to 4 MB: where the query as written (direct only, no retry) is refused and the query
/// answers, the retry ran; at least one limit must be such.
#[test]
fn a_streamed_retry_holds_nothing_of_the_refused_direct_attempt() {
    let snapshot = joined();
    let text = "SELECT ?y (AVG(?x * 2) AS ?a) \
                WHERE { ?s <urn:p0> ?x . ?s <urn:p1> ?y } GROUP BY ?y";
    let query = SparqlParser::new().parse_query(text).unwrap();
    let with = |limit: usize, as_written: bool, stream_rows: Option<usize>| QueryOptions {
        as_written,
        stream_rows,
        ..options(Some(limit))
    };
    let (mut wrong, mut retried) = (Vec::new(), 0);
    for limit in (2..=40).map(|k| k * 100_000) {
        let (general, after) = evaluated(&snapshot, &query, &with(limit, false, None));
        let (streamed, alone) = evaluated(&snapshot, &query, &with(limit, false, Some(4_096)));
        let (direct, _) = evaluated(&snapshot, &query, &with(limit, true, None));
        match (&general, &streamed) {
            (Err(refused), Ok(_)) => wrong.push(format!(
                "at {limit}: refused ({refused}) where the streamed route answers"
            )),
            (Ok(general), Ok(streamed)) if general != streamed => {
                wrong.push(format!("at {limit}: the routes answer differently"));
            }
            (Ok(_), Ok(_)) if direct.is_err() && after != alone => wrong.push(format!(
                "at {limit}: {after} bytes held after the retry, the streamed route {alone}"
            )),
            _ => {}
        }
        retried += usize::from(general.is_ok() && direct.is_err());
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert!(
        retried > 0,
        "no limit had the direct attempt refused and the query answered"
    );
}
