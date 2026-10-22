//! The validation report (design §6): results with their terms decoded, and the report as
//! an RDF graph.

use std::fmt;

use crate::graph::{contains_within, stopped};
use nrese_engine::TermId;
use nrese_exec::{Budget, ChargedVec, Reservation, Ticker};
use nrese_rdf::vocab::{rdf, xsd};
use nrese_rdf::{BlankNode, Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use nrese_sparql::{QueryEvaluationError, ReadView, Resources};
use std::sync::Arc;

use crate::model::{Component, Path, SH, Severity, Shapes};
use crate::validate::RawBatch;

/// What validating a data graph against a shapes graph found.
#[derive(Debug, Default)]
pub struct ValidationReport {
    results: Vec<ValidationResult>,
    failures: Vec<String>,
    applicable: bool,
    // Original raw failure tokens precede the fresh lease-vector header token in held.
    _failure_leases: Vec<Reservation>,
    // Payload fields precede their lease. Whole-report Clone/owned extraction is absent.
    held: Option<Reservation>,
}

impl PartialEq for ValidationReport {
    fn eq(&self, other: &Self) -> bool {
        self.results == other.results
            && self.failures == other.failures
            && self.applicable == other.applicable
    }
}
impl Eq for ValidationReport {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationResult {
    pub focus_node: Term,
    /// The property shape's path, or the offending predicate of a closed shape.
    pub path: Option<PropertyPath>,
    pub value: Option<Term>,
    pub source_shape: Term,
    /// `sh:sourceConstraintComponent`: a built-in component, or a SPARQL-based one of the
    /// shapes graph.
    pub component: NamedNode,
    /// `sh:sourceConstraint`: the SPARQL constraint or validator that found it.
    pub source_constraint: Option<Term>,
    /// `sh:Violation`, `sh:Warning`, `sh:Info` or the shape's own severity IRI.
    pub severity: NamedNode,
    /// The source shape's `sh:message` literals.
    pub messages: Vec<Literal>,
}

/// A property path with its predicates decoded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PropertyPath {
    Predicate(NamedNode),
    Inverse(Box<PropertyPath>),
    Sequence(Vec<PropertyPath>),
    Alternative(Vec<PropertyPath>),
    ZeroOrMore(Box<PropertyPath>),
    OneOrMore(Box<PropertyPath>),
    ZeroOrOne(Box<PropertyPath>),
}

/// The path in SPARQL property path syntax.
impl fmt::Display for PropertyPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let joined = |f: &mut fmt::Formatter<'_>, parts: &[Self], separator: &str| {
            f.write_str("(")?;
            for (i, part) in parts.iter().enumerate() {
                if i > 0 {
                    f.write_str(separator)?;
                }
                write!(f, "{part}")?;
            }
            f.write_str(")")
        };
        match self {
            Self::Predicate(predicate) => write!(f, "{predicate}"),
            Self::Inverse(inner) => write!(f, "^{inner}"),
            Self::Sequence(parts) => joined(f, parts, "/"),
            Self::Alternative(parts) => joined(f, parts, "|"),
            Self::ZeroOrMore(inner) => write!(f, "({inner})*"),
            Self::OneOrMore(inner) => write!(f, "({inner})+"),
            Self::ZeroOrOne(inner) => write!(f, "({inner})?"),
        }
    }
}

fn sh(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{SH}{local}"))
}

type Result<T> = std::result::Result<T, QueryEvaluationError>;

fn term<V: ReadView>(
    view: &V,
    id: TermId,
    held: &mut Reservation,
    ticker: &mut Ticker,
) -> Result<Term> {
    ticker.check().map_err(stopped)?;
    let term = view.decode_within(id, held)?.ok_or_else(|| {
        QueryEvaluationError::Unexpected(format!("term #{} isn't known", id.raw()))
    })?;
    ticker.check().map_err(stopped)?;
    Ok(term)
}

fn sh_within(local: &str, held: &mut Reservation) -> Result<NamedNode> {
    held.charge(
        SH.len()
            .checked_add(local.len())
            .ok_or_else(|| held.budget().refusal(usize::MAX))?,
    )?;
    let mut text = String::with_capacity(SH.len() + local.len());
    text.push_str(SH);
    text.push_str(local);
    Ok(NamedNode::new_unchecked(text))
}

