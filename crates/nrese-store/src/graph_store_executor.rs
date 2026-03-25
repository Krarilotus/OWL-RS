use oxigraph::io::{RdfParser, RdfSerializer};
use oxigraph::model::{GraphNameRef, NamedNode};
use oxigraph::store::{Store, Transaction};

use crate::error::{StoreError, StoreResult};
use crate::graph_store::{
    GraphDeleteReport, GraphReadRequest, GraphReadResult, GraphTarget, GraphWriteReport,
    GraphWriteRequest,
};

pub fn execute_graph_read(
    store: &Store,
    request: &GraphReadRequest,
) -> StoreResult<GraphReadResult> {
    let mut writer =
        RdfSerializer::from_format(request.format.into_oxigraph()).for_writer(Vec::new());

    match &request.target {
        GraphTarget::DefaultGraph => {
            for quad in store.quads_for_pattern(None, None, None, Some(GraphNameRef::DefaultGraph))
            {
                writer.serialize_triple(quad?.as_ref())?;
            }
        }
        GraphTarget::NamedGraph(iri) => {
            let named = parse_named_graph_iri(iri)?;
            for quad in store.quads_for_pattern(None, None, None, Some(named.as_ref().into())) {
                writer.serialize_triple(quad?.as_ref())?;
            }
        }
    }

    Ok(GraphReadResult {
        media_type: request.format.media_type(),
        payload: writer.finish()?,
    })
}

pub fn execute_graph_write(
    store: &Store,
    request: &GraphWriteRequest,
) -> StoreResult<GraphWriteReport> {
    let parser = parser_for_target(request)?;
    let mut transaction = store.start_transaction()?;

    if request.replace {
        clear_target_graph_in_transaction(&mut transaction, &request.target)?;
    }

    transaction.load_from_slice(parser, request.payload.as_slice())?;
    transaction.commit()?;

    Ok(GraphWriteReport {
        modified: true,
        revision: 0,
    })
}

pub fn execute_graph_delete(store: &Store, target: &GraphTarget) -> StoreResult<GraphDeleteReport> {
    match target {
        GraphTarget::DefaultGraph => {
            store.clear_graph(GraphNameRef::DefaultGraph)?;
        }
        GraphTarget::NamedGraph(iri) => {
            let named = parse_named_graph_iri(iri)?;
            store.remove_named_graph(named.as_ref())?;
        }
    }

    Ok(GraphDeleteReport {
        target: target.clone(),
        modified: true,
        revision: 0,
    })
}

fn clear_target_graph_in_transaction(
    transaction: &mut Transaction<'_>,
    target: &GraphTarget,
) -> StoreResult<()> {
    match target {
        GraphTarget::DefaultGraph => transaction.clear_graph(GraphNameRef::DefaultGraph)?,
        GraphTarget::NamedGraph(iri) => {
            let named = parse_named_graph_iri(iri)?;
            transaction.clear_graph(named.as_ref())?;
        }
    }

    Ok(())
}

fn parser_for_target(request: &GraphWriteRequest) -> StoreResult<RdfParser> {
    let parser = RdfParser::from_format(request.format.into_oxigraph()).without_named_graphs();

    let parser = match &request.target {
        GraphTarget::DefaultGraph => parser,
        GraphTarget::NamedGraph(iri) => {
            let named = parse_named_graph_iri(iri)?;
            parser.with_default_graph(named.as_ref())
        }
    };

    Ok(parser)
}

fn parse_named_graph_iri(iri: &str) -> StoreResult<NamedNode> {
    NamedNode::new(iri).map_err(|_| StoreError::InvalidGraphIri(iri.to_owned()))
}
