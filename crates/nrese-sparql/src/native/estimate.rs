//! EXPLAIN before running (the query plan's step 3, docs/design/query-plan.md): the
//! plan after the rewrites, each node with an estimate of its rows, read from the store's
//! statistics without evaluating anything.
//!
//! | Node | Estimate |
//! |---|---|
//! | Triple pattern | Its exact count (an index range) |
//! | Join of triple patterns (a basic graph pattern) | The join orderer's ([`super::plan`]): counts, distinct values, independence |
//! | Other joins, `LATERAL` | Inputs sharing a variable: the larger (each row meets about one partner); none shared: the product |
//! | `OPTIONAL` | The join, at least the left side |
//! | Filter | Its input times the conjuncts' selectivities ([`super::pushdown::selectivity`]) |
//! | Union | The sum; `MINUS` the left side |
//! | Path | The statements of its predicates (the edges it may follow) |
//! | `GROUP BY` | One row without keys; at most its input with them |
//! | Slice | Its input past the offset, at most the limit |
//! | `VALUES` | Its rows |
//! | `SERVICE` | Unknown (`None`), and so is what contains it |
//!
//! Projection, `DISTINCT`, `REDUCED`, `ORDER BY` and extensions keep their input's estimate
//! (an upper bound for `DISTINCT`). The estimates order work; they aren't promises.

use nrese_rdf::Variable;
use nrese_sparql_syntax::algebra::PropertyPathExpression;
use nrese_sparql_syntax::term::NamedNodePattern;

use super::{Context, GraphScope, bound_variables, pushdown};
use crate::plan::Plan;
use crate::query::PlannedStep;

