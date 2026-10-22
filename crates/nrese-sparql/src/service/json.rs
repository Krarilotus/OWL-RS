//! Same-reservation JSON construction. Header metadata keeps its existing exemption.
use super::*;
use nrese_sparql_results::{
    QueryResultsFormat, QueryResultsParseError, QueryResultsParser, SliceQueryResultsParserOutput,
    SolutionsCallbacks,
};
use std::io;

struct Build<'a> {
    result: ServiceResults,
    stop: &'a dyn StopCheck,
}

impl ServiceResults {
    /// Decode one endpoint response on its actual invocation budget. JSON uses
    /// scoped admitted construction and moves rows; other formats retain their
    /// ordinary parser/copy path and its explicit unadmitted parser boundary.
    /// Variable metadata, opaque validators and initial UTF-8/location work are
    /// not a completed parser-allocation/cancellation claim.
    pub fn from_slice(
        format: QueryResultsFormat,
        bytes: &[u8],
        max_rows: usize,
        budget: &Arc<Budget>,
        stop: &dyn StopCheck,
    ) -> Result<Self, QueryEvaluationError> {
        if format != QueryResultsFormat::Json {
            return Self::from_slice_copying(format, bytes, max_rows, budget, stop);
        }
        stop.check().map_err(QueryEvaluationError::from)?;
        // The response's original reservation exists before lexical/parser startup.
        let mut build = Build {
            result: Self::new(Vec::new(), budget),
            stop,
        };
        let hooks = SolutionsCallbacks {
            reserve: |build: &mut Build<'_>, n| {
                build
                    .result
                    .payload
                    .charge(n)
                    .map_err(QueryEvaluationError::from)
                    .map_err(io::Error::other)
            },
            free: |build, n| build.result.payload.release(n),
            check: |build| {
                build
                    .stop
                    .check()
                    .map_err(QueryEvaluationError::from)
                    .map_err(io::Error::other)
            },
            variables: |build, vars| {
                build.result.variables = vars.to_vec();
                Ok(())
            },
            row: |build, row| {
                // The parser has already admitted this Vec and its recursive heap
                // against payload. Own it even if outer-capacity admission fails.
                build
                    .result
                    .rows
                    .reserve(1)
                    .map_err(QueryEvaluationError::from)
                    .map_err(io::Error::other)?;
                build
                    .result
                    .rows
                    .push(row)
                    .map_err(QueryEvaluationError::from)
                    .map_err(io::Error::other)
            },
        };
        let result = QueryResultsParser::from_format(format)
            .read_solutions_with(bytes, max_rows, &mut build, &hooks);
        stop.check().map_err(QueryEvaluationError::from)?;
        result.map_err(parse_error)?;
        Ok(build.result)
    }

    // The existing actual controlled collector, retained for unported formats.
    // Tests can compare this same function for JSON, rather than a toy decoder.
    pub(super) fn from_slice_copying(
        format: QueryResultsFormat,
        bytes: &[u8],
        max_rows: usize,
        budget: &Arc<Budget>,
        stop: &dyn StopCheck,
    ) -> Result<Self, QueryEvaluationError> {
        stop.check().map_err(QueryEvaluationError::from)?;
        let parsed = QueryResultsParser::from_format(format)
            .for_slice(bytes)
            .map_err(|e| QueryEvaluationError::Service(Box::new(e)));
        stop.check().map_err(QueryEvaluationError::from)?;
        match parsed? {
            SliceQueryResultsParserOutput::Boolean(_) => Err(QueryEvaluationError::Service(
                "the endpoint answered a boolean".into(),
            )),
            SliceQueryResultsParserOutput::Solutions(mut solutions) => {
                let mut result = Self::new(solutions.variables().to_vec(), budget);
                loop {
                    stop.check().map_err(QueryEvaluationError::from)?;
                    let next = solutions.next();
                    stop.check().map_err(QueryEvaluationError::from)?;
                    let Some(solution) = next else {
                        break;
                    };
                    if result.rows().len() == max_rows {
                        return Err(QueryEvaluationError::Service(
                            format!("more than {max_rows} rows (federation.max_rows)").into(),
                        ));
                    }
                    let solution =
                        solution.map_err(|e| QueryEvaluationError::Service(Box::new(e)))?;
                    result.push_row(solution.values(), stop)?;
                }
                stop.check().map_err(QueryEvaluationError::from)?;
                Ok(result)
            }
        }
    }
}

fn parse_error(error: QueryResultsParseError) -> QueryEvaluationError {
    match error {
        QueryResultsParseError::Io(error) => match error.downcast::<QueryEvaluationError>() {
            Ok(error) => error,
            Err(error) => QueryEvaluationError::Service(Box::new(error)),
        },
        error => QueryEvaluationError::Service(Box::new(error)),
    }
}
