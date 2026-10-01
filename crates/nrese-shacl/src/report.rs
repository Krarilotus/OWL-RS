//! The validation report (design §6): results with their terms decoded, and the report as
//! an RDF graph.

use std::fmt;

use nrese_engine::TermId;
use nrese_rdf::vocab::{rdf, xsd};
use nrese_rdf::{BlankNode, Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use nrese_sparql::ReadView;

use crate::model::{Component, Path, SH, Severity, Shapes};
use crate::validate::RawResult;

/// What validating a data graph against a shapes graph found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ValidationReport {
    pub results: Vec<ValidationResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationResult {
    pub focus_node: Term,
    /// The property shape's path, or the offending predicate of a closed shape.
    pub path: Option<PropertyPath>,
    pub value: Option<Term>,
    pub source_shape: Term,
    pub component: Component,
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

/// Decodes `raw` into a report. Ids come from `view`'s dictionary, so every one decodes.
pub(crate) fn decode<V: ReadView>(
    view: &V,
    shapes: &Shapes,
    raw: &[RawResult],
) -> ValidationReport {
    let term = |id: TermId| view.decode(id).expect("ids read from the view decode");
    let results = raw
        .iter()
        .map(|result| {
            let shape = &shapes.shapes[result.shape];
            let severity = match shape.severity {
                Severity::Violation => sh("Violation"),
                Severity::Warning => sh("Warning"),
                Severity::Info => sh("Info"),
                Severity::Other(severity) => match term(severity) {
                    Term::NamedNode(severity) => severity,
                    _ => sh("Violation"),
                },
            };
            ValidationResult {
                focus_node: term(result.focus),
                path: result
                    .path
                    .as_ref()
                    .and_then(|path| decode_path(view, path)),
                value: result.value.map(term),
                source_shape: term(shape.node),
                component: result.component,
                severity,
                messages: shape
                    .messages
                    .iter()
                    .filter_map(|&message| match term(message) {
                        Term::Literal(message) => Some(message),
                        _ => None,
                    })
                    .collect(),
            }
        })
        .collect();
    ValidationReport { results }
}

impl ValidationReport {
    /// Whether the data conforms: no result of any severity.
    pub fn conforms(&self) -> bool {
        self.results.is_empty()
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
                NamedNode::new_unchecked(result.component.iri()).into(),
            );
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
