//! Integers beyond signed 64-bit range: ORDER BY, MIN, MAX, comparisons, equality,
//! grouping, casts, and non-panicking operators across native and reference evaluation.

use nrese_engine::{Engine, EngineConfig, Snapshot};
use nrese_rdf::vocab::xsd;
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_sparql::{QueryOptions, QueryResults, evaluate_query, runs_natively};
use nrese_sparql_syntax::SparqlParser;

const BASE: &str = "urn:big-int:";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const PREFIX: &str = "PREFIX : <urn:big-int:> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> ";

fn node(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{BASE}{local}"))
}

fn typed(lexical: &str, datatype: &str) -> Literal {
    Literal::new_typed_literal(
        lexical,
        NamedNode::new_unchecked(format!("{XSD}{datatype}")),
    )
}

fn collect_rows(results: QueryResults<'_>) -> Vec<Vec<Option<Term>>> {
    let QueryResults::Solutions(solutions) = results else {
        panic!("expected SELECT solutions");
    };
    let variables = solutions.variables().to_vec();
    solutions
        .map(|row| {
            let row = row.unwrap();
            variables.iter().map(|v| row.get(v).cloned()).collect()
        })
        .collect()
}

fn run_both(
    snapshot: &Snapshot,
    body: &str,
    options: &QueryOptions,
    ordered: bool,
) -> Vec<Vec<Option<Term>>> {
    let text = format!("{PREFIX}{body}");
    let query = SparqlParser::new().parse_query(&text).unwrap();
    assert!(runs_natively(&query), "query must run natively: {text}");
    let mut native = collect_rows(evaluate_query(snapshot, &query, options).unwrap());
    let mut reference =
        collect_rows(nrese_sparql_reference::evaluate_query(snapshot, &query, options).unwrap());
    if !ordered {
        let key = |row: &Vec<Option<Term>>| {
            row.iter()
                .map(|t| t.as_ref().map_or_else(String::new, ToString::to_string))
                .collect::<Vec<_>>()
        };
        native.sort_by_key(key);
        reference.sort_by_key(key);
    }
    assert_eq!(native, reference, "native != reference for: {text}");
    native
}

fn load(triples: impl IntoIterator<Item = (NamedNode, NamedNode, Term)>) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for (s, p, o) in triples {
        tx.insert(Quad::new(s, p, o, GraphName::DefaultGraph).as_ref());
    }
    tx.commit().unwrap();
    engine
}

fn single_term(rows: Vec<Vec<Option<Term>>>) -> Option<Term> {
    assert_eq!(rows.len(), 1, "expected 1 row, got {rows:?}");
    assert_eq!(rows[0].len(), 1, "expected 1 column, got {rows:?}");
    rows.into_iter().next().unwrap().into_iter().next().unwrap()
}

