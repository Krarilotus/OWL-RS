//! EXPLAIN before running (the query plan's step 3, docs/plan/2026-10-02-plan-ir.md): the
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
//! | Path | Alone: one between constants, the fan-out from a constant end, the pairs between variables; from bound values, their number times the fan-out ([`Context::path_estimate`]) |
//! | `GROUP BY` | One row without keys; at most its input with them |
//! | Slice | Its input past the offset, at most the limit |
//! | `VALUES` | Its rows |
//! | `SERVICE` | Unknown (`None`), and so is what contains it |
//!
//! Projection, `DISTINCT`, `REDUCED`, `ORDER BY` and extensions keep their input's estimate
//! (an upper bound for `DISTINCT`). The estimates order work; they aren't promises.
//!
//! EXPLAIN after running (`explain_query`) shows the same estimate for each operator the
//! evaluation runs, beside its rows, and for the operators inside one: a basic graph
//! pattern's steps (the orderer's), a pattern evaluated from rows already computed
//! (`sideways`: the rows times each pattern's count over the distinct values it shares),
//! filters (their selectivity), a `LIMIT` that stops a pattern (`limit pushdown`), closures,
//! `EXISTS` as a semi- or anti-join (all rows, nine in ten), group walks (the key's
//! distinct values). Full-text and vector searches have no statistics: no estimate.

use nrese_rdf::Variable;
use nrese_sparql_syntax::algebra::{GraphPattern, PropertyPathExpression};
use nrese_sparql_syntax::term::{NamedNodePattern, TermPattern};

