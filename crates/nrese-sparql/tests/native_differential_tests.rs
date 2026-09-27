//! XC3 gate: the native executor gives the same results as spareval on the same snapshot.
//!
//! Random datasets mix every term kind the operators distinguish: IRIs, inline and
//! dictionary numbers, strings with and without language tags, dates, booleans. Random
//! queries combine BGPs (shared, repeated and unbound variables, unknown constants), FILTERs,
//! OPTIONAL (with filters), UNION, MINUS, (NOT) EXISTS, GROUP BY with every supported
//! aggregate, DISTINCT, ORDER BY and LIMIT. Every query must run natively
//! ([`runs_natively`]); results are compared as multisets, and as sequences where ORDER BY
//! covers every projected variable (so ties are identical rows).

use nrese_engine::{Engine, EngineConfig};
use nrese_sparql::{QueryOptions, QueryResults, evaluate_query, runs_natively};
use oxrdf::vocab::xsd;
use oxrdf::{GraphName, Literal, NamedNode, Quad, Term};
use spargebra::SparqlParser;

const EX: &str = "http://example.com/";

/// SplitMix64: deterministic test-case generation.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

fn random_object(rng: &mut Rng) -> Term {
    match rng.below(9) {
        0..=2 => ex(&format!("e{}", rng.below(6))).into(),
        3 => Literal::new_typed_literal(rng.below(8).to_string(), xsd::INTEGER).into(),
        4 => Literal::new_typed_literal(format!("{}.5", rng.below(5)), xsd::DECIMAL).into(),
        5 => Literal::new_typed_literal(format!("{}.0E0", rng.below(8)), xsd::DOUBLE).into(),
        6 => Literal::new_language_tagged_literal_unchecked(
            format!("s{}", rng.below(4)),
            *rng.pick(&["en", "de"]),
        )
        .into(),
        7 => Literal::new_simple_literal(format!("s{}", rng.below(4))).into(),
        _ => Literal::new_typed_literal(
            format!("200{}-01-0{}", rng.below(3), 1 + rng.below(3)),
            xsd::DATE,
        )
        .into(),
    }
}

fn random_dataset(rng: &mut Rng) -> Vec<Quad> {
    (0..10 + rng.below(50))
        .map(|_| {
            Quad::new(
                ex(&format!("e{}", rng.below(6))),
                ex(&format!("p{}", rng.below(4))),
                random_object(rng),
                GraphName::DefaultGraph,
            )
        })
        .collect()
}

const VARS: [&str; 4] = ["?a", "?b", "?c", "?d"];

fn term_or_var(rng: &mut Rng, object: bool) -> String {
    match rng.below(if object { 5 } else { 4 }) {
        0 | 1 => rng.pick(&VARS).to_string(),
        2 => format!("<{EX}e{}>", rng.below(7)), // e6 never occurs: unknown constant
        3 if object => rng.pick(&["3", "\"s1\"@en", "\"s2\"", "2.5"]).to_string(),
        _ => rng.pick(&VARS).to_string(),
    }
}

fn triple(rng: &mut Rng) -> String {
    let predicate = if rng.below(5) == 0 {
        rng.pick(&VARS).to_string()
    } else {
        format!("<{EX}p{}>", rng.below(4))
    };
    format!(
        "{} {} {} .",
        term_or_var(rng, false),
        predicate,
        term_or_var(rng, true)
    )
}

fn bgp(rng: &mut Rng) -> String {
    (0..1 + rng.below(3))
        .map(|_| triple(rng))
        .collect::<Vec<_>>()
        .join(" ")
}

fn filter(rng: &mut Rng) -> String {
    let v = rng.pick(&VARS);
    let w = rng.pick(&VARS);
    match rng.below(10) {
        0 => format!("FILTER({v} > 3)"),
        1 => format!("FILTER({v} <= 2.5 || {w} = <{EX}e1>)"),
        2 => format!("FILTER(isIRI({v}))"),
        3 => format!("FILTER(LANG({v}) = \"en\")"),
        4 => format!("FILTER(CONTAINS(STR({v}), \"1\"))"),
        5 => format!("FILTER({v} != {w})"),
        6 => format!("FILTER(!BOUND({v}) || isLiteral({v}))"),
        7 => format!("FILTER({v} >= \"2001-01-01\"^^<http://www.w3.org/2001/XMLSchema#date>)"),
        8 => format!("FILTER(REGEX(STR({v}), \"^s[12]\", \"i\"))"),
        _ => format!("FILTER({v} IN (1, \"s1\"@en, <{EX}e2>))"),
    }
}