impl Context<'_> {
    /// Whether grouping `side` by `keys` before it is joined reduces it enough to pay for
    /// the extra group ([`crate::plan::Plan::eager_aggregation_where`]): its estimated rows
    /// at least twice the distinct key values (each key's fewest distinct values over
    /// the side's triple patterns, the keys taken as independent). Unknown estimates leave
    /// the plan as written.
    pub(super) fn pre_aggregation_pays(&self, side: &Plan, keys: &[Variable]) -> bool {
        let Some(rows) = self.estimate_plan(side, 0, &mut Vec::new()) else {
            return false;
        };
        let Plan::Join(inputs) = side else {
            return false;
        };
        let mut groups = 1.0f64;
        for key in keys {
            let distinct = inputs
                .iter()
                .filter_map(|input| match input {
                    Plan::Scan(triple) => Some(triple),
                    _ => None,
                })
                .filter_map(|triple| {
                    let scan = self.scan_pattern(triple)?;
                    scan.vars().contains(key).then(|| {
                        let count = self.snapshot.estimate_in(self.model, &scan.quad_pattern());
                        self.distinct(&scan, key, count) as f64
                    })
                })
                .fold(f64::INFINITY, f64::min);
            if !distinct.is_finite() {
                return false;
            }
            groups *= distinct.max(1.0);
        }
        rows >= 2.0 * groups.min(rows)
    }

    /// Appends `plan`'s nodes to `steps` (depth first, a node before its inputs) with their
    /// estimated rows; returns the estimate of `plan` (`None`: unknown).
    pub(super) fn estimate_plan(
        &self,
        plan: &Plan,
        depth: usize,
        steps: &mut Vec<PlannedStep>,
    ) -> Option<f64> {
        let at = steps.len();
        steps.push(PlannedStep {
            depth,
            operator: String::new(),
            detail: String::new(),
            estimated_rows: None,
        });
        let input =
            |plan: &Plan, steps: &mut Vec<PlannedStep>| self.estimate_plan(plan, depth + 1, steps);
        let (operator, detail, rows): (&str, String, Option<f64>) = match plan {
            Plan::Scan(triple) => ("scan", triple.to_string(), Some(self.scan_rows(triple))),
            Plan::Path {
                subject,
                path,
                object,
            } => (
                "path",
                format!("{subject} {path} {object}"),
                Some(self.path_rows(path)),
            ),
            Plan::Join(inputs) if inputs.iter().all(|i| matches!(i, Plan::Scan(_))) => {
                let rows = self.bgp_rows(inputs, depth + 1, steps);
                ("bgp", format!("{} triple patterns", inputs.len()), rows)
            }
            Plan::Join(inputs) => {
                let mut rows = Some(1.0);
                let mut bound: Vec<Variable> = Vec::new();
                for plan in inputs {
                    let next = input(plan, steps);
                    let variables = variables(plan);
                    let shared = variables.iter().any(|v| bound.contains(v));
                    rows = join(rows, next, shared);
                    bound.extend(variables);
                }
                ("join", String::new(), rows)
            }
            Plan::LeftJoin {
                left,
                right,
                condition,
            } => {
                let l = input(left, steps);
                let r = input(right, steps);
                let shared = shares(left, right);
                let rows = join(l, r, shared).zip(l).map(|(j, l)| j.max(l));
                let detail = condition
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default();
                ("optional", detail, rows)
            }
            Plan::Lateral { left, right } => {
                let l = input(left, steps);
                let r = input(right, steps);
                ("lateral", String::new(), join(l, r, shares(left, right)))
            }
            Plan::Filter {
                condition,
                input: inner,
            } => {
                let rows = input(inner, steps).map(|r| r * pushdown::selectivity(condition));
                ("filter", condition.to_string(), rows)
            }
            Plan::Union(inputs) => {
                let rows = inputs
                    .iter()
                    .map(|plan| input(plan, steps))
                    .try_fold(0.0, |sum, rows| rows.map(|r| sum + r));
                ("union", String::new(), rows)
            }
            Plan::Minus { left, right } => {
                let rows = input(left, steps);
                input(right, steps);
                ("minus", String::new(), rows)
            }
            Plan::Graph { name, input: inner } => {
                let scope = match name {
                    NamedNodePattern::NamedNode(n) => self
                        .lookup_const(n.as_ref().into())
                        .filter(|&id| self.is_named_graph(id))
                        .map_or(GraphScope::Missing, GraphScope::Named),
                    NamedNodePattern::Variable(v) => GraphScope::Variable(v.clone()),
                };
                let outer = self.graph.replace(scope);
                let rows = input(inner, steps);
                *self.graph.borrow_mut() = outer;
                ("graph", name.to_string(), rows)
            }
            Plan::Extend {
                input: inner,
                variable,
                expression,
            } => (
                "extend",
                format!("{variable} := {expression}"),
                input(inner, steps),
            ),
            Plan::Values { variables, rows } => {
                ("values", names(variables), Some(rows.len() as f64))
            }
            Plan::OrderBy { input: inner, keys } => (
                "order",
                keys.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" "),
                input(inner, steps),
            ),
            Plan::Project {
                input: inner,
                variables,
            } => ("project", names(variables), input(inner, steps)),
            Plan::Distinct(inner) => ("distinct", String::new(), input(inner, steps)),
            Plan::Reduced(inner) => ("reduced", String::new(), input(inner, steps)),
            Plan::Slice {
                input: inner,
                start,
                length,
            } => {
                let rows = input(inner, steps).map(|r| {
                    let past = (r - *start as f64).max(0.0);
                    length.map_or(past, |l| past.min(l as f64))
                });
                let detail = match length {
                    Some(length) => format!("offset {start} limit {length}"),
                    None => format!("offset {start}"),
                };
                ("slice", detail, rows)
            }
            Plan::Group {
                input: inner, keys, ..
            } => {
                let rows = input(inner, steps);
                let rows = if keys.is_empty() { Some(1.0) } else { rows };
                ("group", names(keys), rows)
            }
            Plan::Service {
                name, input: inner, ..
            } => {
                input(inner, steps);
                ("service", name.to_string(), None)
            }
        };
        steps[at].operator = operator.to_owned();
        steps[at].detail = detail;
        steps[at].estimated_rows = rows.map(|r| r.max(0.0).round() as u64);
        rows
    }

    /// The matches of one triple pattern in the active graph (0 if a constant isn't in the
    /// store).
    fn scan_rows(&self, triple: &nrese_sparql_syntax::term::TriplePattern) -> f64 {
        self.scan_pattern(triple).map_or(0.0, |scan| {
            self.snapshot.estimate_in(self.model, &scan.quad_pattern()) as f64
        })
    }

    /// A basic graph pattern: its triple patterns as steps in the order the join orderer
    /// chooses, each with its count; the estimate after the last.
    fn bgp_rows(&self, inputs: &[Plan], depth: usize, steps: &mut Vec<PlannedStep>) -> Option<f64> {
        let triples: Vec<_> = inputs
            .iter()
            .filter_map(|plan| match plan {
                Plan::Scan(triple) => Some(triple),
                _ => None,
            })
            .collect();
        let scans: Option<Vec<_>> = triples.iter().map(|t| self.scan_pattern(t)).collect();
        let Some(scans) = scans else {
            // A constant the store doesn't have: no matches.
            for triple in &triples {
                steps.push(PlannedStep {
                    depth,
                    operator: "scan".to_owned(),
                    detail: triple.to_string(),
                    estimated_rows: Some(0),
                });
            }
            return Some(0.0);
        };
        let counts: Vec<u64> = scans
            .iter()
            .map(|scan| self.snapshot.estimate_in(self.model, &scan.quad_pattern()))
            .collect();
        let plan = self.join_order(&scans, &counts);
        for &i in &plan.order {
            steps.push(PlannedStep {
                depth,
                operator: "scan".to_owned(),
                detail: triples[i].to_string(),
                estimated_rows: Some(counts[i]),
            });
        }
        // Two patterns: the orderer only sorts them; the larger bounds the join.
        let rows = plan.rows.last().copied().filter(|r| r.is_finite());
        Some(rows.unwrap_or_else(|| counts.iter().copied().max().unwrap_or(0) as f64))
    }

    /// The statements a path may follow: those of its predicates.
    fn path_rows(&self, path: &PropertyPathExpression) -> f64 {
        let mut predicates = Vec::new();
        collect_predicates(path, &mut predicates);
        predicates
            .iter()
            .map(|predicate| {
                let triple = nrese_sparql_syntax::term::TriplePattern {
                    subject: Variable::new_unchecked("_s").into(),
                    predicate: predicate.clone().into(),
                    object: Variable::new_unchecked("_o").into(),
                };
                self.scan_rows(&triple)
            })
            .sum()
    }
}

