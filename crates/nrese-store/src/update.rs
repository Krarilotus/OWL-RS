#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SparqlUpdateRequest {
    pub update: String,
    /// Protocol `using-graph-uri` values. If this or `using_named_graphs` is non-empty,
    /// both replace the `USING` / `USING NAMED` clauses of every operation.
    pub using_graphs: Vec<String>,
    /// Protocol `using-named-graph-uri` values.
    pub using_named_graphs: Vec<String>,
    /// The effort its `WHERE` clauses may spend choosing join orders: the server's
    /// `budgets.planner_effort`, as for queries ([`crate::SparqlQueryRequest::planner`]).
    pub planner: nrese_sparql::PlannerOptions,
}

impl SparqlUpdateRequest {
    pub fn new(update: impl Into<String>) -> Self {
        Self {
            update: update.into(),
            using_graphs: Vec::new(),
            using_named_graphs: Vec::new(),
            planner: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UpdateExecutionReport {
    pub applied: bool,
    pub revision: u64,
}
