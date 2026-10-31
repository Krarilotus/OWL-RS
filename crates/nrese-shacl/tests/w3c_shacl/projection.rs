//! How the W3C SHACL test suite compares a validation report with the expected one
//! ("Validate RDF data", full compliance, in the suite's `index.html`): the expected report
//! is a test's `mf:result` with its results and their paths; of a processor's report only
//! what the suite compares is kept; the two pass if they are isomorphic. Every reader of
//! this comparison passes the cases of `projection-cases.trig`.
//!
//! Kept: the report's `sh:conforms` and `sh:result`; each result's `sh:focusNode`,
//! `sh:resultPath`, `sh:resultSeverity`, `sh:sourceConstraint`,
//! `sh:sourceConstraintComponent`, `sh:sourceShape` and `sh:value` as the terms they are, so
//! a datatype and a blank node's co-references count; its `sh:resultMessage` only where the
//! expected report has that message. The report and each result become blank nodes of their
//! one type; everything else goes, results nested through `sh:detail` too. A path is read
//! strictly and kept as its SPARQL syntax: equal syntax is what isomorphic path structures,
//! each result's cloned apart, come to.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use nrese_rdf::{
    BlankNode, Graph, Literal, NamedNode, NamedOrBlankNode, NamedOrBlankNodeRef, Term, TermRef,
    Triple,
};
use nrese_shacl::PropertyPath;

use crate::{RDF, SH, iri, node};

/// The fields of a result compared as they are.
const FIELDS: [&str; 6] = [
    "focusNode",
    "resultSeverity",
    "sourceConstraint",
    "sourceConstraintComponent",
    "sourceShape",
    "value",
];

/// `Ok` if `actual`, a processor's report graph, is the report `expected` gives at `root`
/// (a test's `mf:result`); else the triples that differ.
pub fn compare(
    expected: &Graph,
    root: NamedOrBlankNodeRef<'_>,
    actual: &Graph,
) -> Result<(), String> {
    let mut wanted = project(expected, root, None)?;
    let message = iri(SH, "resultMessage");
    let messages: BTreeSet<Term> = wanted
        .triples_for_predicate(&message)
        .map(|triple| triple.object.into_owned())
        .collect();
    let mut got = project(actual, report_node(actual)?, Some(&messages))?;
    wanted.canonicalize();
    got.canonicalize();
    if wanted == got {
        return Ok(());
    }
    let (wanted, got) = (rows(&wanted), rows(&got));
    let mut difference = "the reports differ".to_owned();
    for row in wanted.iter().filter(|row| !got.contains(row)) {
        write!(difference, "\n    missing    {row}").unwrap();
    }
    for row in got.iter().filter(|row| !wanted.contains(row)) {
        write!(difference, "\n    unexpected {row}").unwrap();
    }
    if wanted.len() != got.len() {
        write!(
            difference,
            "\n    {} nodes expected, {} given",
            wanted.len(),
            got.len()
        )
        .unwrap();
    } else if wanted == got {
        difference.push_str(": the same fields, but other blank nodes shared");
    }
    Err(difference)
}

/// The report and each result of a projected report as their sorted fields, blank nodes unnamed:
/// how a difference is shown (the comparison itself is isomorphism).
fn rows(graph: &Graph) -> Vec<String> {
    let result = iri(SH, "result");
    let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for triple in graph
        .iter()
        .filter(|triple| triple.predicate != result.as_ref())
    {
        let predicate = match triple.predicate.as_str() {
            p if p == format!("{RDF}type") => "a".to_owned(),
            p => p
                .strip_prefix(SH)
                .map_or_else(|| format!("<{p}>"), |local| format!("sh:{local}")),
        };
        let object = match triple.object {
            TermRef::BlankNode(_) => "_:".to_owned(),
            object => object.to_string(),
        };
        fields
            .entry(triple.subject.to_string())
            .or_default()
            .push(format!("{predicate} {object}"));
    }
    let mut rows: Vec<String> = fields
        .into_values()
        .map(|mut fields| {
            fields.sort();
            fields.join("; ")
        })
        .collect();
    rows.sort();
    rows
}

