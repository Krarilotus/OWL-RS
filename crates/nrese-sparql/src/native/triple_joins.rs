//! Joins through a triple term: a condition `sameTerm(?t, TRIPLE(s, p, o))`
//! (`triple_terms`' composed form) between what a basic graph pattern has bound and its
//! next pattern, used in the direction the bound side allows:
//!
//! - **composing**: the parts are bound in every row and `?t` is free in the next pattern
//!   (a reifier's `?r rdf:reifies ?t`). `?t` is the triple term of the row's parts, one
//!   hash probe ([`Snapshot::triple_term`]); a row whose parts make no stored triple term
//!   has no match. The pattern is then joined on `?t`, by the index where the rows are few;
//! - **taking apart**: `?t` is bound and some of its parts are free in the next pattern
//!   (the edge `?a :knows ?b`). They are read from the triple term's key
//!   ([`Snapshot::components`]), the bound ones compared, and the pattern joined on them.
//!
//! Without it the two sides share no variable: a cross product that the condition then
//! filters (every reifier against every edge). With it a hop costs a probe of the triple
//! term and one of the index per edge, nothing decoded. The condition stays in the filters
//! and runs on every row afterwards (on ids, `fast`), so the rows are exactly the
//! condition's over the cross product. Where a row's values aren't decided on ids (a value
//! the query computed, a triple term keyed by the first RDF 1.2 version's text), the link
//! isn't used and the join goes as without it.

use super::fast::{Id, IdTerm, compile_term};
use super::*;
use nrese_sparql_syntax::algebra::Function;

/// A condition that joins the bound rows to the next pattern through a triple term.
pub(super) struct Link {
    /// The side computed from each row.
    value: IdTerm,
    /// The side the value is matched against: a variable or `TRIPLE(…)` of variables and
    /// constants, which binds the next pattern's free variables among them.
    pattern: IdTerm,
    /// The next pattern's free variables `pattern` binds.
    binds: Vec<Variable>,
}

/// A conjunct of `filters` that links `bound` (bound in every row) to `scan` through a
/// triple term, if there is one.
pub(super) fn link(
    filters: &[(&Expression, Vec<Variable>)],
    bound: &Solutions,
    scan: &ScanPattern,
    snapshot: &Snapshot,
) -> Option<Link> {
    let free = scan.vars();
    let free: Vec<&Variable> = free.iter().filter(|v| bound.column(v).is_none()).collect();
    if free.is_empty() {
        return None;
    }
    let everywhere = |v: &Variable| {
        bound
            .column(v)
            .is_some_and(|c| !bound.table.column(c).contains(&UNDEF))
    };
    for (conjunct, _) in filters {
        let Expression::SameTerm(x, y) = conjunct else {
            continue;
        };
        // Only through a triple term: `sameTerm(?a, ?b)` alone is `equijoin`'s.
        if matches!(
            (&**x, &**y),
            (Expression::Variable(_), Expression::Variable(_))
        ) {
            continue;
        }
        let (Some(x), Some(y)) = (compile_term(x, snapshot), compile_term(y, snapshot)) else {
            continue;
        };
        for (value, pattern) in [(&x, &y), (&y, &x)] {
            let mut read = Vec::new();
            value.variables(&mut read);
            if !read.into_iter().all(everywhere) || !matchable(pattern) {
                continue;
            }
            let mut wanted = Vec::new();
            pattern.variables(&mut wanted);
            let mut binds: Vec<Variable> = Vec::new();
            for v in wanted {
                if free.contains(&v) && !binds.contains(v) {
                    binds.push(v.clone());
                }
            }
            if !binds.is_empty() {
                return Some(Link {
                    value: value.clone(),
                    pattern: pattern.clone(),
                    binds,
                });
            }
        }
    }
    None
}

/// Whether a value can be matched against `pattern` by binding its variables: variables,
/// constants and `TRIPLE(…)` of such (`SUBJECT(…)` can't be inverted).
fn matchable(pattern: &IdTerm) -> bool {
    match pattern {
        IdTerm::Var(_) | IdTerm::Const(_) => true,
        IdTerm::Part(..) => false,
        IdTerm::Triple(parts) => parts.iter().all(matchable),
    }
}

/// The variables the links of `filters` join, for the join orderer: per condition
/// `sameTerm(?v, e)` with `e` over triple terms, `?v` and the variables of `e`. A pattern
/// that binds all of `e`'s counts as sharing `?v` with the patterns that bind it.
pub(super) fn planned(filters: &[(&Expression, Vec<Variable>)]) -> Vec<(Variable, Vec<Variable>)> {
    let mut out = Vec::new();
    for (conjunct, _) in filters {
        let Expression::SameTerm(x, y) = conjunct else {
            continue;
        };
        for (whole, other) in [(x, y), (y, x)] {
            let (Expression::Variable(v), Expression::FunctionCall(function, _)) =
                (&**whole, &**other)
            else {
                continue;
            };
            if !matches!(
                function,
                Function::Triple | Function::Subject | Function::Predicate | Function::Object
            ) {
                continue;
            }
            let mut read = expression_variables(other);
            read.sort_by(|a, b| a.as_str().cmp(b.as_str()));
            read.dedup();
            if !read.is_empty() && !read.contains(v) {
                out.push((v.clone(), read));
            }
        }
    }
    out
}

