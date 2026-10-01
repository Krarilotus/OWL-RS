//! SHACL validation as a store operation (design `docs/design/shacl.md`, slice C1b).
//!
//! Shapes live in a graph of the repository ([`StoreConfig::shapes_graph`] unless the
//! request names another), managed like any graph. A request can also bring its own
//! shapes: they are validated against without being stored.
//!
//! [`StoreConfig::shapes_graph`]: crate::StoreConfig::shapes_graph

use nrese_engine::{GraphSelector, ReadModel, TermId};
use nrese_rdf::{GraphName, NamedNode, NamedNodeRef, Term};
use nrese_shacl::{
    PropertyPath, Selection, ShapeError, Shapes, ValidationReport, compile, validate,
};
use nrese_sparql::ReadView;

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

fn shapes_error(errors: Vec<ShapeError>) -> StoreError {
    StoreError::ShaclShapes(errors.iter().map(ToString::to_string).collect())
}

/// Validates `view` against the shapes in the graph `shapes` (none, if the graph holds
/// nothing), leaving `also_excluded` out of the data as well.
fn validate_view<V: ReadView>(
    view: &V,
    shapes: Option<TermId>,
    also_excluded: Option<TermId>,
    request: &ShaclValidationRequest,
) -> StoreResult<(ValidationReport, usize)> {
    let Some(shapes_graph) = shapes else {
        return Ok((ValidationReport::default(), 0));
    };
    let shapes: Shapes = compile(
        view,
        Selection::asserted(GraphSelector::Exact(shapes_graph)),
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
    Ok((validate(view, &shapes, data), shapes.len()))
}

impl StoreService {
    /// Validates the repository's data against SHACL shapes. Ill-formed shapes are a
    /// request error ([`StoreError::ShaclShapes`]).
    pub fn validate_shacl(&self, request: &ShaclValidationRequest) -> StoreResult<ShaclValidation> {
        let stored = graph_iri(&self.config().shapes_graph)?;
        match &request.shapes {
            ShapesSource::Stored | ShapesSource::Graph(_) => {
                let shapes_graph = match &request.shapes {
                    ShapesSource::Graph(iri) => graph_iri(iri)?,
                    _ => stored.clone(),
                };
                let snapshot = self.engine().snapshot();
                let (report, shapes) = validate_view(
                    &snapshot,
                    snapshot.lookup(shapes_graph.as_ref().into()),
                    snapshot.lookup(stored.as_ref().into()),
                    request,
                )?;
                Ok(ShaclValidation {
                    report,
                    revision: snapshot.revision(),
                    shapes,
                })
            }
            ShapesSource::Payload {
                format,
                base_iri,
                payload,
            } => {
                // The shapes go into a graph of an open transaction that is never
                // committed: they share the repository's dictionary, as stored shapes do,
                // and leave no trace. The transaction holds the writer while it validates.
                let scratch = NamedNodeRef::new_unchecked(REQUEST_SHAPES_GRAPH);
                let quads = parse_graph(
                    *format,
                    base_iri.as_deref(),
                    payload.as_slice(),
                    GraphName::NamedNode(scratch.into_owned()),
                    BlankNodes::Fresh,
                )?;
                let mut tx = self.engine().transaction();
                let revision = tx.base().revision();
                for quad in &quads {
                    tx.insert(quad.as_ref());
                }
                let (report, shapes) = validate_view(
                    &tx,
                    tx.lookup(scratch.into()),
                    tx.lookup(stored.as_ref().into()),
                    request,
                )?;
                Ok(ShaclValidation {
                    report,
                    revision,
                    shapes,
                })
            }
        }
    }
}