/// The one `sh:ValidationReport` of a report graph.
pub fn report_node(graph: &Graph) -> Result<NamedOrBlankNodeRef<'_>, String> {
    let (kind, report) = (iri(RDF, "type"), iri(SH, "ValidationReport"));
    let reports: Vec<_> = graph
        .triples_for_predicate(&kind)
        .filter(|triple| triple.object == TermRef::NamedNode(report.as_ref()))
        .map(|triple| triple.subject)
        .collect();
    match reports.as_slice() {
        [report] => Ok(*report),
        _ => Err(format!("{} sh:ValidationReport nodes", reports.len())),
    }
}

/// The report at `root` as the suite compares it, with the messages in `messages` (all
/// of them without).
fn project(
    graph: &Graph,
    root: NamedOrBlankNodeRef<'_>,
    messages: Option<&BTreeSet<Term>>,
) -> Result<Graph, String> {
    let kind = iri(RDF, "type");
    let (conforms, result, path, message) = (
        iri(SH, "conforms"),
        iri(SH, "result"),
        iri(SH, "resultPath"),
        iri(SH, "resultMessage"),
    );
    // The report and each result become one fresh blank node, wherever they appear: a result
    // that is also a focus node or value stays that (Sol, B-1408 r1 S1).
    let results: Vec<NamedOrBlankNodeRef<'_>> = graph
        .objects_for_subject_predicate(root, &result)
        .map(|found| node(found).ok_or("a literal as sh:result"))
        .collect::<Result<_, _>>()?;
    let fresh: BTreeMap<Term, BlankNode> = [root]
        .into_iter()
        .chain(results.iter().copied())
        .map(|node| (Term::from(node.into_owned()), BlankNode::default()))
        .collect();
    let term = |value: TermRef<'_>| -> Term {
        let value = value.into_owned();
        fresh.get(&value).map_or(value, |node| node.clone().into())
    };
    let report = fresh[&Term::from(root.into_owned())].clone();
    let mut out = Graph::new();
    out.insert(&Triple::new(
        report.clone(),
        kind.clone(),
        iri(SH, "ValidationReport"),
    ));
    for value in graph.objects_for_subject_predicate(root, &conforms) {
        out.insert(&Triple::new(report.clone(), conforms.clone(), term(value)));
    }
    for found in results {
        let blank = fresh[&Term::from(found.into_owned())].clone();
        out.insert(&Triple::new(report.clone(), result.clone(), blank.clone()));
        out.insert(&Triple::new(
            blank.clone(),
            kind.clone(),
            iri(SH, "ValidationResult"),
        ));
        for field in FIELDS.map(|local| iri(SH, local)) {
            for value in graph.objects_for_subject_predicate(found, &field) {
                out.insert(&Triple::new(blank.clone(), field.clone(), term(value)));
            }
        }
        for value in graph.objects_for_subject_predicate(found, &path) {
            let read = read_path(graph, value, &mut BTreeSet::new())?;
            let syntax = Literal::new_simple_literal(read.to_string());
            out.insert(&Triple::new(blank.clone(), path.clone(), syntax));
        }
        for value in graph.objects_for_subject_predicate(found, &message) {
            let value = value.into_owned();
            if messages.is_none_or(|kept| kept.contains(&value)) {
                out.insert(&Triple::new(blank.clone(), message.clone(), value));
            }
        }
    }
    Ok(out)
}

/// The value of `predicate` on `subject`, if it has one; two are refused.
fn single<'a>(
    graph: &'a Graph,
    subject: NamedOrBlankNodeRef<'a>,
    predicate: &NamedNode,
) -> Result<Option<TermRef<'a>>, String> {
    let mut values = graph
        .triples_for_subject(subject)
        .filter(|triple| triple.predicate == predicate.as_ref())
        .map(|triple| triple.object);
    let first = values.next();
    match values.next() {
        Some(_) => Err(format!("{subject} has two values of {predicate}")),
        None => Ok(first),
    }
}

