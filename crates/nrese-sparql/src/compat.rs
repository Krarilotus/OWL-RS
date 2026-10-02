//! Parsing with the rewrites that make queries written for other stores mean here what
//! they mean there. Each rewrite turns a query into standard SPARQL algebra, so both
//! executors evaluate it without knowing about the extension.
//!
//! **`HAVING` on a `SELECT` alias.** `SELECT (SUM(?x) AS ?total) … HAVING (?total > 0)`:
//! in the standard, `HAVING` is evaluated before the `SELECT` expressions, so `?total` is
//! unbound there and no group passes. Jena and QLever read the alias as its expression,
//! and queries in the wild rely on it. [`parse_query`] replaces such a
//! variable in `HAVING` by the expression it names.
//!
//! **Blazegraph's query hints.** `hint:Query hint:optimizer "None" .` and the like
//! (namespace `http://www.bigdata.com/queryHints#`) are triple patterns that tell
//! Blazegraph's planner what to do; ResearchSpace's and other Blazegraph clients' queries
//! carry them. Read as data they match nothing, and the query with them. They are dropped:
//! NRESE plans on its own.

use std::collections::HashMap;

use nrese_rdf::Variable;
use nrese_sparql_syntax::algebra::{Expression, GraphPattern, OrderExpression};
use nrese_sparql_syntax::term::{NamedNodePattern, TermPattern, TriplePattern};
use nrese_sparql_syntax::{Query, SparqlParser, SparqlSyntaxError};

/// Parses a query and applies the compatibility rewrites.
pub fn parse_query(
    text: &str,
    prefixes: Option<&std::collections::BTreeMap<String, String>>,
) -> Result<Query, SparqlSyntaxError> {
    let mut parser = SparqlParser::new();
    for (prefix, namespace) in prefixes.into_iter().flatten() {
        // A namespace that isn't an IRI can't be a prefix; the query fails as without it.
        parser = match parser
            .clone()
            .with_prefix(prefix.clone(), namespace.clone())
        {
            Ok(with) => with,
            Err(_) => parser,
        };
    }
    let mut query = parser.parse_query(text)?;
    let (Query::Select { pattern, .. }
    | Query::Construct { pattern, .. }
    | Query::Describe { pattern, .. }
    | Query::Ask { pattern, .. }) = &mut query;
    having_aliases(pattern);
    Ok(query)
}

/// Blazegraph's query hints' namespace.
const QUERY_HINTS: &str = "http://www.bigdata.com/queryHints#";

/// Whether `triple` is a Blazegraph query hint (its subject or predicate in the hints'
/// namespace).
fn is_hint(triple: &TriplePattern) -> bool {
    let in_hints = |iri: &str| iri.starts_with(QUERY_HINTS);
    matches!(&triple.predicate, NamedNodePattern::NamedNode(p) if in_hints(p.as_str()))
        || matches!(&triple.subject, TermPattern::NamedNode(s) if in_hints(s.as_str()))
}

/// The `HAVING` expression under a chain of `SELECT` expressions, which are collected
/// into `aliases` on the way down. A trailing `VALUES` sits between the two.
fn having_under<'a>(
    pattern: &'a mut GraphPattern,
    aliases: &mut Vec<(Variable, Expression)>,
) -> Option<&'a mut Expression> {
    match pattern {
        GraphPattern::Extend {
            inner,
            variable,
            expression,
        } => {
            aliases.push((variable.clone(), expression.clone()));
            having_under(inner, aliases)
        }
        GraphPattern::Join { left, right } if matches!(**right, GraphPattern::Values { .. }) => {
            having_under(left, aliases)
        }
        GraphPattern::Filter { expr, inner } if matches!(**inner, GraphPattern::Group { .. }) => {
            Some(expr)
        }
        _ => None,
    }
}

