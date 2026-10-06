//! Draft checks: one SHACL, query or reasoning check over pinned inputs, in a throwaway
//! in-memory store. Nothing reads or writes a stored repository.
//!
//! The protocol is the Datamodel Workflow's backend-agnostic store check
//! ([`DRAFT_CHECK_PROTOCOL`]); any store can implement it. Inputs are N-Triples, as DMW
//! pins them. The semantics follow DMW's own offline checks, so a store answer and an
//! offline answer mean the same:
//!
//! - **SHACL**: data and schema in the default graph, asserted statements only, SHACL
//!   Core. SHACL-SPARQL, SHACL-AF and `sh:entailment` are reported unsupported rather
//!   than evaluated. The shapes are checked for well-formedness by the compiler.
//! - **Query**: the data alone, asserted statements only; `ASK` and `SELECT`. More rows
//!   than the limit is a failure, never a truncated answer.
//! - **Reasoning**: the OWL 2 RL rules decide consistency over data and schema; the
//!   OWL 2 DL classification lists unsatisfiable classes. Whatever the rules skipped,
//!   and whatever left the classification incomplete, is reported unsupported, and the
//!   answer is then not complete.
//!
//! The caller enforces the time limit and owns the cancellation token.

use std::collections::{BTreeMap, BTreeSet};

use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use serde::{Deserialize, Serialize};

use crate::query::{GraphResultFormat, QueryResultKind, SparqlQueryRequest};
use crate::{
    CancellationToken, GraphTarget, GraphWriteRequest, ReadModel, ReadScope, ShaclValidation,
    ShaclValidationRequest, ShapesSource, StoreConfig, StoreError, StoreService, ValidatedGraphs,
    parse_payload,
};

/// The protocol a draft-check request and reply follow.
pub const DRAFT_CHECK_PROTOCOL: &str = "dmw-store-check/1";
/// What this store's draft checks mean: the module documentation above. A change of
/// meaning gets a new identifier, so old evidence stays interpretable.
pub const DRAFT_CHECK_SEMANTICS: &str = "nrese:draft-check/1";
/// The profiles a draft check accepts.
pub const DRAFT_CHECK_PROFILES: &[&str] = &["owl2-rl"];

