//! XC3 gate: the native executor gives the same results as spareval on the same snapshot.
//!
//! Random datasets mix every term kind the operators distinguish: IRIs, inline and
//! dictionary numbers, strings with and without language tags, dates, booleans. Random
//! queries combine BGPs (shared, repeated and unbound variables, unknown constants), FILTERs,
//! OPTIONAL (with filters), UNION, MINUS, (NOT) EXISTS, GROUP BY with every supported
//! aggregate, DISTINCT, ORDER BY and LIMIT. Every query must run natively
//! ([`runs_natively`]); results are compared as multisets, and as sequences where ORDER BY
//! covers every projected variable (so ties are identical rows). A third of the
//! default-graph statements are inferred, and every query runs under a random read model
//! (asserted, inferred or both).

use std::collections::HashSet;

use nrese_engine::{EncodedTriple, Engine, EngineConfig, ReadModel};
use nrese_sparql::{QueryOptions, QueryResults, evaluate_query, explain_query, runs_natively};
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
    match rng.below(12) {
        // Range pruning edge cases: dates with timezones next to the FILTER bounds, a
        // non-canonical integer and an xsd:int (both dictionary typed literals).
        9 => Literal::new_typed_literal(
            *rng.pick(&[
                "2001-01-01Z",
                "2000-12-31+14:00",
                "2001-01-02-14:00",
                "2000-01-01+05:30",
            ]),
            xsd::DATE,
        )
        .into(),
        10 => Literal::new_typed_literal(format!("0{}", rng.below(8)), xsd::INTEGER).into(),
        11 => Literal::new_typed_literal(rng.below(8).to_string(), xsd::INT).into(),
        0..=2 => ex(&format!("e{}", rng.below(6))).into(),
        3 => Literal::new_typed_literal(rng.below(8).to_string(), xsd::INTEGER).into(),
        4 => Literal::new_typed_literal(format!("{}.5", rng.below(5)), xsd::DECIMAL).into(),
        5 => Literal::new_typed_literal(format!("{}.25E0", rng.below(8)), xsd::DOUBLE).into(),
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
    match rng.below(25) {
        19 => format!("FILTER(YEAR({v}) = 2001 || MONTH({w}) = 1)"),
        20 => format!("FILTER(ABS({v} - 3) < 2 || ROUND({w}) = 3)"),
        21 => format!("FILTER({v} * 2 > 5 && {v} / 2 != 1)"),
        22 => format!(
            "FILTER(CONCAT(STR({v}), \"x\") = \"s1x\" || STRAFTER(STR({w}), \"e\") = \"2\")"
        ),
        23 => format!("FILTER(-{v} <= -2 && CEIL({v}) >= FLOOR({v}))"),
        24 => format!(
            "FILTER(STRBEFORE({v}, \"1\") = \"s\" || STRLANG(STR({w}), \"en\") = \"s1\"@en)"
        ),
        17 => format!(
            "FILTER({v} >= \"2000-01-02\"^^<http://www.w3.org/2001/XMLSchema#date> && {v} < \"2001-01-02\"^^<http://www.w3.org/2001/XMLSchema#date>)"
        ),
        18 => format!("FILTER({v} > 1 && 5 >= {v} && {w} != 2)"),
        // The compiled (id-level) shapes, alone and combined:
        10 => format!("FILTER(CONTAINS({v}, \"1\") || STRSTARTS(STR({w}), \"http\"))"),
        11 => format!("FILTER(LANGMATCHES(LANG({v}), \"EN\") && !isBlank({w}))"),
        12 => format!("FILTER({v} = <{EX}e1> || {w} != <{EX}e9>)"),
        13 => format!("FILTER(3 < {v} && {v} <= 6)"),
        14 => format!("FILTER(STRENDS({v}, \"2\") || \"de\" = LANG({v}))"),
        15 => format!("FILTER(REGEX({v}, \"S\", \"i\"))"),
        16 => format!("FILTER({v} = 2 || sameTerm({w}, <{EX}e3>))"),
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
        let part = match rng.below(if depth > 1 { 3 } else { 10 }) {
            8 | 9 => {
                // A property path, ends variables or constants (e6 is unknown to the store).
                let path = rng.pick(&[
                    "<P0>*",
                    "<P1>+",
                    "(<P0>|<P1>)+",
                    "^<P2>/<P0>",
                    "!(<P0>|<P1>)",
                    "<P3>?",
                    "(<P0>/<P1>)*",
                    "<P2>/<P2>",
                    "(^<P1>)*",
                    "<P0>|<P3>",
                ]);
                let end = |rng: &mut Rng| match rng.below(3) {
                    0 => format!("<{EX}e{}>", rng.below(7)),
                    _ => rng.pick(&VARS).to_string(),
                };
                let path = path.replace("<P", &format!("<{EX}p"));
                format!("{} {path} {} .", end(rng), end(rng))
            }
            7 => {
                let v = rng.pick(&VARS);
                let expression = rng.pick(&[
                    "VAR + 1",
                    "VAR * 1.5",
                    "VAR / 4",
                    "YEAR(VAR)",
                    "CONCAT(STR(VAR), \"-\")",
                    "STRDT(STR(VAR), <http://www.w3.org/2001/XMLSchema#integer>)",
                    "FLOOR(VAR)",
                ]);
                format!(
                    "BIND({} AS ?e{})",
                    expression.replace("VAR", v),
                    rng.below(1_000_000)
                )
            }
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
    if rng.below(8) == 0 {
        // LIMIT over one filtered pattern (the streamed scan); see `limited` below.
        return (
            format!(
                "SELECT * WHERE {{ {} {} }} LIMIT {}",
                triple(rng),
                filter(rng),
                1 + rng.below(4)
            ),
            false,
        );
    }
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

/// Integer literals by value: spareval's ORDER BY + LIMIT path outputs `"07"` and
/// `"7"^^xsd:int` as `"7"^^xsd:integer`, while the native executor returns the stored
/// terms (RDF term identity). Both are the same values; the test compares values there.
fn by_value(term: &Term) -> Term {
    const INTEGERS: [&str; 3] = [
        "http://www.w3.org/2001/XMLSchema#integer",
        "http://www.w3.org/2001/XMLSchema#int",
        "http://www.w3.org/2001/XMLSchema#long",
    ];
    match term {
        Term::Literal(l) if INTEGERS.contains(&l.datatype().as_str()) => {
            match l.value().parse::<i64>() {
                Ok(v) => Literal::new_typed_literal(v.to_string(), xsd::INTEGER).into(),
                Err(_) => term.clone(),
            }
        }
        _ => term.clone(),
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
                        .map_or("UNDEF".to_owned(), |t| by_value(t).to_string())
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

/// The LIMIT of a generated `SELECT * … LIMIT n` query without ORDER BY.
fn limited(text: &str) -> Option<usize> {
    (!text.contains("ORDER BY"))
        .then(|| text.rsplit_once(" LIMIT ")?.1.parse().ok())
        .flatten()
}

#[test]
fn native_results_equal_spareval_on_random_queries() {
    let mut rng = Rng(20_260_927);
    let (mut checked, mut fallbacks) = (0, Vec::new());
    for dataset_case in 0..150 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in random_dataset(&mut rng) {
            if quad.graph_name.is_default_graph() && rng.below(3) == 0 {
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
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for query_case in 0..50 {
            let model = *rng.pick(&[
                ReadModel::Materialised,
                ReadModel::Asserted,
                ReadModel::Inferred,
            ]);
            let native_options = QueryOptions {
                read_model: model,
                ..QueryOptions::default()
            };
            let spareval = QueryOptions {
                force_spareval: true,
                read_model: model,
                ..QueryOptions::default()
            };
            let (text, ordered) = random_query(&mut rng);
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            if !runs_natively(&query) {
                fallbacks.push(text);
                continue;
            }
            let native = rows(
                evaluate_query(&snapshot, &query, &native_options).unwrap(),
                ordered,
            );
            if let Some(limit) = limited(&text) {
                // LIMIT without ORDER BY may return any `limit` solutions: the native rows
                // must be that many, and all of them solutions of the unlimited query.
                let unlimited = text[..text.rfind(" LIMIT ").unwrap()].to_owned();
                let query = SparqlParser::new().parse_query(&unlimited).unwrap();
                let mut all = rows(evaluate_query(&snapshot, &query, &spareval).unwrap(), false);
                assert_eq!(native.len(), all.len().min(limit), "{text}");
                for row in &native {
                    let position = all.iter().position(|r| r == row);
                    assert!(position.is_some(), "{row} is not a solution: {text}");
                    all.remove(position.unwrap());
                }
                checked += 1;
                continue;
            }
            let expected = rows(
                evaluate_query(&snapshot, &query, &spareval).unwrap(),
                ordered,
            );
            assert_eq!(
                native, expected,
                "dataset {dataset_case}, query {query_case}, {model:?}: {text}"
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

/// Cyclic BGPs (triangles, 4-cycles, with types, constants, repeated variables and a
/// variable predicate) over dense graphs: the worst-case-optimal join equals spareval.
#[test]
fn cyclic_bgps_equal_spareval() {
    let mut rng = Rng(20_260_929);
    let spareval = QueryOptions {
        force_spareval: true,
        ..QueryOptions::default()
    };
    let rdf_type = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    let mut checked = 0;
    for _ in 0..40 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for _ in 0..40 + rng.below(120) {
            let quad = Quad::new(
                ex(&format!("e{}", rng.below(8))),
                ex(&format!("p{}", rng.below(3))),
                ex(&format!("e{}", rng.below(8))),
                GraphName::DefaultGraph,
            );
            tx.insert(quad.as_ref());
        }
        for e in 0..8 {
            if rng.below(2) == 0 {
                let quad = Quad::new(
                    ex(&format!("e{e}")),
                    NamedNode::new_unchecked(rdf_type),
                    ex(&format!("T{}", rng.below(2))),
                    GraphName::DefaultGraph,
                );
                tx.insert(quad.as_ref());
            }
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for _ in 0..25 {
            let p = |rng: &mut Rng| format!("<{EX}p{}>", rng.below(3));
            let edge = |rng: &mut Rng, a: &str, b: &str| {
                let predicate = p(rng);
                if rng.below(2) == 0 {
                    format!("{a} {predicate} {b} .")
                } else {
                    format!("{b} {predicate} {a} .")
                }
            };
            let mut body = match rng.below(6) {
                0 | 1 => format!(
                    "{} {} {}",
                    edge(&mut rng, "?a", "?b"),
                    edge(&mut rng, "?b", "?c"),
                    edge(&mut rng, "?c", "?a")
                ),
                2 => format!(
                    "{} {} {} {}",
                    edge(&mut rng, "?a", "?b"),
                    edge(&mut rng, "?b", "?c"),
                    edge(&mut rng, "?c", "?d"),
                    edge(&mut rng, "?d", "?a")
                ),
                3 => format!(
                    "?a ?q ?b . {} {}",
                    edge(&mut rng, "?b", "?c"),
                    edge(&mut rng, "?c", "?a")
                ),
                4 => format!(
                    "?a <{EX}p0> ?a . {} {}",
                    edge(&mut rng, "?a", "?b"),
                    edge(&mut rng, "?b", "?a")
                ),
                _ => {
                    let (x, y) = (
                        format!("<{EX}e{}>", rng.below(8)),
                        format!("<{EX}e{}>", rng.below(8)),
                    );
                    format!(
                        "{} {} {}",
                        edge(&mut rng, "?a", "?b"),
                        edge(&mut rng, "?b", &x),
                        edge(&mut rng, &y, "?a")
                    )
                }
            };
            if rng.below(2) == 0 {
                body.push_str(&format!(" ?a a <{EX}T{}> .", rng.below(2)));
            }
            let text = format!("SELECT * WHERE {{ {body} }}");
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            assert!(runs_natively(&query), "{text}");
            let native = rows(
                evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
                false,
            );
            let expected = rows(evaluate_query(&snapshot, &query, &spareval).unwrap(), false);
            assert_eq!(native, expected, "{text}");
            checked += 1;
        }
    }
    assert_eq!(checked, 1000);
}

/// A selective pattern joined with a much larger one runs as a parallel index nested-loop
/// join (enough rows for several chunks); it equals spareval.
#[test]
fn parallel_probe_join_equals_spareval() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    for i in 0..12_000 {
        let quad = Quad::new(
            ex(&format!("t{i}")),
            rdf_type.clone(),
            ex("T"),
            GraphName::DefaultGraph,
        );
        tx.insert(quad.as_ref());
        for j in 0..(i % 4) {
            let quad = Quad::new(
                ex(&format!("t{i}")),
                ex("p"),
                ex(&format!("o{}", (i * 7 + j) % 5000)),
                GraphName::DefaultGraph,
            );
            tx.insert(quad.as_ref());
        }
    }
    for i in 0..400_000 {
        let quad = Quad::new(
            ex(&format!("u{}", i / 4)),
            ex("p"),
            ex(&format!("o{}", i % 5000)),
            GraphName::DefaultGraph,
        );
        tx.insert(quad.as_ref());
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let spareval = QueryOptions {
        force_spareval: true,
        ..QueryOptions::default()
    };
    for text in [
        format!("SELECT ?x ?y WHERE {{ ?x a <{EX}T> . ?x <{EX}p> ?y }}"),
        format!("SELECT ?x ?y WHERE {{ ?x a <{EX}T> OPTIONAL {{ ?x <{EX}p> ?y }} }}"),
    ] {
        let query = SparqlParser::new().parse_query(&text).unwrap();
        assert!(runs_natively(&query), "{text}");
        let native = rows(
            evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
            false,
        );
        let expected = rows(evaluate_query(&snapshot, &query, &spareval).unwrap(), false);
        assert!(native.len() >= 12_000, "{text}");
        assert_eq!(native, expected, "{text}");
    }
}

/// EXPLAIN runs the query and reports every operator: the BGP's join order with estimated
/// and actual rows, and the operators above it.
#[test]
fn explain_reports_the_plan() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    for i in 0..500 {
        let x = ex(&format!("x{i}"));
        let class = if i % 10 == 0 { "Grad" } else { "Student" };
        let quads = [
            Quad::new(
                x.clone(),
                rdf_type.clone(),
                ex(class),
                GraphName::DefaultGraph,
            ),
            Quad::new(
                x.clone(),
                ex("memberOf"),
                ex(&format!("d{}", i % 25)),
                GraphName::DefaultGraph,
            ),
            Quad::new(
                x,
                ex("takes"),
                ex(&format!("c{}", i % 40)),
                GraphName::DefaultGraph,
            ),
        ];
        for quad in &quads {
            tx.insert(quad.as_ref());
        }
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let text = format!(
        "SELECT ?x ?c WHERE {{ ?x a <{EX}Grad> . ?x <{EX}memberOf> <{EX}d0> . ?x <{EX}takes> ?c FILTER(?c != <{EX}c1>) }}"
    );
    let query = SparqlParser::new().parse_query(&text).unwrap();
    let explanation = explain_query(&snapshot, &query, &QueryOptions::default()).unwrap();
    let solutions = rows(
        evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
        false,
    );
    assert_eq!(explanation.executor, "native");
    assert_eq!(explanation.rows, solutions.len() as u64);
    let operators: Vec<(usize, &str)> = explanation
        .steps
        .iter()
        .map(|s| (s.depth, s.operator.as_str()))
        .collect();
    assert_eq!(operators[0], (0, "project"));
    assert_eq!(operators[1], (1, "filter"));
    let bgp: Vec<_> = explanation.steps.iter().filter(|s| s.depth == 2).collect();
    assert_eq!(bgp.len(), 3, "{:#?}", explanation.steps);
    // memberOf d0 (20 rows) is the most selective start; every join step has an estimate.
    assert!(bgp[0].detail.contains("memberOf"), "{:#?}", bgp);
    assert!(bgp.iter().all(|s| s.estimated_rows.is_some()), "{:#?}", bgp);
    assert_eq!(bgp[0].rows, 20);

    let construct = SparqlParser::new()
        .parse_query("CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }")
        .unwrap();
    let native = explain_query(&snapshot, &construct, &QueryOptions::default()).unwrap();
    assert_eq!((native.executor, native.rows), ("native", 1500));
    let describe = SparqlParser::new()
        .parse_query(&format!("DESCRIBE <{EX}x1>"))
        .unwrap();
    let fallback = explain_query(&snapshot, &describe, &QueryOptions::default()).unwrap();
    assert_eq!((fallback.executor, fallback.rows), ("spareval", 3));
}

/// Filters and aggregates over tables large enough to run in parallel chunks (with terms
/// computed by BIND, regular expressions, strings, decimals) equal spareval.
#[test]
fn parallel_filters_and_aggregates_equal_spareval() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for i in 0..40_000u32 {
        let s = ex(&format!("s{i}"));
        let values: [Term; 3] = [
            Literal::new_typed_literal((i % 997).to_string(), xsd::INTEGER).into(),
            Literal::new_typed_literal(format!("{}.{}", i % 89, i % 7), xsd::DECIMAL).into(),
            if i % 3 == 0 {
                Literal::new_language_tagged_literal_unchecked(format!("name {}", i % 311), "en")
                    .into()
            } else {
                Literal::new_simple_literal(format!("label-{}-{}", i % 53, i % 5)).into()
            },
        ];
        for (p, value) in ["n", "d", "l"].iter().zip(values) {
            let quad = Quad::new(s.clone(), ex(p), value, GraphName::DefaultGraph);
            tx.insert(quad.as_ref());
        }
        let group = Quad::new(
            s,
            ex("g"),
            ex(&format!("g{}", i % 1500)),
            GraphName::DefaultGraph,
        );
        tx.insert(group.as_ref());
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let spareval = QueryOptions {
        force_spareval: true,
        ..QueryOptions::default()
    };
    let queries = [
        format!("SELECT ?s ?l WHERE {{ ?s <{EX}l> ?l FILTER(REGEX(STR(?l), \"-1[0-9]-\")) }}"),
        format!("SELECT ?s WHERE {{ ?s <{EX}l> ?l FILTER(CONTAINS(UCASE(STR(?l)), \"NAME 3\")) }}"),
        format!(
            "SELECT ?s ?x WHERE {{ ?s <{EX}n> ?n . ?s <{EX}d> ?d BIND(CONCAT(STR(?n), \"/\", STR(?d)) AS ?x) FILTER(STRLEN(?x) > 7 && ?d > 40) }}"
        ),
        format!(
            "SELECT ?g (COUNT(*) AS ?c) (SUM(?n) AS ?sn) (AVG(?d) AS ?ad) (MIN(?l) AS ?ml) (MAX(?d) AS ?xd) (COUNT(DISTINCT ?l) AS ?cl) WHERE {{ ?s <{EX}g> ?g . ?s <{EX}n> ?n . ?s <{EX}d> ?d . ?s <{EX}l> ?l }} GROUP BY ?g"
        ),
        format!(
            "SELECT ?g (SUM(?n * 2) AS ?s2) (MAX(STRLEN(STR(?l))) AS ?ml) (SAMPLE(?n) AS ?one) WHERE {{ ?s <{EX}g> ?g . ?s <{EX}n> ?n . ?s <{EX}l> ?l }} GROUP BY ?g"
        ),
        format!(
            "SELECT ?k (COUNT(*) AS ?c) WHERE {{ ?s <{EX}n> ?n BIND(CONCAT(\"k\", STR(?n / 100)) AS ?k) }} GROUP BY ?k"
        ),
    ];
    for text in queries {
        let query = SparqlParser::new()
            .parse_query(&text)
            .unwrap_or_else(|e| panic!("{e}: {text}"));
        assert!(runs_natively(&query), "{text}");
        let native = rows(
            evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
            false,
        );
        let expected = rows(evaluate_query(&snapshot, &query, &spareval).unwrap(), false);
        assert!(!expected.is_empty(), "{text}");
        if native != expected {
            let (a, b): (HashSet<_>, HashSet<_>) =
                (native.iter().collect(), expected.iter().collect());
            let only_native: Vec<_> = a.difference(&b).take(5).collect();
            let only_spareval: Vec<_> = b.difference(&a).take(5).collect();
            panic!(
                "{text}
{} native vs {} spareval rows
only native: {only_native:?}
only spareval: {only_spareval:?}",
                native.len(),
                expected.len()
            );
        }
    }
}

/// A graph result as sorted triple strings, every blank node written as `_:b` (they are
/// fresh per solution, so only their positions can be compared).
fn graph(results: QueryResults<'_>) -> Vec<String> {
    let QueryResults::Graph(triples) = results else {
        panic!("not a graph result");
    };
    let blank = |term: String| {
        if term.starts_with("_:") {
            "_:b".to_owned()
        } else {
            term
        }
    };
    let mut out: Vec<String> = triples
        .map(|t| {
            let t = t.unwrap();
            format!(
                "{} {} {}",
                blank(t.subject.to_string()),
                t.predicate,
                blank(t.object.to_string())
            )
        })
        .collect();
    out.sort();
    out
}

/// CONSTRUCT over the random generator's patterns (with a template of variables, bound or
/// not, constants, a blank node and a literal in subject position) equals spareval.
#[test]
fn construct_equals_spareval() {
    use spargebra::term::{BlankNode, NamedNodePattern, TermPattern, TriplePattern};
    use spargebra::{Query, algebra::GraphPattern};

    let mut rng = Rng(20_260_929);
    let var = |name: &str| TermPattern::Variable(oxrdf::Variable::new_unchecked(name));
    let template = vec![
        TriplePattern {
            subject: var("a"),
            predicate: NamedNodePattern::NamedNode(ex("q")),
            object: var("b"),
        },
        TriplePattern {
            subject: TermPattern::BlankNode(BlankNode::new_unchecked("x")),
            predicate: NamedNodePattern::NamedNode(ex("r")),
            object: var("a"),
        },
        TriplePattern {
            subject: var("b"),
            predicate: NamedNodePattern::Variable(oxrdf::Variable::new_unchecked("c")),
            object: TermPattern::Literal(Literal::new_simple_literal("k")),
        },
        TriplePattern {
            subject: var("d"),
            predicate: NamedNodePattern::NamedNode(ex("s")),
            object: TermPattern::BlankNode(BlankNode::new_unchecked("x")),
        },
    ];
    let mut checked = 0;
    for _ in 0..60 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in random_dataset(&mut rng) {
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for _ in 0..30 {
            let (text, _) = random_query(&mut rng);
            let Query::Select { pattern, .. } = SparqlParser::new().parse_query(&text).unwrap()
            else {
                unreachable!()
            };
            let pattern: GraphPattern = pattern;
            let query = Query::Construct {
                template: template.clone(),
                dataset: None,
                pattern,
                base_iri: None,
            };
            if !runs_natively(&query) {
                continue;
            }
            let native =
                graph(evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap());
            let spareval = QueryOptions {
                force_spareval: true,
                ..QueryOptions::default()
            };
            let expected = graph(evaluate_query(&snapshot, &query, &spareval).unwrap());
            assert_eq!(native, expected, "{query}");
            checked += 1;
        }
    }
    assert!(checked > 1000, "{checked}");
}

/// GRAPH patterns (a graph variable or constant, mixed with default-graph patterns, the
/// graph variable shared across GRAPH blocks and grouped on) equal spareval.
#[test]
fn graph_patterns_equal_spareval() {
    let mut rng = Rng(20_260_930);
    let (mut checked, mut fallbacks) = (0, 0);
    for _ in 0..80 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in random_dataset(&mut rng) {
            // e1 is also a graph name, so ?g can join with entities.
            let graph: GraphName = match rng.below(5) {
                0 | 1 => GraphName::DefaultGraph,
                2 => ex("g0").into(),
                3 => ex("g1").into(),
                _ => ex("e1").into(),
            };
            let quad = Quad::new(quad.subject, quad.predicate, quad.object, graph);
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for _ in 0..40 {
            let inner = group_pattern(&mut rng, 0);
            let other = triple(&mut rng);
            let graph = rng
                .pick(&[
                    "?g",
                    "<http://example.com/g0>",
                    "<http://example.com/e1>",
                    "<http://example.com/g9>",
                ])
                .to_string();
            let text = match rng.below(5) {
                0 => format!("SELECT * WHERE {{ GRAPH {graph} {{ {inner} }} }}"),
                1 => format!("SELECT * WHERE {{ {other} GRAPH {graph} {{ {inner} }} }}"),
                2 => format!("SELECT * WHERE {{ GRAPH ?g {{ {inner} }} GRAPH ?g {{ {other} }} }}"),
                3 => format!(
                    "SELECT ?g (COUNT(*) AS ?n) WHERE {{ GRAPH ?g {{ {inner} }} }} GROUP BY ?g"
                ),
                _ => format!(
                    "SELECT * WHERE {{ GRAPH {graph} {{ {inner} OPTIONAL {{ {other} }} }} }}"
                ),
            };
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            if !runs_natively(&query) {
                fallbacks += 1;
                continue;
            }
            let native = rows(
                evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
                false,
            );
            let spareval = QueryOptions {
                force_spareval: true,
                ..QueryOptions::default()
            };
            let expected = rows(evaluate_query(&snapshot, &query, &spareval).unwrap(), false);
            assert_eq!(native, expected, "{text}");
            checked += 1;
        }
    }
    assert!(
        checked > 1500 && fallbacks * 4 < checked,
        "{checked} checked, {fallbacks} fallbacks"
    );
}

/// SUBSTR, REPLACE, ENCODE_FOR_URI, IRI, the XSD casts and GROUP_CONCAT over random data
/// (dates with timezones, non-canonical integers, language strings) equal spareval.
/// GROUP_CONCAT joins in row order, which SPARQL leaves open: its parts are compared sorted.
#[test]
fn string_functions_casts_and_group_concat_equal_spareval() {
    let expressions = [
        "SUBSTR(STR(?b), 2)",
        "SUBSTR(STR(?b), 2, 3)",
        "SUBSTR(?b, 1, 2)",
        "SUBSTR(STR(?b), 0, 2)",
        "REPLACE(STR(?b), \"[0-9]\", \"#\")",
        "REPLACE(?b, \"(s)([0-9])\", \"$2$1\")",
        "REPLACE(STR(?b), \"S\", \"x\", \"i\")",
        "REPLACE(STR(?b), \"(\", \"x\")",
        "ENCODE_FOR_URI(STR(?a))",
        "ENCODE_FOR_URI(?b)",
        "IRI(CONCAT(\"http://example.com/n/\", ENCODE_FOR_URI(STR(?b))))",
        "IRI(\"relative\")",
        "IRI(?a)",
        "<http://www.w3.org/2001/XMLSchema#integer>(?b)",
        "<http://www.w3.org/2001/XMLSchema#decimal>(?b)",
        "<http://www.w3.org/2001/XMLSchema#double>(?b)",
        "<http://www.w3.org/2001/XMLSchema#float>(?b)",
        "<http://www.w3.org/2001/XMLSchema#boolean>(?b)",
        "<http://www.w3.org/2001/XMLSchema#string>(?b)",
        "<http://www.w3.org/2001/XMLSchema#string>(?a)",
        "<http://www.w3.org/2001/XMLSchema#date>(?b)",
        "<http://www.w3.org/2001/XMLSchema#dateTime>(?b)",
        "<http://www.w3.org/2001/XMLSchema#dateTime>(\"2020-01-01T10:00:00Z\")",
        "<http://www.w3.org/2001/XMLSchema#integer>(STR(?b))",
    ];
    let mut rng = Rng(20_260_931);
    let spareval = QueryOptions {
        force_spareval: true,
        ..QueryOptions::default()
    };
    // GROUP_CONCAT values in a canonical order: the parts of each literal sorted.
    let sorted_parts = |row: &str| -> String {
        row.split(' ')
            .map(
                |term| match term.strip_prefix('"').and_then(|t| t.split_once('"')) {
                    Some((value, rest)) => {
                        let mut parts: Vec<&str> = value.split(',').collect();
                        parts.sort_unstable();
                        format!("\"{}\"{rest}", parts.join(","))
                    }
                    None => term.to_owned(),
                },
            )
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut checked = 0;
    for _ in 0..40 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in random_dataset(&mut rng) {
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for expression in expressions {
            for text in [
                format!("SELECT ?a ?b ({expression} AS ?x) WHERE {{ ?a ?p ?b }}"),
                format!(
                    "SELECT ?a ?b (COALESCE({expression}, \"none\") AS ?x) WHERE {{ ?a ?p ?b FILTER(isLiteral({expression}) || isIRI({expression}) || ?p = <{EX}p0>) }}"
                ),
            ] {
                let query = SparqlParser::new()
                    .parse_query(&text)
                    .unwrap_or_else(|e| panic!("{e}: {text}"));
                assert!(runs_natively(&query), "{text}");
                let native = rows(
                    evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
                    false,
                );
                let expected = rows(evaluate_query(&snapshot, &query, &spareval).unwrap(), false);
                assert_eq!(native, expected, "{text}");
                checked += 1;
            }
        }
        for concat in [
            "GROUP_CONCAT(?b; separator=\",\")",
            "GROUP_CONCAT(DISTINCT ?b; separator=\",\")",
            "GROUP_CONCAT(STR(?b); separator=\",\")",
            "GROUP_CONCAT(DISTINCT STR(?p); separator=\",\")",
        ] {
            let text = format!("SELECT ?a ({concat} AS ?x) WHERE {{ ?a ?p ?b }} GROUP BY ?a");
            let query = SparqlParser::new().parse_query(&text).unwrap();
            assert!(runs_natively(&query), "{text}");
            let normalise = |rows: Vec<String>| {
                let mut rows: Vec<String> = rows.iter().map(|r| sorted_parts(r)).collect();
                rows.sort();
                rows
            };
            let native = normalise(rows(
                evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
                false,
            ));
            let expected = normalise(rows(
                evaluate_query(&snapshot, &query, &spareval).unwrap(),
                false,
            ));
            assert_eq!(native, expected, "{text}");
            checked += 1;
        }
    }
    assert_eq!(checked, 40 * (24 * 2 + 4));
}

/// Every quad of `engine`, blank nodes written as `_:b`, sorted.
fn contents(engine: &Engine) -> Vec<String> {
    let snapshot = engine.snapshot();
    let blank = |t: String| {
        if t.starts_with("_:") {
            "_:b".to_owned()
        } else {
            t
        }
    };
    let mut out: Vec<String> = snapshot
        .quads_for_pattern(&nrese_engine::QuadPattern::all())
        .map(|q| {
            let q = snapshot.decode_quad(q).unwrap();
            format!(
                "{} {} {} {}",
                blank(q.subject.to_string()),
                q.predicate,
                blank(q.object.to_string()),
                blank(q.graph_name.to_string())
            )
        })
        .collect();
    out.sort();
    out
}

/// DELETE/INSERT … WHERE with the WHERE evaluated natively (over random patterns, with
/// blank nodes, graph variables and named graphs in the templates) changes the store
/// exactly as with the WHERE on spareval.
#[test]
fn native_updates_equal_spareval_updates() {
    use nrese_sparql::{UpdateOptions, apply_update};
    let mut rng = Rng(20_260_932);
    let templates = [
        (
            "DELETE { ?a ?p ?b }",
            "INSERT { ?b <http://example.com/q> ?a }",
        ),
        (
            "DELETE { ?a <http://example.com/p1> ?b }",
            "INSERT { _:x <http://example.com/r> ?a . _:x <http://example.com/r> ?b }",
        ),
        ("", "INSERT { GRAPH <http://example.com/g> { ?a ?p ?b } }"),
        (
            "DELETE { ?b ?p ?a }",
            "INSERT { GRAPH ?a { ?a <http://example.com/s> ?c } }",
        ),
        ("DELETE { ?a ?p ?b }", ""),
    ];
    let mut checked = 0;
    for _ in 0..60 {
        let data = random_dataset(&mut rng);
        let engines = [
            Engine::new(EngineConfig::default()).unwrap(),
            Engine::new(EngineConfig::default()).unwrap(),
        ];
        for engine in &engines {
            let mut tx = engine.transaction();
            for quad in &data {
                tx.insert(quad.as_ref());
            }
            tx.commit().unwrap();
        }
        for _ in 0..10 {
            let pattern = group_pattern(&mut rng, 0);
            let (delete, insert) = *rng.pick(&templates);
            let text = format!("{delete} {insert} WHERE {{ ?a ?p ?b . {pattern} }}");
            let update = SparqlParser::new()
                .parse_update(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            for (engine, force_spareval) in engines.iter().zip([false, true]) {
                let options = UpdateOptions {
                    force_spareval,
                    ..UpdateOptions::default()
                };
                let mut tx = engine.transaction();
                apply_update(&mut tx, &update, &options).unwrap();
                tx.commit().unwrap();
            }
            let (native, spareval) = (contents(&engines[0]), contents(&engines[1]));
            if native != spareval {
                let only = |a: &[String], b: &[String]| -> Vec<String> {
                    a.iter().filter(|x| !b.contains(x)).cloned().collect()
                };
                panic!(
                    "{text}
only native: {:?}
only spareval: {:?}",
                    only(&native, &spareval),
                    only(&spareval, &native)
                );
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 600);
}

/// A quad one solution deletes and another inserts is present afterwards: every deletion
/// comes before any insertion (SPARQL 1.1 Update 3.1.3), on both paths.
#[test]
fn deletions_precede_insertions() {
    use nrese_sparql::{UpdateOptions, apply_update};
    let text = format!(
        "DELETE {{ ?d <{EX}q> <{EX}y> }} INSERT {{ ?i <{EX}q> <{EX}y> }} WHERE {{ {{ BIND(<{EX}x> AS ?i) }} UNION {{ BIND(<{EX}x> AS ?d) }} }}"
    );
    let update = SparqlParser::new().parse_update(&text).unwrap();
    for force_spareval in [false, true] {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let quad = Quad::new(ex("x"), ex("q"), ex("y"), GraphName::DefaultGraph);
        let mut tx = engine.transaction();
        tx.insert(quad.as_ref());
        tx.commit().unwrap();
        let options = UpdateOptions {
            force_spareval,
            ..UpdateOptions::default()
        };
        let mut tx = engine.transaction();
        apply_update(&mut tx, &update, &options).unwrap();
        tx.commit().unwrap();
        assert_eq!(contents(&engine).len(), 1, "spareval: {force_spareval}");
    }
}
