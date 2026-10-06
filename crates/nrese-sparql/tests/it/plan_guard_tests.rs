//! Guards for the query engine's measured wins (docs/design/performance.md §0): each
//! query below must still run with the operator that made it fast, read from EXPLAIN.
//! A planner change that loses one fails here at once, not in a benchmark run.

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad};
use nrese_sparql::{PlanStep, QueryOptions, explain_query};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.org/";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

/// A store of 3,000 nodes: a cyclic `knows` graph (each node knows three others), a
/// `label` per node and a `kind` among ten.
fn engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for i in 0..3000u32 {
        let node = ex(&format!("n{i}"));
        for step in [1, 7, 31] {
            tx.insert(
                Quad::new(
                    node.clone(),
                    ex("knows"),
                    ex(&format!("n{}", (i + step) % 3000)),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            );
        }
        tx.insert(
            Quad::new(
                node.clone(),
                ex("label"),
                Literal::new_simple_literal(format!("label number {i}")),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
        tx.insert(
            Quad::new(
                node,
                ex("kind"),
                ex(&format!("k{}", i % 10)),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    tx.commit().unwrap();
    engine
}

/// How `query` ran: every operator with its estimated and actual rows.
fn steps(engine: &Engine, query: &str) -> Vec<PlanStep> {
    let text = format!("PREFIX e: <{EX}> {query}");
    let query = SparqlParser::new()
        .parse_query(&text)
        .unwrap_or_else(|e| panic!("{e}: {text}"));
    explain_query(&engine.snapshot(), &query, &QueryOptions::default())
        .unwrap()
        .steps
}

/// The operators `query` ran with.
fn operators(engine: &Engine, query: &str) -> Vec<String> {
    steps(engine, query)
        .into_iter()
        .map(|s| s.operator)
        .collect()
}

fn assert_uses(engine: &Engine, query: &str, operator: &str) {
    let used = operators(engine, query);
    assert!(
        used.iter().any(|o| o == operator),
        "{query}\nno {operator:?} among {used:?}"
    );
}

/// Group counts on the index: COUNT … GROUP BY one variable of one pattern reads run
/// lengths of the permutation sorted on it (Wikidata q07: 2,101 → 0.18 ms).
#[test]
fn group_counts_walk_the_index() {
    assert_uses(
        &engine(),
        "SELECT ?k (COUNT(*) AS ?n) WHERE { ?s e:kind ?k } GROUP BY ?k",
        "group count",
    );
}

/// The distinct values of a variable by a group walk where a set suffices, as for the
/// side of a NOT EXISTS (DBpedia q13, people without a death date: 25 → 3.1 ms).
#[test]
fn distinct_values_walk_the_index() {
    assert_uses(
        &engine(),
        "SELECT (COUNT(*) AS ?n) WHERE { ?s e:kind ?k FILTER NOT EXISTS { ?s e:label ?l } }",
        "group walk",
    );
}

/// Worst-case-optimal joins for cyclic patterns (LUBM 100 q2: 18 → 4.8 ms).
#[test]
fn triangles_join_worst_case_optimally() {
    assert_uses(
        &engine(),
        "SELECT * WHERE { ?a e:knows ?b . ?b e:knows ?c . ?c e:knows ?a }",
        "wcoj",
    );
}

/// Eager aggregation: a group over a join aggregates the side its aggregates read before
/// the join, `AVG`, `MIN` and `MAX` included (BSBM BI q4's shape, an average of offer
/// prices per product feature).
#[test]
fn averages_over_joins_aggregate_before_joining() {
    let text = format!(
        "PREFIX e: <{EX}> SELECT ?k (AVG(STRLEN(?l)) AS ?mean) (MAX(?l) AS ?last) WHERE {{ ?s e:kind ?k . ?s e:knows ?o . ?o e:label ?l }} GROUP BY ?k"
    );
    let query = SparqlParser::new().parse_query(&text).unwrap();
    let explained = explain_query(&engine().snapshot(), &query, &QueryOptions::default()).unwrap();
    assert!(
        explained.rewrites.contains(&"eager-aggregation"),
        "{:?}",
        explained.rewrites
    );
    assert_eq!(explained.rows, 10);
}

/// String filters on the dictionary: CONTAINS on a large pattern's object runs once per
/// distinct term, and the passing ids semi-join the scan.
#[test]
fn string_filters_test_the_dictionary() {
    assert_uses(
        &engine(),
        "SELECT ?s WHERE { ?s e:label ?l FILTER(CONTAINS(?l, \"number 12\")) }",
        "dictionary string test",
    );
}

/// Sideways information passing: a pattern joined to rows already computed is evaluated
/// from them, probing the index per row (Zebratlas Q03: about 1,200x).
#[test]
fn patterns_joined_to_rows_are_evaluated_from_them() {
    let steps = steps(
        &engine(),
        "SELECT * WHERE { VALUES ?s { e:n1 e:n2 } ?s e:knows ?o . ?o e:label ?l }",
    );
    let sideways = steps.iter().find(|s| s.operator == "sideways");
    assert!(
        sideways.is_some_and(|s| s.rows == 6 && s.detail.contains("from 2 rows")),
        "{steps:#?}"
    );
}

/// LIMIT pushdown: a LIMIT without ORDER BY stops the pattern's joins once it has enough
/// rows (Wikidata q04, LIMIT 100 k: 3,380 -> 21 ms).
#[test]
fn limits_stop_the_joins_early() {
    let steps = steps(
        &engine(),
        "SELECT * WHERE { ?s e:knows ?o . ?o e:label ?l } LIMIT 5",
    );
    let limit = steps.iter().find(|s| s.operator == "limit pushdown");
    // 1,024 rows of the first pattern in the first morsel, not all 3,000 or 9,000.
    assert!(
        limit.is_some_and(|s| s.detail.contains("1 morsel(s), 1024 of")),
        "{steps:#?}"
    );
}

/// Closures by components: `p+` and `p*` through strongly connected components; `COUNT`
/// of a closure from the components' sizes (YAGO q08: 11,432 -> 24 ms).
#[test]
fn closures_run_by_components() {
    let steps = steps(
        &engine(),
        "SELECT (COUNT(*) AS ?n) WHERE { ?a e:knows+ ?b }",
    );
    assert!(steps.iter().any(|s| s.operator == "closure"), "{steps:#?}");
}

/// EXISTS as sets: the pattern evaluated once and semi- or anti-joined in place
/// (DBpedia q13: 25 -> 3.1 ms).
#[test]
fn exists_runs_once_as_a_set() {
    let engine = engine();
    let not = operators(
        &engine,
        "SELECT ?s WHERE { ?s e:kind e:k1 FILTER NOT EXISTS { ?s e:knows e:n5 } }",
    );
    assert!(not.iter().any(|o| o == "anti join"), "{not:?}");
    let exists = operators(
        &engine,
        "SELECT ?s WHERE { ?s e:kind e:k1 FILTER EXISTS { ?s e:knows ?o } }",
    );
    assert!(exists.iter().any(|o| o == "semi join"), "{exists:?}");
}

/// EXPLAIN shows every operator's estimated rows beside its actual ones (the merge
/// checklist's item): the operators the query names and those inside them.
#[test]
fn explain_estimates_every_operator() {
    let engine = engine();
    for query in [
        "SELECT ?k (COUNT(*) AS ?n) WHERE { ?s e:kind ?k } GROUP BY ?k",
        "SELECT (COUNT(*) AS ?n) WHERE { ?s e:kind ?k FILTER NOT EXISTS { ?s e:label ?l } }",
        "SELECT * WHERE { ?a e:knows ?b . ?b e:knows ?c . ?c e:knows ?a }",
        "SELECT ?s WHERE { ?s e:label ?l FILTER(CONTAINS(?l, \"number 12\")) }",
        "SELECT * WHERE { VALUES ?s { e:n1 e:n2 } ?s e:knows ?o . ?o e:label ?l }",
        "SELECT * WHERE { ?s e:knows ?o . ?o e:label ?l } LIMIT 5",
        "SELECT * WHERE { ?s e:label ?l } LIMIT 5",
        "SELECT (COUNT(*) AS ?n) WHERE { ?a e:knows+ ?b }",
        "SELECT ?s WHERE { ?s e:kind e:k1 FILTER NOT EXISTS { ?s e:knows e:n5 } }",
        "SELECT ?s WHERE { ?s e:kind e:k1 FILTER EXISTS { ?s e:knows ?o } }",
        "SELECT ?s ?o WHERE { ?s e:kind e:k1 OPTIONAL { ?s e:knows ?o FILTER(?o != e:n3) } }",
        "SELECT * WHERE { { ?s e:kind e:k1 } UNION { ?s e:kind e:k2 } MINUS { ?s e:knows e:n9 } }",
        "SELECT DISTINCT ?k WHERE { ?s e:kind ?k BIND(STR(?k) AS ?t) } ORDER BY ?t LIMIT 3",
        "SELECT * WHERE { ?s e:kind e:k3 . ?s e:knows/e:knows ?f }",
        "SELECT * WHERE { e:n1 e:knows+ ?f . ?f e:kind e:k2 }",
        "SELECT ?k (AVG(STRLEN(?l)) AS ?m) WHERE { ?s e:kind ?k . ?s e:knows ?o . ?o e:label ?l } GROUP BY ?k",
        "SELECT * WHERE { ?s e:kind e:k1 { SELECT ?s (COUNT(*) AS ?n) WHERE { ?s e:knows ?o } GROUP BY ?s } }",
        "SELECT * WHERE { GRAPH ?g { ?s e:kind ?k } }",
    ] {
        let steps = steps(&engine, query);
        let missing: Vec<&PlanStep> = steps
            .iter()
            .filter(|s| s.estimated_rows.is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "{query}
{missing:#?}"
        );
    }
}

/// Estimates within their bounds (LUBM-10 materialised, `perf_lab --qerror`: q-error
/// median 15 → 1.00, p90 2,540 → 1.89): a pattern whose variables are all bound keeps at
/// most the rows it checks, a step over non-empty inputs is never estimated at 0, and two
/// small single-variable patterns on one variable are estimated by the values they share.
#[test]
fn estimates_never_exceed_their_bounds() {
    let engine = engine();
    // Two patterns: the check `?o e:kind e:k1` keeps at most the 3 rows it checks (it
    // was estimated by the larger pattern, 300).
    let plan = steps(
        &engine,
        "SELECT * WHERE { e:n1 e:knows ?o . ?o e:kind e:k1 }",
    );
    let check = plan
        .iter()
        .find(|s| s.detail.contains("kind") && s.operator != "scan")
        .unwrap_or_else(|| panic!("{plan:#?}"));
    assert!(check.estimated_rows.is_some_and(|e| e <= 3), "{plan:#?}");
    // Kind k1 (300 nodes) and the 3 nodes knowing n2 share 2 (n1, n2971): counted.
    let plan = steps(
        &engine,
        "SELECT * WHERE { ?s e:kind e:k1 . ?s e:knows e:n2 }",
    );
    let bgp = plan.iter().find(|s| s.operator == "bgp").unwrap();
    assert_eq!((bgp.estimated_rows, bgp.rows), (Some(2), 2), "{plan:#?}");
    for query in [
        "SELECT * WHERE { ?a e:knows ?b . ?b e:knows ?c . ?c e:kind e:k3 . ?a e:kind e:k1 }",
        "SELECT * WHERE { ?s e:kind e:k1 . ?s e:knows ?o . ?o e:kind e:k8 . ?o e:label ?l }",
        "SELECT * WHERE { e:n1 e:knows ?o . ?o e:kind e:k1 }",
    ] {
        let plan = steps(&engine, query);
        let zero: Vec<&PlanStep> = plan
            .iter()
            .filter(|s| s.rows > 0 && s.estimated_rows == Some(0))
            .collect();
        assert!(zero.is_empty(), "{query}\n{zero:#?}");
    }
}

/// The seeks and searches from the root an EXPLAIN detail reports (`… | N seeks, M from the
/// root`, or `… | L lookups (N seeks, M from the root), …`).
fn seeks(detail: &str) -> Option<(u64, u64)> {
    let counts = detail.rsplit_once(" | ")?.1;
    let counts = counts.split_once('(').map_or(counts, |(_, inner)| inner);
    let mut numbers = counts
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<u64>().unwrap());
    Some((numbers.next()?, numbers.next()?))
}

/// Seekable cursors (investigation §1 #3): an index join reads the index forward, its
/// keys sorted in the index's order, one search from the root per run; the
/// worst-case-optimal join checks candidates by leapfrog seeks kept across them (LUBM-10
/// q9: 96.9 k searches from the root → 3,573 of 59,766 seeks).
#[test]
fn probes_seek_forward_instead_of_from_the_root() {
    let engine = engine();
    // 27 rows reached in three steps (10 distinct `?c`), their labels probed (3,000).
    let plan = steps(
        &engine,
        "SELECT * WHERE { e:n1 e:knows ?a . ?a e:knows ?b . ?b e:knows ?c . ?c e:label ?l }",
    );
    let probed: Vec<(u64, u64)> = plan
        .iter()
        .filter(|s| s.operator == "index join")
        .filter_map(|s| seeks(&s.detail))
        .collect();
    assert!(probed.iter().any(|&(seeks, _)| seeks >= 8), "{plan:#?}");
    for (seeks, roots) in probed {
        assert!(
            roots * 8 <= seeks.max(8),
            "{seeks} seeks, {roots} from the root"
        );
    }
    let plan = steps(
        &engine,
        "SELECT * WHERE { ?a e:knows ?b . ?b e:knows ?c . ?c e:knows ?a }",
    );
    let order = plan
        .iter()
        .find(|s| s.operator == "wcoj order")
        .unwrap_or_else(|| panic!("{plan:#?}"));
    let (seeks, roots) = seeks(&order.detail).unwrap_or_else(|| panic!("{order:#?}"));
    assert!(roots * 4 <= seeks, "{}", order.detail);
}

/// Paths planned with the patterns they are joined to: a path from a constant that
/// reaches few nodes goes first and the large pattern is probed from what it reaches,
/// instead of the pattern being joined whole and the path after it.
#[test]
fn selective_paths_join_first() {
    let steps = steps(
        &engine(),
        "SELECT * WHERE { ?s e:label ?l . e:n1 e:knows? ?s }",
    );
    let planned = steps
        .iter()
        .find(|s| s.operator == "paths ordered with patterns");
    assert!(
        planned.is_some_and(|s| s.detail.starts_with("<http://example.org/n1>") && s.rows == 4),
        "{steps:#?}"
    );
    assert!(
        steps.iter().any(|s| s.operator == "sideways"),
        "the labels probed from the path's rows: {steps:#?}"
    );
}

/// Plan parts cached and shared across queries (the result cache, QLever's level): a
/// query whose join another query computed before, under other variable names, gets the
/// join from the cache, and EXPLAIN marks it; the answer is the one computed afresh.
#[test]
fn parts_are_shared_across_queries_through_the_result_cache() {
    let engine = engine();
    let cache = std::sync::Arc::new(nrese_sparql::ResultCache::new(16 << 20).admitting_all());
    let cached = QueryOptions {
        result_cache: Some(std::sync::Arc::clone(&cache)),
        ..QueryOptions::default()
    };
    let run = |query: &str, options: &QueryOptions| {
        let text = format!("PREFIX e: <{EX}> {query}");
        let query = SparqlParser::new().parse_query(&text).unwrap();
        explain_query(&engine.snapshot(), &query, options).unwrap()
    };
    run(
        "SELECT ?a ?b WHERE { ?a e:knows ?b . ?b e:kind e:k3 }",
        &cached,
    );
    let shared = "SELECT ?x (COUNT(?l) AS ?n) WHERE { ?x e:knows ?y . ?y e:kind e:k3 . ?x e:label ?l } GROUP BY ?x";
    let explanation = run(shared, &cached);
    let hit = explanation.steps.iter().find(|s| s.cache == Some("hit"));
    assert!(
        hit.is_some_and(|s| s.rows == 900),
        "the join of knows and kind from the cache: {:#?}",
        explanation.steps
    );
    assert_eq!(explanation.rows, run(shared, &QueryOptions::default()).rows);
}
