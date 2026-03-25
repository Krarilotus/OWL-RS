use oxigraph::io::RdfSerializer;
use oxigraph::sparql::results::QueryResultsSerializer;
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;

use crate::error::StoreResult;
use crate::query::{QueryResultKind, SerializedQueryResult, SparqlQueryRequest};

pub fn execute_query(
    store: &Store,
    request: &SparqlQueryRequest,
) -> StoreResult<SerializedQueryResult> {
    let prepared_query = SparqlEvaluator::new().parse_query(&request.query)?;
    let results = prepared_query.on_store(store).execute()?;
    serialize_results(results, request)
}

fn serialize_results(
    results: QueryResults<'static>,
    request: &SparqlQueryRequest,
) -> StoreResult<SerializedQueryResult> {
    match results {
        QueryResults::Boolean(value) => {
            let format = request.solutions_format.into_oxigraph();
            let serializer = QueryResultsSerializer::from_format(format);
            let payload = serializer.serialize_boolean_to_writer(Vec::new(), value)?;

            Ok(SerializedQueryResult {
                kind: QueryResultKind::Boolean,
                media_type: request.solutions_format.media_type(),
                payload,
            })
        }
        QueryResults::Solutions(solutions) => {
            let format = request.solutions_format.into_oxigraph();
            let serializer = QueryResultsSerializer::from_format(format);
            let mut writer = serializer
                .serialize_solutions_to_writer(Vec::new(), solutions.variables().to_vec())?;

            for solution in solutions {
                writer.serialize(&solution?)?;
            }

            Ok(SerializedQueryResult {
                kind: QueryResultKind::Solutions,
                media_type: request.solutions_format.media_type(),
                payload: writer.finish()?,
            })
        }
        QueryResults::Graph(triples) => {
            let mut writer = RdfSerializer::from_format(request.graph_format.into_oxigraph())
                .for_writer(Vec::new());

            for triple in triples {
                writer.serialize_triple(&triple?)?;
            }

            Ok(SerializedQueryResult {
                kind: QueryResultKind::Graph,
                media_type: request.graph_format.media_type(),
                payload: writer.finish()?,
            })
        }
    }
}
