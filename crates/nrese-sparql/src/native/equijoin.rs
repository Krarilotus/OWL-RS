//! `FILTER(?a = ?b)` or `FILTER(sameTerm(?a, ?b))` between what a basic graph pattern has
//! bound (`?a`) and its next pattern, which shares no variable with it (`?b`): a join on
//! the two variables instead of their cross product. BSBM BI q2 crossed a product's 21
//! features with all 600 k product features to keep the 8 k equal pairs.
//!
//! `?a = ?b` holds between IRIs or blank nodes exactly when they are the same term, and
//! never between one of them and a literal or a triple term. So rows whose `?a` is an IRI
//! or a blank node join the pattern on `?b` set to `?a`'s value (an index probe, or a hash
//! join). A literal or triple term may equal another one with a different id (`1` and
//! `"01"^^xsd:integer`), so those rows meet the pattern's rows whose `?b` is neither an IRI
//! nor a blank node in a cross product. `sameTerm` is identity: every row joins by id. The
//! conjunct stays in the filters and runs on every row afterwards, so the results are the
//! filter's over the cross product.

use super::*;

/// The variables a conjunct `?a = ?b` or `sameTerm(?a, ?b)` links: `a` bound in `bound`
/// (in every row), `b` among `free` and not bound; and whether it is `sameTerm`.
pub(super) fn link(
    filters: &[(&Expression, Vec<Variable>)],
    bound: &Solutions,
    free: &[Variable],
) -> Option<(Variable, Variable, bool)> {
    for (conjunct, _) in filters {
        let (x, y, same_term) = match conjunct {
            Expression::Equal(x, y) => (x, y, false),
            Expression::SameTerm(x, y) => (x, y, true),
            _ => continue,
        };
        let (Expression::Variable(x), Expression::Variable(y)) = (&**x, &**y) else {
            continue;
        };
        for (a, b) in [(x, y), (y, x)] {
            if let Some(column) = bound.column(a)
                && !bound.table.column(column).contains(&UNDEF)
                && free.contains(b)
                && bound.column(b).is_none()
            {
                return Some((a.clone(), b.clone(), same_term));
            }
        }
    }
    None
}

/// Whether `id` is a term that only equals itself under `=`: an IRI or a blank node.
fn only_itself(id: u64) -> bool {
    matches!(
        TermId::from_raw(id).kind(),
        nrese_engine::TermKind::Iri | nrese_engine::TermKind::BlankNode
    )
}

/// The rows of `solutions` that `keep` selects.
fn rows_where(solutions: &Solutions, keep: &[bool]) -> Solutions {
    let columns: Vec<Vec<u64>> = solutions
        .table
        .columns()
        .iter()
        .map(|column| {
            column
                .iter()
                .zip(keep)
                .filter(|&(_, &keep)| keep)
                .map(|(&id, _)| id)
                .collect()
        })
        .collect();
    Solutions {
        vars: solutions.vars.clone(),
        table: IdTable::from_columns(columns),
        ordered: false,
    }
}

impl Context<'_> {
    /// `result` joined with `scan` (`count` matches) where `?a` and `?b` may be equal
    /// (module docs). The rows are those of the cross product the conjunct passes, and
    /// some it doesn't (literals): the caller still applies it.
    pub(super) fn equality_join(
        &self,
        result: Solutions,
        scan: &ScanPattern,
        count: u64,
        a: &Variable,
        b: &Variable,
        same_term: bool,
    ) -> NativeResult<Solutions> {
        let column = result.column(a).expect("linked variables are bound");
        let ids = result.table.column(column);
        let by_id: Vec<bool> = ids.iter().map(|&id| same_term || only_itself(id)).collect();
        let by_value = !by_id.iter().all(|&joined| joined);
        // By id: `?b` as a copy of `?a`, then the pattern joined on `?b`.
        let mut left = rows_where(&result, &by_id);
        let copy = left.table.column(column).to_vec();
        let mut columns = left.table.into_columns();
        columns.push(copy);
        left.vars.push(b.clone());
        left.table = IdTable::from_columns(columns);
        let crossed = by_value.then(|| {
            let keep: Vec<bool> = by_id.iter().map(|&joined| !joined).collect();
            rows_where(&result, &keep)
        });
        self.consumed(&result);
        let left = self.produced(left)?;
        let shared = [b.clone()];
        let probe = self.merge_set.is_none()
            && (left.table.len() as u64).saturating_mul(PROBE_FACTOR) < count;
        let joined = if probe {
            self.probe_join(left, scan, &shared)?
        } else {
            let scanned = self.scan(scan, Some(b))?;
            self.join(left, scanned)?
        };
        let Some(crossed) = crossed else {
            return Ok(joined);
        };
        // By value: rows with literals or triple terms against the pattern's such rows.
        let crossed = self.produced(crossed)?;
        let mut scanned = self.scan(scan, None)?;
        let at = scanned.column(b).expect("the pattern binds ?b");
        scanned.table.retain(|table, row| {
            let id = table.get(row, at);
            id != UNDEF && !only_itself(id)
        });
        let crossed = self.join(crossed, scanned)?;
        self.union(joined, crossed)
    }
}
