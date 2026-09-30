//! Filter pushdown: every `FILTER` conjunct moves to the smallest sub-pattern that binds
//! all its variables in every solution, so rows are dropped before they are joined.
//!
//! The parser leaves a group's filters on top of the group: in
//! `{ ?a :p ?b . ?b :q ?c OPTIONAL { ?c :r ?d } FILTER(?a = :x) }` the filter runs after
//! the join and the OPTIONAL. Evaluated as written, a selective filter doesn't keep the
//! intermediate results small; on data where they are large (materialised `owl:sameAs`,
//! for one), that is the difference between milliseconds and running out of memory.
//!
//! What moves where. A conjunct `c` with variables `V` (`FILTER(a && b)` is `FILTER(a)`
//! then `FILTER(b)`: an error in either drops the row):
//!
//! | Pattern | Rewritten as | If |
//! |---|---|---|
//! | `Join(L, R)` | `c` into `L`, into `R`, or both | `V` is certainly bound there |
//! | `LeftJoin(L, R)`, `Minus(L, R)` | `c` into `L` | `V` is certainly bound in `L` (always, for `Minus`) |
//! | `Union(L, R)` | `c` into both | always |
//! | `Filter`, `Distinct`, `Reduced`, `OrderBy` | below it | always |
//! | `Extend(P, ?v)`, `Graph(?v, P)` | below it | `?v` is not in `V` |
//! | `Project(P, vars)` | below it | `V` is within `vars` |
//!
//! "Certainly bound": bound in every solution ([`certain`]). A variable that may be
//! unbound (from an OPTIONAL, a `BIND` that can fail, one branch of a UNION) keeps its
//! filter above, where the solution is complete.
//!
//! The condition of an OPTIONAL (`OPTIONAL { P FILTER(c) }`) moves into `P` likewise when
//! `P` certainly binds its variables.
//!
//! What stays: conjuncts with `EXISTS` (it reads the whole solution), with `RAND`, `UUID`,
//! `STRUUID` or `BNODE` (evaluated per row: fewer rows, other draws), and without
//! variables.

use oxrdf::Variable;
use spargebra::algebra::{Expression, Function, GraphPattern};
use spargebra::term::{NamedNodePattern, TermPattern};

use super::expression_variables;

/// `pattern` with its filters pushed down, in every sub-pattern.
pub(super) fn push_filters(pattern: GraphPattern) -> GraphPattern {
    let boxed = |pattern: Box<GraphPattern>| Box::new(push_filters(*pattern));
    match pattern {
        GraphPattern::Filter { expr, inner } => {
            let mut pattern = push_filters(*inner);
            let mut kept = Vec::new();
            for conjunct in conjuncts(expr) {
                if !movable(&conjunct) {
                    kept.push(conjunct);
                    continue;
                }
                let variables = expression_variables(&conjunct);
                match sink(conjunct, &variables, pattern) {
                    Ok(pushed) => pattern = pushed,
                    Err(back) => {
                        let (conjunct, unchanged) = *back;
                        kept.push(conjunct);
                        pattern = unchanged;
                    }
                }
            }
            match conjunction(kept) {
                Some(expr) => GraphPattern::Filter {
                    expr,
                    inner: Box::new(pattern),
                },
                None => pattern,
            }
        }
        GraphPattern::LeftJoin {
            left,
            right,
            expression,
        } => {
            let left = boxed(left);
            let mut right = push_filters(*right);
            let mut kept = Vec::new();
            for conjunct in expression.map(conjuncts).unwrap_or_default() {
                let variables = expression_variables(&conjunct);
                if movable(&conjunct) && covers(&right, &variables) {
                    right = place(conjunct, &variables, right);
                } else {
                    kept.push(conjunct);
                }
            }
            GraphPattern::LeftJoin {
                left,
                right: Box::new(right),
                expression: conjunction(kept),
            }
        }
        GraphPattern::Join { left, right } => GraphPattern::Join {
            left: boxed(left),
            right: boxed(right),
        },
        GraphPattern::Union { left, right } => GraphPattern::Union {
            left: boxed(left),
            right: boxed(right),
        },
        GraphPattern::Minus { left, right } => GraphPattern::Minus {
            left: boxed(left),
            right: boxed(right),
        },
        GraphPattern::Graph { name, inner } => GraphPattern::Graph {
            name,
            inner: boxed(inner),
        },
        GraphPattern::Extend {
            inner,
            variable,
            expression,
        } => GraphPattern::Extend {
            inner: boxed(inner),
            variable,
            expression,
        },
        GraphPattern::OrderBy { inner, expression } => GraphPattern::OrderBy {
            inner: boxed(inner),
            expression,
        },
        GraphPattern::Project { inner, variables } => GraphPattern::Project {
            inner: boxed(inner),
            variables,
        },
        GraphPattern::Distinct { inner } => GraphPattern::Distinct {
            inner: boxed(inner),
        },
        GraphPattern::Reduced { inner } => GraphPattern::Reduced {
            inner: boxed(inner),
        },
        GraphPattern::Slice {
            inner,
            start,
            length,
        } => GraphPattern::Slice {
            inner: boxed(inner),
            start,
            length,
        },
        GraphPattern::Group {
            inner,
            variables,
            aggregates,
        } => GraphPattern::Group {
            inner: boxed(inner),
            variables,
            aggregates,
        },
        other => other,
    }
}

