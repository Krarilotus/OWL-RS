//! SHACL validation as a store operation (design `docs/design/shacl.md`, slice C1b).
//!
//! Shapes live in a graph of the repository ([`StoreConfig::shapes_graph`] unless the
//! request names another), managed like any graph. A request can also bring its own
//! shapes: they are validated against without being stored.
//!
//! [`StoreConfig::shapes_graph`]: crate::StoreConfig::shapes_graph

use nrese_engine::{EncodedQuad, GraphSelector, ReadModel, TermId, Transaction};
use nrese_rdf::{GraphName, NamedNode, NamedNodeRef, Term};
use nrese_shacl::{
    CompileError, PropertyPath, Selection, Shapes, ValidationReport, compile, validate,
    validate_changes,
};
use nrese_sparql::{ReadView, Resources};

use crate::error::{StoreError, StoreResult};
use crate::query::GraphResultFormat;
use crate::rdf_io::{BlankNodes, parse_graph, serialize_triples};
use crate::service::StoreService;

/// Where a request's shapes stay while it runs. They are never committed.
const REQUEST_SHAPES_GRAPH: &str = "https://nrese.dev/ns/graph#shacl-request-shapes";

/// The shapes to validate against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShapesSource {
    /// The repository's shapes graph ([`crate::StoreConfig::shapes_graph`]).
    Stored,
    /// Another graph of the repository.
    Graph(String),
    /// Shapes sent with the request.
    Payload {
        format: GraphResultFormat,
        base_iri: Option<String>,
        payload: Vec<u8>,
    },
}

/// The statements to validate.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ValidatedGraphs {
    /// Every graph, except the graphs that hold shapes.
    #[default]
    AllData,
    Default,
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaclValidationRequest {
    pub shapes: ShapesSource,
    pub graphs: ValidatedGraphs,
    /// Asserted and inferred statements (the default), or one of them.
    pub read_model: ReadModel,
}

