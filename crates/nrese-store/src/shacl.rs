//! SHACL validation as a store operation (design `docs/design/shacl.md`, slice C1b).
//!
//! Shapes live in a graph of the repository ([`StoreConfig::shapes_graph`] unless the
//! request names another), managed like any graph. A request can also bring its own
//! shapes: they are validated against without being stored.
//!
//! [`StoreConfig::shapes_graph`]: crate::StoreConfig::shapes_graph

use nrese_engine::{EncodedQuad, GraphSelector, ReadModel, Snapshot, TermId, Transaction};
use nrese_rdf::{GraphName, NamedNode, Term};
use nrese_shacl::{
    PropertyPath, Selection, ShapeError, Shapes, ValidationReport, compile, validate,
    validate_changes, validate_shapes_change,
};
use nrese_sparql::{CancellationToken, ReadView, Resources};

use crate::error::{StoreError, StoreResult};
use crate::query::GraphResultFormat;
use crate::rdf_io::{BlankNodes, parse_graph, serialize_triples};
use crate::service::StoreService;

/// Where a request's shapes stay while it runs (never committed): this graph, or the first
/// of `…-1`, `…-2`, … that holds no statement ([`request_shapes_graph`]).
const REQUEST_SHAPES_GRAPH: &str = "https://nrese.dev/ns/graph#shacl-request-shapes";

/// The graph a request's shapes go into: the first of [`REQUEST_SHAPES_GRAPH`], `…-1`,
/// `…-2`, … in which `base` holds no statement and that isn't `validated`, the graph the
/// request validates by name. The shapes compiled from it are then the request's alone,
/// leaving it out of the data leaves out nothing stored, and no data selection reads the
/// shapes as data. The same store and request give the same graph each time, so repeated
/// requests intern nothing more for it.
fn request_shapes_graph(base: &Snapshot, validated: Option<&NamedNode>) -> NamedNode {
    (0u64..)
        .map(|i| match i {
            0 => NamedNode::new_unchecked(REQUEST_SHAPES_GRAPH),
            i => NamedNode::new_unchecked(format!("{REQUEST_SHAPES_GRAPH}-{i}")),
        })
        .find(|graph| {
            Some(graph) != validated
                && base
                    .lookup(graph.as_ref().into())
                    .is_none_or(|id| !base.contains_named_graph(id))
        })
        .expect("finitely many graphs hold statements")
}

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

fn shapes_error(errors: Vec<ShapeError>) -> StoreError {
    StoreError::ShaclShapes(errors.iter().map(ToString::to_string).collect())
}

/// Validates `view` against the shapes in the graph `shapes` (none, if the graph holds
/// nothing), leaving `also_excluded` out of the data as well, on `resources`.
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
    let report = validate(view, &shapes, data, resources)?;
    Ok((report, shapes.len(), applicable))
}

/// Fails if the commit in `tx` changes the graph `shapes_graph` and leaves shapes in it that
/// don't compile: the shapes graph is a managed object, well-formed whatever wrote it and
/// whether or not the SHACL gate is on ([`StoreError::ShaclShapes`], one message per
/// problem, as `view`'s committer may be told them: [`EvidenceView::shapes_error`]).
///
/// [`EvidenceView::shapes_error`]: crate::mutation::evidence::EvidenceView::shapes_error
pub(crate) fn check_shapes(
    tx: &Transaction<'_>,
    shapes_graph: &str,
    view: &crate::mutation::evidence::EvidenceView<'_, '_>,
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
    )
    .map(drop)
    .map_err(|errors| view.shapes_error(shapes_graph, shapes_error(errors)))
}

/// What the commit in `tx` introduces against the shapes in the graph `shapes_graph` (the
/// commit gate, C2): the results present after it and not before, over every data graph.
/// If the commit changes the shapes, both states are validated in full; otherwise only the
/// focus nodes the changes can affect ([`validate_changes`]). `None` without shapes. The
/// validations run on the write's `resources`; a stop is the error, never a report.
pub(crate) fn introduced(
    tx: &Transaction<'_>,
    shapes_graph: &str,
    resources: &Resources,
) -> StoreResult<Option<ValidationReport>> {
    let iri = graph_iri(shapes_graph)?;
    let Some(shapes_id) = tx.lookup(iri.as_ref().into()) else {
        return Ok(None);
    };
    let changed = || {
        tx.inserted()
            .chain(tx.deleted())
            .chain(
                tx.inferred_inserted()
                    .chain(tx.inferred_deleted())
                    .map(nrese_engine::EncodedTriple::in_default_graph),
            )
            .collect()
    };
    introduced_in(
        tx,
        tx.base(),
        &tx.pending_snapshot(),
        shapes_id,
        changed,
        |_| true,
        resources,
    )
}