fn named_or<V: ReadView>(
    view: &V,
    id: TermId,
    fallback: &str,
    held: &mut Reservation,
    ticker: &mut Ticker,
) -> Result<NamedNode> {
    let before = held.bytes();
    match term(view, id, held, ticker)? {
        Term::NamedNode(node) => Ok(node),
        other => {
            drop(other);
            held.resize(before)?;
            sh_within(fallback, held)
        }
    }
}

fn decode_path<V: ReadView>(
    view: &V,
    path: &Path,
    held: &mut Reservation,
    ticker: &mut Ticker,
) -> Result<Option<PropertyPath>> {
    let before = held.bytes();
    let decoded = decode_path_inner(view, path, held, ticker)?;
    if decoded.is_none() {
        held.resize(before)?;
    }
    Ok(decoded)
}

fn decode_path_inner<V: ReadView>(
    view: &V,
    path: &Path,
    held: &mut Reservation,
    ticker: &mut Ticker,
) -> Result<Option<PropertyPath>> {
    ticker.tick(1).map_err(stopped)?;
    let result = match path {
        Path::Predicate(predicate) => match term(view, *predicate, held, ticker)? {
            Term::NamedNode(n) => PropertyPath::Predicate(n),
            _ => return Ok(None),
        },
        Path::Sequence(parts) | Path::Alternative(parts) => {
            held.charge(
                parts
                    .len()
                    .checked_mul(size_of::<PropertyPath>())
                    .ok_or_else(|| held.budget().refusal(usize::MAX))?,
            )?;
            let mut decoded = Vec::with_capacity(parts.len());
            for part in parts {
                let Some(part) = decode_path(view, part, held, ticker)? else {
                    return Ok(None);
                };
                decoded.push(part);
            }
            if matches!(path, Path::Sequence(_)) {
                PropertyPath::Sequence(decoded)
            } else {
                PropertyPath::Alternative(decoded)
            }
        }
        Path::Inverse(inner)
        | Path::ZeroOrMore(inner)
        | Path::OneOrMore(inner)
        | Path::ZeroOrOne(inner) => {
            let Some(decoded) = decode_path(view, inner, held, ticker)? else {
                return Ok(None);
            };
            held.charge(size_of::<PropertyPath>())?;
            let inner = Box::new(decoded);
            match path {
                Path::Inverse(_) => PropertyPath::Inverse(inner),
                Path::ZeroOrMore(_) => PropertyPath::ZeroOrMore(inner),
                Path::OneOrMore(_) => PropertyPath::OneOrMore(inner),
                Path::ZeroOrOne(_) => PropertyPath::ZeroOrOne(inner),
                _ => unreachable!(),
            }
        }
    };
    Ok(Some(result))
}

