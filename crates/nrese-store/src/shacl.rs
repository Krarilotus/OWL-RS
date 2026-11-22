//! SHACL validation as a store operation (design `docs/design/shacl.md`, slice C1b).
//!
//! Shapes live in a graph of the repository ([`StoreConfig::shapes_graph`] unless the
//! request names another), managed like any graph. A request can also bring its own
//! shapes: they are validated against without being stored.
//!
//! [`StoreConfig::shapes_graph`]: crate::StoreConfig::shapes_graph

use std::cmp::Ordering;
use std::fmt::{self, Write as _};
use std::sync::{Arc, Mutex};

use nrese_engine::{EncodedQuad, GraphSelector, ReadModel, Snapshot, TermId, Transaction};
use nrese_exec::stop::StopSource;
use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Envelope, Reservation};
use nrese_rdf::{GraphName, NamedNode, NamedNodeRef, Term};
use nrese_shacl::{
    PropertyPath, Selection, ShapeError, Shapes, ValidationReport, ValidationResult, compile,
    compile_until, validate, validate_changes, validate_shapes_change,
};
use nrese_sparql::{CancellationToken, QueryEvaluationError, ReadView, Resources};

use crate::error::{StoreError, StoreResult};
use crate::mutation::evidence::{
    Draft, EvidenceView, READABLE_REASON_FRAMING_BYTES, in_readable_graphs,
};
use crate::mutation::{MutationState, MutationTicket};
use crate::query::GraphResultFormat;
use crate::rdf_io::{BlankNodes, each_graph_quad, serialize_triples};
use crate::service::StoreService;

#[cfg(test)]
#[path = "shacl_gate_state_tests.rs"]
mod gate_state_tests;

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

/// The results and validation failures shown to the committer of a refused write.
#[derive(Debug)]
pub struct ShaclRefusal {
    pub results: Vec<ShaclResultText>,
    /// Refusing results left out by the count or memory limit.
    pub results_omitted: usize,
    pub failures: Vec<String>,
    /// Validation failures left out by the count or memory limit.
    pub failures_omitted: usize,
    /// The refusal's detail with its counts alone, as a restricted committer's is framed:
    /// for an answer that lists no result.
    pub count_detail: String,
    memory: Arc<Budget>,
    encoding_floor: usize,
    // Drop the text and vectors before giving their space back.
    text: Mutex<Reservation>,
    _slots: (Reservation, Reservation),
}

impl ShaclRefusal {
    /// The write's budget for preparing and encoding this refusal.
    pub fn memory(&self) -> &Arc<Budget> {
        &self.memory
    }

    /// Holds the encoded buffer alongside the refusal, after mapping the explanation.
    /// Reuse the part of the text reservation that covered temporaries, keeping the
    /// live texts covered until drop.
    pub fn reserve_encoding(&self, bytes: usize) -> Result<Reservation, BudgetExceeded> {
        let mut text = self.text.lock().expect("refusal text reservation");
        let reusable = bytes.min(text.bytes().saturating_sub(self.encoding_floor));
        let mut encoded = Reservation::new(Arc::clone(&self.memory));
        encoded.charge(bytes - reusable)?;
        if reusable > 0 {
            // Keep both portions charged to the same write throughout the transfer.
            let held = std::mem::replace(&mut *text, Reservation::new(Arc::clone(&self.memory)));
            let remaining = held.transfer() - reusable;
            *text = Reservation::adopt(Arc::clone(&self.memory), remaining);
            encoded.absorb(Reservation::adopt(Arc::clone(&self.memory), reusable));
        }
        Ok(encoded)
    }
}

impl ShaclResultText {
    fn of(result: &ValidationResult) -> Self {
        let text = |term: &Term| match term {
            Term::NamedNode(node) => node.as_str().to_owned(),
            other => other.to_string(),
        };
        Self {
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
        }
    }

    fn heap_bytes(&self) -> usize {
        self.focus_node.capacity()
            + self.path.as_ref().map_or(0, String::capacity)
            + self.value.as_ref().map_or(0, String::capacity)
            + self.source_shape.capacity()
            + self.component.capacity()
            + self.severity.capacity()
            + self.messages.capacity() * size_of::<String>()
            + self.messages.iter().map(String::capacity).sum::<usize>()
    }
}

