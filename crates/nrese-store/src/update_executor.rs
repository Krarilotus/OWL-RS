use oxigraph::sparql::SparqlEvaluator;
use oxigraph::store::Store;

use crate::error::StoreResult;
use crate::update::{SparqlUpdateRequest, UpdateExecutionReport};

pub fn execute_update(
    store: &Store,
    request: &SparqlUpdateRequest,
) -> StoreResult<UpdateExecutionReport> {
    SparqlEvaluator::new()
        .parse_update(&request.update)?
        .on_store(store)
        .execute()?;

    Ok(UpdateExecutionReport {
        applied: true,
        revision: 0,
    })
}
