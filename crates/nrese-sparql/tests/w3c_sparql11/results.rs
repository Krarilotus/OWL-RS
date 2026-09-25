//! Comparable query results.
//!
//! Solution sets and graphs are compared as canonicalised RDF graphs, so blank nodes in
//! results compare by structure, not by label. A solution sequence becomes one blank node
//! per row with a `urn:var:<name>` edge per binding, plus a `urn:index` edge when the order
//! is part of the expectation (only the result-set vocabulary with `rs:index` states that).
//!
//! Numeric literals are compared by value: the expected files use XSD 1.0 lexical forms
//! (`"2.0E-1"^^xsd:double`) where evaluators print `"0.2"`. The same normalisation applies
//! to every backend; all other terms compare exactly.

use oxrdf::graph::CanonicalizationAlgorithm;
use oxrdf::{
    BlankNode, Graph, Literal, NamedNode, NamedOrBlankNode, Term, Triple, TripleRef, Variable,
};
use sparesults::{QueryResultsFormat, QueryResultsParser, SliceQueryResultsParserOutput};

const RS: &str = "http://www.w3.org/2001/sw/DataAccess/tests/result-set#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

#[derive(Debug, PartialEq, Eq)]
pub enum Results {
    Boolean(bool),
    /// Canonicalised solution graph (see the module docs).
    Solutions(Graph),
    /// Canonicalised result graph.
    Graph(Graph),
}

pub fn canonical(graph: Graph) -> Graph {
    let mut graph: Graph = graph
        .iter()
        .map(|triple| {
            let triple = triple.into_owned();
            Triple::new(
                triple.subject,
                triple.predicate,
                numeric_by_value(triple.object),
            )
        })
        .collect();
    graph.canonicalize(CanonicalizationAlgorithm::Unstable);
    graph
}

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// Rewrites a numeric literal's lexical form to a canonical rendering of its value.
fn numeric_by_value(term: Term) -> Term {
    let Term::Literal(literal) = &term else {
        return term;
    };
    let Some(local) = literal.datatype().as_str().strip_prefix(XSD) else {
        return term;
    };
    let value = literal.value().trim();
    let normalized = match local {
        "double" | "float" => match value {
            "INF" | "+INF" => Some("INF".to_owned()),
            "-INF" => Some("-INF".to_owned()),
            "NaN" => Some("NaN".to_owned()),
            _ => match local {
                "float" => value
                    .parse::<f32>()
                    .ok()
                    .map(|v| format!("{:?}", f64::from(v))),
                _ => value.parse::<f64>().ok().map(|v| format!("{v:?}")),
            },
        },
        "decimal" => normalize_decimal(value),
        "integer" | "int" | "long" | "short" | "byte" | "nonNegativeInteger"
        | "positiveInteger" | "nonPositiveInteger" | "negativeInteger" | "unsignedLong"
        | "unsignedInt" | "unsignedShort" | "unsignedByte" => {
            value.parse::<i128>().ok().map(|v| v.to_string())
        }
        _ => None,
    };
    match normalized {
        Some(lexical) => Literal::new_typed_literal(lexical, literal.datatype()).into(),
        None => term,
    }
}

/// `+01.50` → `1.5`, `2` → `2.0`, `-0.0` → `0.0`.
fn normalize_decimal(value: &str) -> Option<String> {
    let (negative, unsigned) = match value.as_bytes().first()? {
        b'-' => (true, &value[1..]),
        b'+' => (false, &value[1..]),
        _ => (false, value),
    };
    let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if !(integer.bytes().chain(fraction.bytes())).all(|b| b.is_ascii_digit())
        || integer.len() + fraction.len() == 0
    {
        return None;
    }
    let integer = integer.trim_start_matches('0');
    let fraction = fraction.trim_end_matches('0');
    let integer = if integer.is_empty() { "0" } else { integer };
    let fraction = if fraction.is_empty() { "0" } else { fraction };
    let zero = integer == "0" && fraction == "0";
    let sign = if negative && !zero { "-" } else { "" };
    Some(format!("{sign}{integer}.{fraction}"))
}

/// Rows of `(variable, value)` bindings; `ordered` adds the row index.
/// One solution: its bindings.
pub type Row = Vec<(Variable, Term)>;

