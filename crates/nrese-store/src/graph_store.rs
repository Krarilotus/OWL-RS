use crate::query::GraphResultFormat;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphTarget {
    DefaultGraph,
    NamedGraph(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphReadRequest {
    pub target: GraphTarget,
    pub format: GraphResultFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphReadResult {
    pub media_type: &'static str,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphWriteRequest {
    pub target: GraphTarget,
    pub format: GraphResultFormat,
    pub base_iri: Option<String>,
    pub payload: Vec<u8>,
    pub replace: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphWriteReport {
    pub target: GraphTarget,
    pub modified: bool,
    pub created: bool,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphDeleteReport {
    pub target: GraphTarget,
    pub modified: bool,
    pub revision: u64,
}