impl Default for ShaclValidationRequest {
    fn default() -> Self {
        Self {
            shapes: ShapesSource::Stored,
            graphs: ValidatedGraphs::AllData,
            read_model: ReadModel::Materialised,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaclValidation {
    pub report: ValidationReport,
    /// The revision that was validated.
    pub revision: u64,
    /// The number of shapes, nested ones included; 0 means there was nothing to validate
    /// against.
    pub shapes: usize,
    /// Whether anything was checked: some active targeted shape had a focus node and a
    /// constraint ([`nrese_shacl::applicable`]). A conforming report without it is vacuous.
    pub applicable: bool,
}

/// One validation result in plain text, for JSON reports: IRIs as they are, other terms
/// in N-Triples form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaclResultText {
    pub focus_node: String,
    /// A predicate IRI, or a SPARQL property path.
    pub path: Option<String>,
    pub value: Option<String>,
    pub source_shape: String,
    /// The constraint component's IRI.
    pub component: String,
    /// The severity's IRI.
    pub severity: String,
    pub messages: Vec<String>,
}

impl ShaclValidation {
    /// The report as an RDF graph in `format`.
    pub fn serialize(&self, format: GraphResultFormat) -> StoreResult<Vec<u8>> {
        serialize_triples(format, self.report.to_triples())
    }

    /// The results in plain text.
    pub fn results_text(&self) -> Vec<ShaclResultText> {
        let text = |term: &Term| match term {
            Term::NamedNode(node) => node.as_str().to_owned(),
            other => other.to_string(),
        };
        self.report
            .results
            .iter()
            .map(|result| ShaclResultText {
                focus_node: text(&result.focus_node),
                path: result.path.as_ref().map(|path| match path {
                    PropertyPath::Predicate(predicate) => predicate.as_str().to_owned(),
                    other => other.to_string(),
                }),
                value: result.value.as_ref().map(text),
                source_shape: text(&result.source_shape),
                component: result.component.as_str().to_owned(),
                severity: result.severity.as_str().to_owned(),
                messages: result
                    .messages
                    .iter()
                    .map(|message| message.value().to_owned())
                    .collect(),
            })
            .collect()
    }
}

fn graph_iri(iri: &str) -> StoreResult<NamedNode> {
    NamedNode::new(iri).map_err(|_| StoreError::InvalidGraphIri(iri.to_owned()))
}

fn shapes_error(error: CompileError) -> StoreError {
    match error {
        CompileError::Shapes(errors) => StoreError::ShaclShapes(errors),
        CompileError::Evaluation(error) => StoreError::SparqlEvaluation(error),
    }
}

/// Validates `view` against the shapes in the graph `shapes` (none, if the graph holds
/// nothing), leaving `also_excluded` out of the data as well.
fn validate_view<V: ReadView + Sync>(
    view: &V,
    shapes: Option<TermId>,
    also_excluded: Option<TermId>,
    request: &ShaclValidationRequest,
    resources: &Resources,
) -> StoreResult<(ValidationReport, usize, bool)> {
    let Some(shapes_graph) = shapes else {
        return Ok((ValidationReport::default(), 0, false));
    };
    let shapes: Shapes = compile(
        view,
        Selection::asserted(GraphSelector::Exact(shapes_graph)),
        resources,
    )
    .map_err(shapes_error)?;
    let mut data = match &request.graphs {
        ValidatedGraphs::AllData => {
            let data = Selection::of(GraphSelector::Any).excluding(shapes_graph);
            also_excluded.map_or(data, |graph| data.excluding(graph))
        }
        ValidatedGraphs::Default => Selection::of(GraphSelector::Exact(TermId::DEFAULT_GRAPH)),
        ValidatedGraphs::Named(iri) => match view.lookup(graph_iri(iri)?.as_ref().into()) {
            Some(graph) => Selection::of(GraphSelector::Exact(graph)),
            // A graph nobody has written holds nothing. The selection is then empty (a
            // graph, minus itself), which still checks `sh:targetNode` shapes.
            None => Selection::of(GraphSelector::Exact(shapes_graph)).excluding(shapes_graph),
        },
    };
    data.model = request.read_model;
    let applicable = nrese_shacl::applicable(view, &shapes, data);
    Ok((validate(view, &shapes, data), shapes.len(), applicable))
}

/// Fails if the commit in `tx` changes the graph `shapes_graph` and leaves shapes in it that
/// don't compile: the shapes graph is a managed object, well-formed whatever wrote it and
/// whether or not the SHACL gate is on ([`StoreError::ShaclShapes`], one message per
/// problem).
pub(crate) fn check_shapes(
    tx: &Transaction<'_>,
    shapes_graph: &str,
    resources: &Resources,
) -> StoreResult<()> {
    let iri = graph_iri(shapes_graph)?;
    let Some(shapes_id) = tx.lookup(iri.as_ref().into()) else {
        return Ok(());
    };
    if !tx
        .inserted()
        .chain(tx.deleted())
        .any(|quad| quad.graph == shapes_id)
    {
        return Ok(());
    }
    compile(
        &tx.pending_snapshot(),
        Selection::asserted(GraphSelector::Exact(shapes_id)),
        resources,
    )
    .map(drop)
    .map_err(shapes_error)
}

/// What the commit in `tx` introduces against the shapes in the graph `shapes_graph` (the
/// commit gate, C2): the results present after it and not before, over every data graph.
/// If the commit changes the shapes, both states are validated in full; otherwise only the
/// focus nodes the changes can affect ([`validate_changes`]). `None` without shapes.
pub(crate) fn introduced(
    tx: &Transaction<'_>,
    shapes_graph: &str,
    resources: &Resources,
) -> StoreResult<Option<ValidationReport>> {
    let iri = graph_iri(shapes_graph)?;
    let Some(shapes_id) = tx.lookup(iri.as_ref().into()) else {
        return Ok(None);
    };
    let before = tx.base();
    let after = tx.pending_snapshot();
    let data = Selection::of(GraphSelector::Any).excluding(shapes_id);
    let shapes_selection = Selection::asserted(GraphSelector::Exact(shapes_id));
    let shapes_changed = tx
        .inserted()
        .chain(tx.deleted())
        .any(|quad| quad.graph == shapes_id);
    if shapes_changed {
        let shapes_after = compile(&after, shapes_selection, resources).map_err(shapes_error)?;
        let mut report = validate(&after, &shapes_after, data);
        // Shapes that didn't compile before had no results.
        match compile(before, shapes_selection, resources) {
            Ok(shapes_before) => {
                let earlier = validate(before, &shapes_before, data).results;
                report.results.retain(|result| !earlier.contains(result));
            }
            Err(CompileError::Shapes(_)) => {}
            Err(error @ CompileError::Evaluation(_)) => return Err(shapes_error(error)),
        }
        return Ok(Some(report));
    }
    let shapes = compile(before, shapes_selection, resources).map_err(shapes_error)?;
    if shapes.is_empty() {
        return Ok(None);
    }
    let changed: Vec<EncodedQuad> = tx
        .inserted()
        .chain(tx.deleted())
        .chain(
            tx.inferred_inserted()
                .chain(tx.inferred_deleted())
                .map(nrese_engine::EncodedTriple::in_default_graph),
        )
        .collect();
    Ok(Some(validate_changes(
        before, &after, &shapes, data, &changed,
    )))
}

impl StoreService {
    /// Validates the repository's data against SHACL shapes; for a scope that reads every
    /// graph. Ill-formed shapes are a request error ([`StoreError::ShaclShapes`]).
    pub fn validate_shacl(
        &self,
        scope: &crate::ReadScope,
        request: &ShaclValidationRequest,
    ) -> StoreResult<ShaclValidation> {
        scope.require_all("validation")?;
        // Compile slice only; approved transport token threading follows separately.
        let resources = Resources {
            memory: self.envelope(None),
            ..Default::default()
        };
        let stored = graph_iri(&self.config().shapes_graph)?;
        match &request.shapes {
            ShapesSource::Stored | ShapesSource::Graph(_) => {
                let shapes_graph = match &request.shapes {
                    ShapesSource::Graph(iri) => graph_iri(iri)?,
                    _ => stored.clone(),
                };
                let snapshot = self.engine().snapshot();
                let (report, shapes, applicable) = validate_view(
                    &snapshot,
                    snapshot.lookup(shapes_graph.as_ref().into()),
                    snapshot.lookup(stored.as_ref().into()),
                    request,
                    &resources,
                )?;
                Ok(ShaclValidation {
                    report,
                    revision: snapshot.revision(),
                    shapes,
                    applicable,
                })
            }
            ShapesSource::Payload {
                format,
                base_iri,
                payload,
            } => {
                // The shapes go into a graph of a speculative transaction, never committed:
                // they share the repository's dictionary, as stored shapes do, and leave no
                // trace. It takes no writer slot, so commits go on meanwhile.
                let scratch = NamedNodeRef::new_unchecked(REQUEST_SHAPES_GRAPH);
                let quads = parse_graph(
                    *format,
                    base_iri.as_deref(),
                    payload.as_slice(),
                    GraphName::NamedNode(scratch.into_owned()),
                    BlankNodes::Fresh,
                )?;
                let mut tx = self.engine().speculative();
                let revision = tx.base().revision();
                for quad in &quads {
                    tx.insert(quad.as_ref());
                }
                let (report, shapes, applicable) = validate_view(
                    &tx,
                    tx.lookup(scratch.into()),
                    tx.lookup(stored.as_ref().into()),
                    request,
                    &resources,
                )?;
                Ok(ShaclValidation {
                    report,
                    revision,
                    shapes,
                    applicable,
                })
            }
        }
    }
}

#[cfg(test)]
mod compile_resource_tests {
    use super::*;

    #[test]
    fn diagnostics_conversion_keeps_payload_and_reservation() {
        let engine = nrese_engine::Engine::new(nrese_engine::EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in parse_graph(GraphResultFormat::Turtle, None,
            b"@prefix sh: <http://www.w3.org/ns/shacl#> . <http://example.com/S> sh:targetNode <http://example.com/a> ; sh:minCount \"many\" .".as_slice(),
            GraphName::DefaultGraph, BlankNodes::Fresh).unwrap() {
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let memory = nrese_exec::Envelope::new(Some(64 * 1024), None);
        let resources = Resources {
            memory: memory.clone(),
            ..Default::default()
        };
        let compiled = compile(
            &engine.snapshot(),
            Selection::asserted(GraphSelector::Exact(TermId::DEFAULT_GRAPH)),
            &resources,
        )
        .unwrap_err();
        let before = memory.used();
        assert!(before > 0);
        let error = shapes_error(compiled);
        drop(resources);
        assert_eq!(memory.used(), before);
        assert!(error.is_request_error());
        assert_eq!(
            error.to_string(),
            "ill-formed SHACL shapes: shape <http://example.com/S>: sh:minCount isn't a non-negative integer"
        );
        let StoreError::ShaclShapes(errors) = &error else {
            panic!("classification");
        };
        assert_eq!(errors.errors().len(), 1);
        drop(error);
        assert_eq!(memory.used(), 0);
    }
}