/// SHACL features outside SHACL Core, reported unsupported (predicate, feature).
const UNSUPPORTED_SHACL: &[(&str, &str)] = &[
    (
        "http://www.w3.org/ns/shacl#sparql",
        "SHACL-SPARQL constraint (sh:sparql)",
    ),
    (
        "http://www.w3.org/ns/shacl#select",
        "SHACL-SPARQL validator (sh:select)",
    ),
    (
        "http://www.w3.org/ns/shacl#ask",
        "SHACL-SPARQL validator (sh:ask)",
    ),
    (
        "http://www.w3.org/ns/shacl#construct",
        "SHACL-AF rule (sh:construct)",
    ),
    ("http://www.w3.org/ns/shacl#js", "SHACL-JS (sh:js)"),
    (
        "http://www.w3.org/ns/shacl#target",
        "SHACL-AF target (sh:target)",
    ),
    ("http://www.w3.org/ns/shacl#rule", "SHACL-AF rule (sh:rule)"),
    (
        "http://www.w3.org/ns/shacl#expression",
        "SHACL-AF expression (sh:expression)",
    ),
    ("http://www.w3.org/ns/shacl#entailment", "sh:entailment"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DraftOperation {
    Shacl,
    Query,
    Reasoning,
}

/// The pinned inputs, as N-Triples (the query as SPARQL text).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftInputs {
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub shapes: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftLimits {
    pub max_seconds: f64,
    pub max_results: u64,
    pub max_triples: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DraftStatus {
    Completed,
    Failed,
    Timeout,
    Unsupported,
}

/// An answer term: `uri`, `literal` or `bnode`, as DMW records terms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftTerm {
    pub kind: String,
    pub value: String,
    pub datatype: Option<String>,
    pub language: Option<String>,
}

/// One check's terminal outcome. Fields that don't belong to the operation stay empty.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DraftOutcome {
    pub terminal_status: DraftStatus,
    /// The answer covers everything asked: nothing skipped, nothing unsupported.
    pub complete: bool,
    pub unsupported: Vec<String>,
    pub detail: String,
    pub observed_ask: Option<bool>,
    pub observed_rows: Option<Vec<BTreeMap<String, DraftTerm>>>,
    pub shacl_conforms: Option<bool>,
    pub shacl_applicable: Option<bool>,
    pub shacl_meta_validated: Option<bool>,
    pub logical_consistent: Option<bool>,
    pub unsatisfiable_classes: Vec<String>,
}

impl DraftOutcome {
    fn terminal(terminal_status: DraftStatus, detail: impl Into<String>) -> Self {
        Self {
            terminal_status,
            complete: false,
            unsupported: Vec::new(),
            detail: detail.into(),
            observed_ask: None,
            observed_rows: None,
            shacl_conforms: None,
            shacl_applicable: None,
            shacl_meta_validated: None,
            logical_consistent: None,
            unsatisfiable_classes: Vec::new(),
        }
    }

    /// An outcome for a check that didn't run: the request asked for something the
    /// server can't answer as asked.
    pub fn refused(terminal_status: DraftStatus, detail: impl Into<String>) -> Self {
        Self::terminal(terminal_status, detail)
    }

    fn completed(detail: impl Into<String>) -> Self {
        Self {
            complete: true,
            ..Self::terminal(DraftStatus::Completed, detail)
        }
    }

    fn unsupported(features: Vec<String>) -> Self {
        Self {
            unsupported: features,
            ..Self::terminal(
                DraftStatus::Unsupported,
                "the inputs use unsupported features",
            )
        }
    }
}

/// Runs one draft check. Every problem becomes a terminal outcome; this never panics on
/// input and never touches a stored repository.
pub fn run_draft_check(
    operation: DraftOperation,
    inputs: &DraftInputs,
    limits: &DraftLimits,
    cancellation: &CancellationToken,
    memory_limit: Option<usize>,
) -> DraftOutcome {
    let checked = match operation {
        DraftOperation::Shacl => check_shacl(inputs, limits),
        DraftOperation::Query => check_query(inputs, limits, cancellation, memory_limit),
        DraftOperation::Reasoning => check_reasoning(inputs, limits, cancellation),
    };
    checked.unwrap_or_else(|outcome| outcome)
}

type Checked = Result<DraftOutcome, DraftOutcome>;

fn failed(detail: impl Into<String>) -> DraftOutcome {
    DraftOutcome::terminal(DraftStatus::Failed, detail)
}

fn cancelled_or(cancellation: &CancellationToken, detail: impl Into<String>) -> DraftOutcome {
    if cancellation.is_cancelled() {
        DraftOutcome::terminal(DraftStatus::Timeout, "the check exceeded its time limit")
    } else {
        failed(detail)
    }
}

/// A throwaway store holding `graphs` in its default graph, within the triple limit.
fn store_with(graphs: &[Option<&str>], limits: &DraftLimits) -> Result<StoreService, DraftOutcome> {
    let statements: u64 = graphs
        .iter()
        .flatten()
        .map(|text| {
            text.lines()
                .filter(|line| {
                    let line = line.trim_start();
                    !line.is_empty() && !line.starts_with('#')
                })
                .count() as u64
        })
        .sum();
    if statements > limits.max_triples {
        return Err(failed(format!(
            "the inputs hold {statements} statements, more than max_triples {}",
            limits.max_triples
        )));
    }
    let store = StoreService::new(StoreConfig::in_memory())
        .map_err(|error| failed(format!("no scratch store: {error}")))?;
    for text in graphs.iter().flatten() {
        store
            .execute_graph_write(&GraphWriteRequest {
                target: GraphTarget::DefaultGraph,
                format: GraphResultFormat::NTriples,
                base_iri: None,
                payload: text.as_bytes().to_vec(),
                replace: false,
            })
            .map_err(|error| failed(format!("an input is not valid N-Triples: {error}")))?;
    }
    Ok(store)
}

fn check_shacl(inputs: &DraftInputs, limits: &DraftLimits) -> Checked {
    let shapes = inputs
        .shapes
        .as_deref()
        .ok_or_else(|| failed("a SHACL check needs shapes"))?;
    let quads = parse_payload(GraphResultFormat::NTriples, None, shapes.as_bytes())
        .map_err(|error| failed(format!("the shapes are not valid N-Triples: {error}")))?;
    let features: BTreeSet<&str> = quads
        .iter()
        .filter_map(|quad| {
            UNSUPPORTED_SHACL
                .iter()
                .find(|(predicate, _)| quad.predicate.as_str() == *predicate)
                .map(|(_, feature)| *feature)
        })
        .collect();
    if !features.is_empty() {
        return Err(DraftOutcome::unsupported(
            features.into_iter().map(str::to_owned).collect(),
        ));
    }
    let store = store_with(&[inputs.data.as_deref(), inputs.schema.as_deref()], limits)?;
    let request = ShaclValidationRequest {
        shapes: ShapesSource::Payload {
            format: GraphResultFormat::NTriples,
            base_iri: None,
            payload: shapes.as_bytes().to_vec(),
        },
        graphs: ValidatedGraphs::Default,
        read_model: ReadModel::Asserted,
    };
    let ShaclValidation {
        report, applicable, ..
    } = match store.validate_shacl(&ReadScope::All, &request) {
        Ok(validation) => validation,
        Err(StoreError::ShaclShapes(problems)) => {
            return Err(failed(format!(
                "invalid SHACL graph: {}",
                problems.join("; ")
            )));
        }
        Err(error) => return Err(failed(format!("SHACL validation failed: {error}"))),
    };
    if !report.failures.is_empty() {
        return Err(failed(format!(
            "SHACL validation reached no verdict: {}",
            report.failures.join("; ")
        )));
    }
    Ok(DraftOutcome {
        shacl_conforms: Some(report.results.is_empty()),
        shacl_applicable: Some(applicable),
        shacl_meta_validated: Some(true),
        ..DraftOutcome::completed(format!("{} validation results", report.results.len()))
    })
}

fn check_query(
    inputs: &DraftInputs,
    limits: &DraftLimits,
    cancellation: &CancellationToken,
    memory_limit: Option<usize>,
) -> Checked {
    let text = inputs
        .query
        .as_deref()
        .ok_or_else(|| failed("a query check needs a query"))?;
    let store = store_with(&[inputs.data.as_deref()], limits)?;
    let mut request = SparqlQueryRequest::all(text);
    request.read_model = Some(ReadModel::Asserted);
    request.memory_limit = memory_limit;
    let prepared = store
        .prepare_query(&request)
        .map_err(|error| failed(format!("the query does not parse: {error}")))?;
    if prepared.kind() == QueryResultKind::Graph {
        return Err(DraftOutcome::unsupported(vec![
            "CONSTRUCT and DESCRIBE queries".to_owned(),
        ]));
    }
    let mut payload = Vec::new();
    store
        .run_query(&prepared, cancellation, &mut payload)
        .map_err(|error| cancelled_or(cancellation, format!("the query failed: {error}")))?;
    if cancellation.is_cancelled() {
        return Err(cancelled_or(cancellation, ""));
    }
    let results: serde_json::Value = serde_json::from_slice(&payload)
        .map_err(|error| failed(format!("unreadable query results: {error}")))?;
    if let Some(answer) = results.get("boolean").and_then(serde_json::Value::as_bool) {
        return Ok(DraftOutcome {
            observed_ask: Some(answer),
            ..DraftOutcome::completed("ASK answered")
        });
    }
    let bindings = results
        .pointer("/results/bindings")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| failed("query results without bindings"))?;
    if bindings.len() as u64 > limits.max_results {
        return Err(failed(format!(
            "SELECT result exceeds {} rows",
            limits.max_results
        )));
    }
    let rows = bindings
        .iter()
        .map(solution)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DraftOutcome {
        observed_rows: Some(rows),
        ..DraftOutcome::completed(format!("{} rows", bindings.len()))
    })
}

