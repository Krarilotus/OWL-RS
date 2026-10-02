//! The parser's own decisions, beyond what the W3C suites check: associativity, signed
//! literals, deterministic generated names, nesting limits, where aggregates may stand,
//! and a writer that keeps the meaning of what it writes.

use nrese_rdf::vocab::xsd;
use nrese_rdf::{Literal, NamedNode, Variable};
use nrese_sparql_syntax::algebra::{Expression, GraphPattern};
use nrese_sparql_syntax::term::{TermPattern, TriplePattern};
use nrese_sparql_syntax::{Query, SparqlParser};

fn parse(text: &str) -> Query {
    SparqlParser::new()
        .parse_query(text)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
}

/// The expression of `SELECT (e AS ?r) {}`.
fn expression(e: &str) -> Expression {
    let query = parse(&format!("SELECT ({e} AS ?r) {{}}"));
    let GraphPattern::Project { inner, .. } = query.pattern() else {
        panic!("no projection")
    };
    let GraphPattern::Extend { expression, .. } = inner.as_ref() else {
        panic!("no extension")
    };
    expression.clone()
}

fn int(value: &str) -> Expression {
    Literal::new_typed_literal(value, xsd::INTEGER).into()
}

fn sub(a: Expression, b: Expression) -> Expression {
    Expression::Subtract(Box::new(a), Box::new(b))
}

#[test]
fn operators_associate_to_the_left() {
    assert_eq!(
        expression("10 - 5 - 2"),
        sub(sub(int("10"), int("5")), int("2"))
    );
    assert_eq!(
        expression("8 / 4 / 2"),
        Expression::Divide(
            Box::new(Expression::Divide(Box::new(int("8")), Box::new(int("4")))),
            Box::new(int("2"))
        )
    );
}

#[test]
fn a_sign_before_digits_makes_a_literal() {
    assert_eq!(expression("-1"), int("-1"));
    assert_eq!(expression("+1"), int("+1"));
    assert_eq!(
        expression("- 1"),
        Expression::UnaryMinus(Box::new(int("1")))
    );
    let x: Expression = Variable::new_unchecked("x").into();
    // After an operand the sign is the operator, as the grammar's rule 116 has it.
    let query = parse("SELECT (?x -1 AS ?r) { BIND(1 AS ?x) }");
    let GraphPattern::Project { inner, .. } = query.pattern() else {
        panic!()
    };
    let GraphPattern::Extend { expression, .. } = inner.as_ref() else {
        panic!()
    };
    assert_eq!(*expression, sub(x, int("1")));
}

#[test]
fn the_same_text_gives_the_same_algebra() {
    let text = "SELECT ?s (COUNT(*) AS ?n) { ?s <http://e/p> [ <http://e/q> (1 2) ] } GROUP BY ?s";
    assert_eq!(parse(text), parse(text));
    // The generated names are numbered, not random.
    assert!(format!("{:?}", parse(text)).contains("__agg"));
}

#[test]
fn generated_names_keep_clear_of_the_query_s_own() {
    let query = parse("SELECT ?__agg0 (COUNT(*) AS ?n) { ?__agg0 ?p [] } GROUP BY ?__agg0");
    let debug = format!("{query:?}");
    assert!(debug.contains("\"___agg"), "{debug}");
}

#[test]
fn nesting_is_bounded_and_deep_input_is_an_error() {
    // At the default limit a release build needs about 330 KiB of stack, so a 1 MiB stack
    // (Windows' main thread) is enough; a debug build needs about 2.2 MiB.
    let stack = if cfg!(debug_assertions) {
        4 << 20
    } else {
        1 << 20
    };
    let parse = move |text: String| {
        std::thread::Builder::new()
            .stack_size(stack)
            .spawn(move || SparqlParser::new().parse_query(&text).map(|_| ()))
            .unwrap()
            .join()
            .unwrap()
    };
    let deep = format!(
        "SELECT * {{ {}?s ?p ?o{} }}",
        "{ ".repeat(1000),
        " }".repeat(1000)
    );
    let error = parse(deep).unwrap_err();
    assert!(error.message().contains("nested deeper"), "{error}");
    let brackets = format!(
        "SELECT ({}1{} AS ?x) {{}}",
        "(".repeat(10_000),
        ")".repeat(10_000)
    );
    assert!(parse(brackets).is_err());
    let depth = nrese_sparql_syntax::DEFAULT_MAX_NESTING - 1;
    parse(format!(
        "SELECT ({}1{} AS ?x) {{}}",
        "(".repeat(depth),
        ")".repeat(depth)
    ))
    .unwrap();
    parse(format!(
        "SELECT * {{ {}?s ?p ?o{} }}",
        "{ ".repeat(depth - 1),
        " }".repeat(depth - 1)
    ))
    .unwrap();
}