/// `id` matched against `pattern` in row `row` of `bound`: whether it matches, with the
/// values of `binds` set in `values`; `None` where ids don't decide it.
fn unify(
    pattern: &IdTerm,
    id: TermId,
    bound: &Solutions,
    row: usize,
    binds: &[Variable],
    values: &mut [u64],
    snapshot: &Snapshot,
) -> Option<bool> {
    match pattern {
        IdTerm::Var(v) => {
            if let Some(at) = binds.iter().position(|b| b == v) {
                if values[at] == UNDEF {
                    values[at] = id.raw();
                    return Some(true);
                }
                return same(TermId::from_raw(values[at]), id, snapshot);
            }
            match bound.column(v).map(|c| bound.table.get(row, c)) {
                // Neither bound nor bound here: the condition checks it later.
                None | Some(UNDEF) => Some(true),
                Some(have) if computed_index(have).is_some() => None,
                Some(have) => same(TermId::from_raw(have), id, snapshot),
            }
        }
        IdTerm::Const(Id::Stored(constant)) => same(*constant, id, snapshot),
        IdTerm::Const(Id::Absent | Id::Error) => Some(false),
        IdTerm::Const(Id::Unknown) | IdTerm::Part(..) => None,
        IdTerm::Triple(parts) => {
            if id.kind() != nrese_engine::TermKind::Triple {
                return Some(false);
            }
            let components = snapshot.components(id)?;
            for (part, component) in parts.iter().zip(components) {
                if !unify(part, component, bound, row, binds, values, snapshot)? {
                    return Some(false);
                }
            }
            Some(true)
        }
    }
}

/// Whether stored ids `a` and `b` are the same term; `None` where ids don't decide it.
fn same(a: TermId, b: TermId, snapshot: &Snapshot) -> Option<bool> {
    match fast::same_term(Id::Stored(a), Id::Stored(b), snapshot) {
        fast::Tri::True => Some(true),
        fast::Tri::False => Some(false),
        _ => None,
    }
}

impl Context<'_> {
    /// The rows of `bound` the link keeps and, per kept row, the values of the variables
    /// it binds; `None` where a row isn't decided on ids.
    pub(super) fn linked_rows(
        &self,
        bound: &Solutions,
        link: &Link,
    ) -> Option<(Vec<bool>, IdTable)> {
        let rows = bound.table.len();
        let mut keep = Vec::with_capacity(rows);
        // The bound values of the kept rows, charged as they grow (refused: undecided here,
        // as for a row not decided on ids).
        let mut columns = IdTable::new(link.binds.len());
        let mut values = vec![UNDEF; link.binds.len()];
        let snapshot: &Snapshot = &self.snapshot;
        for row in 0..rows {
            let value = |v: &Variable| bound.column(v).map_or(UNDEF, |c| bound.table.get(row, c));
            let kept = match link.value.value(&value, snapshot) {
                Id::Stored(id) => {
                    values.fill(UNDEF);
                    unify(
                        &link.pattern,
                        id,
                        bound,
                        row,
                        &link.binds,
                        &mut values,
                        snapshot,
                    )?
                }
                // No stored term is an error or a term the snapshot hasn't.
                Id::Error | Id::Absent => false,
                Id::Unknown => return None,
            };
            if kept {
                columns.push_row(&values, &self.budget).ok()?;
            }
            keep.push(kept);
        }
        Some((keep, columns))
    }

    /// `result` joined with `scan` (`count` matches) through `link`, with `linked` from
    /// [`Self::linked_rows`]: the kept rows with the bound variables, joined on them and on
    /// the variables they share already.
    pub(super) fn triple_join(
        &self,
        result: Solutions,
        scan: &ScanPattern,
        count: u64,
        link: &Link,
        linked: (Vec<bool>, IdTable),
    ) -> NativeResult<Solutions> {
        let (keep, values) = linked;
        let left = self.reshaped(result, |mut left| {
            left.table.retain_mask(&keep);
            let mut columns = std::mem::take(&mut left.table).into_columns();
            columns.append(values.into_columns(), &self.budget)?;
            left.vars.extend(link.binds.iter().cloned());
            left.table = IdTable::from_columns(columns);
            Ok(left)
        })?;
        let shared: Vec<Variable> = scan
            .vars()
            .into_iter()
            .filter(|v| left.column(v).is_some())
            .collect();
        let probe = self.index_join(left.table.len(), count);
        if probe {
            self.probe_join(left, scan, &shared)
        } else {
            let scanned = self.scan(scan, shared.first())?;
            self.join(left, scanned)
        }
    }
}
