//! SPARQL 1.1 Graph Store Protocol operations: read from a view, write into a transaction.

use nrese_engine::{ReadModel, Transaction};
use nrese_sparql::ReadView;

use crate::error::StoreResult;
use crate::graph_store::{
    GraphDeleteReport, GraphReadRequest, GraphTarget, GraphWriteReport, GraphWriteRequest,
};
use crate::rdf_io::{BlankNodes, parse_graph, write_quads};

/// Whether the target graph exists in `view` under `model`: the default graph always, a
/// named graph while it holds a statement. Nothing is decoded (Graph Store Protocol `HEAD`).
pub(crate) fn graph_exists(
    view: &impl ReadView,
    target: &GraphTarget,
    model: ReadModel,
) -> StoreResult<bool> {
    let pattern = target.pattern_in(view)?;
    Ok(matches!(target, GraphTarget::DefaultGraph)
        || pattern.is_some_and(|p| view.quads_for_pattern_in(model, &p).next().is_some()))
}

/// Writes the target graph to `out` in the request's format as it is read, its statements
/// as triples; `cancel` stops it (a timeout, a client gone) between statements. A graph
/// that doesn't exist writes nothing and returns `false`, so a transport can still answer
/// 404 (Graph Store Protocol §5.2).
pub(crate) fn write_graph(
    view: &impl ReadView,
    request: &GraphReadRequest,
    model: ReadModel,
    cancel: &nrese_sparql::CancellationToken,
    out: impl std::io::Write,
) -> StoreResult<bool> {
    if !graph_exists(view, &request.target, model)? {
        return Ok(false);
    }
    let patterns: Vec<_> = request.target.pattern_in(view)?.into_iter().collect();
    let format = (request.format.rdf_format(), false);
    write_quads(view, model, &patterns, format, cancel, out)?;
    Ok(true)
}

/// Adds (or, with `replace`, substitutes) the payload's triples in the target graph. The
/// payload is parsed completely before the transaction is touched.
pub(crate) fn apply_graph_write(
    tx: &mut Transaction<'_>,
    request: &GraphWriteRequest,
) -> StoreResult<GraphWriteReport> {
    let quads = parse_graph(
        request.format,
        request.base_iri.as_deref(),
        request.payload.as_slice(),
        request.target.graph_name()?,
        BlankNodes::Fresh,
    )?;
    let existing = request.target.pattern_in(&*tx)?;
    let created = match (&request.target, existing) {
        (GraphTarget::DefaultGraph, _) => false,
        (GraphTarget::NamedGraph(_), Some(pattern)) => {
            tx.quads_for_pattern(&pattern).next().is_none()
        }
        (GraphTarget::NamedGraph(_), None) => true,
    };
    let before = tx.pending();
    if request.replace
        && let Some(pattern) = existing
    {
        tx.remove_matching(&pattern);
    }
    for quad in &quads {
        tx.insert(quad.as_ref());
    }
    Ok(GraphWriteReport {
        target: request.target.clone(),
        // Net change: replacing a graph with identical content modifies nothing.
        modified: tx.pending() != before,
        created: created && !quads.is_empty(),
        revision: 0,
    })
}

pub(crate) fn apply_graph_delete(
    tx: &mut Transaction<'_>,
    target: &GraphTarget,
) -> StoreResult<GraphDeleteReport> {
    let removed = match target.pattern_in(&*tx)? {
        Some(pattern) => tx.remove_matching(&pattern),
        None => 0,
    };
    Ok(GraphDeleteReport {
        target: target.clone(),
        modified: removed > 0,
        revision: 0,
    })
}