#[test]
fn big_integer_order_by_max_min_and_comparisons() {
    let huge_pos = format!("1{}", "0".repeat(400));
    let huge_neg = format!("-1{}", "0".repeat(400));
    let entries: Vec<(&str, String, &str)> = vec![
        ("i0", "9223372036854775807".into(), "integer"),
        ("i1", "9223372036854775808".into(), "integer"),
        ("i2", "100000000000000000000".into(), "integer"),
        ("ix", "+09223372036854775807".into(), "integer"),
        ("ix1", "+09223372036854775808".into(), "integer"),
        ("u0", "9223372036854775807".into(), "unsignedLong"),
        ("u1", "9223372036854775808".into(), "unsignedLong"),
        ("u2", "18446744073709551615".into(), "unsignedLong"),
        ("n0", "9223372036854775807".into(), "nonNegativeInteger"),
        ("n1", "9223372036854775808".into(), "nonNegativeInteger"),
        ("n2", "100000000000000000001".into(), "nonNegativeInteger"),
        ("p2", "100000000000000000002".into(), "positiveInteger"),
        ("m0", "-9223372036854775808".into(), "integer"),
        ("m1", "-9223372036854775809".into(), "integer"),
        ("m2", "-100000000000000000000".into(), "negativeInteger"),
        ("hp", huge_pos.clone(), "integer"),
        ("hn", huge_neg.clone(), "integer"),
        ("dpos", "1e300".into(), "double"),
        ("dneg", "-1e300".into(), "double"),
        ("pinf", "INF".into(), "double"),
        ("ninf", "-INF".into(), "double"),
        ("dec", "100000000000000000000.0".into(), "decimal"),
        ("bad_ul", "18446744073709551616".into(), "unsignedLong"),
        ("bad_l", "9223372036854775808".into(), "long"),
    ];
    let p_v = node("v");
    let engine = load(
        entries
            .iter()
            .map(|(s, lex, dt)| (node(s), p_v.clone(), Term::from(typed(lex, dt)))),
    );
    let snapshot = engine.snapshot();
    let opts = QueryOptions::default();

    let iris = |rows: Vec<Vec<Option<Term>>>| -> Vec<String> {
        rows.into_iter()
            .map(|r| match r[0].as_ref().unwrap() {
                Term::NamedNode(n) => n.as_str().strip_prefix(BASE).unwrap().to_owned(),
                other => panic!("expected IRI, got {other}"),
            })
            .collect()
    };

    // Ascending ORDER BY over the 6 positive values across i64::MAX, u64::MAX, and 10^20+2.
    assert_eq!(
        iris(run_both(
            &snapshot,
            "SELECT ?s { VALUES ?s { :i0 :n1 :u2 :i2 :n2 :p2 } ?s :v ?v } ORDER BY ?v DESC(STR(?s))",
            &opts,
            true,
        )),
        vec!["i0", "n1", "u2", "i2", "n2", "p2"]
    );

    // Descending ORDER BY with LIMIT (exercises top-k selection in order_ranks).
    assert_eq!(
        iris(run_both(
            &snapshot,
            "SELECT ?s { VALUES ?s { :i0 :n1 :u2 :i2 :n2 :p2 } ?s :v ?v } ORDER BY DESC(?v) LIMIT 3",
            &opts,
            true,
        )),
        vec!["p2", "n2", "i2"]
    );
    assert_eq!(
        iris(run_both(
            &snapshot,
            "SELECT ?s { VALUES ?s { :i0 :n1 :u2 :i2 :n2 :p2 } ?s :v ?v } ORDER BY ?v LIMIT 3",
            &opts,
            true,
        )),
        vec!["i0", "n1", "u2"]
    );

    // Full numeric order spanning -INF, -10^400, -1e300, negative big ints, i64 bounds,
    // positive big ints, 1e300, +10^400, +INF, and ill-typed integers sorting after numbers.
    assert_eq!(
        iris(run_both(
            &snapshot,
            "SELECT ?s { VALUES ?s { :ninf :hn :dneg :m2 :m1 :m0 :i0 :i1 :u2 :i2 :p2 :dpos :hp :pinf :bad_ul :bad_l } ?s :v ?v } ORDER BY ?v",
            &opts,
            true,
        )),
        vec![
            "ninf", "hn", "dneg", "m2", "m1", "m0", "i0", "i1", "u2", "i2", "p2", "dpos", "hp",
            "pinf", "bad_ul", "bad_l"
        ]
    );

    // MAX and MIN over positive values.
    assert_eq!(
        single_term(run_both(
            &snapshot,
            "SELECT (MAX(?v) AS ?x) { VALUES ?s { :i0 :n1 :u2 :i2 :n2 :p2 } ?s :v ?v }",
            &opts,
            false,
        )),
        Some(Term::from(typed(
            "100000000000000000002",
            "positiveInteger"
        )))
    );
    assert_eq!(
        single_term(run_both(
            &snapshot,
            "SELECT (MIN(?v) AS ?x) { VALUES ?s { :i0 :n1 :u2 :i2 :n2 :p2 } ?s :v ?v }",
            &opts,
            false,
        )),
        Some(Term::from(typed("9223372036854775807", "integer")))
    );

    // MIN and MAX with negative big integers and 400-digit integers.
    assert_eq!(
        single_term(run_both(
            &snapshot,
            "SELECT (MIN(?v) AS ?x) { VALUES ?s { :m2 :m1 :m0 :i0 :p2 } ?s :v ?v }",
            &opts,
            false,
        )),
        Some(Term::from(typed(
            "-100000000000000000000",
            "negativeInteger"
        )))
    );
    assert_eq!(
        single_term(run_both(
            &snapshot,
            "SELECT (MAX(?v) AS ?x) { VALUES ?s { :m2 :m1 :m0 } ?s :v ?v }",
            &opts,
            false,
        )),
        Some(Term::from(typed("-9223372036854775808", "integer")))
    );
    assert_eq!(
        single_term(run_both(
            &snapshot,
            "SELECT (MAX(?v) AS ?x) { VALUES ?s { :dpos :hp } ?s :v ?v }",
            &opts,
            false,
        )),
        Some(Term::from(typed(&huge_pos, "integer")))
    );
    assert_eq!(
        single_term(run_both(
            &snapshot,
            "SELECT (MIN(?v) AS ?x) { VALUES ?s { :dneg :hn } ?s :v ?v }",
            &opts,
            false,
        )),
        Some(Term::from(typed(&huge_neg, "integer")))
    );

    // FILTER comparisons: variable-to-variable and variable-to-constant (exercises range hints
    // and fast filter compilation).
    assert_eq!(
        iris(run_both(
            &snapshot,
            "SELECT ?s { VALUES ?s { :i0 :i1 :u1 :n1 :bad_l :bad_ul } :i0 :v ?a . ?s :v ?b . FILTER(?a < ?b) } ORDER BY STR(?s)",
            &opts,
            true,
        )),
        vec!["i1", "n1", "u1"]
    );
    assert_eq!(
        iris(run_both(
            &snapshot,
            "SELECT ?s { ?s :v ?v . FILTER(?v > 9223372036854775807 && ?v <= 100000000000000000002) } ORDER BY STR(?s)",
            &opts,
            true,
        )),
        vec!["dec", "i1", "i2", "ix1", "n1", "n2", "p2", "u1", "u2"]
    );
    assert_eq!(
        iris(run_both(
            &snapshot,
            "SELECT ?s { ?s :v ?v . FILTER(?v < -9223372036854775808) } ORDER BY STR(?s)",
            &opts,
            true,
        )),
        vec!["dneg", "hn", "m1", "m2", "ninf"]
    );

    // Value equality across integer subtypes, non-canonical lexical forms, and decimals.
    assert_eq!(
        iris(run_both(
            &snapshot,
            "SELECT ?s { ?s :v ?v . FILTER(?v = 9223372036854775808) } ORDER BY STR(?s)",
            &opts,
            true,
        )),
        vec!["i1", "ix1", "n1", "u1"]
    );
    assert_eq!(
        iris(run_both(
            &snapshot,
            "SELECT ?s { ?s :v ?v . FILTER(?v = 100000000000000000000) } ORDER BY STR(?s)",
            &opts,
            true,
        )),
        vec!["dec", "i2"]
    );
    assert_eq!(
        iris(run_both(
            &snapshot,
            "SELECT ?s { VALUES ?s { :i0 :i1 :u1 :n1 :p2 :bad_l } ?s :v ?v . FILTER(?v IN (9223372036854775808, 100000000000000000002)) } ORDER BY STR(?s)",
            &opts,
            true,
        )),
        vec!["i1", "n1", "p2", "u1"]
    );
}