/// Raw remains owned and charged until every decoded result and failure is complete.
pub(crate) fn decode<V: ReadView>(
    view: &V,
    shapes: &Shapes,
    raw: RawBatch<'_>,
    resources: &Resources,
) -> Result<ValidationReport> {
    resources.check()?;
    let budget: Arc<Budget> = raw.budget.clone();
    let mut ticker = resources.stop().ticker().map_err(stopped)?;
    let (mut rows, mut count, mut leases) = (0usize, 0usize, 0usize);
    for chunk in raw.chunks.iter() {
        ticker.tick(1).map_err(stopped)?;
        rows = rows
            .checked_add(chunk.results.len())
            .ok_or_else(|| budget.refusal(usize::MAX))?;
        count = count
            .checked_add(chunk.failures.len())
            .ok_or_else(|| budget.refusal(usize::MAX))?;
        leases += usize::from(chunk.failure_held.is_some());
    }
    let mut held = Reservation::new(budget.clone());
    // Declared before results/failures: their payloads drop before these old tokens,
    // including a partial transfer that stops inside the preadmitted Room below.
    let mut failure_leases = ChargedVec::with_capacity(budget.clone(), leases)?;
    let mut results = ChargedVec::with_capacity(budget.clone(), rows)?;
    let mut failures = ChargedVec::with_capacity(budget, count)?;
    for result in raw.results() {
        ticker.tick(1).map_err(stopped)?;
        let shape = &shapes.shapes[result.shape];
        let severity = match shape.severity {
            Severity::Violation => sh_within("Violation", &mut held)?,
            Severity::Warning => sh_within("Warning", &mut held)?,
            Severity::Info => sh_within("Info", &mut held)?,
            Severity::Other(id) => named_or(view, id, "Violation", &mut held, &mut ticker)?,
        };
        let messages = result.messages.unwrap_or(&shape.messages);
        held.charge(
            messages
                .len()
                .checked_mul(size_of::<Literal>())
                .ok_or_else(|| held.budget().refusal(usize::MAX))?,
        )?;
        let mut decoded_messages = Vec::with_capacity(messages.len());
        for &id in messages {
            let before = held.bytes();
            match term(view, id, &mut held, &mut ticker)? {
                Term::Literal(l) => decoded_messages.push(l),
                other => {
                    drop(other);
                    held.resize(before)?;
                }
            }
        }
        let path = match result.path.as_deref() {
            Some(path) => decode_path(view, path, &mut held, &mut ticker)?,
            None => None,
        };
        results.push(ValidationResult {
            focus_node: term(view, result.focus, &mut held, &mut ticker)?,
            path,
            value: result
                .value
                .map(|id| term(view, id, &mut held, &mut ticker))
                .transpose()?,
            source_shape: term(view, shape.node, &mut held, &mut ticker)?,
            component: match result.component {
                Component::Custom(id) => {
                    named_or(view, id, "ConstraintComponent", &mut held, &mut ticker)?
                }
                c => sh_within(c.local_name(), &mut held)?,
            },
            source_constraint: result
                .source
                .map(|id| term(view, id, &mut held, &mut ticker))
                .transpose()?,
            severity,
            messages: decoded_messages,
        })?;
    }
    let applicable = raw.applicable;
    let (chunks, _chunk_slots) = raw.chunks.into_parts();
    failures.fill(count, |room| {
        for chunk in chunks {
            ticker.tick(1).map_err(stopped)?;
            let crate::validate::RawChunk {
                results: rows,
                failures: messages,
                held: row_token,
                failure_held,
            } = chunk;
            drop(rows);
            drop(row_token);
            if let Some(token) = failure_held {
                // Exactly one slot was admitted for each Some above. Store the old
                // token before moving any String, so partial failure cannot release it.
                failure_leases
                    .push(token)
                    .expect("preadmitted raw failure lease slots");
                let old_headers = messages.capacity() * size_of::<String>();
                for message in messages {
                    ticker.tick(1).map_err(stopped)?;
                    room.push(message);
                }
                // The consumed old Vec/IntoIter has freed its header before this trim.
                failure_leases.last_mut().unwrap().release(old_headers);
            } else {
                debug_assert!(messages.is_empty());
            }
        }
        Ok::<_, QueryEvaluationError>(())
    })??;
    ticker.finish().map_err(stopped)?;
    let (results, token) = results.into_parts();
    held.absorb(token);
    let (failures, token) = failures.into_parts();
    held.absorb(token);
    let (failure_leases, token) = failure_leases.into_parts();
    // Only this fresh phase's vector-header token joins fresh decoded admission.
    held.absorb(token);
    Ok(ValidationReport {
        results,
        failures,
        applicable,
        _failure_leases: failure_leases,
        held: Some(held),
    })
}

#[cfg(all(test, debug_assertions))]
mod resource_measure_tests;

fn path_heap(path: &PropertyPath) -> usize {
    match path {
        PropertyPath::Predicate(n) => n.heap_bytes(),
        PropertyPath::Sequence(parts) | PropertyPath::Alternative(parts) => {
            parts.capacity() * size_of::<PropertyPath>()
                + parts.iter().map(path_heap).sum::<usize>()
        }
        PropertyPath::Inverse(p)
        | PropertyPath::ZeroOrMore(p)
        | PropertyPath::OneOrMore(p)
        | PropertyPath::ZeroOrOne(p) => size_of::<PropertyPath>() + path_heap(p),
    }
}
fn result_heap(result: &ValidationResult) -> usize {
    result.focus_node.heap_bytes()
        + result.path.as_ref().map_or(0, path_heap)
        + result.value.as_ref().map_or(0, Term::heap_bytes)
        + result.source_shape.heap_bytes()
        + result.component.heap_bytes()
        + result
            .source_constraint
            .as_ref()
            .map_or(0, Term::heap_bytes)
        + result.severity.heap_bytes()
        + result.messages.capacity() * size_of::<Literal>()
        + result
            .messages
            .iter()
            .map(Literal::heap_bytes)
            .sum::<usize>()
}

impl ValidationReport {
    pub fn results(&self) -> &[ValidationResult] {
        &self.results
    }
    pub fn failures(&self) -> &[String] {
        &self.failures
    }
    pub fn applicable(&self) -> bool {
        self.applicable
    }

