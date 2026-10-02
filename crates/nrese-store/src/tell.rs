use crate::graph_store::{GraphTarget, GraphWriteRequest};
use crate::query::GraphResultFormat;

/// `TELL`: add the payload's triples to a graph. Semantically a non-replacing graph write,
/// with fresh blank nodes per request (as `INSERT DATA` has).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TellRequest {
    pub target: GraphTarget,
    pub format: GraphResultFormat,
    pub base_iri: Option<String>,
    pub payload: Vec<u8>,
}

impl TellRequest {
    pub(crate) fn as_graph_write(&self) -> GraphWriteRequest {
        GraphWriteRequest {
            target: self.target.clone(),
            format: self.format,
            base_iri: self.base_iri.clone(),
            payload: self.payload.clone(),
            replace: false,
        }
    }
}
