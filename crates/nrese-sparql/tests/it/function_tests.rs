//! String functions against the values their specifications give (SPARQL 1.1 §17.4.3,
//! XPath and XQuery Functions and Operators 3.1): the reference evaluator shares the
//! native one's functions, so the differential tests can't catch them.

use nrese_engine::{Engine, EngineConfig};
use nrese_sparql::{QueryOptions, QueryResults, evaluate_query};
use nrese_sparql_syntax::SparqlParser;

/// The value of `expression` (its lexical form, or `None` where it is an error and the
/// variable stays unbound).
fn value(expression: &str) -> Option<String> {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let snapshot = engine.snapshot();
    let text = format!("SELECT ?x WHERE {{ BIND({expression} AS ?x) }}");
    let query = SparqlParser::new()
        .parse_query(&text)
        .unwrap_or_else(|e| panic!("{e}: {text}"));
    let QueryResults::Solutions(solutions) =
        evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap()
    else {
        panic!("solutions")
    };
    let rows: Vec<_> = solutions.map(Result::unwrap).collect();
    assert_eq!(rows.len(), 1, "{text}");
    rows[0].get("x").map(|term| match term {
        nrese_rdf::Term::Literal(literal) => literal.value().to_owned(),
        other => other.to_string(),
    })
}

/// SUBSTR's positions are integers (SPARQL 1.1 §17.4.3.3), zero and negative ones too,
/// clipped as `fn:substring` clips them; a position of another numeric type is an error.
#[test]
fn substr_takes_signed_integer_positions() {
    for (expression, expected) in [
        (r#"SUBSTR("motor car", 6)"#, Some(" car")),
        (r#"SUBSTR("metadata", 4, 3)"#, Some("ada")),
        (r#"SUBSTR("12345", 0, 3)"#, Some("12")),
        (r#"SUBSTR("12345", -3, 5)"#, Some("1")),
        (r#"SUBSTR("12345", 5, -3)"#, Some("")),
        (r#"SUBSTR("12345", 0)"#, Some("12345")),
        (r#"SUBSTR("12345", -9)"#, Some("12345")),
        (r#"SUBSTR("12345", 9)"#, Some("")),
        (r#"SUBSTR("12345", 2, 0)"#, Some("")),
        (
            r#"SUBSTR("12345", -9223372036854775807, 9223372036854775807)"#,
            Some(""),
        ),
        (r#"SUBSTR("12345", 1.5, 2)"#, None),
        (r#"SUBSTR("12345", 1, 2.0e0)"#, None),
    ] {
        assert_eq!(value(expression).as_deref(), expected, "{expression}");
    }
}

/// REPLACE's replacement is XPath's: `$N` a group (the longest valid number, an absent
/// group the empty string), `\$` and `\\` escapes, any other `$` or `\` an error
/// (FORX0004); a pattern matching the empty string is an error (FORX0003); with `q` the
/// replacement is literal too.
#[test]
fn replace_uses_xpath_replacement_syntax() {
    for (expression, expected) in [
        (r#"REPLACE("abracadabra", "bra", "*")"#, Some("a*cada*")),
        (
            r#"REPLACE("abracadabra", "a(.)", "a$1$1")"#,
            Some("abbraccaddabbra"),
        ),
        (
            r#"REPLACE("darted", "^(.*?)d(.*)$", "$1c$2")"#,
            Some("carted"),
        ),
        (r#"REPLACE("AAAA", "A+", "b")"#, Some("b")),
        (r#"REPLACE("abc", "b", "$1a")"#, Some("aac")),
        (r#"REPLACE("abc", "(b)", "$12")"#, Some("ab2c")),
        (r#"REPLACE("a$b", "\\$", "\\\\")"#, Some("a\\b")),
        (r#"REPLACE("ab", "b", "\\$")"#, Some("a$")),
        (r#"REPLACE("a.b", ".", "$1", "q")"#, Some("a$1b")),
        (r#"REPLACE("x", "(x)", "$0/$01/$12/$99")"#, Some("x/x/x2/9")),
        (r#"REPLACE("x", "(?:x)", "$1a")"#, Some("a")),
        // With q the pattern is literal and x has no effect: a space is a space.
        (r#"REPLACE("a b", " ", "_", "qx")"#, Some("a_b")),
        (r#"REPLACE("a b", " ", "_", "xq")"#, Some("a_b")),
        (r#"REPLACE("A.a", "a", "_", "qi")"#, Some("_._")),
        (r#"REPLACE("a b", " ", "_", "x")"#, None),
        (r#"REPLACE("abracadabra", ".*?", "$1")"#, None),
        (r#"REPLACE("x", "x", "\\q")"#, None),
        (r#"REPLACE("x", "x", "$")"#, None),
    ] {
        assert_eq!(value(expression).as_deref(), expected, "{expression}");
    }
}

/// A group reference's number is found in one pass over its digits: a reference of 300,000
/// leading zeros then 300,000 ones (each shorter prefix parsed again would be 9e10 digit
/// visits) is group 1 and the ones after it.
#[test]
fn a_long_group_reference_is_read_in_one_pass() {
    let digits = format!("{}{}", "0".repeat(300_000), "1".repeat(300_000));
    let expression = format!(r#"REPLACE("x", "(x)", "${digits}")"#);
    let expected = format!("x{}", "1".repeat(299_999));
    assert_eq!(value(&expression), Some(expected));
}