    /// Consumes the after report, retaining its payload/lease while removing results
    /// present before. This adapts the existing store difference without mutable fields.
    pub fn without_results_from(mut self, earlier: &Self, resources: &Resources) -> Result<Self> {
        let mut ticker = resources.stop().ticker().map_err(stopped)?;
        let mut released = 0;
        let mut kept = 0;
        for read in 0..self.results.len() {
            ticker.tick(1).map_err(stopped)?;
            if contains_within(&earlier.results, &self.results[read], &mut ticker)? {
                released += result_heap(&self.results[read]);
            } else {
                self.results.swap(kept, read);
                kept += 1;
            }
        }
        // Compact in place, preserving the old retain order. Refusal/stop above drops
        // the entire owner with its full lease; success frees rows before returning it.
        self.results.truncate(kept);
        ticker.finish().map_err(stopped)?;
        if let Some(held) = &mut self.held {
            held.release(released);
        }
        Ok(self)
    }

    /// Whether the data conforms: no result of any severity (and no failure).
    pub fn conforms(&self) -> bool {
        self.results.is_empty() && self.failures.is_empty()
    }

    /// The report as an RDF graph (`sh:ValidationReport`), with fresh blank nodes.
    pub fn to_triples(&self) -> Vec<Triple> {
        let mut out = Vec::new();
        let report = BlankNode::default();
        let mut add = |subject: &BlankNode, predicate: NamedNode, object: Term| {
            out.push(Triple::new(subject.clone(), predicate, object));
        };
        add(
            &report,
            rdf::TYPE.into_owned(),
            sh("ValidationReport").into(),
        );
        add(
            &report,
            sh("conforms"),
            Literal::new_typed_literal(self.conforms().to_string(), xsd::BOOLEAN).into(),
        );
        let mut paths = Vec::new();
        for result in &self.results {
            let node = BlankNode::default();
            add(&report, sh("result"), node.clone().into());
            add(&node, rdf::TYPE.into_owned(), sh("ValidationResult").into());
            add(&node, sh("focusNode"), result.focus_node.clone());
            if let Some(path) = &result.path {
                add(&node, sh("resultPath"), path_node(path, &mut paths));
            }
            if let Some(value) = &result.value {
                add(&node, sh("value"), value.clone());
            }
            add(&node, sh("sourceShape"), result.source_shape.clone());
            add(
                &node,
                sh("sourceConstraintComponent"),
                result.component.clone().into(),
            );
            if let Some(source) = &result.source_constraint {
                add(&node, sh("sourceConstraint"), source.clone());
            }
            add(&node, sh("resultSeverity"), result.severity.clone().into());
            for message in &result.messages {
                add(&node, sh("resultMessage"), message.clone().into());
            }
        }
        out.extend(paths);
        out
    }
}

/// The node that denotes `path` in RDF, adding its triples to `out`.
fn path_node(path: &PropertyPath, out: &mut Vec<Triple>) -> Term {
    let wrap = |predicate: &str, object: Term, out: &mut Vec<Triple>| -> Term {
        let node = BlankNode::default();
        out.push(Triple::new(node.clone(), sh(predicate), object));
        node.into()
    };
    match path {
        PropertyPath::Predicate(predicate) => predicate.clone().into(),
        PropertyPath::Inverse(inner) => {
            let inner = path_node(inner, out);
            wrap("inversePath", inner, out)
        }
        PropertyPath::ZeroOrMore(inner) => {
            let inner = path_node(inner, out);
            wrap("zeroOrMorePath", inner, out)
        }
        PropertyPath::OneOrMore(inner) => {
            let inner = path_node(inner, out);
            wrap("oneOrMorePath", inner, out)
        }
        PropertyPath::ZeroOrOne(inner) => {
            let inner = path_node(inner, out);
            wrap("zeroOrOnePath", inner, out)
        }
        PropertyPath::Sequence(parts) => list(parts, out),
        PropertyPath::Alternative(parts) => {
            let members = list(parts, out);
            wrap("alternativePath", members, out)
        }
    }
}

/// An RDF list of the nodes of `parts`.
fn list(parts: &[PropertyPath], out: &mut Vec<Triple>) -> Term {
    let mut head: Term = rdf::NIL.into_owned().into();
    for part in parts.iter().rev() {
        let member = path_node(part, out);
        let node = BlankNode::default();
        out.push(Triple::new(node.clone(), rdf::FIRST.into_owned(), member));
        out.push(Triple::new(node.clone(), rdf::REST.into_owned(), head));
        head = NamedOrBlankNode::from(node).into();
    }
    head
}