/// [`introduced`] over the views `before` and `after` of the commit in `tx`, with the
/// `changed` quads of the views; `None` also for shapes `validated` refuses.
fn introduced_in<A: ReadView + Sync, B: ReadView + Sync>(
    tx: &Transaction<'_>,
    before: &A,
    after: &B,
    shapes_id: TermId,
    changed: impl FnOnce() -> Vec<EncodedQuad>,
    validated: impl Fn(&Shapes) -> bool,
    resources: &Resources,
) -> StoreResult<Option<ValidationReport>> {
    let data = Selection::of(GraphSelector::Any).excluding(shapes_id);
    let shapes_selection = Selection::asserted(GraphSelector::Exact(shapes_id));
    let shapes_changed = tx
        .inserted()
        .chain(tx.deleted())
        .any(|quad| quad.graph == shapes_id);
    if shapes_changed {
        let shapes_after = compile(after, shapes_selection).map_err(shapes_error)?;
        // Shapes that didn't compile before had no results.
        let shapes_before = compile(before, shapes_selection).ok();
        if !validated(&shapes_after) || shapes_before.as_ref().is_some_and(|s| !validated(s)) {
            return Ok(None);
        }
        let report = validate_shapes_change(
            before,
            shapes_before.as_ref(),
            after,
            &shapes_after,
            data,
            resources,
        )?;
        return Ok(Some(report));
    }
    let shapes = compile(before, shapes_selection).map_err(shapes_error)?;
    // A shapes graph with only `sh:entailment` still makes every validation fail.
    if shapes.is_vacuous() || !validated(&shapes) {
        return Ok(None);
    }
    Ok(Some(validate_changes(
        before,
        after,
        &shapes,
        data,
        &changed(),
        resources,
    )?))
}

/// The SHACL reason a restricted committer's view shows: the results the commit introduces
/// in the statements asserted in graphs it may read, against the shapes of the shapes
/// graph, if it may read that ([`crate::mutation::evidence::Readable`]). `None` if there
/// are none, that isn't known within `resources`, or the shapes use SHACL-SPARQL, whose
/// queries the view can't confine (D16-065).
pub(crate) fn view_reason(
    view: &crate::mutation::evidence::EvidenceView<'_, '_>,
    shapes_graph: &str,
    rejects: impl Fn(&nrese_shacl::ValidationResult) -> bool,
    resources: &Resources,
) -> Option<crate::mutation::evidence::Draft> {
    let tx = view.tx();
    let shapes_id = tx.lookup(graph_iri(shapes_graph).ok()?.as_ref().into())?;
    if !view.graph(shapes_id) {
        return None;
    }
    let after = tx.pending_snapshot();
    let (before, after) = (view.readable(tx.base())?, view.readable(&after)?);
    let changed = || {
        tx.inserted()
            .chain(tx.deleted())
            .filter(|quad| view.graph(quad.graph))
            .collect()
    };
    let report = introduced_in(
        tx,
        &before,
        &after,
        shapes_id,
        changed,
        |shapes| !shapes.uses_sparql(),
        resources,
    )
    .ok()??;
    let rejecting: Vec<_> = report.results.iter().filter(|r| rejects(r)).collect();
    // The first by what the view's statements say, never by the results' order, which is
    // the dictionary's: term ids follow what was interned first, in any graph.
    // Every field, as decoded terms (`nrese_rdf::Term`'s order: the kind, then the value; a
    // blank node by its label, the store's name for it in the committer's view), never
    // their text: equal keys are equal results, so either is shown the same.
    let first = rejecting.iter().min_by_key(|result| {
        (
            result.focus_node.clone(),
            result.component.clone(),
            result.path.clone(),
            result.value.clone(),
            result.source_shape.clone(),
            result.severity.clone(),
            result.source_constraint.clone(),
            result.messages.clone(),
        )
    })?;
    let reason = format!(
        "the commit introduces {} SHACL result(s) the gate rejects; first: focus node {}, {}",
        rejecting.len(),
        first.focus_node,
        first.component
    );
    Some(crate::mutation::evidence::shown(
        reason,
        first.component.as_str(),
        first.focus_node.to_string(),
        Vec::new(),
    ))
}

impl StoreService {
    /// Validates the repository's data against SHACL shapes; for a scope that reads every
    /// graph. Ill-formed shapes are a request error ([`StoreError::ShaclShapes`]).
    ///
    /// It runs as a query does: stopped by `cancellation` (the transport cancels it at its
    /// deadline and when the client is gone) and within the server's query memory, its
    /// SHACL-SPARQL queries included; a stop is [`StoreError::SparqlEvaluation`] with
    /// [`nrese_sparql::QueryEvaluationError::Cancelled`] or `MemoryLimit`.
    pub fn validate_shacl(
        &self,
        scope: &crate::ReadScope,
        request: &ShaclValidationRequest,
        cancellation: &CancellationToken,
    ) -> StoreResult<ShaclValidation> {
        scope.require_all("validation")?;
        let resources = Resources {
            cancellation: Some(cancellation.clone()),
            memory: self.envelope(None),
            ..Resources::default()
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
                // trace. It takes no writer slot, so commits go on meanwhile. The graph holds
                // no stored statement (SYN-58).
                // V1: private once V2 reads a private transaction through SPARQL.
                let mut tx = self.engine().speculative_shared();
                let revision = tx.base().revision();
                let validated = match &request.graphs {
                    ValidatedGraphs::Named(iri) => Some(graph_iri(iri)?),
                    ValidatedGraphs::AllData | ValidatedGraphs::Default => None,
                };
                let graph = request_shapes_graph(tx.base(), validated.as_ref());
                let scratch = graph.as_ref();
                let quads = parse_graph(
                    *format,
                    base_iri.as_deref(),
                    payload.as_slice(),
                    GraphName::NamedNode(scratch.into_owned()),
                    BlankNodes::Fresh,
                )?;
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