#[test]
fn aggregates_only_where_the_grammar_allows_them() {
    for text in [
        "SELECT * { ?s ?p ?o FILTER(COUNT(?s) > 1) }",
        "SELECT ?s { ?s ?p ?o BIND(SUM(?o) AS ?x) } GROUP BY ?s",
        "SELECT (COUNT(SUM(?o)) AS ?n) { ?s ?p ?o }",
        "SELECT ?s { ?s ?p ?o } GROUP BY (COUNT(?o))",
    ] {
        assert!(SparqlParser::new().parse_query(text).is_err(), "{text}");
    }
    for text in [
        "SELECT ?s { ?s ?p ?o } GROUP BY ?s HAVING (COUNT(?o) > 1) ORDER BY DESC(SUM(?o))",
        "CONSTRUCT { ?s ?p ?s } { ?s ?p ?o } GROUP BY ?s ?p HAVING (COUNT(*) > 1)",
    ] {
        parse(text);
    }
}

#[test]
fn sparql_1_2_and_extensions_can_be_switched_off() {
    let strict = SparqlParser::new()
        .with_sparql_12(false)
        .with_lateral(false);
    for text in [
        "SELECT * { <<( ?s ?p ?o )>> ?q ?r }",
        "SELECT * { ?s ?p ?o ~ ?r }",
        "SELECT (\"x\"@en--ltr AS ?l) {}",
        "VERSION \"1.2\" SELECT * {}",
        "SELECT * { ?s ?p ?o LATERAL { ?o ?q ?r } }",
        "SELECT (!!true AS ?t) {}",
    ] {
        assert!(strict.parse_query(text).is_err(), "{text}");
        parse(text);
    }
}

#[test]
fn errors_say_where() {
    let error = SparqlParser::new()
        .parse_query("SELECT *\nWHERE { ?s ?p }")
        .unwrap_err();
    let at = error.location().unwrap();
    assert_eq!((at.line, at.column), (1, 14), "{error}");
    let error = SparqlParser::new()
        .parse_query("SELECT * { ?s ex:p ?o }")
        .unwrap_err();
    assert!(error.message().contains("'ex:'"), "{error}");
}

/// Written and parsed again, `query` keeps its algebra.
fn round_trip(query: &Query) -> Query {
    let text = query.to_string();
    SparqlParser::new()
        .parse_query(&text)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn bgp(s: &str) -> GraphPattern {
    GraphPattern::Bgp {
        patterns: vec![TriplePattern::new(
            Variable::new_unchecked(s),
            NamedNode::new_unchecked("http://e/p"),
            TermPattern::from(Variable::new_unchecked("o")),
        )],
    }
}

#[test]
fn the_writer_keeps_a_filter_where_it_was() {
    // `Join(Filter(A), B)`: written as `A FILTER(f) B` it would come back as
    // `Filter(Join(A, B))`, and a federated query would mean something else.
    let filter = GraphPattern::Filter {
        expr: Expression::Bound(Variable::new_unchecked("b")),
        inner: Box::new(bgp("a")),
    };
    let pattern = GraphPattern::Join {
        left: Box::new(filter.clone()),
        right: Box::new(GraphPattern::Union {
            left: Box::new(bgp("b")),
            right: Box::new(bgp("c")),
        }),
    };
    let query = Query::Select {
        dataset: None,
        pattern: GraphPattern::Project {
            inner: Box::new(pattern),
            variables: vec![Variable::new_unchecked("o")],
        },
        base_iri: None,
    };
    assert_eq!(round_trip(&query), query);

    // An optional part that is itself a filter, without a condition of its own: SPARQL
    // has no exact text for it, so it goes into a subquery and stays a filter.
    let optional = GraphPattern::LeftJoin {
        left: Box::new(bgp("a")),
        right: Box::new(filter),
        expression: None,
    };
    let query = Query::Select {
        dataset: None,
        pattern: GraphPattern::Project {
            inner: Box::new(optional),
            variables: vec![Variable::new_unchecked("o")],
        },
        base_iri: None,
    };
    let again = round_trip(&query);
    let GraphPattern::Project { inner, .. } = again.pattern() else {
        panic!()
    };
    let GraphPattern::LeftJoin {
        right, expression, ..
    } = inner.as_ref()
    else {
        panic!("{again:?}")
    };
    assert!(expression.is_none());
    assert!(
        matches!(right.as_ref(), GraphPattern::Project { inner, .. } if matches!(inner.as_ref(), GraphPattern::Filter { .. }))
    );
}

#[test]
fn a_bare_pattern_is_written_as_select_star() {
    // What federation sends: a SERVICE's inner pattern as a query of its own.
    let query = Query::Select {
        dataset: None,
        pattern: bgp("s"),
        base_iri: None,
    };
    let text = query.to_string();
    assert_eq!(text, "SELECT * WHERE { ?s <http://e/p> ?o . }");
}
