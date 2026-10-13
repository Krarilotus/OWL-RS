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
        "SELECT ?s WHERE { ?s e:kind ?k FILTER NOT EXISTS { ?s e:label ?l } }",
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

/// `COUNT(*)` of a cyclic BGP: the worst-case-optimal join binds the order's prefix and
/// counts the rest as a forest, never producing the solutions. 4-cycles bind `?a` and
/// join two lists of 2-paths per node: one check per `?a` candidate, where enumerating
/// them checked each of the 27,000 2-paths' last step.
#[test]
fn cycles_are_counted_without_their_solutions() {
    let plan = steps(
        &engine(),
        "SELECT (COUNT(*) AS ?n) WHERE { ?a e:knows ?b . ?b e:knows ?c . ?c e:knows ?d . ?d e:knows ?a }",
    );
    let order = plan
        .iter()
        .find(|s| s.operator == "wcoj order")
        .unwrap_or_else(|| panic!("{plan:#?}"));
    assert!(
        order.detail.ends_with("?b, ?c, ?d counted as a forest"),
        "{}",
        order.detail
    );
    let lookups: u64 = order
        .detail
        .split(" | ")
        .nth(1)
        .and_then(|d| d.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{}", order.detail));
    assert!(lookups <= 3000, "{}", order.detail);
}

/// The numbers of a `counted join` step: (group walks, weights listed, pairs kept, matches
/// read at the roots).
fn counted(detail: &str) -> (u64, u64, u64, u64) {
    let number = |label: &str| -> u64 {
        let end = detail
            .find(label)
            .unwrap_or_else(|| panic!("{label}: {detail}"));
        detail[..end]
            .rsplit(|c: char| !c.is_ascii_digit())
            .find(|s| !s.is_empty())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("{label}: {detail}"))
    };
    (
        number(" group walks"),
        number(" weights listed"),
        number(" pairs kept"),
        number(" matches read"),
    )
}

/// `COUNT(*)` of an acyclic join by message passing, without its rows (Sparqloscope's
/// queries 4, 8, 12, 13 and 19 held 9 to over 26 GB of rows on DBLP): a star counts from
/// two group walks of 3,000 values each; a chain whose end has an OPTIONAL of three
/// matches per value is rooted at its middle pattern, whose 9,000 matches are read once
/// and not kept (rooted at a variable, the OPTIONAL's values would send their 9,000
/// partners as pairs). No row is made.
#[test]
fn joins_are_counted_without_their_rows() {
    let engine = engine();
    let step = |query: &str| {
        let plan = steps(&engine, query);
        plan.into_iter()
            .find(|s| s.operator == "counted join")
            .unwrap_or_else(|| panic!("{query}: no counted join"))
    };
    // Query 4's shape: 9,000 rows.
    let star = step("SELECT (COUNT(*) AS ?n) WHERE { ?s e:knows ?o1 . ?s e:label ?o2 }");
    let (walks, listed, pairs, read) = counted(&star.detail);
    assert_eq!((walks, pairs, read), (2, 0, 0), "{}", star.detail);
    assert!(listed <= 6_000, "{}", star.detail);
    // Query 19's shape: 27,000 rows.
    let chain = step(
        "SELECT (COUNT(*) AS ?n) WHERE { ?a e:kind ?k . ?a e:knows ?b OPTIONAL { ?b e:knows ?c } }",
    );
    let (walks, listed, pairs, read) = counted(&chain.detail);
    assert_eq!((walks, pairs, read), (2, 0, 9_000), "{}", chain.detail);
    assert!(listed <= 9_000, "{}", chain.detail);
}

/// `COUNT` of one pattern under a FILTER that bounds a variable to id ranges is a sum of
/// the index's range counts (Sparqloscope 104, the literals among every statement's
/// objects; 091-093, a number at least a bound): no match read where the ranges decide.
#[test]
fn filters_that_bound_a_variable_count_ranges() {
    let engine = engine();
    for (query, ranges) in [
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o FILTER(isLITERAL(?o)) }",
            "2 ranges",
        ),
        (
            "SELECT (COUNT(?s) AS ?n) WHERE { ?s e:knows ?o FILTER(isIRI(?o)) }",
            "1 ranges",
        ),
    ] {
        let plan = steps(&engine, query);
        let step = plan
            .iter()
            .find(|s| s.operator == "range count")
            .unwrap_or_else(|| panic!("{query}: {plan:#?}"));
        assert!(
            step.detail
                .contains(&format!("{ranges} counted by the index, 0 values")),
            "{}",
            step.detail
        );
    }
}

/// The count's memory is a weight per value, not the join's rows: 1,000 subjects with 20
/// `knows` and 20 `label` each make 400,000 rows (9.6 MB as three columns of ids), counted
/// within a budget of 1 MiB.
#[test]
fn a_counted_join_holds_its_values_not_its_rows() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for i in 0..1000u32 {
        for j in 0..20u32 {
            for (p, o) in [("knows", format!("o{j}")), ("label", format!("l{j}"))] {
                let quad = Quad::new(ex(&format!("s{i}")), ex(p), ex(&o), GraphName::DefaultGraph);
                tx.insert(quad.as_ref());
            }
        }
    }
    tx.commit().unwrap();
    let text = format!(
        "PREFIX e: <{EX}> SELECT (COUNT(*) AS ?n) WHERE {{ ?s e:knows ?o1 . ?s e:label ?o2 }}"
    );
    let query = SparqlParser::new().parse_query(&text).unwrap();
    let options = QueryOptions {
        resources: nrese_sparql::Resources {
            memory_limit: Some(1 << 20),
            ..Default::default()
        },
        ..QueryOptions::default()
    };
    let snapshot = engine.snapshot();
    let explained = explain_query(&snapshot, &query, &options).unwrap();
    assert!(
        explained.steps.iter().any(|s| s.operator == "counted join"),
        "{:#?}",
        explained.steps
    );
    let nrese_sparql::QueryResults::Solutions(mut solutions) =
        nrese_sparql::evaluate_query(&snapshot, &query, &options).unwrap()
    else {
        panic!("a SELECT")
    };
    let count = solutions.next().unwrap().unwrap();
    assert_eq!(
        count.get("n").map(ToString::to_string).as_deref(),
        Some("\"400000\"^^<http://www.w3.org/2001/XMLSchema#integer>")
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
/// join from the cache, and EXPLAIN marks it; the answer is the one computed afresh. The
/// same join alone under other names is answered from the cache too.
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
    let renamed = "SELECT ?p ?q WHERE { ?p e:knows ?q . ?q e:kind e:k3 }";
    let hits = cache.stats().hits;
    assert_eq!(
        run(renamed, &cached).rows,
        run(renamed, &QueryOptions::default()).rows
    );
    assert!(cache.stats().hits > hits, "{:?}", cache.stats());
    // MAX needs the rows (a COUNT is counted without them, `counted join`).
    let shared = "SELECT ?x (MAX(?l) AS ?n) WHERE { ?x e:knows ?y . ?y e:kind e:k3 . ?x e:label ?l } GROUP BY ?x";
    let explanation = run(shared, &cached);
    let hit = explanation.steps.iter().find(|s| s.cache == Some("hit"));
    assert!(
        hit.is_some_and(|s| s.rows == 900),
        "the join of knows and kind from the cache: {:#?}",
        explanation.steps
    );
    assert_eq!(explanation.rows, run(shared, &QueryOptions::default()).rows);
}