impl ShaclValidation {
    /// The report as an RDF graph in `format`.
    pub fn serialize(&self, format: GraphResultFormat) -> StoreResult<Vec<u8>> {
        serialize_triples(format, self.report.to_triples())
    }

    /// The results in plain text.
    pub fn results_text(&self) -> Vec<ShaclResultText> {
        self.report
            .results
            .iter()
            .map(ShaclResultText::of)
            .collect()
    }
}

fn graph_iri(iri: &str) -> StoreResult<NamedNode> {
    NamedNode::new(iri).map_err(|_| StoreError::InvalidGraphIri(iri.to_owned()))
}

fn shapes_error(errors: Vec<ShapeError>) -> StoreError {
    StoreError::ShaclShapes(errors.iter().map(ToString::to_string).collect())
}

fn commit_shapes_error(
    ticket: &MutationTicket,
    errors: Vec<ShapeError>,
    format: impl FnOnce(Vec<ShapeError>) -> StoreError,
) -> StoreError {
    if ticket.end_pending() == MutationState::Cancelled {
        nrese_sparql::QueryEvaluationError::Cancelled.into()
    } else {
        format(errors)
    }
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

/// Whether the commit in `tx` changes the asserted statements of the graph `graph`.
fn changes_graph(tx: &Transaction<'_>, graph: TermId) -> bool {
    tx.inserted()
        .chain(tx.deleted())
        .any(|quad| quad.graph == graph)
}

/// Fails if the commit in `tx` changes the graph `shapes_graph` and leaves shapes in it that
/// don't compile: the shapes graph is a managed object, well-formed whatever wrote it and
/// whether or not the SHACL gate is on ([`StoreError::ShaclShapes`], one message per
/// problem, as `view`'s committer may be told them: [`EvidenceView::shapes_error`]).
/// Where it changes them, the shapes it leaves: their number, nested ones included, and
/// whether they can report nothing ([`Shapes::is_vacuous`]), kept with the publication.
///
/// [`EvidenceView::shapes_error`]: crate::mutation::evidence::EvidenceView::shapes_error
pub(crate) fn check_shapes(
    tx: &Transaction<'_>,
    shapes_graph: &str,
    ticket: &MutationTicket,
    view: &crate::mutation::evidence::EvidenceView<'_, '_>,
) -> StoreResult<Option<(usize, bool)>> {
    let iri = graph_iri(shapes_graph)?;
    let Some(shapes_id) = tx.lookup(iri.as_ref().into()) else {
        return Ok(None);
    };
    if !changes_graph(tx, shapes_id) {
        return Ok(None);
    }
    compile(
        &tx.pending_snapshot(),
        Selection::asserted(GraphSelector::Exact(shapes_id)),
    )
    .map(|shapes| Some((shapes.len(), shapes.is_vacuous())))
    .map_err(|errors| {
        commit_shapes_error(ticket, errors, |errors| {
            view.shapes_error(shapes_graph, shapes_error(errors))
        })
    })
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
    ticket: &MutationTicket,
) -> StoreResult<Option<(ValidationReport, Snapshot)>> {
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
            .take_while(|_| resources.check().is_ok())
            .collect()
    };
    let after = tx.pending_snapshot();
    let report = introduced_in(
        tx,
        tx.base(),
        &after,
        (shapes_id, Some(ticket)),
        changed,
        |_| true,
        resources,
    )?;
    Ok(report.map(|report| (report, after)))
}