use super::{Context, GraphScope, PathPattern, ScanPattern, bound_variables, pushdown};
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
            } => {
                let pattern = PathPattern {
                    subject,
                    path,
                    object,
                    filter: None,
                };
                let rows = Some(self.path_size(&pattern));
                ("path", format!("{subject} {path} {object}"), rows)
            }
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
        if scans.is_empty() {
            // The empty pattern: one solution, binding nothing.
            return Some(1.0);
        }
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

    /// The estimate of `pattern`'s rows ([`Self::estimate_plan`]): what EXPLAIN shows
    /// beside the rows each operator gave.
    pub(super) fn estimate_rows(&self, pattern: &GraphPattern) -> Option<u64> {
        self.estimate_plan(&Plan::of(pattern), 0, &mut Vec::new())
            .map(|rows| rows.max(0.0).round() as u64)
    }

    /// A basic graph pattern's rows by the join orderer; two patterns, which it only
    /// sorts, by the larger (as [`Self::estimate_plan`]).
    pub(super) fn bgp_estimate(&self, scans: &[ScanPattern], counts: &[u64]) -> Option<u64> {
        let plan = self.join_order(scans, counts);
        match plan.rows.last().copied().filter(|r| r.is_finite()) {
            Some(rows) => Some(rows.max(0.0).round() as u64),
            None => counts.iter().copied().max(),
        }
    }

    /// The rows of a path pattern ([`Self::path_size`]); from `bound` values of one of its
    /// variable ends where it is followed from rows already computed.
    pub(super) fn path_estimate(
        &self,
        path: &PathPattern<'_>,
        bound: Option<usize>,
    ) -> Option<u64> {
        let rows = match bound {
            Some(values) => {
                let forward = self.path_fanout(path.path, true);
                let backward = self.path_fanout(path.path, false);
                values as f64 * forward.min(backward)
            }
            None => self.path_size(path),
        };
        let kept = path.filter.map_or(1.0, pushdown::selectivity);
        Some((rows * kept).max(0.0).round() as u64)
    }

    /// The rows of a path pattern alone. Both ends constant: one. One constant end: the
    /// path's fan-out from it (a single link's exact count). Both ends variables:
    /// [`Self::path_open`].
    pub(super) fn path_size(&self, path: &PathPattern<'_>) -> f64 {
        let constant = |term: &TermPattern| {
            !matches!(term, TermPattern::Variable(_) | TermPattern::BlankNode(_))
        };
        match (constant(path.subject), constant(path.object)) {
            (true, true) => 1.0,
            (false, false) => self.path_open(path.path),
            (subject, _) => {
                if let PropertyPathExpression::NamedNode(predicate) = path.path {
                    let triple = nrese_sparql_syntax::term::TriplePattern {
                        subject: path.subject.clone(),
                        predicate: predicate.clone().into(),
                        object: path.object.clone(),
                    };
                    return self.scan_rows(&triple);
                }
                self.path_fanout(path.path, subject)
            }
        }
    }

    /// The pairs of a path between two variables: a link's statements; a sequence's first
    /// part times the second's fan-out; an alternative's sum; a closure's starts (the
    /// step's pairs over its fan-out) times what each reaches ([`reach`]), and for `*`
    /// each start with itself; a negated set every statement.
    fn path_open(&self, path: &PropertyPathExpression) -> f64 {
        match path {
            PropertyPathExpression::NamedNode(predicate) => self.scan_rows(&link(predicate)),
            PropertyPathExpression::Reverse(p) | PropertyPathExpression::ZeroOrOne(p) => {
                self.path_open(p)
            }
            PropertyPathExpression::Sequence(a, b) => self.path_open(a) * self.path_fanout(b, true),
            PropertyPathExpression::Alternative(a, b) => self.path_open(a) + self.path_open(b),
            PropertyPathExpression::OneOrMore(p) | PropertyPathExpression::ZeroOrMore(p) => {
                let (pairs, fanout) = (self.path_open(p), self.path_fanout(p, true));
                let starts = if fanout > 0.0 { pairs / fanout } else { 0.0 };
                let own = f64::from(u8::from(matches!(
                    path,
                    PropertyPathExpression::ZeroOrMore(_)
                )));
                starts * (reach(fanout, pairs) + own)
            }
            PropertyPathExpression::NegatedPropertySet(_) => {
                self.snapshot.len_in(self.model) as f64
            }
        }
    }

    /// The ends a path reaches from one start (`forward`) or the starts that reach one end:
    /// a link's statements per distinct subject (object); a sequence's product, an
    /// alternative's sum; `p?` one more than `p`; a closure what one start reaches
    /// ([`reach`]); a negated set one.
    fn path_fanout(&self, path: &PropertyPathExpression, forward: bool) -> f64 {
        match path {
            PropertyPathExpression::NamedNode(predicate) => {
                let triple = link(predicate);
                let Some(scan) = self.scan_pattern(&triple) else {
                    return 0.0;
                };
                let count = self.snapshot.estimate_in(self.model, &scan.quad_pattern());
                let end = Variable::new_unchecked(if forward { "_s" } else { "_o" });
                let distinct = self.distinct(&scan, &end, count).max(1);
                count as f64 / distinct as f64
            }
            PropertyPathExpression::Reverse(p) => self.path_fanout(p, !forward),
            PropertyPathExpression::Sequence(a, b) => {
                self.path_fanout(a, forward) * self.path_fanout(b, forward)
            }
            PropertyPathExpression::Alternative(a, b) => {
                self.path_fanout(a, forward) + self.path_fanout(b, forward)
            }
            PropertyPathExpression::ZeroOrOne(p) => 1.0 + self.path_fanout(p, forward),
            PropertyPathExpression::OneOrMore(p) => {
                reach(self.path_fanout(p, forward), self.path_open(p))
            }
            PropertyPathExpression::ZeroOrMore(p) => {
                1.0 + reach(self.path_fanout(p, forward), self.path_open(p))
            }
            PropertyPathExpression::NegatedPropertySet(_) => 1.0,
        }
    }
}

/// What one start reaches over a step of fan-out `fanout` in one or more steps: the
/// fan-outs of up to eight steps added up, at most the step's `pairs` (no more nodes than
/// that can be reached).
fn reach(fanout: f64, pairs: f64) -> f64 {
    let mut reached = 0.0;
    let mut level = 1.0;
    for _ in 0..8 {
        level *= fanout;
        reached += level;
    }
    reached.min(pairs.max(fanout))
}

/// `?_s predicate ?_o`.
fn link(predicate: &nrese_rdf::NamedNode) -> nrese_sparql_syntax::term::TriplePattern {
    nrese_sparql_syntax::term::TriplePattern {
        subject: Variable::new_unchecked("_s").into(),
        predicate: predicate.clone().into(),
        object: Variable::new_unchecked("_o").into(),
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
