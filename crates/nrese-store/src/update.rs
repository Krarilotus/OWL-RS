use std::mem::{size_of, size_of_val};
use std::sync::Arc;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StagedUpdate {
    pub(crate) namespaces: Box<[(String, String)]>,
    pub(crate) search_scope: crate::SearchScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SparqlUpdateRequest {
    pub update: String,
    /// Protocol `using-graph-uri` values. If this or `using_named_graphs` is non-empty,
    /// both replace the `USING` / `USING NAMED` clauses of every operation.
    pub using_graphs: Vec<String>,
    /// Protocol `using-named-graph-uri` values.
    pub using_named_graphs: Vec<String>,
    pub(crate) staged: Option<Arc<StagedUpdate>>,
}

impl SparqlUpdateRequest {
    pub fn new(update: impl Into<String>) -> Self {
        Self {
            update: update.into(),
            using_graphs: Vec::new(),
            using_named_graphs: Vec::new(),
            staged: None,
        }
    }

    /// The heap bytes it holds beside its own slot, by capacity.
    pub fn heap_bytes(&self) -> usize {
        let graphs = |graphs: &Vec<String>| {
            graphs.capacity() * std::mem::size_of::<String>()
                + graphs.iter().map(String::capacity).sum::<usize>()
        };
        let staged = self.staged.as_ref().map_or(0, |staged| {
            let entries = staged.namespaces.as_ref();
            let header = size_of::<StagedUpdate>() + 2 * size_of::<usize>();
            let mut bytes = header + size_of_val(entries);
            for (prefix, iri) in entries {
                bytes += prefix.capacity() + iri.capacity();
            }
            bytes
        });
        let graphs = graphs(&self.using_graphs) + graphs(&self.using_named_graphs);
        self.update.capacity() + graphs + staged
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UpdateExecutionReport {
    pub applied: bool,
    pub revision: u64,
}