/// [`introduced`] over the views `before` and `after` of the commit in `tx`, with the
/// `changed` quads of the views; `None` also for shapes `validated` refuses.
fn introduced_in<A: ReadView + Sync, B: ReadView + Sync>(
    tx: &Transaction<'_>,
    before: &A,
    after: &B,
    shapes: (TermId, Option<&MutationTicket>),
    changed: impl FnOnce() -> Vec<EncodedQuad>,
    validated: impl Fn(&Shapes) -> bool,
    resources: &Resources,
) -> StoreResult<Option<ValidationReport>> {
    let (shapes_id, ticket) = shapes;
    let compile_error = |errors| match ticket {
        Some(ticket) => commit_shapes_error(ticket, errors, shapes_error),
        None => shapes_error(errors),
    };
    let data = Selection::of(GraphSelector::Any).excluding(shapes_id);
    let shapes_selection = Selection::asserted(GraphSelector::Exact(shapes_id));
    let cancellation = resources.cancellation.as_ref();
    let shapes_changed = tx
        .inserted()
        .chain(tx.deleted())
        .take_while(|_| resources.check().is_ok())
        .any(|quad| quad.graph == shapes_id);
    resources.check()?;
    if shapes_changed {
        let shapes_after = compile_until(after, shapes_selection, cancellation)
            .ok_or(QueryEvaluationError::Cancelled)?
            .map_err(compile_error)?;
        // Shapes that didn't compile before had no results.
        let shapes_before = compile_until(before, shapes_selection, cancellation)
            .ok_or(QueryEvaluationError::Cancelled)?
            .ok();
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
    let shapes = compile_until(before, shapes_selection, cancellation)
        .ok_or(QueryEvaluationError::Cancelled)?
        .map_err(compile_error)?;
    // A shapes graph with only `sh:entailment` still makes every validation fail.
    if shapes.is_vacuous() || !validated(&shapes) {
        return Ok(None);
    }
    let changed = changed();
    resources.check()?;
    Ok(Some(validate_changes(
        before, after, &shapes, data, &changed, resources,
    )?))
}

/// A result as a refusal names it: its focus node and component, and its messages
/// (`sh:resultMessage`) where it has any, in the order of their text: the report's order is
/// the dictionary's, which statements in graphs a committer may not read can decide.
fn described(result: &ValidationResult) -> String {
    let mut text = format!("focus node {}, {}", result.focus_node, result.component);
    let mut messages: Vec<String> = result.messages.iter().map(ToString::to_string).collect();
    messages.sort_unstable();
    for (i, message) in messages.iter().enumerate() {
        text.push_str(if i == 0 { ", message " } else { ", " });
        text.push_str(message);
    }
    text
}

/// By decoded terms, so interning terms in hidden graphs cannot change the order.
fn result_key(result: &ValidationResult) -> impl Ord + '_ {
    (
        &result.focus_node,
        &result.component,
        &result.path,
        &result.value,
        &result.source_shape,
        &result.severity,
        &result.source_constraint,
        &result.messages,
    )
}

fn sort_messages(result: &mut ValidationResult) -> &ValidationResult {
    // Validation can carry message literals in dictionary order.
    result.messages.sort_unstable();
    result
}

fn compare_results(left: &ValidationResult, right: &ValidationResult) -> Ordering {
    result_key(left).cmp(&result_key(right))
}

/// Orders only the prefix the refusal can list, in the report's already charged storage.
fn order_prefix<T>(items: &mut [T], bound: usize, mut compare: impl FnMut(&T, &T) -> Ordering) {
    let listed = bound.min(items.len());
    if listed < items.len() {
        items.select_nth_unstable_by(listed, &mut compare);
    }
    items[..listed].sort_unstable_by(compare);
}

/// Size the formatted text without allocating it.
fn display_len(value: &impl fmt::Display) -> usize {
    #[derive(Default)]
    struct Count(usize);
    impl fmt::Write for Count {
        fn write_str(&mut self, text: &str) -> fmt::Result {
            self.0 = self.0.saturating_add(text.len());
            Ok(())
        }
    }
    let mut count = Count::default();
    write!(count, "{value}").expect("counting does not fail");
    count.0
}

/// A growing string can hold both its old and new buffers during reallocation.
fn growing_text_bytes(len: usize) -> usize {
    len.max(8).saturating_mul(3)
}

fn term_text_bytes(term: &Term) -> usize {
    match term {
        Term::NamedNode(node) => node.as_str().len(),
        other => growing_text_bytes(display_len(other)),
    }
}

fn path_text_bytes(path: &PropertyPath) -> usize {
    match path {
        PropertyPath::Predicate(node) => node.as_str().len(),
        other => growing_text_bytes(display_len(other)),
    }
}