/// `conjunct` placed in `pattern`: as deep as it may go, else on top.
fn place(conjunct: Expression, variables: &[Variable], pattern: GraphPattern) -> GraphPattern {
    sink(conjunct, variables, pattern).unwrap_or_else(|back| {
        let (expr, inner) = *back;
        GraphPattern::Filter {
            expr,
            inner: Box::new(inner),
        }
    })
}

/// A conjunct and the pattern it must stay above.
type Unmoved = Box<(Expression, GraphPattern)>;

/// Moves `conjunct` below the top operator of `pattern`; `Err` gives both back if it must
/// stay above it.
fn sink(
    conjunct: Expression,
    variables: &[Variable],
    pattern: GraphPattern,
) -> Result<GraphPattern, Unmoved> {
    let placed = |inner: Box<GraphPattern>, conjunct| Box::new(place(conjunct, variables, *inner));
    match pattern {
        GraphPattern::Join { left, right } => {
            let (in_left, in_right) = (covers(&left, variables), covers(&right, variables));
            match (in_left, in_right) {
                (true, true) => Ok(GraphPattern::Join {
                    left: placed(left, conjunct.clone()),
                    right: placed(right, conjunct),
                }),
                (true, false) => Ok(GraphPattern::Join {
                    left: placed(left, conjunct),
                    right,
                }),
                (false, true) => Ok(GraphPattern::Join {
                    left,
                    right: placed(right, conjunct),
                }),
                (false, false) => Err(Box::new((conjunct, GraphPattern::Join { left, right }))),
            }
        }
        GraphPattern::LeftJoin {
            left,
            right,
            expression,
        } if covers(&left, variables) => Ok(GraphPattern::LeftJoin {
            left: placed(left, conjunct),
            right,
            expression,
        }),
        GraphPattern::Minus { left, right } => Ok(GraphPattern::Minus {
            left: placed(left, conjunct),
            right,
        }),
        GraphPattern::Union { left, right } => Ok(GraphPattern::Union {
            left: placed(left, conjunct.clone()),
            right: placed(right, conjunct),
        }),
        // Conjuncts that end at the same place make one filter: the executor reads the
        // value ranges of a filter directly above a basic graph pattern.
        GraphPattern::Filter { expr, inner } => Ok(match sink(conjunct, variables, *inner) {
            Ok(inner) => GraphPattern::Filter {
                expr,
                inner: Box::new(inner),
            },
            Err(back) => {
                let (conjunct, inner) = *back;
                GraphPattern::Filter {
                    expr: Expression::And(Box::new(expr), Box::new(conjunct)),
                    inner: Box::new(inner),
                }
            }
        }),
        GraphPattern::Extend {
            inner,
            variable,
            expression,
        } if !variables.contains(&variable) => Ok(GraphPattern::Extend {
            inner: placed(inner, conjunct),
            variable,
            expression,
        }),
        GraphPattern::Graph { name, inner } if !matches!(&name, NamedNodePattern::Variable(v) if variables.contains(v)) => {
            Ok(GraphPattern::Graph {
                name,
                inner: placed(inner, conjunct),
            })
        }
        GraphPattern::Project {
            inner,
            variables: projected,
        } if variables.iter().all(|v| projected.contains(v)) => Ok(GraphPattern::Project {
            inner: placed(inner, conjunct),
            variables: projected,
        }),
        GraphPattern::Distinct { inner } => Ok(GraphPattern::Distinct {
            inner: placed(inner, conjunct),
        }),
        GraphPattern::Reduced { inner } => Ok(GraphPattern::Reduced {
            inner: placed(inner, conjunct),
        }),
        GraphPattern::OrderBy { inner, expression } => Ok(GraphPattern::OrderBy {
            inner: placed(inner, conjunct),
            expression,
        }),
        other => Err(Box::new((conjunct, other))),
    }
}

