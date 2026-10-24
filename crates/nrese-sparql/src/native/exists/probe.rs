//! A selective merged-default EXISTS needs one matching quad, not its inner relation.
use nrese_engine::TermId;
use nrese_exec::{Scratch, UNDEF, computed_index};
use nrese_sparql_syntax::{algebra::GraphPattern, term::TermPattern};

use super::super::{Context, GraphScope, NativeResult, Slot, Solutions, search};

/// One charged boolean per outer row, with fixed slot metadata and no retained key cache.
/// Other scopes keep their existing set/graph routes. Refusals after a read are errors,
/// including when the iterator ended early or yielded nothing after refusing memory.
pub(super) fn try_mask<'a>(
    ctx: &'a Context<'_>,
    solutions: &Solutions,
    pattern: &GraphPattern,
) -> NativeResult<Option<Scratch<'a, bool>>> {
    // Automatic fresh scans are bounded to one outer row; forcedtrue keeps
    // its per-row scan cost. Decline before scan/term/column/estimate preparation.
    if ctx.as_written
        || solutions.table.is_empty()
        || ctx.index_joins == Some(false)
        || (ctx.index_joins.is_none() && solutions.table.len() != 1)
        || !matches!(*ctx.graph.borrow(), GraphScope::Union)
    {
        return Ok(None);
    }
    let GraphPattern::Bgp { patterns } = pattern else {
        return Ok(None);
    };
    let [triple] = patterns.as_slice() else {
        return Ok(None);
    };
    if search::is_search(triple, patterns)
        || ctx.is_spatial(triple)
        || matches!(
            triple.subject,
            TermPattern::Triple(_) | TermPattern::BlankNode(_)
        )
        || matches!(
            triple.object,
            TermPattern::Triple(_) | TermPattern::BlankNode(_)
        )
    {
        return Ok(None);
    }
    let Some(mut scan) = ctx.scan_pattern(triple) else {
        return Ok(None);
    };
    let columns: [Option<usize>; 3] = std::array::from_fn(|i| match &scan.slots[i] {
        Slot::Var(v) => solutions.column(v),
        _ => None,
    });
    if columns.iter().all(Option::is_none)
        || columns.iter().flatten().any(|&column| {
            solutions
                .table
                .column(column)
                .iter()
                .any(|&id| id == UNDEF || computed_index(id).is_some())
        })
    {
        return Ok(None);
    }
    ctx.check()?;
    let count = ctx.snapshot.estimate_in(ctx.model, &scan.quad_pattern());
    ctx.check()?;
    if !ctx.index_join(solutions.table.len(), count) {
        return Ok(None);
    }
    let repeats: [Option<usize>; 3] = std::array::from_fn(|i| match &scan.slots[i] {
        Slot::Var(v) => (0..i).find(|&j| scan.slots[j].is_var(v)),
        _ => None,
    });
    let graphs = ctx.merge_set.as_ref();
    let mut mask = ctx.mask(solutions.table.len(), false)?;
    // Bound positions are fixed across rows; reuse the ordinary scan owner once.
    for (slot, column) in scan.slots.iter_mut().zip(columns) {
        if column.is_some() {
            *slot = Slot::Const(TermId::DEFAULT_GRAPH);
        }
    }
    let permutation = scan.permutation_for(None);
    let mut bound = scan.quad_pattern();
    for (row, matched) in mask.iter_mut().enumerate() {
        ctx.check()?;
        for (slot, column) in columns.iter().enumerate() {
            if let Some(column) = column {
                let id = Some(TermId::from_raw(solutions.table.get(row, *column)));
                match slot {
                    0 => bound.subject = id,
                    1 => bound.predicate = id,
                    _ => bound.object = id,
                }
            }
        }
        let mut quads = ctx
            .snapshot
            .scan_sorted_in(ctx.model, &bound, permutation)
            .expect("permutation_for returns a usable permutation");
        for (n, quad) in quads.by_ref().enumerate() {
            if n.is_multiple_of(1 << 16) {
                ctx.check()?;
            }
            if graphs.is_some_and(|graphs| !graphs.contains(&quad.graph)) {
                continue;
            }
            let components = quad.components();
            if repeats
                .iter()
                .enumerate()
                .any(|(i, repeated)| repeated.is_some_and(|j| components[i] != components[j]))
            {
                continue;
            }
            *matched = true;
            break;
        }
        drop(quads);
        ctx.check()?;
    }
    Ok(Some(mask))
}