fn result_text_bytes(result: &ValidationResult) -> usize {
    term_text_bytes(&result.focus_node)
        .saturating_add(result.path.as_ref().map_or(0, path_text_bytes))
        .saturating_add(result.value.as_ref().map_or(0, term_text_bytes))
        .saturating_add(term_text_bytes(&result.source_shape))
        .saturating_add(result.component.as_str().len())
        .saturating_add(result.severity.as_str().len())
        .saturating_add(result.messages.iter().fold(0usize, |bytes, message| {
            bytes
                .saturating_add(size_of::<String>())
                .saturating_add(message.value().len())
        }))
}

fn first_result_bytes(result: &ValidationResult, detail: &str) -> usize {
    let focus = display_len(&result.focus_node);
    let component = display_len(&result.component);
    let mut description = "focus node , "
        .len()
        .saturating_add(focus)
        .saturating_add(component);
    let mut message_text = 0usize;
    for message in &result.messages {
        let len = display_len(message);
        description = description.saturating_add(", message ".len().saturating_add(len));
        message_text = message_text
            .saturating_add(size_of::<String>())
            .saturating_add(growing_text_bytes(len));
    }
    let full_detail = detail
        .len()
        .saturating_add("; first: ".len())
        .saturating_add(description);
    // First, the temporary description and its sorted message strings. Then the
    // detail, framing for a restricted committer, and the two explanation summaries
    // that can overlap in nrese-server's reject_view::reject_view, which clones the
    // summary, focus and constraint before the store's explanation drops.
    let describe = growing_text_bytes(description)
        .saturating_add(message_text)
        .saturating_add(detail.len());
    let append = growing_text_bytes(description).saturating_add(growing_text_bytes(full_detail));
    let explain = full_detail
        .saturating_mul(2)
        .saturating_add(growing_text_bytes(
            full_detail.saturating_add(READABLE_REASON_FRAMING_BYTES),
        ))
        .saturating_add(growing_text_bytes(focus))
        .saturating_add(component.saturating_mul(2));
    describe.max(append).max(explain)
}

/// The common refusal for either the full validation or the committer's own validation.
pub(crate) fn refusal(
    mut report: ValidationReport,
    rejects: impl Fn(&ValidationResult) -> bool,
    bound: Option<usize>,
    memory: &Envelope,
    restricted: bool,
    running: impl Fn() -> bool,
) -> Option<Draft> {
    let memory = Arc::new(memory.optional_part());
    let bound = bound.filter(|&bound| bound > 0).unwrap_or(usize::MAX);
    report.results.retain(|result| running() && rejects(result));
    let total_results = report.results.len();
    let total_failures = report.failures.len();
    if !running() || (total_results == 0 && total_failures == 0) {
        return None;
    }
    let mut detail =
        format!("the commit introduces {total_results} SHACL result(s) the gate rejects");
    if total_failures > 0 {
        detail.push_str(&format!(" and {total_failures} failure(s)"));
    }
    let count_detail = if restricted {
        in_readable_graphs(&detail)
    } else {
        detail.clone()
    };
    let mut text = Reservation::new(Arc::clone(&memory));
    let mut results = ChargedVec::new(Arc::clone(&memory));
    let mut live_text_bytes = 0;
    let mut explanation = None;
    for result in report.results.iter_mut().take_while(|_| running()) {
        sort_messages(result);
    }
    order_prefix(&mut report.results, bound, compare_results);
    for result in report.results.iter().take(bound).take_while(|_| running()) {
        let first = results.is_empty();
        let mut bytes = result_text_bytes(result);
        if first {
            bytes = bytes.saturating_add(first_result_bytes(result, &detail));
        }
        if results.reserve(1).is_err() || text.charge(bytes).is_err() {
            break;
        }
        if first {
            write!(detail, "; first: {}", described(result)).expect("writing to a string");
            explanation = Some(nrese_reasoner::RejectExplanation {
                summary: detail.clone(),
                violated_constraint: result.component.as_str().to_owned(),
                focus_resource: result.focus_node.to_string(),
                evidence: Vec::new(),
            });
        }
        let result = ShaclResultText::of(result);
        live_text_bytes += result.heap_bytes();
        results
            .push_within(result)
            .expect("the result's slot was reserved");
    }
    order_prefix(&mut report.failures, bound, Ord::cmp);
    let mut failures = ChargedVec::new(Arc::clone(&memory));
    for failure in report
        .failures
        .drain(..)
        .take(bound)
        .take_while(|_| running())
    {
        let bytes = failure.capacity();
        if failures.reserve(1).is_err() || text.charge(bytes).is_err() {
            break;
        }
        live_text_bytes += bytes;
        failures
            .push_within(failure)
            .expect("the failure's slot was reserved");
    }
    if !running() {
        return None;
    }
    let (results, result_slots) = results.into_parts();
    let (failures, failure_slots) = failures.into_parts();
    if restricted {
        detail = in_readable_graphs(&detail);
    }
    // By encoding time the server holds one copy of the explanation strings. The
    // result and failure vector slots have their own reservations.
    let encoding_floor = live_text_bytes
        + detail.capacity()
        + explanation.as_ref().map_or(0, |explanation| {
            explanation.summary.len()
                + explanation.violated_constraint.len()
                + explanation.focus_resource.len()
        });
    Some(Draft {
        kind: crate::RejectKind::ShapesViolated,
        detail,
        explanation,
        attribution: None,
        shacl: Some(Arc::new(ShaclRefusal {
            results_omitted: total_results - results.len(),
            failures_omitted: total_failures - failures.len(),
            results,
            failures,
            count_detail,
            memory,
            encoding_floor,
            text: Mutex::new(text),
            _slots: (result_slots, failure_slots),
        })),
    })
}