/// Rewrites every `HAVING` in `pattern` (subqueries included) that names `SELECT` aliases,
/// and drops query hints from its basic graph patterns.
fn having_aliases(pattern: &mut GraphPattern) {
    if matches!(pattern, GraphPattern::Extend { .. }) {
        let mut aliases = Vec::new();
        if let Some(having) = having_under(pattern, &mut aliases) {
            // Innermost first: an alias may use the ones defined before it. (The parser
            // in use rejects that next to aggregates; the standard allows it.)
            let mut resolved: HashMap<Variable, Expression> = HashMap::new();
            for (variable, expression) in aliases.into_iter().rev() {
                let expression = substitute(&expression, &resolved);
                resolved.insert(variable, expression);
            }
            *having = substitute(having, &resolved);
        }
    }
    match pattern {
        GraphPattern::Bgp { patterns } => patterns.retain(|triple| !is_hint(triple)),
        GraphPattern::Path { .. } | GraphPattern::Values { .. } => {}
        GraphPattern::Join { left, right }
        | GraphPattern::Union { left, right }
        | GraphPattern::Minus { left, right } => {
            having_aliases(left);
            having_aliases(right);
        }
        GraphPattern::LeftJoin {
            left,
            right,
            expression,
        } => {
            having_aliases(left);
            having_aliases(right);
            if let Some(expression) = expression {
                in_expression(expression);
            }
        }
        GraphPattern::Filter { expr, inner } => {
            in_expression(expr);
            having_aliases(inner);
        }
        GraphPattern::Extend {
            inner, expression, ..
        } => {
            in_expression(expression);
            having_aliases(inner);
        }
        GraphPattern::OrderBy { inner, expression } => {
            for order in expression {
                let (OrderExpression::Asc(e) | OrderExpression::Desc(e)) = order;
                in_expression(e);
            }
            having_aliases(inner);
        }
        GraphPattern::Graph { inner, .. }
        | GraphPattern::Project { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. }
        | GraphPattern::Group { inner, .. }
        | GraphPattern::Service { inner, .. } => having_aliases(inner),
        GraphPattern::Lateral { left, right } => {
            having_aliases(left);
            having_aliases(right);
        }
    }
}

/// Rewrites the patterns inside `EXISTS`.
fn in_expression(expression: &mut Expression) {
    match expression {
        Expression::Exists(pattern) => having_aliases(pattern),
        Expression::NamedNode(_)
        | Expression::Literal(_)
        | Expression::Variable(_)
        | Expression::Bound(_) => {}
        Expression::Or(a, b)
        | Expression::And(a, b)
        | Expression::Equal(a, b)
        | Expression::SameTerm(a, b)
        | Expression::Greater(a, b)
        | Expression::GreaterOrEqual(a, b)
        | Expression::Less(a, b)
        | Expression::LessOrEqual(a, b)
        | Expression::Add(a, b)
        | Expression::Subtract(a, b)
        | Expression::Multiply(a, b)
        | Expression::Divide(a, b) => {
            in_expression(a);
            in_expression(b);
        }
        Expression::In(a, rest) => {
            in_expression(a);
            rest.iter_mut().for_each(in_expression);
        }
        Expression::UnaryPlus(a) | Expression::UnaryMinus(a) | Expression::Not(a) => {
            in_expression(a);
        }
        Expression::If(a, b, c) => {
            in_expression(a);
            in_expression(b);
            in_expression(c);
        }
        Expression::Coalesce(all) | Expression::FunctionCall(_, all) => {
            all.iter_mut().for_each(in_expression);
        }
    }
}

