//! Incremental validation (design §7, slice C2): the results a change introduces.
//!
//! A shape's results for a focus node depend only on the statements its paths reach from
//! the node (and from nested shapes' value nodes), on those nodes' types, and on whether
//! the node is a focus node. A changed statement `(s p o)` can therefore only change the
//! results of focus nodes from which `s` or `o` is reachable along a prefix of a path the
//! shape reads: they are found by following those prefixes backwards from `s` and `o`,
//! before and after the change. Nested shapes (`sh:node`, `sh:property`, the logical and
//! qualified components) work on value nodes: their affected nodes are carried back along
//! the parent's path the same way.
//!
//! Only those focus nodes are validated, against the state before and after, and the
//! results present after but not before are the change's. Data that was invalid before
//! stays reported by full validation, not blamed on the change.
//!
//! Some shapes can't be bounded this way, and are validated whole: closed shapes (any
//! predicate counts), SPARQL constraints (a query reads anything), and any shape when a
//! changed statement is an `rdfs:subClassOf` (class membership of every node may move).
//! So is a shape whose affected nodes pass [`MAX_AFFECTED`].

use std::collections::BTreeSet;

use nrese_engine::{EncodedQuad, GraphSelector, TermId};
use nrese_sparql::ReadView;

use crate::graph::{GraphView, Selection};
use crate::model::{Constraint, Path, ShapeRef, Shapes, Target};
use crate::path;
use crate::report::{ValidationReport, decode};
use crate::validate::{RawResult, Validator};

/// Affected focus nodes per shape beyond which the shape is validated whole: following
/// that many nodes back costs about what a full validation does.
const MAX_AFFECTED: usize = 100_000;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

/// The results that `changed` (the statements inserted and deleted, asserted and inferred)
/// introduce in `data`: validation of the focus nodes the change can affect, in `after`,
/// minus the same validation in `before`. Both views must share one dictionary (a
/// transaction and its base snapshot do), and `shapes` must be compiled from it.
pub fn validate_changes<A: ReadView, B: ReadView>(
    before: &A,
    after: &B,
    shapes: &Shapes,
    data: Selection,
    changed: &[EncodedQuad],
) -> ValidationReport {
    let changed: Vec<EncodedQuad> = changed
        .iter()
        .copied()
        .filter(|quad| in_selection(data, quad.graph))
        .collect();
    let graph_before = GraphView::new(before, data);
    let graph_after = GraphView::new(after, data);
    let sub_class_of = graph_after.iri(RDFS_SUB_CLASS_OF);
    let classes_moved = sub_class_of.is_some_and(|p| changed.iter().any(|q| q.predicate == p));
    let validator_before = Validator::new(before, shapes, data);
    let validator_after = Validator::new(after, shapes, data);
    let mut introduced: BTreeSet<RawResult> = BTreeSet::new();
    let mut failures = Vec::new();
    if !changed.is_empty() {
        let rdf_type = graph_after.iri(RDF_TYPE);
        for shape in shapes.targeted() {
            let affected = if classes_moved {
                None
            } else {
                let seeds = seeds(shapes, shape, &changed, rdf_type);
                affected(
                    shapes,
                    shape,
                    &seeds,
                    &graph_before,
                    &graph_after,
                    &mut Vec::new(),
                )
                .filter(|nodes| nodes.len() <= MAX_AFFECTED)
            };
            let focus = |validator_focus: Vec<TermId>| match &affected {
                Some(nodes) => validator_focus
                    .into_iter()
                    .filter(|node| nodes.contains(node))
                    .collect::<Vec<_>>(),
                None => validator_focus,
            };
            let mut after_results = Vec::new();
            for node in focus(validator_after.focus_nodes(&shapes.shapes[shape])) {
                validator_after.validate_node(shape, node, &mut after_results, &mut Vec::new());
            }
            if after_results.is_empty() {
                continue;
            }
            let mut before_results = Vec::new();
            for node in focus(validator_before.focus_nodes(&shapes.shapes[shape])) {
                validator_before.validate_node(shape, node, &mut before_results, &mut Vec::new());
            }
            let before: BTreeSet<RawResult> = before_results.into_iter().collect();
            introduced.extend(after_results.into_iter().filter(|r| !before.contains(r)));
        }
        failures.extend(validator_after.failures.into_inner());
    }
    let raw: Vec<RawResult> = introduced.into_iter().collect();
    decode(after, shapes, &raw, failures)
}