/// The conjuncts of `a && b && …`.
fn conjuncts(expression: Expression) -> Vec<Expression> {
    match expression {
        Expression::And(a, b) => {
            let mut all = conjuncts(*a);
            all.extend(conjuncts(*b));
            all
        }
        other => vec![other],
    }
}

fn conjunction(conjuncts: Vec<Expression>) -> Option<Expression> {
    conjuncts
        .into_iter()
        .reduce(|a, b| Expression::And(Box::new(a), Box::new(b)))
}

/// The conjuncts of `a && b && …`, by reference.
pub(super) fn conjuncts_of<'e>(expression: &'e Expression, out: &mut Vec<&'e Expression>) {
    match expression {
        Expression::And(a, b) => {
            conjuncts_of(a, out);
            conjuncts_of(b, out);
        }
        other => out.push(other),
    }
}

/// A rough share of the rows a filter conjunct keeps, for ordering joins only:
/// comparisons with a constant for equality keep few, ranges some, string tests a
/// quarter, negations most.
pub(super) fn selectivity(conjunct: &Expression) -> f64 {
    let constant = |e: &Expression| matches!(e, Expression::NamedNode(_) | Expression::Literal(_));
    match conjunct {
        Expression::Equal(a, b) | Expression::SameTerm(a, b) if constant(a) || constant(b) => 0.01,
        Expression::In(_, list) => (0.01 * list.len() as f64).min(1.0),
        Expression::Greater(..)
        | Expression::GreaterOrEqual(..)
        | Expression::Less(..)
        | Expression::LessOrEqual(..) => 0.3,
        Expression::Equal(..) | Expression::SameTerm(..) => 0.1,
        Expression::FunctionCall(..) => 0.25,
        Expression::And(a, b) => selectivity(a) * selectivity(b),
        Expression::Or(a, b) => (selectivity(a) + selectivity(b)).min(1.0),
        Expression::Not(_) => 0.9,
        _ => 1.0,
    }
}

/// Whether a conjunct means the same wherever its variables are bound: it has variables,
/// and neither reads the rest of the solution (`EXISTS`) nor draws a value per row.
pub(super) fn movable(conjunct: &Expression) -> bool {
    !expression_variables(conjunct).is_empty() && !per_solution(conjunct)
}

/// Whether the expression reads the whole solution (`EXISTS`) or draws a value per row.
pub(super) fn per_solution(expression: &Expression) -> bool {
    match expression {
        Expression::Exists(_) => true,
        Expression::NamedNode(_)
        | Expression::Literal(_)
        | Expression::Variable(_)
        | Expression::Bound(_) => false,
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
        | Expression::Divide(a, b) => per_solution(a) || per_solution(b),
        Expression::In(a, list) => per_solution(a) || list.iter().any(per_solution),
        Expression::UnaryPlus(a) | Expression::UnaryMinus(a) | Expression::Not(a) => {
            per_solution(a)
        }
        Expression::If(a, b, c) => per_solution(a) || per_solution(b) || per_solution(c),
        Expression::Coalesce(list) => list.iter().any(per_solution),
        Expression::FunctionCall(function, arguments) => {
            matches!(
                function,
                Function::Rand | Function::Uuid | Function::StrUuid | Function::BNode
            ) || arguments.iter().any(per_solution)
        }
    }
}

/// Whether `pattern` binds all of `variables` in every solution.
fn covers(pattern: &GraphPattern, variables: &[Variable]) -> bool {
    let bound = certain(pattern);
    variables.iter().all(|v| bound.contains(v))
}