/// `expression` with each variable of `definitions` replaced by its definition.
fn substitute(expression: &Expression, definitions: &HashMap<Variable, Expression>) -> Expression {
    let sub = |e: &Expression| Box::new(substitute(e, definitions));
    let all = |es: &[Expression]| es.iter().map(|e| substitute(e, definitions)).collect();
    match expression {
        Expression::Variable(variable) => definitions
            .get(variable)
            .cloned()
            .unwrap_or_else(|| expression.clone()),
        // `BOUND(?alias)` asks about the alias, and `EXISTS` has its own scope.
        Expression::NamedNode(_)
        | Expression::Literal(_)
        | Expression::Bound(_)
        | Expression::Exists(_) => expression.clone(),
        Expression::Or(a, b) => Expression::Or(sub(a), sub(b)),
        Expression::And(a, b) => Expression::And(sub(a), sub(b)),
        Expression::Equal(a, b) => Expression::Equal(sub(a), sub(b)),
        Expression::SameTerm(a, b) => Expression::SameTerm(sub(a), sub(b)),
        Expression::Greater(a, b) => Expression::Greater(sub(a), sub(b)),
        Expression::GreaterOrEqual(a, b) => Expression::GreaterOrEqual(sub(a), sub(b)),
        Expression::Less(a, b) => Expression::Less(sub(a), sub(b)),
        Expression::LessOrEqual(a, b) => Expression::LessOrEqual(sub(a), sub(b)),
        Expression::In(a, rest) => Expression::In(sub(a), all(rest)),
        Expression::Add(a, b) => Expression::Add(sub(a), sub(b)),
        Expression::Subtract(a, b) => Expression::Subtract(sub(a), sub(b)),
        Expression::Multiply(a, b) => Expression::Multiply(sub(a), sub(b)),
        Expression::Divide(a, b) => Expression::Divide(sub(a), sub(b)),
        Expression::UnaryPlus(a) => Expression::UnaryPlus(sub(a)),
        Expression::UnaryMinus(a) => Expression::UnaryMinus(sub(a)),
        Expression::Not(a) => Expression::Not(sub(a)),
        Expression::If(a, b, c) => Expression::If(sub(a), sub(b), sub(c)),
        Expression::Coalesce(es) => Expression::Coalesce(all(es)),
        Expression::FunctionCall(function, es) => {
            Expression::FunctionCall(function.clone(), all(es))
        }
    }
}

#[cfg(test)]
mod tests {
    use nrese_sparql_syntax::SparqlParser;

    use super::parse_query;

    /// The algebra of a query with the names the parser generates for aggregates (random
    /// per parse) numbered in order of appearance.
    fn shape(query: &nrese_sparql_syntax::Query) -> String {
        let printed = query.to_string();
        let mut names: Vec<String> = Vec::new();
        let mut out = String::new();
        let mut rest = printed.as_str();
        while let Some(at) = rest.find('?') {
            let end = rest[at + 1..]
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .map_or(rest.len(), |n| at + 1 + n);
            let name = &rest[at + 1..end];
            out.push_str(&rest[..=at]);
            // Generated names are a random u128 in hexadecimal.
            if name.len() >= 20 && name.bytes().all(|b| b.is_ascii_hexdigit()) {
                let number = names.iter().position(|n| n == name).unwrap_or_else(|| {
                    names.push(name.to_owned());
                    names.len() - 1
                });
                out.push_str(&format!("agg{number}"));
            } else {
                out.push_str(name);
            }
            rest = &rest[end..];
        }
        out.push_str(rest);
        out
    }

    fn rewritten(text: &str) -> String {
        shape(&parse_query(text, None).unwrap())
    }

    fn standard(text: &str) -> String {
        shape(&SparqlParser::new().parse_query(text).unwrap())
    }

    const WHERE: &str = "WHERE { ?x <http://example.com/in> ?g ; <http://example.com/v> ?v }";

