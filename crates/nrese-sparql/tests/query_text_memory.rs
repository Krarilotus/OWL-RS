//! Guard of allocations the query's text multiplies (notes/2026-11-02-column-budget.md): a
//! projection makes a column per unbound variable and a UNION one per variable a side
//! lacks, so k variables in the text make k columns of a charged table's rows; and a
//! sequence of alternatives multiplies the ways through a node, `(p|p)/(p|p)` reaching it
//! four times over one statement. Both draw from the query's budget where they are made:
//! the columns are charged before they are made, and a path's ends are held once each,
//! with how often they are reached. Measured with a counting allocator, so this file is a
//! test binary of its own.

use std::collections::BTreeMap;

use nrese_engine::{Engine, EngineConfig};
use nrese_exec::{Envelope, SharedBudget, heap};
use nrese_rdf::{GraphName, NamedNode, Quad};
use nrese_sparql::{QueryEvaluationError, QueryOptions, QueryResults, Resources, evaluate_query};
use nrese_sparql_syntax::SparqlParser;

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

/// The query's memory limit.
const LIMIT: usize = 1 << 20;

/// What the query's thread may hold beyond its limit: the parsed query, its plan and the
/// evaluator's per-call bookkeeping, which grow with the query's text, not its rows.
const BOOKKEEPING: usize = 1 << 20;

const EX: &str = "http://example.com/";

/// Statements `ex:s<i> ex:p ex:o<i>` for `0..ROWS`: one column of them is 32 KiB.
const ROWS: usize = 1 << 12;

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

fn engine(statements: impl IntoIterator<Item = Quad>) -> Engine {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    let mut tx = engine.transaction();
    for quad in statements {
        tx.insert(quad.as_ref());
    }
    tx.commit().unwrap();
    engine
}

/// The outcome of `query` under [`LIMIT`] (its rows) and the most its thread held.
fn run(
    engine: &Engine,
    query: &str,
    as_written: bool,
) -> (Result<usize, QueryEvaluationError>, usize) {
    let snapshot = engine.snapshot();
    let options = QueryOptions {
        as_written,
        resources: Resources {
            memory: Envelope::new(Some(LIMIT), None),
            ..Resources::default()
        },
        ..QueryOptions::default()
    };
    let query = SparqlParser::new().parse_query(query).unwrap();
    // The profile is the process's: one measured run at a time.
    static MEASURING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _alone = MEASURING.lock().unwrap_or_else(|p| p.into_inner());
    heap::start("query-text");
    heap::begin_step();
    let outcome = match evaluate_query(&snapshot, &query, &options) {
        Ok(QueryResults::Solutions(mut rows)) => rows.try_fold(0, |n, row| row.map(|_| n + 1)),
        Ok(QueryResults::Graph(mut triples)) => triples.try_fold(0, |n, t| t.map(|_| n + 1)),
        Ok(_) => panic!("solutions or a graph"),
        Err(error) => Err(error),
    };
    heap::end_step("query-text");
    let peak = heap::steps().last().unwrap().peak;
    let _ = heap::finish();
    (outcome, peak)
}

/// A solution: each bound variable's value in N-Triples.
type Row = BTreeMap<String, String>;

fn row(bindings: &[(&str, String)]) -> Row {
    bindings
        .iter()
        .map(|(variable, value)| ((*variable).to_owned(), value.clone()))
        .collect()
}

/// `"n"^^xsd:integer` in N-Triples.
fn integer(n: usize) -> String {
    format!("\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>")
}

/// The solutions of `query` with at most `memory` bytes (none: no limit), sorted: a
/// SELECT's answer is a bag, and where ORDER BY leaves ties their order is not specified.
/// Unmeasured, apart from [`run`]'s measurement.
fn answers(
    engine: &Engine,
    query: &str,
    as_written: bool,
    memory: Option<usize>,
) -> Result<Vec<Row>, String> {
    let snapshot = engine.snapshot();
    let options = QueryOptions {
        as_written,
        resources: Resources {
            memory: Envelope::new(memory, None),
            ..Resources::default()
        },
        ..QueryOptions::default()
    };
    let query = SparqlParser::new().parse_query(query).unwrap();
    let QueryResults::Solutions(rows) =
        evaluate_query(&snapshot, &query, &options).map_err(|e| e.to_string())?
    else {
        panic!("solutions");
    };
    let mut rows = rows
        .map(|solution| {
            let solution = solution.map_err(|e| e.to_string())?;
            Ok(solution
                .iter()
                .map(|(variable, value)| (variable.as_str().to_owned(), value.to_string()))
                .collect())
        })
        .collect::<Result<Vec<Row>, String>>()?;
    rows.sort();
    Ok(rows)
}

