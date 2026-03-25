#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SparqlUpdateRequest {
    pub update: String,
}

impl SparqlUpdateRequest {
    pub fn new(update: impl Into<String>) -> Self {
        Self {
            update: update.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UpdateExecutionReport {
    pub applied: bool,
    pub revision: u64,
}