    /// Blazegraph's query hints are dropped, wherever they are; the rest stays.
    #[test]
    fn query_hints_are_dropped() {
        let with_hints = "PREFIX hint: <http://www.bigdata.com/queryHints#>
            SELECT ?x WHERE {
              hint:Query hint:optimizer \"None\" .
              ?x <http://example.com/p> ?y .
              hint:Prior hint:runFirst true .
              OPTIONAL { ?y <http://example.com/q> ?z . hint:Group hint:optimizer \"None\" }
              FILTER EXISTS { ?x <http://example.com/r> ?w . hint:SubQuery hint:runOnce true }
            }";
        let without = "SELECT ?x WHERE {
              ?x <http://example.com/p> ?y .
              OPTIONAL { ?y <http://example.com/q> ?z }
              FILTER EXISTS { ?x <http://example.com/r> ?w }
            }";
        assert_eq!(rewritten(with_hints), standard(without));
    }

    /// An alias in `HAVING` means its expression. `BOUND(?g)` and the group variable keep
    /// their meaning.
    #[test]
    fn having_on_an_alias_means_its_expression() {
        let select = "SELECT ?g (COUNT(?x) AS ?n) (SUM(?v) / COUNT(?x) AS ?mean)";
        let with_alias = format!(
            "{select} {WHERE} GROUP BY ?g HAVING (?n > 1 && ?mean >= 2 && BOUND(?g) && ?g != 3)"
        );
        let written_out = format!(
            "{select} {WHERE} GROUP BY ?g
             HAVING (COUNT(?x) > 1 && SUM(?v) / COUNT(?x) >= 2 && BOUND(?g) && ?g != 3)"
        );
        assert_eq!(rewritten(&with_alias), standard(&written_out));
        assert_ne!(standard(&with_alias), standard(&written_out));
    }

    /// In subqueries, inside `EXISTS`, under `ORDER BY`, and with a trailing `VALUES`.
    #[test]
    fn having_is_found_wherever_a_select_can_stand() {
        let inner = |having: &str| {
            format!("SELECT ?g (COUNT(?x) AS ?n) {WHERE} GROUP BY ?g HAVING ({having} > 1)")
        };
        let around: [&dyn Fn(&str) -> String; 5] = [
            &|q| format!("SELECT ?g WHERE {{ {{ {q} }} ?g ?p ?o }}"),
            &|q| format!("ASK {{ ?g ?p ?o FILTER NOT EXISTS {{ {q} }} }}"),
            &|q| format!("{q} ORDER BY DESC(?n) LIMIT 3"),
            &|q| format!("{q} VALUES ?g {{ <http://example.com/a> }}"),
            &|q| format!("CONSTRUCT {{ ?g ?p ?n }} WHERE {{ ?g ?p ?o OPTIONAL {{ {q} }} }}"),
        ];
        for wrap in around {
            let with_alias = wrap(&inner("?n"));
            let written_out = wrap(&inner("COUNT(?x)"));
            assert_eq!(
                rewritten(&with_alias),
                standard(&written_out),
                "{with_alias}"
            );
            assert_ne!(
                standard(&with_alias),
                standard(&written_out),
                "{with_alias}"
            );
        }
    }

    /// Standard queries come out as the parser built them.
    #[test]
    fn queries_without_the_extension_are_unchanged() {
        for text in [
            "SELECT ?s WHERE { ?s ?p ?o }".to_owned(),
            format!("SELECT ?g (COUNT(?x) AS ?n) {WHERE} GROUP BY ?g HAVING (COUNT(?x) > 1)"),
            format!("SELECT ?g (SUM(?v) AS ?n) {WHERE} GROUP BY ?g HAVING (?g > 1)"),
            // `?n` of the outer query isn't an alias of the subquery that has the HAVING.
            format!("SELECT (?g AS ?n) WHERE {{ SELECT ?g {WHERE} GROUP BY ?g HAVING (?n > 1) }}"),
            // A filter that isn't a HAVING: the alias is defined above it, not visible in it.
            "SELECT (?o + 1 AS ?n) WHERE { ?s ?p ?o FILTER(?n > 1) }".to_owned(),
        ] {
            assert_eq!(rewritten(&text), standard(&text), "{text}");
        }
    }
}