#[test]
fn big_integer_min_max_across_aggregation_routes() {
    let p_in = node("in");
    let p_item = node("item");
    let p_val = node("val");

    let mut triples = Vec::new();
    // Group g1 joins through two items to exercise eager aggregation and streamed GROUP BY.
    for (tag, item) in [("t1", "a1"), ("t2", "a1"), ("t3", "a2")] {
        triples.push((node("g1"), p_in.clone(), Term::from(node(tag))));
        triples.push((node(tag), p_item.clone(), Term::from(node(item))));
    }
    for (item, lex, dt) in [
        ("a1", "9223372036854775807", "integer"),
        ("a1", "9223372036854775808", "nonNegativeInteger"),
        ("a1", "18446744073709551615", "unsignedLong"),
        ("a2", "100000000000000000000", "integer"),
        ("a2", "100000000000000000002", "positiveInteger"),
        ("a2", "+0100000000000000000002", "integer"),
    ] {
        triples.push((node(item), p_val.clone(), Term::from(typed(lex, dt))));
    }
    // Group g2 holds negative big integers.
    triples.push((node("g2"), p_in.clone(), Term::from(node("t4"))));
    triples.push((node("t4"), p_item.clone(), Term::from(node("a3"))));
    for (lex, dt) in [
        ("-9223372036854775808", "integer"),
        ("-9223372036854775809", "integer"),
        ("-100000000000000000005", "negativeInteger"),
    ] {
        triples.push((node("a3"), p_val.clone(), Term::from(typed(lex, dt))));
    }

    let engine = load(triples);
    let snapshot = engine.snapshot();

    // Direct, streamed, and eager aggregation routes must all agree with the reference.
    let direct = QueryOptions::default();
    let streamed = QueryOptions {
        stream_rows: Some(1),
        ..QueryOptions::default()
    };

    for query in [
        "SELECT ?g (MIN(?v) AS ?lo) (MAX(?v) AS ?hi) { ?g :in ?t . ?t :item ?i . ?i :val ?v } GROUP BY ?g ORDER BY STR(?g)",
        "SELECT ?g (MIN(DISTINCT ?v) AS ?lo) (MAX(DISTINCT ?v) AS ?hi) { ?g :in ?t . ?t :item ?i . ?i :val ?v } GROUP BY ?g ORDER BY STR(?g)",
    ] {
        let rows_direct = run_both(&snapshot, query, &direct, true);
        let rows_streamed = run_both(&snapshot, query, &streamed, true);
        assert_eq!(rows_direct, rows_streamed, "direct != streamed: {query}");
        assert_eq!(
            rows_direct,
            vec![
                vec![
                    Some(Term::from(node("g1"))),
                    Some(Term::from(typed("9223372036854775807", "integer"))),
                    Some(Term::from(typed(
                        "100000000000000000002",
                        "positiveInteger"
                    ))),
                ],
                vec![
                    Some(Term::from(node("g2"))),
                    Some(Term::from(typed(
                        "-100000000000000000005",
                        "negativeInteger"
                    ))),
                    Some(Term::from(typed("-9223372036854775808", "integer"))),
                ],
            ]
        );
    }

    // GROUP BY canonicalized big integer merges equal values with different lexical forms/subtypes.
    let grouped = run_both(
        &snapshot,
        "SELECT ?k (COUNT(?v) AS ?n) { :a2 :val ?v BIND(xsd:integer(?v) AS ?k) } GROUP BY ?k ORDER BY ?k",
        &direct,
        true,
    );
    assert_eq!(
        grouped,
        vec![
            vec![
                Some(Term::from(typed("100000000000000000000", "integer"))),
                Some(Term::from(Literal::new_typed_literal("1", xsd::INTEGER))),
            ],
            vec![
                Some(Term::from(typed("100000000000000000002", "integer"))),
                Some(Term::from(Literal::new_typed_literal("2", xsd::INTEGER))),
            ],
        ]
    );
}