/// One SPARQL JSON solution as DMW terms; triple terms have no DMW form yet.
fn solution(binding: &serde_json::Value) -> Result<BTreeMap<String, DraftTerm>, DraftOutcome> {
    let object = binding
        .as_object()
        .ok_or_else(|| failed("a query solution is not an object"))?;
    object
        .iter()
        .map(|(variable, term)| {
            let field = |name: &str| term.get(name).and_then(serde_json::Value::as_str);
            let value = field("value").unwrap_or_default().to_owned();
            let kind = match field("type") {
                Some("uri") => "uri",
                Some("literal" | "typed-literal") => "literal",
                Some("bnode") => "bnode",
                Some(other) => {
                    return Err(DraftOutcome::unsupported(vec![format!(
                        "query answers with {other} terms"
                    )]));
                }
                None => return Err(failed("a query answer term has no type")),
            };
            Ok((
                variable.clone(),
                DraftTerm {
                    kind: kind.to_owned(),
                    value,
                    datatype: field("datatype").map(str::to_owned),
                    language: field("xml:lang").map(str::to_owned),
                },
            ))
        })
        .collect()
}

fn check_reasoning(
    inputs: &DraftInputs,
    limits: &DraftLimits,
    cancellation: &CancellationToken,
) -> Checked {
    let store = store_with(&[inputs.data.as_deref(), inputs.schema.as_deref()], limits)?;
    let Some(program) = ReasonerConfig::for_mode(ReasoningMode::Owl2Rl).materialised_program()
    else {
        return Err(failed("no OWL 2 RL rule program"));
    };
    let stop = || cancellation.is_cancelled();
    let report = store
        .rematerialise_until(program, &stop)
        .map_err(|error| cancelled_or(cancellation, format!("reasoning failed: {error}")))?;
    let classification = store
        .classify(&ReadScope::All)
        .map_err(|error| failed(format!("classification failed: {error}")))?;
    let mut unsupported: Vec<String> = report
        .diagnostics
        .iter()
        .map(|diagnostic| {
            format!(
                "owl2-rl: {} in the axiom {} {} (rules {})",
                diagnostic.kind, diagnostic.subject, diagnostic.predicate, diagnostic.rules
            )
        })
        .collect();
    if report.diagnostics_total > report.diagnostics.len() {
        unsupported.push(format!(
            "owl2-rl: {} more unusable axioms",
            report.diagnostics_total - report.diagnostics.len()
        ));
    }
    unsupported.extend(
        classification
            .incomplete
            .iter()
            .map(|why| format!("owl2-dl classification: {why}")),
    );
    let consistent = report.violations == 0;
    Ok(DraftOutcome {
        complete: unsupported.is_empty(),
        unsupported,
        logical_consistent: Some(consistent),
        unsatisfiable_classes: classification.unsatisfiable,
        ..DraftOutcome::terminal(
            DraftStatus::Completed,
            format!(
                "{} consistency violations, {} inferred statements",
                report.violations, report.inferred
            ),
        )
    })
}

/// The inputs whose SHA-256 the caller pinned, by the protocol's hash keys.
const HASHED_INPUTS: &[&str] = &["@active_data", "@active_schema", "@active_shapes", "@query"];

/// Why the inputs differ from the hashes the caller pinned, if they do. A pinned hash
/// needs its input and must match its UTF-8 bytes; other hash keys identify inputs the
/// caller checks itself and are only echoed.
pub fn input_hash_mismatch(
    inputs: &DraftInputs,
    hashes: &BTreeMap<String, String>,
) -> Option<String> {
    use sha2::{Digest, Sha256};
    HASHED_INPUTS.iter().find_map(|key| {
        let input = match *key {
            "@active_data" => inputs.data.as_deref(),
            "@active_schema" => inputs.schema.as_deref(),
            "@active_shapes" => inputs.shapes.as_deref(),
            _ => inputs.query.as_deref(),
        };
        match (hashes.get(*key), input) {
            (None, _) => None,
            (Some(_), None) => Some(format!("{key} is pinned but its input is missing")),
            (Some(expected), Some(text)) => {
                let actual = format!("{:x}", Sha256::digest(text.as_bytes()));
                (actual != *expected).then(|| format!("{key} does not match its input"))
            }
        }
    })
}
