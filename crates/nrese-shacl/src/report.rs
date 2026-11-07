//! The validation report (design §6): results with their terms decoded, and the report as
//! an RDF graph.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use nrese_engine::TermId;
use nrese_exec::Reservation;
use nrese_rdf::vocab::{rdf, xsd};
use nrese_rdf::{BlankNode, Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use nrese_sparql::{QueryEvaluationError, ReadView, Resources};

use crate::model::{Component, Path, SH, Severity, Shapes};
use crate::validate::RawResult;

/// What validating a data graph against a shapes graph found.
#[derive(Debug, Clone, Default)]
pub struct ValidationReport {
    pub results: Vec<ValidationResult>,
    /// Validation failures (SHACL §3.6.2): a SPARQL-based constraint that couldn't be
    /// evaluated or reported `?failure`. A report with failures is no verdict.
    pub failures: Vec<String>,
    /// The report's charge to the envelope of the request that made it, held while the
    /// report (or a clone of it) lives.
    _held: Option<Arc<Reservation>>,
}

/// Reports are equal when what they report is: their charge isn't part of it.
impl PartialEq for ValidationReport {
    fn eq(&self, other: &Self) -> bool {
        self.results == other.results && self.failures == other.failures
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
    /// `sh:sourceConstraint`: the `sh:sparql` constraint that found it; none for a
    /// SPARQL-based constraint component's results.
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

fn decode_path<V: ReadView>(view: &V, path: &Path) -> Option<PropertyPath> {
    let inner = |path: &Path| decode_path(view, path).map(Box::new);
    let all = |parts: &[Path]| -> Option<Vec<PropertyPath>> {
        parts.iter().map(|part| decode_path(view, part)).collect()
    };
    Some(match path {
        Path::Predicate(predicate) => match view.decode(*predicate)? {
            Term::NamedNode(predicate) => PropertyPath::Predicate(predicate),
            _ => return None,
        },
        Path::Inverse(path) => PropertyPath::Inverse(inner(path)?),
        Path::Sequence(parts) => PropertyPath::Sequence(all(parts)?),
        Path::Alternative(parts) => PropertyPath::Alternative(all(parts)?),
        Path::ZeroOrMore(path) => PropertyPath::ZeroOrMore(inner(path)?),
        Path::OneOrMore(path) => PropertyPath::OneOrMore(inner(path)?),
        Path::ZeroOrOne(path) => PropertyPath::ZeroOrOne(inner(path)?),
    })
}

/// The bytes a decoded term holds besides its own slot: its text.
fn term_bytes(term: &Term) -> usize {
    match term {
        Term::NamedNode(node) => node.as_str().len(),
        Term::BlankNode(node) => node.as_str().len(),
        Term::Literal(literal) => {
            literal.value().len()
                + literal.datatype().as_str().len()
                + literal.language().map_or(0, str::len)
        }
        Term::Triple(triple) => {
            std::mem::size_of::<Triple>()
                + match &triple.subject {
                    NamedOrBlankNode::NamedNode(node) => node.as_str().len(),
                    NamedOrBlankNode::BlankNode(node) => node.as_str().len(),
                }
                + triple.predicate.as_str().len()
                + term_bytes(&triple.object)
        }
    }
}

/// The bytes a decoded path holds besides its own slot.
fn property_path_bytes(path: &PropertyPath) -> usize {
    match path {
        PropertyPath::Predicate(predicate) => predicate.as_str().len(),
        PropertyPath::Inverse(inner)
        | PropertyPath::ZeroOrMore(inner)
        | PropertyPath::OneOrMore(inner)
        | PropertyPath::ZeroOrOne(inner) => {
            std::mem::size_of::<PropertyPath>() + property_path_bytes(inner)
        }
        PropertyPath::Sequence(parts) | PropertyPath::Alternative(parts) => {
            parts.capacity() * std::mem::size_of::<PropertyPath>()
                + parts.iter().map(property_path_bytes).sum::<usize>()
        }
    }
}

/// The terms a report names, each decoded once from the view and charged as it is kept:
/// one term is held just before its charge, which is the data's own size, not the
/// report's. Dropped, the terms are freed, then their charge returned.
struct Terms<'v, V> {
    view: &'v V,
    decoded: HashMap<TermId, Term>,
    held: Reservation,
}

impl<V: ReadView> Terms<'_, V> {
    /// Makes sure `id` is decoded.
    fn load(&mut self, id: TermId) -> Result<(), QueryEvaluationError> {
        if !self.decoded.contains_key(&id) {
            let term = self.view.decode(id).expect("ids read from the view decode");
            self.held
                .charge(2 * std::mem::size_of::<(TermId, Term)>() + term_bytes(&term))?;
            self.decoded.insert(id, term);
        }
        Ok(())
    }

    fn get(&self, id: TermId) -> &Term {
        &self.decoded[&id]
    }
}

/// Decodes `raw` into a report. Ids come from `view`'s dictionary, so every one decodes.
///
/// The report is charged to `resources`' envelope, each result before it is built (its
/// messages, which every result of a shape copies, included), and holds that charge while
/// it lives; `resources`' cancellation is checked before each result and before the report
/// is returned. A refusal or a cancellation is the outcome, with no report.
pub(crate) fn decode<V: ReadView>(
    view: &V,
    shapes: &Shapes,
    raw: &[RawResult],
    failures: Vec<String>,
    resources: &Resources,
) -> Result<ValidationReport, QueryEvaluationError> {
    let budget = Arc::new(resources.memory.part());
    let mut held = Reservation::new(Arc::clone(&budget));
    let mut terms = Terms {
        view,
        decoded: HashMap::new(),
        held: Reservation::new(budget),
    };
    let sh_bytes = SH.len() + "ConstraintComponent".len();
    let mut results = Vec::new();
    for result in raw {
        resources.check()?;
        let shape = &shapes.shapes[result.shape];
        let custom = match result.component {
            Component::Custom(component) => Some(component),
            _ => None,
        };
        let severity_id = match shape.severity {
            Severity::Other(severity) => Some(severity),
            _ => None,
        };
        let messages = result.messages.as_deref().unwrap_or(&shape.messages);
        for id in [
            Some(result.focus),
            result.value,
            Some(shape.node),
            result.source,
            custom,
            severity_id,
        ]
        .into_iter()
        .flatten()
        .chain(messages.iter().copied())
        {
            terms.load(id)?;
        }
        let path = result
            .path
            .as_ref()
            .and_then(|path| decode_path(view, path));
        // What this result holds once built, charged before any of it is copied.
        let mut bytes = 2 * std::mem::size_of::<ValidationResult>()
            + term_bytes(terms.get(result.focus))
            + result.value.map_or(0, |id| term_bytes(terms.get(id)))
            + term_bytes(terms.get(shape.node))
            + result.source.map_or(0, |id| term_bytes(terms.get(id)))
            + 2 * sh_bytes
            + path.as_ref().map_or(0, property_path_bytes)
            + messages.len() * std::mem::size_of::<Literal>();
        for &message in messages {
            bytes += term_bytes(terms.get(message));
        }
        for id in [custom, severity_id].into_iter().flatten() {
            bytes += term_bytes(terms.get(id));
        }
        held.charge(bytes)?;
        let severity = match severity_id.map(|id| terms.get(id)) {
            None => match shape.severity {
                Severity::Warning => sh("Warning"),
                Severity::Info => sh("Info"),
                _ => sh("Violation"),
            },
            Some(Term::NamedNode(severity)) => severity.clone(),
            Some(_) => sh("Violation"),
        };
        results.push(ValidationResult {
            focus_node: terms.get(result.focus).clone(),
            path,
            value: result.value.map(|id| terms.get(id).clone()),
            source_shape: terms.get(shape.node).clone(),
            component: match custom.map(|id| terms.get(id)) {
                None => NamedNode::new_unchecked(result.component.iri()),
                Some(Term::NamedNode(component)) => component.clone(),
                Some(_) => sh("ConstraintComponent"),
            },
            source_constraint: result.source.map(|id| terms.get(id).clone()),
            severity,
            messages: messages
                .iter()
                .filter_map(|&message| match terms.get(message) {
                    Term::Literal(message) => Some(message.clone()),
                    _ => None,
                })
                .collect(),
        });
    }
    // The terms are freed, and their charge returned; the failures are the report's now.
    drop(terms);
    for failure in &failures {
        held.charge(2 * std::mem::size_of::<String>() + failure.len())?;
    }
    resources.check()?;
    Ok(ValidationReport {
        results,
        failures,
        _held: Some(Arc::new(held)),
    })
}

impl ValidationReport {
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