/// Where `got` and `expected` (both sorted) differ, in a line: their lengths and the first
/// solution that differs.
fn difference(got: &[Row], expected: &[Row]) -> Option<String> {
    if got == expected {
        return None;
    }
    let first = got.iter().zip(expected).find(|(g, e)| g != e);
    Some(format!(
        "{} solutions, not {}; first difference: {first:?}",
        got.len(),
        expected.len()
    ))
}

/// Spec: a query's intermediate results draw from its memory envelope (`Resources::memory`,
/// the v1.5 bar's "no crash"), wherever their size comes from. Over 4,096 rows, 256
/// unbound variables projected, or a UNION with a side binding 256 others, make 8 MiB of
/// columns (256 × 4,096 × 8 bytes): more than 1 MiB, so the query fails with
/// `MemoryLimit`, its thread holding at most the limit and its bookkeeping. 4 variables
/// make 128 KiB, which fits: answered.
/// On the default path and as written alike.
#[test]
fn columns_made_per_variable_draw_from_the_query_budget() {
    let p = ex("p");
    let engine = engine((0..ROWS).map(|i| {
        let (subject, object) = (ex(&format!("s{i}")), ex(&format!("o{i}")));
        Quad::new(subject, p.clone(), object, GraphName::DefaultGraph)
    }));
    let rows = format!("?s <{EX}p> ?o");
    for (k, fits) in [(4usize, true), (256, false)] {
        let vars = (0..k)
            .map(|i| format!("?a{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let queries = [
            (
                "unbound variables projected",
                format!("SELECT ?s {vars} WHERE {{ {rows} }}"),
            ),
            (
                "a UNION's other side's variables",
                format!("SELECT * WHERE {{ {{ {rows} }} UNION {{ VALUES ({vars}) {{ }} }} }}"),
            ),
        ];
        for (case, query) in &queries {
            for as_written in [false, true] {
                let (outcome, peak) = run(&engine, query, as_written);
                let case = format!("{case}, {k} of them, as_written={as_written}");
                if fits {
                    assert_eq!(outcome.map_err(|e| e.to_string()), Ok(ROWS), "{case}");
                } else {
                    assert!(
                        matches!(outcome, Err(QueryEvaluationError::MemoryLimit(_))),
                        "{case}: {:?}, not the memory error",
                        outcome.map_err(|e| e.to_string())
                    );
                }
                assert!(
                    peak <= LIMIT + BOOKKEEPING,
                    "{case}: the query's thread held {peak} bytes under a {LIMIT}-byte limit"
                );
            }
        }
    }
}

/// Spec: SPARQL 1.1 §18.4: `p+` reaches each end once per start, however many ways lead
/// there. Over one statement `ex:a ex:p ex:a`, a sequence of 20 `(ex:p|ex:p)` has 2^20
/// ways from `ex:a` to `ex:a`, one end: `ex:a ((ex:p|ex:p)/…)+ ?y` answers one row, and
/// the query's thread holds at most the limit and its bookkeeping (a copy of the end per
/// way through would be 8 MiB). On the default path and as written alike.
#[test]
fn a_closure_over_a_sequence_holds_its_ends_once() {
    let engine = engine([Quad::new(
        ex("a"),
        ex("p"),
        ex("a"),
        GraphName::DefaultGraph,
    )]);
    let sequence = vec![format!("(<{EX}p>|<{EX}p>)"); 20].join("/");
    let query = format!("SELECT ?y WHERE {{ <{EX}a> ({sequence})+ ?y }}");
    for as_written in [false, true] {
        let (outcome, peak) = run(&engine, &query, as_written);
        assert_eq!(
            outcome.map_err(|e| e.to_string()),
            Ok(1),
            "as_written={as_written}"
        );
        assert!(
            peak <= LIMIT + BOOKKEEPING,
            "as_written={as_written}: the query's thread held {peak} bytes under a \
             {LIMIT}-byte limit"
        );
    }
}

/// Spec: as above, for the other places where the query's text multiplies what a query
/// makes per row (notes/2026-11-02-column-budget.md CB-6, S1-S6). Over 4,096 rows, 256 of
/// each make 8 MiB or more (256 × 4,096 × 8 bytes) where each is a column or a term per
/// row: a column per EXISTS (S1), per GROUP BY variable (S2), per LATERAL variable put in
/// (S4), a sort key per
/// ORDER BY expression (S3), the 256 strings a CONCAT of 256 parts interns per row (S5):
/// more than 1 MiB, so the query fails with `MemoryLimit`. What only repeats what is there
/// makes nothing: ORDER BY unbound variables sorts by nothing (§15.1: unbound sorts first,
/// so all rows tie), and a closure over 256 times the same link reaches what one does
/// (§18.4: `p+` is a set), each answered. A CONSTRUCT of 256 template triples makes 1 M
/// triples, or 16 MiB from one row with a 64 KiB literal, and SAMPLE of that CONCAT a 6 KiB
/// string per group (4,096 groups), and a GROUP_CONCAT whose 4 KiB separator comes
/// between each two of 4,096 values (16 MiB): refused; over IRIs it has no string
/// (§18.5.1.7: unbound), so nothing to make, however long its separator: answered.
/// Either way, the query's thread holds at most the limit and its bookkeeping. An answered
/// query's solutions, under the same limit: ORDER BY of unbound variables each subject
/// once; the closure each `(ex:s<i>, ex:o<i>)` (the 4,096 links, none longer); the
/// GROUP_CONCAT `"none"` (COALESCE of its unbound value). On the default path and as
/// written alike.
#[test]
fn what_the_query_text_multiplies_draws_from_the_query_budget() {
    let p = ex("p");
    // And one statement whose object is a 64 KiB literal.
    let large = nrese_rdf::Literal::new_simple_literal("x".repeat(1 << 16));
    let large = Quad::new(ex("large"), ex("q"), large, GraphName::DefaultGraph);
    let engine = engine(
        (0..ROWS)
            .map(|i| {
                let (subject, object) = (ex(&format!("s{i}")), ex(&format!("o{i}")));
                Quad::new(subject, p.clone(), object, GraphName::DefaultGraph)
            })
            .chain([large]),
    );
    let rows = format!("?s <{EX}p> ?o");
    let list = |item: &dyn Fn(usize) -> String, separator: &str| {
        (0..256).map(item).collect::<Vec<_>>().join(separator)
    };
    let exists = list(&|_| format!("EXISTS {{ {rows} }}"), " || ");
    let unbound = list(&|i| format!("?u{i}"), " ");
    let left = list(&|i| format!("?a{i}"), " ");
    let ones = list(&|_| "1".to_owned(), " ");
    let mentioned = list(&|i| format!("?a{i} != 0"), " && ");
    let keys = list(&|i| format!("CONCAT(STR(?s), \"{i}\")"), " ");
    let parts = list(&|_| "STR(?s)".to_owned(), ", ");
    let links = list(&|_| format!("<{EX}p>"), "|");
    let template = list(&|i| format!("?s <{EX}t{i}> ?o ."), " ");
    let separator = "x".repeat(1 << 12);
    let subjects: Vec<Row> = (0..ROWS)
        .map(|i| row(&[("s", format!("<{EX}s{i}>"))]))
        .collect();
    let reached: Vec<Row> = (0..ROWS)
        .map(|i| row(&[("x", format!("<{EX}s{i}>")), ("y", format!("<{EX}o{i}>"))]))
        .collect();
    // The solutions of each answered case (None: the memory error).
    let cases: [(&str, String, Option<Vec<Row>>); 13] = [
        (
            "S1 FILTER of 256 EXISTS",
            format!("SELECT ?s WHERE {{ {rows} FILTER({exists}) }}"),
            None,
        ),
        (
            "S1 BIND of 256 EXISTS",
            format!("SELECT ?s WHERE {{ {rows} BIND(({exists}) AS ?b) }}"),
            None,
        ),
        (
            "S2 GROUP BY 256 unbound variables",
            format!("SELECT ?s WHERE {{ {rows} }} GROUP BY ?s {unbound}"),
            None,
        ),
        (
            "S3 ORDER BY 256 expressions",
            format!("SELECT ?s WHERE {{ {rows} }} ORDER BY {keys}"),
            None,
        ),
        (
            "S3 ORDER BY 256 unbound variables",
            format!("SELECT ?s WHERE {{ {rows} }} ORDER BY {unbound}"),
            Some(subjects),
        ),
        (
            "S4 LATERAL putting in 256 variables",
            format!(
                "SELECT ?s WHERE {{ VALUES ({left}) {{ ({ones}) }} \
                 LATERAL {{ {rows} FILTER({mentioned}) }} }}"
            ),
            None,
        ),
        (
            "S5 CONCAT of 256 parts",
            format!("SELECT ?x WHERE {{ {rows} BIND(CONCAT({parts}) AS ?x) }}"),
            None,
        ),
        (
            "S6 closure of 256 times one link",
            format!("SELECT ?x ?y WHERE {{ ?x ({links})+ ?y }}"),
            Some(reached),
        ),
        (
            "CONSTRUCT of 256 template triples",
            format!("CONSTRUCT {{ {template} }} WHERE {{ {rows} }}"),
            None,
        ),
        (
            "CONSTRUCT of 256 template triples of one 64 KiB literal",
            format!("CONSTRUCT {{ {template} }} WHERE {{ ?s <{EX}q> ?o }}"),
            None,
        ),
        (
            "SAMPLE of a CONCAT of 256 parts per group",
            format!("SELECT ?s (SAMPLE(CONCAT({parts})) AS ?v) WHERE {{ {rows} }} GROUP BY ?s"),
            None,
        ),
        (
            "GROUP_CONCAT with a 4 KiB separator",
            format!(
                "SELECT (GROUP_CONCAT(STR(?s); SEPARATOR=\"{separator}\") AS ?v) \
                 WHERE {{ {rows} }}"
            ),
            None,
        ),
        (
            "GROUP_CONCAT of 64 IRIs with a 32 KiB separator (unbound)",
            format!(
                "SELECT (COALESCE(GROUP_CONCAT(?s; SEPARATOR=\"{}\"), \"none\") AS ?v) \
                 WHERE {{ {{ SELECT ?s WHERE {{ {rows} }} LIMIT 64 }} }}",
                "x".repeat(1 << 15)
            ),
            Some(vec![row(&[("v", "\"none\"".to_owned())])]),
        ),
    ];
    // Every case runs, and each one that fails is listed.
    let mut failed = Vec::new();
    for (case, query, expected) in &cases {
        let answered = expected.as_ref().map(Vec::len);
        for as_written in [false, true] {
            let (outcome, peak) = run(&engine, query, as_written);
            let case = format!("{case}, as_written={as_written}");
            let outcome = match outcome {
                Err(QueryEvaluationError::MemoryLimit(_)) => None,
                other => Some(other.map_err(|e| e.to_string())),
            };
            if outcome != answered.map(Ok) {
                failed.push(format!(
                    "{case}: {outcome:?}, not {answered:?} (None: the memory error)"
                ));
            } else if let Some(expected) = expected {
                let mut expected = expected.clone();
                expected.sort();
                match answers(&engine, query, as_written, Some(LIMIT)) {
                    Ok(got) => {
                        failed.extend(difference(&got, &expected).map(|d| format!("{case}: {d}")))
                    }
                    Err(error) => failed.push(format!("{case}: {error}, unmeasured")),
                }
            }
            if peak > LIMIT + BOOKKEEPING {
                failed.push(format!(
                    "{case}: held {peak} bytes under a {LIMIT}-byte limit"
                ));
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// Spec: SPARQL 1.1 §11.2: grouping on `?s ?s` partitions as grouping on `?s`, so a key the
/// query names 256 times is one key, and what the grouping holds per row grows with the
/// distinct keys, not with the text (PR #89 review r1, R89-1). Two projected subqueries
/// sharing no variable, `{ SELECT ?s … } { SELECT ?a … }`, over the 4,096 statements
/// `ex:s<i> ex:p ex:o<i>`, grouped by 256 times `?s`, count their 16 M-row product: with
/// no limit each subject once, with a count of 4,096 (default path; written as is, the
/// product is made whole, 256 MiB). Under 1 MiB the product does not fit: refused with
/// `MemoryLimit`, the query's thread holding at most the limit and its bookkeeping (a key
/// of 256 ids per row would be 8 MiB). On the default path and as written alike.
#[test]
fn a_crossed_group_by_a_key_named_again_draws_from_the_query_budget() {
    let p = ex("p");
    let engine = engine((0..ROWS).map(|i| {
        let (subject, object) = (ex(&format!("s{i}")), ex(&format!("o{i}")));
        Quad::new(subject, p.clone(), object, GraphName::DefaultGraph)
    }));
    let keys = vec!["?s"; 256].join(" ");
    let query = format!(
        "SELECT ?s (COUNT(*) AS ?count) WHERE {{ \
         {{ SELECT ?s WHERE {{ ?s <{EX}p> ?o }} }} \
         {{ SELECT ?a WHERE {{ ?a <{EX}p> ?b }} }} }} GROUP BY {keys}"
    );
    let mut failed = Vec::new();
    for as_written in [false, true] {
        let (outcome, peak) = run(&engine, &query, as_written);
        if !matches!(outcome, Err(QueryEvaluationError::MemoryLimit(_))) {
            failed.push(format!(
                "as_written={as_written}: {:?}, not the memory error",
                outcome.map_err(|e| e.to_string())
            ));
        }
        if peak > LIMIT + BOOKKEEPING {
            failed.push(format!(
                "as_written={as_written}: held {peak} bytes under a {LIMIT}-byte limit"
            ));
        }
    }
    let mut expected: Vec<Row> = (0..ROWS)
        .map(|i| row(&[("s", format!("<{EX}s{i}>")), ("count", integer(ROWS))]))
        .collect();
    expected.sort();
    match answers(&engine, &query, false, None) {
        Ok(got) => failed.extend(difference(&got, &expected).map(|d| format!("no limit: {d}"))),
        Err(error) => failed.push(format!("no limit: {error}")),
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// Spec: SPARQL 1.1 §18.4: an alternative is a UNION of its arms' bags, and `p+` and `p?`
/// are sets. Over n self-loops `ex:n<i> ex:p ex:n<i>`, a hub `ex:h ex:p ex:n<i>` and
/// `ex:m ex:q ex:m`, the k arms `!(ex:q)`, `!(ex:q|ex:q)`, … (arm i names `ex:q` i times:
/// k distinct arms, each every `ex:p` statement) give each of the 2n `ex:p` pairs k times;
/// under `+` each pair once (the self-pairs and the hub's), from the hub each `ex:n<i>`
/// once, under `?` each pair once and every node with itself (2n + 2), COUNT under `+` 2n,
/// `^(…)+` each reversed pair once. Outside a closure the bag keeps its copies: 4 arms
/// answer each pair 4 times; 64 arms make 128n rows, over 1 MiB for n = 1,024 and 4,096:
/// `MemoryLimit`. Each way, the query's thread holds at most the limit and its bookkeeping
/// (holding every arm's bag at once would be 64 × 2n × 16 bytes, 8 MiB for n = 4,096; the
/// package review r1 of the executor memory stack, EMS-R1). On the default path and as
/// written alike.
#[test]
fn overlapping_alternative_arms_draw_from_the_query_budget() {
    let mut failed = Vec::new();
    for n in [1usize << 10, 1 << 12] {
        let (p, hub) = (ex("p"), ex("h"));
        let node = |i: usize| ex(&format!("n{i}"));
        let marker = Quad::new(ex("m"), ex("q"), ex("m"), GraphName::DefaultGraph);
        let engine = engine(
            (0..n)
                .flat_map(|i| {
                    [
                        Quad::new(node(i), p.clone(), node(i), GraphName::DefaultGraph),
                        Quad::new(hub.clone(), p.clone(), node(i), GraphName::DefaultGraph),
                    ]
                })
                .chain([marker]),
        );
        let iri = |name: &str| format!("<{EX}{name}>");
        let selves = || (0..n).map(|i| (iri(&format!("n{i}")), iri(&format!("n{i}"))));
        let spokes = || (0..n).map(|i| (iri("h"), iri(&format!("n{i}"))));
        let pairs = |pairs: Vec<(String, String)>| -> Vec<Row> {
            let rows = pairs.into_iter().map(|(x, y)| row(&[("x", x), ("y", y)]));
            rows.collect()
        };
        for k in [4usize, 64] {
            let arms = (1..=k)
                .map(|i| format!("!({})", vec![iri("q"); i].join("|")))
                .collect::<Vec<_>>()
                .join(" | ");
            let once: Vec<(String, String)> = selves().chain(spokes()).collect();
            let copies = once
                .iter()
                .flat_map(|pair| std::iter::repeat_n(pair.clone(), k));
            let mut zero_or_one = once.clone();
            zero_or_one.extend([(iri("h"), iri("h")), (iri("m"), iri("m"))]);
            let reversed = once.iter().map(|(x, y)| (y.clone(), x.clone())).collect();
            let ends = (0..n)
                .map(|i| row(&[("y", iri(&format!("n{i}")))]))
                .collect();
            let count = vec![row(&[("c", integer(2 * n))])];
            // The solutions of each case (None: the memory error).
            let cases: [(&str, String, Option<Vec<Row>>); 6] = [
                (
                    "under +",
                    format!("SELECT ?x ?y WHERE {{ ?x ({arms})+ ?y }}"),
                    Some(pairs(once.clone())),
                ),
                (
                    "from the hub under +",
                    format!("SELECT ?y WHERE {{ {} ({arms})+ ?y }}", iri("h")),
                    Some(ends),
                ),
                (
                    "under ?",
                    format!("SELECT ?x ?y WHERE {{ ?x ({arms})? ?y }}"),
                    Some(pairs(zero_or_one)),
                ),
                (
                    "reversed under +",
                    format!("SELECT ?x ?y WHERE {{ ?x (^({arms}))+ ?y }}"),
                    Some(pairs(reversed)),
                ),
                (
                    "COUNT under +",
                    format!("SELECT (COUNT(*) AS ?c) WHERE {{ ?x ({arms})+ ?y }}"),
                    Some(count),
                ),
                (
                    "outside a closure",
                    format!("SELECT ?x ?y WHERE {{ ?x ({arms}) ?y }}"),
                    (k == 4).then(|| pairs(copies.collect())),
                ),
            ];
            for (case, query, expected) in &cases {
                let answered = expected.as_ref().map(Vec::len);
                for as_written in [false, true] {
                    let (outcome, peak) = run(&engine, query, as_written);
                    let case = format!("{case}, {k} arms, n={n}, as_written={as_written}");
                    let outcome = match outcome {
                        Err(QueryEvaluationError::MemoryLimit(_)) => None,
                        other => Some(other.map_err(|e| e.to_string())),
                    };
                    if outcome != answered.map(Ok) {
                        failed.push(format!(
                            "{case}: {outcome:?}, not {answered:?} (None: the memory error)"
                        ));
                    } else if let Some(expected) = expected {
                        let mut expected = expected.clone();
                        expected.sort();
                        match answers(&engine, query, as_written, Some(LIMIT)) {
                            Ok(got) => failed.extend(
                                difference(&got, &expected).map(|d| format!("{case}: {d}")),
                            ),
                            Err(error) => failed.push(format!("{case}: {error}, unmeasured")),
                        }
                    }
                    if peak > LIMIT + BOOKKEEPING {
                        failed.push(format!(
                            "{case}: held {peak} bytes under a {LIMIT}-byte limit"
                        ));
                    }
                }
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// Spec: a table the query holds stays charged while it lives (`Resources::memory`): the
/// live result of a SELECT keeps at least a word a row and column charged. A projection
/// that drops a column must release only that column's charge: the graph's column that
/// `GRAPH ?g` adds per graph (one named graph here), and one of two columns a `GROUP BY`
/// of the same variable twice makes (SPARQL 1.1 §11.2: grouping on `?x ?x` groups on
/// `?x`; one row). On the default path and as written alike.
#[test]
fn projections_release_only_the_columns_they_drop() {
    let graph = GraphName::NamedNode(ex("g"));
    let engine = engine([Quad::new(ex("a"), ex("p"), ex("a"), graph)]);
    let values = (0..1_000).map(|i| i.to_string()).collect::<Vec<_>>();
    let cases = [
        (
            "the graph's column dropped",
            format!(
                "SELECT ?x WHERE {{ GRAPH ?g {{ VALUES ?x {{ {} }} }} }}",
                values.join(" ")
            ),
            1_000,
        ),
        (
            "a variable grouped on twice",
            "SELECT ?x WHERE { VALUES ?x { 1 } } GROUP BY ?x ?x".to_owned(),
            1,
        ),
    ];
    for (case, query, expected) in &cases {
        for as_written in [false, true] {
            let shared = SharedBudget::new(1 << 30);
            let options = QueryOptions {
                as_written,
                resources: Resources {
                    memory: Envelope::new(None, Some(shared.clone())),
                    ..Resources::default()
                },
                ..QueryOptions::default()
            };
            let parsed = SparqlParser::new().parse_query(query).unwrap();
            let snapshot = engine.snapshot();
            let result = evaluate_query(&snapshot, &parsed, &options);
            let held = shared.used();
            let Ok(QueryResults::Solutions(rows)) = result else {
                panic!("{case}, as_written={as_written}: no solutions");
            };
            let rows = rows.count();
            assert_eq!(rows, *expected, "{case}, as_written={as_written}");
            assert!(
                held >= rows * 8,
                "{case}, as_written={as_written}: the live result of {rows} rows keeps \
                 {held} bytes charged"
            );
        }
    }
}