/// The path `term` denotes (SHACL §2.3.1): an IRI, a blank node with exactly one of the
/// path predicates, or a list (a sequence). Anything else is refused, not guessed, a path
/// within itself too (`within`: the nodes being read; Sol, B-1408 r1 S2).
fn read_path(
    graph: &Graph,
    term: TermRef<'_>,
    within: &mut BTreeSet<NamedOrBlankNode>,
) -> Result<PropertyPath, String> {
    let cell = match term {
        TermRef::NamedNode(predicate) => {
            return Ok(PropertyPath::Predicate(predicate.into_owned()));
        }
        TermRef::BlankNode(cell) => NamedOrBlankNodeRef::from(cell),
        TermRef::Literal(_) | TermRef::Triple(_) => return Err(format!("{term} as a path")),
    };
    if !within.insert(cell.into_owned()) {
        return Err(format!("{term} is a path within itself"));
    }
    let read = path_at(graph, term, cell, within);
    within.remove(&cell.into_owned());
    read
}

/// The path at the blank node `cell` (`term`), its parts read by [`read_path`].
fn path_at(
    graph: &Graph,
    term: TermRef<'_>,
    cell: NamedOrBlankNodeRef<'_>,
    within: &mut BTreeSet<NamedOrBlankNode>,
) -> Result<PropertyPath, String> {
    let mut forms = Vec::new();
    for local in [
        "inversePath",
        "zeroOrMorePath",
        "oneOrMorePath",
        "zeroOrOnePath",
        "alternativePath",
    ] {
        if let Some(inner) = single(graph, cell, &iri(SH, local))? {
            forms.push((local, inner));
        }
    }
    let list = single(graph, cell, &iri(RDF, "first"))?.is_some();
    Ok(match (forms.as_slice(), list) {
        ([], true) => PropertyPath::Sequence(members(graph, term, within)?),
        ([(form, inner)], false) => {
            if *form == "alternativePath" {
                return Ok(PropertyPath::Alternative(members(graph, *inner, within)?));
            }
            let inner = Box::new(read_path(graph, *inner, within)?);
            match *form {
                "inversePath" => PropertyPath::Inverse(inner),
                "zeroOrMorePath" => PropertyPath::ZeroOrMore(inner),
                "oneOrMorePath" => PropertyPath::OneOrMore(inner),
                _ => PropertyPath::ZeroOrOne(inner),
            }
        }
        _ => return Err(format!("{term} is no path")),
    })
}

/// The paths of the list at `head`, which must end in `rdf:nil` without coming back to a cell.
fn members(
    graph: &Graph,
    head: TermRef<'_>,
    within: &mut BTreeSet<NamedOrBlankNode>,
) -> Result<Vec<PropertyPath>, String> {
    let (first, rest, nil) = (iri(RDF, "first"), iri(RDF, "rest"), iri(RDF, "nil"));
    let mut paths = Vec::new();
    let mut cells = BTreeSet::new();
    let mut current = head;
    while current != TermRef::NamedNode(nil.as_ref()) {
        let cell = match current {
            TermRef::BlankNode(cell) => NamedOrBlankNodeRef::from(cell),
            _ => return Err(format!("{current} in a path list")),
        };
        if !cells.insert(cell.into_owned()) {
            return Err(format!("{cell}: a path list that comes back to itself"));
        }
        let (Some(member), Some(next)) =
            (single(graph, cell, &first)?, single(graph, cell, &rest)?)
        else {
            return Err(format!("{cell} is no list cell"));
        };
        paths.push(read_path(graph, member, within)?);
        current = next;
    }
    Ok(paths)
}