/// The gate log counts every result, in the same order a refusal would use.
pub(crate) fn reported(mut report: ValidationReport) -> String {
    let first = report
        .results
        .iter_mut()
        .map(sort_messages)
        .min_by(|left, right| compare_results(left, right))
        .map_or_else(String::new, |result| {
            format!("; first: {}", described(result))
        });
    format!(
        "the commit introduces {} SHACL result(s) and {} failure(s){first}",
        report.results.len(),
        report.failures.len()
    )
}

/// The SHACL reason a restricted committer's view shows: the results the commit introduces
/// in the statements asserted in graphs it may read, against the shapes of the shapes
/// graph, if it may read that ([`crate::mutation::evidence::Readable`]). `None` if there
/// are none, that isn't known within `resources`, or the shapes use SHACL-SPARQL, whose
/// queries the view can't confine (D16-065).
pub(crate) fn view_reason(
    view: &EvidenceView<'_, '_>,
    pending: Option<&Snapshot>,
    shapes_graph: &str,
    rejects: impl Fn(&ValidationResult) -> bool,
    bound: Option<usize>,
    resources: &Resources,
) -> Option<Draft> {
    let tx = view.tx();
    let shapes_id = tx.lookup(graph_iri(shapes_graph).ok()?.as_ref().into())?;
    if !view.graph(shapes_id) {
        return None;
    }
    let before = view.readable(tx.base(), resources)?;
    let changed = || {
        tx.inserted()
            .chain(tx.deleted())
            .take_while(|_| resources.check().is_ok())
            .filter(|quad| view.graph(quad.graph))
            .collect()
    };
    let core = |shapes: &Shapes| !shapes.uses_sparql();
    // Reuse the SHACL gate's indexed snapshot. After an earlier refusal, build one
    // for the whole evidence pass so each pattern read can use its indexes.
    let built;
    let pending = match pending {
        Some(pending) => pending,
        None => {
            resources.check().ok()?;
            built = tx.pending_snapshot();
            &built
        }
    };
    resources.check().ok()?;
    let after = view.readable(pending, resources)?;
    let report = introduced_in(
        tx,
        &before,
        &after,
        (shapes_id, None),
        changed,
        core,
        resources,
    )
    .ok()??;
    refusal(report, rejects, bound, &resources.memory, true, || {
        resources.check().is_ok()
    })
}

/// The SHACL commit gate's state: its policy, and the shapes it checks commits against (the
/// repository's shapes graph) as the store keeps them ([`StoreService::shacl_gate_state`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaclGateState {
    pub gate: crate::ShaclGate,
    /// The dataset revision that supplies this state.
    pub view_revision: u64,
    /// The revision that last changed the shapes graph, or the open's revision.
    pub revision: u64,
    /// The number of shapes, nested ones included (as a validation counts them); `None`
    /// where a count isn't available ([`Self::not_counted`]).
    pub shapes: Option<usize>,
    /// Whether the shapes can report nothing: no shape and no `sh:entailment`. An enforcing
    /// gate then accepts every write. `None` where a count isn't available.
    pub vacuous: Option<bool>,
    /// Why the shapes weren't counted.
    pub not_counted: Option<String>,
}

