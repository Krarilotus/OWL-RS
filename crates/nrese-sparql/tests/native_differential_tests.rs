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

use nrese_engine::{EncodedTriple, Engine, EngineConfig, ReadModel};
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
