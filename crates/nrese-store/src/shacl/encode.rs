//! JSON copy-out borrows the decoded report; only the encoded bytes grow.

use std::io::{self, Write};

use nrese_exec::{ChargedVec, Stop, Ticker};
use nrese_rdf::Term;
use nrese_shacl::{PropertyPath, ValidationResult};
use nrese_sparql::{QueryEvaluationError, Resources};
use serde::ser::{Serialize, SerializeSeq, SerializeStruct, Serializer};

use super::ShaclValidation;
use crate::error::{StoreError, StoreResult};

fn stopped(stop: Stop) -> StoreError {
    StoreError::SparqlEvaluation(match stop {
        Stop::Cancelled => QueryEvaluationError::Cancelled,
        Stop::Memory(error) => QueryEvaluationError::MemoryLimit(error),
        other => QueryEvaluationError::Unexpected(other.to_string()),
    })
}

struct Output {
    bytes: ChargedVec<u8>,
    ticker: Ticker,
    // serde_json wraps Write failures; keep the original operational cause typed.
    failure: Option<StoreError>,
}

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = self
            .ticker
            .tick(bytes.len().min(u32::MAX as usize) as u32)
            .map_err(stopped)
            .and_then(|()| {
                self.bytes
                    .extend_from_slice(bytes)
                    .map_err(StoreError::from)
            });
        match result {
            Ok(()) => Ok(bytes.len()),
            Err(error) => {
                self.failure = Some(error);
                Err(io::Error::other("SHACL JSON output stopped"))
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn json(
    validation: &ShaclValidation,
    resources: &Resources,
) -> StoreResult<ChargedVec<u8>> {
    let mut out = Output {
        bytes: ChargedVec::new(std::sync::Arc::new(resources.memory.part())),
        ticker: resources.stop().ticker().map_err(stopped)?,
        failure: None,
    };
    if let Err(error) = serde_json::to_writer(&mut out, &Report(validation)) {
        return Err(out
            .failure
            .take()
            .unwrap_or_else(|| StoreError::Io(io::Error::other(error))));
    }
    out.ticker.finish().map_err(stopped)?;
    Ok(out.bytes)
}

struct Report<'a>(&'a ShaclValidation);
impl Serialize for Report<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let validation = self.0;
        let mut report = serializer.serialize_struct("ShaclReport", 5)?;
        // Preserve the existing object fields and their sorted-map order.
        report.serialize_field("conforms", &validation.report.conforms())?;
        report.serialize_field("failures", validation.report.failures())?;
        report.serialize_field("results", &Results(validation.report.results()))?;
        report.serialize_field("revision", &validation.revision)?;
        report.serialize_field("shapes", &validation.shapes)?;
        report.end()
    }
}

struct Results<'a>(&'a [ValidationResult]);
impl Serialize for Results<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut results = serializer.serialize_seq(Some(self.0.len()))?;
        for result in self.0 {
            results.serialize_element(&ResultText(result))?;
        }
        results.end()
    }
}

struct ResultText<'a>(&'a ValidationResult);
impl Serialize for ResultText<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let value = self.0;
        let mut result = serializer.serialize_struct("ShaclResult", 7)?;
        result.serialize_field("focusNode", &TermText(&value.focus_node))?;
        result.serialize_field("resultMessage", &Messages(&value.messages))?;
        result.serialize_field("resultPath", &value.path.as_ref().map(PathText))?;
        result.serialize_field("resultSeverity", value.severity.as_str())?;
        result.serialize_field("sourceConstraintComponent", value.component.as_str())?;
        result.serialize_field("sourceShape", &TermText(&value.source_shape))?;
        result.serialize_field("value", &value.value.as_ref().map(TermText))?;
        result.end()
    }
}

struct TermText<'a>(&'a Term);
impl Serialize for TermText<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Term::NamedNode(node) => serializer.serialize_str(node.as_str()),
            // serde_json 1.0.149 formats directly through its escaped Write adapter.
            other => serializer.collect_str(other),
        }
    }
}

struct PathText<'a>(&'a PropertyPath);
impl Serialize for PathText<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            PropertyPath::Predicate(predicate) => serializer.serialize_str(predicate.as_str()),
            other => serializer.collect_str(other),
        }
    }
}

struct Messages<'a>(&'a [nrese_rdf::Literal]);
impl Serialize for Messages<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut messages = serializer.serialize_seq(Some(self.0.len()))?;
        for message in self.0 {
            messages.serialize_element(message.value())?;
        }
        messages.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrese_exec::Envelope;
    use nrese_shacl::ValidationReport;
    use nrese_sparql::CancellationToken;

    /// Encoding keeps the existing empty-report wire shape, holds output capacity,
    /// and returns the original typed refusal/stop while freeing partial output.
    #[test]
    fn json_output_retains_capacity_and_preserves_typed_stops() {
        let memory = Envelope::new(Some(1024), None);
        let token = CancellationToken::new();
        let mut validation = ShaclValidation {
            report: ValidationReport::default(),
            revision: 7,
            shapes: 0,
            applicable: false,
            resources: Resources {
                memory: memory.clone(),
                cancellation: Some(token.clone()),
                ..Default::default()
            },
        };
        let output = validation.serialize_json().unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output).unwrap(),
            serde_json::json!({"conforms":true,"failures":[],"results":[],"revision":7,"shapes":0})
        );
        assert!(memory.used() >= output.len());
        assert!(output.charged() > 0);
        drop(output);
        assert_eq!(memory.used(), 0);
        token.cancel();
        assert!(matches!(
            validation.serialize_json(),
            Err(StoreError::SparqlEvaluation(
                QueryEvaluationError::Cancelled
            ))
        ));
        assert_eq!(memory.used(), 0);

        let tiny = Envelope::new(Some(8), None);
        validation.resources.memory = tiny.clone();
        validation.resources.cancellation = None;
        assert!(matches!(
            validation.serialize_json(),
            Err(StoreError::SparqlEvaluation(
                QueryEvaluationError::MemoryLimit(_)
            ))
        ));
        assert_eq!(tiny.used(), 0);
    }
}