/// The result of compiling the watched graph, without the compiled shapes.
pub(crate) struct ShapesCounted(pub(crate) Result<(usize, bool), Box<str>>);

/// A display bound for the info response and its startup log. Validation reports all errors.
pub(crate) const SHAPES_DIAGNOSTIC_BYTES: usize = 512;
const SHAPES_DONT_COMPILE: &str = "the stored shapes don't compile";

/// The latest shapes revision whose count was deferred for each reason. An older refresh
/// can finish later than a newer one. These markers only describe unfilled cells.
#[derive(Default)]
pub(crate) struct ShapesDeferred {
    stopped: DeferredRevision,
    memory: DeferredRevision,
}

type DeferredRevision = std::sync::OnceLock<std::sync::atomic::AtomicU64>;

fn defer(marker: &DeferredRevision, revision: u64) {
    marker
        .get_or_init(|| std::sync::atomic::AtomicU64::new(revision))
        .fetch_max(revision, std::sync::atomic::Ordering::Relaxed);
}

impl ShapesDeferred {
    fn why(&self, revision: u64) -> &'static str {
        let at = |marker: &DeferredRevision| {
            marker
                .get()
                .is_some_and(|at| at.load(std::sync::atomic::Ordering::Relaxed) == revision)
        };
        if at(&self.stopped) {
            "stopped. Retried at the next load, replica batch or start"
        } else if at(&self.memory) {
            "not enough memory to count the stored shapes"
        } else {
            "being compiled"
        }
    }
}

/// Why stored shapes don't compile: their first problem and how many more, so that what is
/// kept stays small whatever the shapes graph holds.
fn not_compiled(errors: &[ShapeError]) -> Box<str> {
    let suffix = if errors.len() > 1 {
        format!(" (and {} more problems)", errors.len() - 1)
    } else {
        String::new()
    };
    let mut why = String::from(SHAPES_DONT_COMPILE);
    let limit = SHAPES_DIAGNOSTIC_BYTES - suffix.len() - '…'.len_utf8();
    if let Some(first) = errors.first() {
        for part in [": shape ", &first.shape, ": ", &first.message] {
            let end = part.floor_char_boundary(limit - why.len());
            why.push_str(&part[..end]);
            if end < part.len() {
                why.push('…');
                break;
            }
        }
    }
    why.push_str(&suffix);
    why.into_boxed_str()
}

impl StoreService {
    /// Fills the view's cell unless a writer already counted these shapes. A later change
    /// of the shapes graph has its own cell.
    pub(crate) fn refresh_kept_shapes(&self, view: &Snapshot, stop: &StopSource) {
        let cell = view
            .watched()
            .expect("the store registered its shapes graph");
        if cell.kept().is_some() {
            return;
        }
        if stop.check().is_err() {
            defer(&self.shapes_deferred.stopped, cell.changed_at());
            return;
        }
        let iri = NamedNodeRef::new_unchecked(&self.config().shapes_graph);
        let counted = if let Some(graph) = view.lookup(iri.into()) {
            if self.memory_stop().exceeded() {
                defer(&self.shapes_deferred.memory, cell.changed_at());
                return;
            }
            let cancel = stop.flag().map(CancellationToken::from);
            let result = compile_until(
                view,
                Selection::asserted(GraphSelector::Exact(graph)),
                cancel.as_ref(),
            );
            match result {
                Some(Ok(shapes)) => Ok((shapes.len(), shapes.is_vacuous())),
                Some(Err(errors)) => Err(not_compiled(&errors)),
                None => {
                    defer(&self.shapes_deferred.stopped, cell.changed_at());
                    return;
                }
            }
        } else {
            Ok((0, true))
        };
        if stop.check().is_err() {
            defer(&self.shapes_deferred.stopped, cell.changed_at());
            return;
        }
        cell.keep(std::sync::Arc::new(ShapesCounted(counted)));
    }