pub fn solutions(rows: impl IntoIterator<Item = Row>, ordered: bool) -> Results {
    let mut graph = Graph::new();
    for (index, row) in rows.into_iter().enumerate() {
        let node = NamedOrBlankNode::from(BlankNode::default());
        if ordered {
            graph.insert(&Triple::new(
                node.clone(),
                NamedNode::new_unchecked("urn:index"),
                Literal::from(index as i64),
            ));
        }
        for (variable, value) in row {
            graph.insert(&Triple::new(
                node.clone(),
                NamedNode::new_unchecked(format!("urn:var:{}", variable.as_str())),
                numeric_by_value(value),
            ));
        }
    }
    graph.canonicalize(CanonicalizationAlgorithm::Unstable);
    Results::Solutions(graph)
}

/// Parses an expected-results file: SPARQL XML/JSON/TSV results, or an RDF graph that is
/// either a `rs:ResultSet` or a CONSTRUCT result.
pub fn parse_expected(
    extension: &str,
    text: &str,
    graph: impl FnOnce() -> Result<Graph, String>,
) -> Result<Results, String> {
    let format = match extension {
        "srx" => QueryResultsFormat::Xml,
        "srj" => QueryResultsFormat::Json,
        "tsv" => QueryResultsFormat::Tsv,
        _ => return Ok(from_graph(graph()?)),
    };
    let parsed = QueryResultsParser::from_format(format)
        .for_slice(text.as_bytes())
        .map_err(|error| error.to_string())?;
    Ok(match parsed {
        SliceQueryResultsParserOutput::Boolean(value) => Results::Boolean(value),
        SliceQueryResultsParserOutput::Solutions(rows) => {
            let rows = rows
                .map(|row| {
                    row.map(|row| {
                        row.iter()
                            .map(|(variable, term)| (variable.clone(), term.clone()))
                            .collect()
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            solutions(rows, false)
        }
    })
}

/// A result graph: `rs:ResultSet` vocabulary if present, else a CONSTRUCT result.
fn from_graph(graph: Graph) -> Results {
    let rs = |local: &str| NamedNode::new_unchecked(format!("{RS}{local}"));
    let result_set = graph
        .subjects_for_predicate_object(&NamedNode::new_unchecked(RDF_TYPE), &rs("ResultSet"))
        .next()
        .map(|node| node.into_owned());
    let Some(result_set) = result_set else {
        return Results::Graph(canonical(graph));
    };
    if let Some(Term::Literal(value)) = graph
        .object_for_subject_predicate(&result_set, &rs("boolean"))
        .map(|term| term.into_owned())
    {
        return Results::Boolean(value.value() == "true");
    }
    let mut ordered = false;
    let mut rows: Vec<(Option<i64>, Row)> = Vec::new();
    for solution in graph.objects_for_subject_predicate(&result_set, &rs("solution")) {
        let Some(solution) = subject(solution.into_owned()) else {
            continue;
        };
        let index = graph
            .object_for_subject_predicate(&solution, &rs("index"))
            .and_then(|term| match term {
                oxrdf::TermRef::Literal(literal) => literal.value().parse().ok(),
                _ => None,
            });
        ordered |= index.is_some();
        let mut row = Vec::new();
        for binding in graph.objects_for_subject_predicate(&solution, &rs("binding")) {
            let Some(binding) = subject(binding.into_owned()) else {
                continue;
            };
            let variable = graph.object_for_subject_predicate(&binding, &rs("variable"));
            let value = graph.object_for_subject_predicate(&binding, &rs("value"));
            if let (Some(oxrdf::TermRef::Literal(variable)), Some(value)) = (variable, value) {
                row.push((
                    Variable::new_unchecked(variable.value()),
                    value.into_owned(),
                ));
            }
        }
        rows.push((index, row));
    }
    rows.sort_by_key(|(index, _)| *index);
    solutions(rows.into_iter().map(|(_, row)| row), ordered)
}

fn subject(term: Term) -> Option<NamedOrBlankNode> {
    match term {
        Term::NamedNode(node) => Some(node.into()),
        Term::BlankNode(node) => Some(node.into()),
        _ => None,
    }
}

/// True if the expected solutions carry an order (then ours are compared in order too).
pub fn is_ordered(results: &Results) -> bool {
    match results {
        Results::Solutions(graph) => graph
            .iter()
            .any(|TripleRef { predicate, .. }| predicate.as_str() == "urn:index"),
        _ => false,
    }
}
