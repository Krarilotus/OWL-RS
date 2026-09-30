//! Parsing with the rewrites that make queries written for other stores mean here what
//! they mean there. Each rewrite turns a query into standard SPARQL algebra, so both
//! executors evaluate it without knowing about the extension.
//!
//! **`HAVING` on a `SELECT` alias.** `SELECT (SUM(?x) AS ?total) … HAVING (?total > 0)`:
//! in the standard, `HAVING` is evaluated before the `SELECT` expressions, so `?total` is
//! unbound there and no group passes. Jena, RDF4J, Virtuoso and QLever read the alias as
//! its expression, and queries in the wild rely on it. [`parse_query`] replaces such a
//! variable in `HAVING` by the expression it names.

use std::collections::HashMap;

use oxrdf::Variable;
use spargebra::algebra::{Expression, GraphPattern, OrderExpression};
use spargebra::{Query, SparqlParser, SparqlSyntaxError};

/// Parses a query and applies the compatibility rewrites.
pub fn parse_query(text: &str) -> Result<Query, SparqlSyntaxError> {
    let mut query = SparqlParser::new().parse_query(text)?;
    let (Query::Select { pattern, .. }
    | Query::Construct { pattern, .. }
    | Query::Describe { pattern, .. }
    | Query::Ask { pattern, .. }) = &mut query;
    having_aliases(pattern);
    Ok(query)
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

/// Rewrites every `HAVING` in `pattern` (subqueries included) that names `SELECT` aliases.
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
        GraphPattern::Bgp { .. } | GraphPattern::Path { .. } | GraphPattern::Values { .. } => {}
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
        // `LATERAL` isn't part of the language the server parses: the variant exists only
        // in builds where a test dependency switches the parser feature on.
        #[allow(unreachable_patterns)]
        _ => {}
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
    use spargebra::SparqlParser;

    use super::parse_query;

    /// The algebra of a query with the names the parser generates for aggregates (random
    /// per parse) numbered in order of appearance.
    fn shape(query: &spargebra::Query) -> String {
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
        shape(&parse_query(text).unwrap())
    }

    fn standard(text: &str) -> String {
        shape(&SparqlParser::new().parse_query(text).unwrap())
    }

    const WHERE: &str = "WHERE { ?x <http://example.com/in> ?g ; <http://example.com/v> ?v }";

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