#[test]
fn big_integer_operators_and_casts_never_panic() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let snapshot = engine.snapshot();
    let opts = QueryOptions::default();

    let check = |expr: &str, expected: Option<Term>| {
        let query = format!("SELECT ({expr} AS ?r) {{}}");
        let got = single_term(run_both(&snapshot, &query, &opts, false));
        assert_eq!(got, expected, "{expr}");
    };

    let bool_term = |b: bool| Some(Term::from(Literal::from(b)));

    // Type predicates & EBV.
    check("isNumeric(9223372036854775808)", bool_term(true));
    check(
        r#"isNumeric("18446744073709551615"^^xsd:unsignedLong)"#,
        bool_term(true),
    );
    check(
        r#"isNumeric("18446744073709551616"^^xsd:unsignedLong)"#,
        bool_term(false),
    );
    check(
        r#"isNumeric("9223372036854775808"^^xsd:long)"#,
        bool_term(false),
    );
    check("xsd:boolean(9223372036854775808)", bool_term(true));
    check("xsd:boolean(-9223372036854775809)", bool_term(true));
    check(
        r#"xsd:boolean("18446744073709551616"^^xsd:unsignedLong)"#,
        None,
    );
    check("!(9223372036854775808)", bool_term(false));

    // Lexical / datatype preservation and casts.
    check(
        r#"STR("+09223372036854775808"^^xsd:nonNegativeInteger)"#,
        Some(Term::from(Literal::new_simple_literal(
            "+09223372036854775808",
        ))),
    );
    check(
        r#"DATATYPE("18446744073709551615"^^xsd:unsignedLong)"#,
        Some(Term::from(NamedNode::new_unchecked(format!(
            "{XSD}unsignedLong"
        )))),
    );
    check(
        r#"xsd:string("+0100000000000000000002"^^xsd:positiveInteger)"#,
        Some(Term::from(Literal::new_simple_literal(
            "100000000000000000002",
        ))),
    );
    check(
        r#"xsd:integer("+0100000000000000000002"^^xsd:positiveInteger)"#,
        Some(Term::from(typed("100000000000000000002", "integer"))),
    );
    check(
        r#"xsd:integer("100000000000000000002")"#,
        Some(Term::from(typed("100000000000000000002", "integer"))),
    );
    check(
        "xsd:decimal(100000000000000000000)",
        Some(Term::from(typed("100000000000000000000", "decimal"))),
    );
    check(
        "xsd:double(100000000000000000000)",
        Some(Term::from(typed("1.0E20", "double"))),
    );
    check(
        "xsd:float(100000000000000000000)",
        Some(Term::from(typed("1.0E20", "float"))),
    );

    // 400-digit integer casts to double/float yield INF / -INF without panicking.
    let huge = format!("1{}", "0".repeat(400));
    check(
        &format!("xsd:double({huge})"),
        Some(Term::from(typed("INF", "double"))),
    );
    check(
        &format!("xsd:double(-{huge})"),
        Some(Term::from(typed("-INF", "double"))),
    );
    check(
        &format!("xsd:float({huge})"),
        Some(Term::from(typed("INF", "float"))),
    );
    check(&format!("xsd:decimal({huge})"), None);

    // Arithmetic, rounding, totals, and SUBSTR on big integers cleanly error (unbound, no panic).
    for expr in [
        "100000000000000000000 + 1",
        "100000000000000000000 - 1",
        "100000000000000000000 * 2",
        "100000000000000000000 / 2",
        "-100000000000000000000 - 1",
        "ABS(-100000000000000000000)",
        "ROUND(100000000000000000000)",
        "CEIL(100000000000000000000)",
        "FLOOR(100000000000000000000)",
        r#"SUBSTR("abcdef", 100000000000000000000, 2)"#,
    ] {
        check(expr, None);
    }

    for agg in ["SUM", "AVG"] {
        let query = format!(
            "SELECT ({agg}(?v) AS ?r) {{ VALUES ?v {{ 9223372036854775808 100000000000000000000 }} }}"
        );
        assert_eq!(single_term(run_both(&snapshot, &query, &opts, false)), None);
    }
}
