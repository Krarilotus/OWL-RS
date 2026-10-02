#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SparqlUpdateRequest {
    pub update: String,
    /// Protocol `using-graph-uri` values. If this or `using_named_graphs` is non-empty,
    /// both replace the `USING` / `USING NAMED` clauses of every operation.
    pub using_graphs: Vec<String>,
    /// Protocol `using-named-graph-uri` values.
    pub using_named_graphs: Vec<String>,
}

impl SparqlUpdateRequest {
    pub fn new(update: impl Into<String>) -> Self {
        Self {
            update: update.into(),
            using_graphs: Vec::new(),
            using_named_graphs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UpdateExecutionReport {
    pub applied: bool,
    pub revision: u64,
}
