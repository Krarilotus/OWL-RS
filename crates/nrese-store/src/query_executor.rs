//! Query execution: parse, evaluate on a read view (L2), serialise the results.

use nrese_sparql::{QueryOptions, QueryResults, ReadView, evaluate_query};
use sparesults::{QueryResultsFormat, QueryResultsSerializer};
use spargebra::SparqlParser;

use crate::error::StoreResult;
use crate::query::{
    QueryResultKind, SerializedQueryResult, SolutionsResultFormat, SparqlQueryRequest,
};
use crate::rdf_io::serialize_triples;

impl SolutionsResultFormat {
    fn results_format(self) -> QueryResultsFormat {
        match self {
            Self::Json => QueryResultsFormat::Json,
            Self::Xml => QueryResultsFormat::Xml,
            Self::Csv => QueryResultsFormat::Csv,
            Self::Tsv => QueryResultsFormat::Tsv,
        }
    }
}

pub fn execute_query(
    view: &impl ReadView,
    request: &SparqlQueryRequest,
) -> StoreResult<SerializedQueryResult> {
    let query = SparqlParser::new().parse_query(&request.query)?;
    let results = evaluate_query(view, &query, &QueryOptions::default())?;
    let serializer = QueryResultsSerializer::from_format(request.solutions_format.results_format());
    let (kind, media_type, payload) = match results {
        QueryResults::Boolean(value) => (
            QueryResultKind::Boolean,
            request.solutions_format.media_type(),
            serializer.serialize_boolean_to_writer(Vec::new(), value)?,
        ),
        QueryResults::Solutions(solutions) => {
            let mut writer = serializer
                .serialize_solutions_to_writer(Vec::new(), solutions.variables().to_vec())?;
            for solution in solutions {
                writer.serialize(&solution?)?;
            }
            (
                QueryResultKind::Solutions,
                request.solutions_format.media_type(),
                writer.finish()?,
            )
        }
        QueryResults::Graph(triples) => {
            let triples = triples.collect::<Result<Vec<_>, _>>()?;
            (
                QueryResultKind::Graph,
                request.graph_format.media_type(),
                serialize_triples(request.graph_format, triples)?,
            )
        }
    };
    Ok(SerializedQueryResult {
        kind,
        media_type,
        payload,
    })
}