    /// The SHACL gate's state ([`ShaclGateState`]): its policy and the shapes as kept,
    /// compiled at the open and after each write that changed the shapes graph. A read of
    /// what is kept: it never compiles, and never waits for a compilation.
    pub fn shacl_gate_state(&self) -> ShaclGateState {
        let view = self.engine().snapshot();
        let cell = view
            .watched()
            .expect("the store registered its shapes graph");
        let counted = cell.kept().map(|value| {
            &value
                .downcast_ref::<ShapesCounted>()
                .expect("shapes count")
                .0
        });
        let (shapes, vacuous, not_counted) = match counted {
            Some(Ok((shapes, vacuous))) => (Some(*shapes), Some(*vacuous), None),
            Some(Err(why)) => (None, None, Some(why.to_string())),
            None => (
                None,
                None,
                Some(self.shapes_deferred.why(cell.changed_at()).to_owned()),
            ),
        };
        ShaclGateState {
            gate: self.config().shacl_gate,
            view_revision: view.revision(),
            revision: cell.changed_at(),
            shapes,
            vacuous,
            not_counted,
        }
    }

    /// Logs the SHACL gate of `repository` at its start: its policy and its shapes. Warns
    /// when a gate that is on checks nothing: no shapes are compiled from the shapes graph
    /// (every write passes it), or its shapes don't compile (every write is refused until
    /// the shapes graph is repaired or replaced).
    pub fn log_shacl_gate_state(&self, repository: &str) {
        let state = self.shacl_gate_state();
        let gate = state.gate.name();
        let severity = state.gate.severity().map(crate::GateSeverity::name);
        let on = state.gate != crate::ShaclGate::Off;
        match (state.shapes, state.vacuous) {
            (_, Some(true)) if on => tracing::warn!(
                repository,
                gate,
                severity,
                shapes = state.shapes,
                "SHACL gate: no shapes are compiled from the shapes graph, so it checks nothing \
                 and every write passes it"
            ),
            (None, _)
                if on
                    && state
                        .not_counted
                        .as_deref()
                        .is_some_and(|why| why.starts_with(SHAPES_DONT_COMPILE)) =>
            {
                tracing::warn!(
                    repository,
                    gate,
                    severity,
                    not_counted = state.not_counted.as_deref(),
                    "SHACL gate: the stored shapes don't compile, so every write is refused until \
                     the shapes graph is repaired or replaced"
                )
            }
            (None, _) if on => tracing::warn!(
                repository,
                gate,
                severity,
                not_counted = state.not_counted.as_deref(),
                "SHACL gate: the stored shapes haven't been counted"
            ),
            _ => tracing::info!(
                repository,
                gate,
                severity,
                shapes = state.shapes,
                vacuous = state.vacuous,
                not_counted = state.not_counted.as_deref(),
                "SHACL gate"
            ),
        }
    }

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
                // The shapes go into a graph of a private transaction, never committed: their
                // terms are its own, charged to the request's envelope, and none enters the
                // dictionary (dictionary lifecycle V1); stored terms keep their ids in it. Each
                // is inserted as it is read, its terms and pending growth charged first. It
                // takes no writer slot, so commits go on meanwhile. The graph holds no stored
                // statement (SYN-58). One budget for the request, so its pending snapshot is
                // made once for every check (`pending_snapshot_within`'s memo).
                let reads = std::sync::Arc::new(resources.memory.part());
                let mut tx = self.engine().speculative(std::sync::Arc::clone(&reads));
                let revision = tx.base().revision();
                let validated = match &request.graphs {
                    ValidatedGraphs::Named(iri) => Some(graph_iri(iri)?),
                    ValidatedGraphs::AllData | ValidatedGraphs::Default => None,
                };
                let graph = request_shapes_graph(tx.base(), validated.as_ref());
                let scratch = graph.as_ref();
                each_graph_quad(
                    *format,
                    base_iri.as_deref(),
                    payload.as_slice(),
                    GraphName::NamedNode(scratch.into_owned()),
                    BlankNodes::Fresh,
                    |quad| {
                        tx.try_insert(quad)?;
                        Ok(())
                    },
                )?;
                let (report, shapes, applicable) = validate_view(
                    &nrese_sparql::PendingRead::new(&tx, &reads),
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