fn collect_predicates(path: &PropertyPathExpression, out: &mut Vec<nrese_rdf::NamedNode>) {
    match path {
        PropertyPathExpression::NamedNode(n) => out.push(n.clone()),
        PropertyPathExpression::NegatedPropertySet(set) => out.extend(set.iter().cloned()),
        PropertyPathExpression::Reverse(p)
        | PropertyPathExpression::ZeroOrMore(p)
        | PropertyPathExpression::OneOrMore(p)
        | PropertyPathExpression::ZeroOrOne(p) => collect_predicates(p, out),
        PropertyPathExpression::Sequence(a, b) | PropertyPathExpression::Alternative(a, b) => {
            collect_predicates(a, out);
            collect_predicates(b, out);
        }
    }
}

/// Two inputs joined: sharing a variable, the larger; else the product.
fn join(left: Option<f64>, right: Option<f64>, shared: bool) -> Option<f64> {
    let (l, r) = (left?, right?);
    Some(if shared { l.max(r) } else { l * r })
}

fn variables(plan: &Plan) -> Vec<Variable> {
    let mut out = Vec::new();
    bound_variables(&plan.lower(), &mut out);
    out
}

fn shares(left: &Plan, right: &Plan) -> bool {
    let left = variables(left);
    variables(right).iter().any(|v| left.contains(v))
}

fn names(variables: &[Variable]) -> String {
    variables
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}
