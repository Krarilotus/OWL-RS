//! SPARQL 1.1 Graph Store Protocol operations: read from a view, write into a transaction.

use nrese_engine::{ReadModel, Transaction};
use nrese_sparql::ReadView;

use crate::error::StoreResult;
use crate::graph_store::{
    GraphDeleteReport, GraphReadRequest, GraphReadResult, GraphTarget, GraphWriteReport,
    GraphWriteRequest,
};
use crate::rdf_io::{BlankNodes, parse_graph, serialize_triples};
use crate::view::decoded_quads;

pub fn execute_graph_read(
    view: &impl ReadView,
    request: &GraphReadRequest,
) -> StoreResult<GraphReadResult> {
    let triples = match request.target.pattern_in(view)? {
        Some(pattern) => decoded_quads(view, ReadModel::Materialised, &pattern)
            .map(|quad| quad.map(Into::into))
            .collect::<StoreResult<Vec<_>>>()?,
        None => Vec::new(),
    };
    Ok(GraphReadResult {
        media_type: request.format.media_type(),
        payload: serialize_triples(request.format, triples)?,
    })
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
