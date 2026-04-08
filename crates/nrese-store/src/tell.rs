use oxigraph::store::Store;

use crate::error::{StoreError, StoreResult};
use crate::graph_store::{GraphReadRequest, GraphTarget, GraphWriteRequest};
use crate::graph_store_executor::{execute_graph_read, execute_graph_write};
use crate::query::GraphResultFormat;
use crate::update::SparqlUpdateRequest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TellRequest {
    pub target: GraphTarget,
    pub format: GraphResultFormat,
    pub base_iri: Option<String>,
    pub payload: Vec<u8>,
}

pub fn compile_tell_update(request: &TellRequest) -> StoreResult<SparqlUpdateRequest> {
    let staging_store = Store::new()?;
    execute_graph_write(
        &staging_store,
        &GraphWriteRequest {
            target: request.target.clone(),
            format: request.format,
            base_iri: request.base_iri.clone(),
            payload: request.payload.clone(),
            replace: false,
        },
    )?;

    let canonical_graph = execute_graph_read(
        &staging_store,
        &GraphReadRequest {
            target: request.target.clone(),
            format: GraphResultFormat::NTriples,
        },
    )?;
    let triples = String::from_utf8(canonical_graph.payload).map_err(|error| {
        StoreError::Configuration(format!("canonical tell payload is not UTF-8: {error}"))
    })?;

    let update = match &request.target {
        GraphTarget::DefaultGraph => format!("INSERT DATA {{\n{triples}}}"),
        GraphTarget::NamedGraph(iri) => {
            format!("INSERT DATA {{\nGRAPH <{iri}> {{\n{triples}}}\n}}")
        }
    };

    Ok(SparqlUpdateRequest::new(update))
}

#[cfg(test)]
mod tests {
    use crate::graph_store::GraphTarget;
    use crate::query::GraphResultFormat;

    use super::{TellRequest, compile_tell_update};

    #[test]
    fn tell_request_compiles_default_graph_insert_data() {
        let request = TellRequest {
            target: GraphTarget::DefaultGraph,
            format: GraphResultFormat::Turtle,
            base_iri: None,
            payload: br#"@prefix ex: <http://example.com/> .
ex:s ex:p ex:o .
"#
            .to_vec(),
        };

        let update = compile_tell_update(&request).expect("tell update");

        assert!(update.update.starts_with("INSERT DATA"));
        assert!(
            update
                .update
                .contains("<http://example.com/s> <http://example.com/p> <http://example.com/o>")
        );
    }

    #[test]
    fn tell_request_compiles_named_graph_insert_data() {
        let request = TellRequest {
            target: GraphTarget::NamedGraph("http://example.com/g".to_owned()),
            format: GraphResultFormat::Turtle,
            base_iri: None,
            payload: br#"@prefix ex: <http://example.com/> .
ex:s ex:p "value" .
"#
            .to_vec(),
        };

        let update = compile_tell_update(&request).expect("tell update");

        assert!(update.update.contains("GRAPH <http://example.com/g>"));
        assert!(update.update.contains("\"value\""));
    }
}