/// The variables bound in every solution of `pattern`.
pub(super) fn certain(pattern: &GraphPattern) -> Vec<Variable> {
    let mut out = Vec::new();
    let mut add = |variable: &Variable| {
        if !out.contains(variable) {
            out.push(variable.clone());
        }
    };
    let term = |term: &TermPattern| match term {
        TermPattern::Variable(v) => Some(v.clone()),
        _ => None,
    };
    match pattern {
        GraphPattern::Bgp { patterns } => {
            for triple in patterns {
                term(&triple.subject).iter().for_each(&mut add);
                if let NamedNodePattern::Variable(v) = &triple.predicate {
                    add(v);
                }
                term(&triple.object).iter().for_each(&mut add);
            }
        }
        GraphPattern::Path {
            subject, object, ..
        } => {
            term(subject).iter().for_each(&mut add);
            term(object).iter().for_each(&mut add);
        }
        GraphPattern::Join { left, right } => {
            certain(left).iter().for_each(&mut add);
            certain(right).iter().for_each(&mut add);
        }
        GraphPattern::Union { left, right } => {
            let right = certain(right);
            certain(left)
                .iter()
                .filter(|v| right.contains(v))
                .for_each(&mut add);
        }
        GraphPattern::Graph { name, inner } => {
            if let NamedNodePattern::Variable(v) = name {
                add(v);
            }
            certain(inner).iter().for_each(&mut add);
        }
        GraphPattern::Values {
            variables,
            bindings,
        } => {
            for (column, variable) in variables.iter().enumerate() {
                if bindings.iter().all(|row| row[column].is_some()) {
                    add(variable);
                }
            }
        }
        GraphPattern::Project { inner, variables } => {
            certain(inner)
                .iter()
                .filter(|v| variables.contains(v))
                .for_each(&mut add);
        }
        GraphPattern::Group {
            inner, variables, ..
        } => {
            certain(inner)
                .iter()
                .filter(|v| variables.contains(v))
                .for_each(&mut add);
        }
        // The left side's rows, some extended or removed. `BIND` may fail and leave its
        // variable unbound.
        GraphPattern::LeftJoin { left: inner, .. }
        | GraphPattern::Minus { left: inner, .. }
        | GraphPattern::Filter { inner, .. }
        | GraphPattern::Extend { inner, .. }
        | GraphPattern::OrderBy { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. } => return certain(inner),
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use spargebra::{Query, SparqlParser};

    use super::push_filters;

    /// The WHERE clause of `SELECT * WHERE { … }` after pushdown, as SPARQL.
    fn pushed(group: &str) -> String {
        rewritten(&format!("SELECT * WHERE {{ {group} }}"))
    }

    fn rewritten(query: &str) -> String {
        let prefixed = format!("PREFIX : <http://example.com/> {query}");
        let Query::Select { pattern, .. } = SparqlParser::new().parse_query(&prefixed).unwrap()
        else {
            panic!("not a SELECT");
        };
        let text = push_filters(pattern).to_string();
        text.replace("http://example.com/", "")
    }

    /// The same, for a group whose filters are already where they belong.
    fn written(group: &str) -> String {
        let prefixed = format!("PREFIX : <http://example.com/> SELECT * WHERE {{ {group} }}");
        let Query::Select { pattern, .. } = SparqlParser::new().parse_query(&prefixed).unwrap()
        else {
            panic!("not a SELECT");
        };
        pattern.to_string().replace("http://example.com/", "")
    }

    #[test]
    fn a_filter_moves_below_joins_and_optionals_to_what_binds_its_variables() {
        // Below the OPTIONAL and the join with the path, onto the pattern that binds ?a.
        let as_parsed = "?a :p ?b . ?b :q+ ?c OPTIONAL { ?c :r ?d } FILTER(?a = :x)";
        assert_ne!(pushed(as_parsed), written(as_parsed));
        assert_eq!(
            pushed("?a :p ?b . ?b :q+ ?c OPTIONAL { ?c :r ?d } FILTER(?a = :x)"),
            written("{ ?a :p ?b FILTER(?a = :x) } ?b :q+ ?c OPTIONAL { ?c :r ?d }")
        );
        // A variable both sides bind: into both.
        assert_eq!(
            pushed("?a :p ?b . ?b :q+ ?c FILTER(?b != :x)"),
            written("{ ?a :p ?b FILTER(?b != :x) } { ?b :q+ ?c FILTER(?b != :x) }")
        );
        // Conjuncts go their own ways; those that end together make one filter.
        assert_eq!(
            pushed("?a :p ?b . ?b :q+ ?c FILTER(?a = :x && ?c = :y && ISIRI(?a) && ?a != ?c)"),
            written(
                "{ ?a :p ?b FILTER(?a = :x && ISIRI(?a)) } { ?b :q+ ?c FILTER(?c = :y) }
                 FILTER(?a != ?c)"
            )
        );
    }

    #[test]
    fn a_filter_on_a_possibly_unbound_variable_stays_above() {
        for group in [
            // ?d comes from the OPTIONAL.
            "?a :p ?b OPTIONAL { ?b :r ?d } FILTER(!BOUND(?d))",
            "?a :p ?b OPTIONAL { ?b :r ?d } FILTER(?d = :x)",
            // ?e comes from a BIND, which may fail.
            "?a :p ?b BIND(?b + 1 AS ?e) FILTER(?e > 1)",
            // EXISTS reads the whole solution.
            "?a :p ?b . ?b :q+ ?c FILTER EXISTS { ?a :r ?c }",
            "?a :p ?b . ?b :q+ ?c FILTER(?a = :x || EXISTS { ?a :r ?c })",
            // A draw per row, and a constant.
            "?a :p ?b . ?b :q+ ?c FILTER(RAND() < 0.5 || ?a = :x)",
            "?a :p ?b . ?b :q+ ?c FILTER(1 = 1)",
            // A filter on aggregates (HAVING) stays on its group.
            "{ SELECT ?a (COUNT(?b) AS ?n) WHERE { ?a :p ?b } GROUP BY ?a HAVING(COUNT(?b) > 1) }",
            // LIMIT: the filter sees only the rows the subquery returns.
            "{ SELECT ?a WHERE { ?a :p ?b } LIMIT 3 } FILTER(?a = :x)",
        ] {
            let query = format!("PREFIX : <http://example.com/> SELECT * WHERE {{ {group} }}");
            let Query::Select { pattern, .. } = SparqlParser::new().parse_query(&query).unwrap()
            else {
                panic!("not a SELECT");
            };
            assert_eq!(push_filters(pattern.clone()), pattern, "{group}");
        }
        // ?c is bound in one branch of the UNION only: not below the join...
        assert_eq!(
            pushed("{ ?a :p ?c } UNION { ?a :q ?b } ?a :r+ ?z FILTER(?c = :x)"),
            written("{ ?a :p ?c } UNION { ?a :q ?b } ?a :r+ ?z FILTER(?c = :x)")
        );
        // ... but a filter directly on a UNION holds for each branch.
        assert_eq!(
            pushed("{ ?a :p ?c } UNION { ?a :q ?b } FILTER(?c = :x)"),
            written("{ ?a :p ?c FILTER(?c = :x) } UNION { ?a :q ?b FILTER(?c = :x) }")
        );
    }

    #[test]
    fn filters_pass_binds_minus_graphs_and_projections_that_keep_their_variables() {
        assert_eq!(
            pushed("?a :p ?b BIND(?b + 1 AS ?e) FILTER(?a = :x)"),
            written("{ ?a :p ?b FILTER(?a = :x) } BIND(?b + 1 AS ?e)")
        );
        assert_eq!(
            pushed("?a :p ?b MINUS { ?a :q ?c } FILTER(?a = :x)"),
            written("{ ?a :p ?b FILTER(?a = :x) } MINUS { ?a :q ?c }")
        );
        assert_eq!(
            pushed("GRAPH ?g { ?a :p ?b } FILTER(?a = :x)"),
            written("GRAPH ?g { ?a :p ?b FILTER(?a = :x) }")
        );
        let graph_variable = "GRAPH ?g { ?a :p ?b } FILTER(?g = :x)";
        assert_eq!(pushed(graph_variable), written(graph_variable));
        // Into a subquery that projects the variable, not into one that hides it.
        assert_eq!(
            pushed("{ SELECT DISTINCT ?a ?b WHERE { ?a :p ?b . ?b :q+ ?c } } FILTER(?a = :x)"),
            written("{ SELECT DISTINCT ?a ?b WHERE { { ?a :p ?b FILTER(?a = :x) } ?b :q+ ?c } }")
        );
        let hidden = "{ SELECT ?a WHERE { ?a :p ?b } } ?b :q+ ?c FILTER(?b = :x)";
        assert_eq!(
            pushed(hidden),
            written("{ SELECT ?a WHERE { ?a :p ?b } } { ?b :q+ ?c FILTER(?b = :x) }")
        );
    }

    #[test]
    fn the_condition_of_an_optional_moves_into_it_when_it_binds_the_variables() {
        assert_eq!(
            pushed("?a :p ?b OPTIONAL { ?b :r ?d . ?d :q+ ?e FILTER(?d != :x && ?d != ?a) }"),
            written(
                "?a :p ?b OPTIONAL { { ?b :r ?d FILTER(?d != :x) } { ?d :q+ ?e FILTER(?d != :x) }
                 FILTER(?d != ?a) }"
            )
        );
    }

    #[test]
    fn filters_inside_nested_patterns_are_pushed_too() {
        assert_eq!(
            rewritten(
                "SELECT ?a (COUNT(?c) AS ?n) WHERE { ?a :p ?b . ?b :q+ ?c FILTER(?a = :x) }
                 GROUP BY ?a ORDER BY ?a LIMIT 3"
            )
            .matches("FILTER")
            .count(),
            1
        );
        assert_eq!(
            pushed("{ ?a :p ?b . ?b :q+ ?c FILTER(?a = :x) } UNION { ?a :r ?b }"),
            written("{ { ?a :p ?b FILTER(?a = :x) } ?b :q+ ?c } UNION { ?a :r ?b }")
        );
    }
}