fn in_selection(data: Selection, graph: TermId) -> bool {
    let selected = match data.graphs {
        GraphSelector::Exact(selected) => graph == selected,
        GraphSelector::AnyNamed => !graph.is_default_graph(),
        _ => true,
    };
    selected && !data.excluded.contains(&Some(graph))
}

/// The nodes of the changed statements that can matter to `shape`: subjects and objects
/// of statements whose predicate the shape (with its nested shapes) reads, or that decide
/// its targets. `None` for any predicate.
fn seeds(
    shapes: &Shapes,
    shape: ShapeRef,
    changed: &[EncodedQuad],
    rdf_type: Option<TermId>,
) -> Vec<TermId> {
    let mut read = BTreeSet::new();
    let bounded = predicates(shapes, shape, &mut read, &mut Vec::new());
    if let Some(rdf_type) = rdf_type {
        read.insert(rdf_type);
    }
    let mut nodes: Vec<TermId> = changed
        .iter()
        .filter(|quad| !bounded || read.contains(&quad.predicate))
        .flat_map(|quad| [quad.subject, quad.object])
        .collect();
    nodes.sort_unstable();
    nodes.dedup();
    nodes
}

/// Adds the predicates `shape` reads (paths, property pairs, targets) to `read`, through
/// nested shapes; `false` if it can read any predicate (closed, SPARQL).
fn predicates(
    shapes: &Shapes,
    shape: ShapeRef,
    read: &mut BTreeSet<TermId>,
    seen: &mut Vec<ShapeRef>,
) -> bool {
    if seen.contains(&shape) {
        return true;
    }
    seen.push(shape);
    let shape_ = &shapes.shapes[shape];
    if let Some(path) = &shape_.path {
        path_predicates(path, read);
    }
    for target in &shape_.targets {
        if let Target::SubjectsOf(p) | Target::ObjectsOf(p) = target {
            read.insert(*p);
        }
    }
    let mut bounded = true;
    for constraint in &shape_.constraints {
        bounded &= match constraint {
            Constraint::Closed(_) | Constraint::Sparql(_) => false,
            Constraint::Equals(p)
            | Constraint::Disjoint(p)
            | Constraint::LessThan(p)
            | Constraint::LessThanOrEquals(p) => {
                read.insert(*p);
                true
            }
            _ => nested(constraint)
                .iter()
                .all(|&inner| predicates(shapes, inner, read, seen)),
        };
    }
    bounded
}

fn path_predicates(path: &Path, read: &mut BTreeSet<TermId>) {
    match path {
        Path::Predicate(p) => {
            read.insert(*p);
        }
        Path::Inverse(inner)
        | Path::ZeroOrMore(inner)
        | Path::OneOrMore(inner)
        | Path::ZeroOrOne(inner) => path_predicates(inner, read),
        Path::Sequence(parts) | Path::Alternative(parts) => {
            parts.iter().for_each(|part| path_predicates(part, read));
        }
    }
}

/// The shapes a constraint validates value nodes against.
fn nested(constraint: &Constraint) -> Vec<ShapeRef> {
    match constraint {
        Constraint::Not(shape) | Constraint::Node(shape) | Constraint::Property(shape) => {
            vec![*shape]
        }
        Constraint::Logical(_, shapes) => shapes.clone(),
        Constraint::Qualified {
            shape, siblings, ..
        } => std::iter::once(*shape)
            .chain(siblings.iter().copied())
            .collect(),
        _ => Vec::new(),
    }
}

