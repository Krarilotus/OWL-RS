//! SPARQL 1.1 Graph Store Protocol operations: read from a view, write into a transaction.

use nrese_engine::{ReadModel, Transaction};
use nrese_sparql::ReadView;

use crate::error::StoreResult;
use crate::graph_store::{
    GraphDeleteReport, GraphReadRequest, GraphTarget, GraphWriteReport, GraphWriteRequest,
};
use crate::rdf_io::{BlankNodes, each_graph_quad, write_quads};

/// Whether the target graph exists in `view` under `model`: the default graph always, a
/// named graph while it holds a statement. Nothing is decoded (Graph Store Protocol `HEAD`).
pub(crate) fn graph_exists(
    view: &impl ReadView,
    target: &GraphTarget,
    model: ReadModel,
) -> StoreResult<bool> {
    let pattern = target.pattern_in(view)?;
    Ok(matches!(target.resolved()?, GraphTarget::DefaultGraph)
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

/// Adds (or, with `replace`, substitutes) the payload's triples in the target graph, each as
/// it is read: an invalid payload fails the write, whose transaction is then discarded (the
/// mutation pipeline commits only a write that ends). The new terms it interned stay in the
/// shared dictionary (stated limit L3 for v1.5, dictionary lifecycle contract).
pub(crate) fn apply_graph_write(
    tx: &mut Transaction<'_>,
    request: &GraphWriteRequest,
) -> StoreResult<GraphWriteReport> {
    let graph = request.target.graph_name()?;
    let existing = request.target.pattern_in(&*tx)?;
    let created = match (request.target.resolved()?, existing) {
        (GraphTarget::DefaultGraph, _) => false,
        (GraphTarget::NamedGraph(_), Some(pattern)) => {
            tx.quads_for_pattern(&pattern).next().is_none()
        }
        (GraphTarget::NamedGraph(_), None) => true,
    };
    let before = tx.pending();
    // The pending changes' growth is charged before it happens (D16-039).
    if request.replace
        && let Some(pattern) = existing
    {
        tx.remove_matching(&pattern)?;
    }
    let mut any = false;
    each_graph_quad(
        request.format,
        request.base_iri.as_deref(),
        &request.payload,
        graph,
        BlankNodes::Fresh,
        |quad| {
            tx.try_insert(quad)?;
            any = true;
            Ok(())
        },
    )?;
    Ok(GraphWriteReport {
        target: request.target.clone(),
        // Net change: replacing a graph with identical content modifies nothing.
        modified: tx.pending() != before,
        created: created && any,
        revision: 0,
    })
}

pub(crate) fn apply_graph_delete(
    tx: &mut Transaction<'_>,
    target: &GraphTarget,
) -> StoreResult<GraphDeleteReport> {
    let removed = match target.pattern_in(&*tx)? {
        Some(pattern) => tx.remove_matching(&pattern)?,
        None => 0,
    };
    Ok(GraphDeleteReport {
        target: target.clone(),
        modified: removed > 0,
        revision: 0,
    })
}