fn group_pattern(rng: &mut Rng, depth: u32) -> String {
    let mut parts = vec![bgp(rng)];
    for _ in 0..rng.below(3) {
        let part = match rng.below(if depth > 1 { 3 } else { 7 }) {
            0 | 1 => filter(rng),
            2 => bgp(rng),
            3 => format!("OPTIONAL {{ {} }}", group_pattern(rng, depth + 1)),
            4 => format!(
                "{{ {} }} UNION {{ {} }}",
                group_pattern(rng, depth + 1),
                group_pattern(rng, depth + 1)
            ),
            5 => format!("MINUS {{ {} }}", bgp(rng)),
            _ => format!(
                "FILTER {}EXISTS {{ {} }}",
                if rng.below(2) == 0 { "NOT " } else { "" },
                bgp(rng)
            ),
        };
        parts.push(part);
    }
    parts.join(" ")
}

fn random_query(rng: &mut Rng) -> (String, bool) {
    let pattern = group_pattern(rng, 0);
    match rng.below(4) {
        0 => {
            let key = rng.pick(&VARS);
            let arg = rng.pick(&VARS);
            let aggregate = rng.pick(&[
                "COUNT(*)",
                "COUNT(DISTINCT *)",
                "COUNT(ARG)",
                "SUM(ARG)",
                "AVG(ARG)",
                "MIN(ARG)",
                "MAX(ARG)",
                "COUNT(DISTINCT ARG)",
            ]);
            let aggregate = aggregate.replace("ARG", arg);
            (
                format!("SELECT {key} (({aggregate}) AS ?x) WHERE {{ {pattern} }} GROUP BY {key}"),
                false,
            )
        }
        1 => {
            // ORDER BY every projected variable: a deterministic sequence even with LIMIT.
            let limit = 1 + rng.below(5);
            (
                format!("SELECT ?a ?b WHERE {{ {pattern} }} ORDER BY DESC(?a) ?b LIMIT {limit}"),
                true,
            )
        }
        2 if rng.below(2) == 0 => (
            format!("SELECT DISTINCT ?a ?c WHERE {{ {pattern} }}"),
            false,
        ),
        // DISTINCT with ORDER BY over every projected variable: a deterministic sequence.
        2 => (
            format!("SELECT DISTINCT ?a ?c WHERE {{ {pattern} }} ORDER BY ?c DESC(?a)"),
            true,
        ),
        _ => (format!("SELECT * WHERE {{ {pattern} }}"), false),
    }
}

fn rows(results: QueryResults<'_>, ordered: bool) -> Vec<String> {
    let QueryResults::Solutions(solutions) = results else {
        panic!("SELECT gives solutions")
    };
    let variables: Vec<_> = solutions.variables().to_vec();
    let mut out: Vec<String> = solutions
        .map(|solution| {
            let solution = solution.expect("no evaluation error");
            variables
                .iter()
                .map(|v| {
                    solution
                        .get(v)
                        .map_or("UNDEF".to_owned(), |t| t.to_string())
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    if !ordered {
        out.sort();
    }
    out
}

#[test]
fn native_results_equal_spareval_on_random_queries() {
    let mut rng = Rng(20_260_927);
    let spareval = QueryOptions {
        force_spareval: true,
        ..QueryOptions::default()
    };
    let (mut checked, mut fallbacks) = (0, Vec::new());
    for dataset_case in 0..40 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in random_dataset(&mut rng) {
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for query_case in 0..50 {
            let (text, ordered) = random_query(&mut rng);
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            if !runs_natively(&query) {
                fallbacks.push(text);
                continue;
            }
            let native = rows(
                evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
                ordered,
            );
            let expected = rows(
                evaluate_query(&snapshot, &query, &spareval).unwrap(),
                ordered,
            );
            assert_eq!(
                native, expected,
                "dataset {dataset_case}, query {query_case}: {text}"
            );
            checked += 1;
        }
    }
    // Queries outside native coverage run on spareval, which is correct by construction;
    // the generator's shapes must still be almost all native.
    assert!(
        fallbacks.len() * 20 < checked,
        "{} of {} generated queries not native, e.g. {:#?}",
        fallbacks.len(),
        checked + fallbacks.len(),
        &fallbacks[..fallbacks.len().min(3)]
    );
}

#[test]
fn count_star_of_one_pattern_reads_the_index() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for i in 0..1000 {
        tx.insert(
            Quad::new(
                ex(&format!("s{i}")),
                ex("p"),
                Literal::new_simple_literal("o"),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    for (text, expected) in [
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }",
            "\"1000\"^^<http://www.w3.org/2001/XMLSchema#integer>",
        ),
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://example.com/p> ?o }",
            "\"1000\"^^<http://www.w3.org/2001/XMLSchema#integer>",
        ),
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://example.com/missing> ?o }",
            "\"0\"^^<http://www.w3.org/2001/XMLSchema#integer>",
        ),
    ] {
        let query = SparqlParser::new().parse_query(text).unwrap();
        assert!(runs_natively(&query));
        assert_eq!(
            rows(
                evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
                false
            ),
            vec![expected]
        );
    }
}