/// The nodes whose validation against `shape` (as a focus or value node) `seeds` can
/// change, in either state; `None` if unbounded.
fn affected<A: ReadView, B: ReadView>(
    shapes: &Shapes,
    shape: ShapeRef,
    seeds: &[TermId],
    before: &GraphView<'_, A>,
    after: &GraphView<'_, B>,
    stack: &mut Vec<ShapeRef>,
) -> Option<BTreeSet<TermId>> {
    if stack.contains(&shape) {
        // A recursive reference: the shape's own walk covers it.
        return Some(BTreeSet::new());
    }
    stack.push(shape);
    let shape_ = &shapes.shapes[shape];
    // The nodes this shape's checks start from: the seeds themselves (a node's own
    // statements), and, for nested shapes, their affected value nodes.
    let mut reached: Vec<TermId> = seeds.to_vec();
    for constraint in &shape_.constraints {
        match constraint {
            Constraint::Closed(_) | Constraint::Sparql(_) => {
                stack.pop();
                return None;
            }
            _ => {
                for inner in nested(constraint) {
                    let inner = affected(shapes, inner, seeds, before, after, stack);
                    match inner {
                        Some(nodes) => reached.extend(nodes),
                        None => {
                            stack.pop();
                            return None;
                        }
                    }
                }
            }
        }
    }
    stack.pop();
    reached.sort_unstable();
    reached.dedup();
    let mut out: BTreeSet<TermId> = seeds.iter().copied().collect();
    match &shape_.path {
        // A node shape's value is its focus node.
        None => out.extend(reached),
        Some(path) => {
            // Value nodes affected, and the seeds anywhere along the path: back along every
            // prefix of it, in both states.
            for prefix in prefixes(&normalised(path)) {
                for graph_nodes in [
                    path::back(before, &prefix, &reached),
                    path::back(after, &prefix, &reached),
                ] {
                    out.extend(graph_nodes);
                    if out.len() > MAX_AFFECTED {
                        return None;
                    }
                }
            }
        }
    }
    Some(out)
}

/// `path` with inverses pushed down to predicates, so that its prefixes are sequences of
/// forward and backward steps.
fn normalised(path: &Path) -> Path {
    fn inverse(path: &Path) -> Path {
        match path {
            Path::Predicate(_) => Path::Inverse(Box::new(path.clone())),
            Path::Inverse(inner) => normalised(inner),
            Path::Sequence(parts) => Path::Sequence(parts.iter().rev().map(inverse).collect()),
            Path::Alternative(parts) => Path::Alternative(parts.iter().map(inverse).collect()),
            Path::ZeroOrMore(inner) => Path::ZeroOrMore(Box::new(inverse(inner))),
            Path::OneOrMore(inner) => Path::OneOrMore(Box::new(inverse(inner))),
            Path::ZeroOrOne(inner) => Path::ZeroOrOne(Box::new(inverse(inner))),
        }
    }
    match path {
        Path::Predicate(_) => path.clone(),
        Path::Inverse(inner) => inverse(inner),
        Path::Sequence(parts) => Path::Sequence(parts.iter().map(normalised).collect()),
        Path::Alternative(parts) => Path::Alternative(parts.iter().map(normalised).collect()),
        Path::ZeroOrMore(inner) => Path::ZeroOrMore(Box::new(normalised(inner))),
        Path::OneOrMore(inner) => Path::OneOrMore(Box::new(normalised(inner))),
        Path::ZeroOrOne(inner) => Path::ZeroOrOne(Box::new(normalised(inner))),
    }
}

/// Every non-empty prefix of a (normalised) path: the paths along which a node can reach
/// a statement the path reads.
fn prefixes(path: &Path) -> Vec<Path> {
    match path {
        Path::Predicate(_) | Path::Inverse(_) => vec![path.clone()],
        Path::Sequence(parts) => {
            let mut out = Vec::new();
            for (k, part) in parts.iter().enumerate() {
                for tail in prefixes(part) {
                    let mut steps = parts[..k].to_vec();
                    steps.push(tail);
                    out.push(if steps.len() == 1 {
                        steps.pop().expect("one step")
                    } else {
                        Path::Sequence(steps)
                    });
                }
            }
            out
        }
        Path::Alternative(parts) => parts.iter().flat_map(prefixes).collect(),
        // Any number of whole repetitions, then a prefix of one more.
        Path::ZeroOrMore(inner) | Path::OneOrMore(inner) => prefixes(inner)
            .into_iter()
            .map(|tail| Path::Sequence(vec![Path::ZeroOrMore(inner.clone()), tail]))
            .collect(),
        Path::ZeroOrOne(inner) => prefixes(inner),
    }
}
