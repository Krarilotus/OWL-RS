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

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::sync::Arc;

use nrese_engine::{EncodedQuad, GraphSelector, TermId};
use nrese_exec::Reservation;
use nrese_rdf::Term;
use nrese_sparql::{QueryEvaluationError, ReadView, Resources};

use crate::graph::{GraphView, Selection};
use crate::model::{Constraint, Path, Shape, ShapeRef, Shapes, Target};
use crate::path;
use crate::report::{ValidationReport, decode};
use crate::validate::{self, RawResult, Validator};

/// Affected focus nodes per shape beyond which the shape is validated whole: following
/// that many nodes back costs about what a full validation does.
const MAX_AFFECTED: usize = 100_000;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

/// The results that `changed` (the statements inserted and deleted, asserted and inferred)
/// introduce in `data`: validation of the focus nodes the change can affect, in `after`,
/// minus the same validation in `before`. Both views must share one dictionary (a
/// transaction and its base snapshot do), and `shapes` must be compiled from it.
///
/// Both validations run on `resources` and stop as [`crate::validate`] does: a stop in
/// either state is the outcome, never a difference of partial results.
pub fn validate_changes<A: ReadView + Sync, B: ReadView + Sync>(
    before: &A,
    after: &B,
    shapes: &Shapes,
    data: Selection,
    changed: &[EncodedQuad],
    resources: &Resources,
) -> Result<ValidationReport, QueryEvaluationError> {
    resources.check()?;
    let changed: Vec<EncodedQuad> = changed
        .iter()
        .copied()
        .filter(|quad| in_selection(data, quad.graph))
        .collect();
    let graph_before = GraphView::new(before, data);
    let graph_after = GraphView::new(after, data);
    let sub_class_of = graph_after.iri(RDFS_SUB_CLASS_OF);
    let classes_moved = sub_class_of.is_some_and(|p| changed.iter().any(|q| q.predicate == p));
    // Both states tell a query's terms stored or computed by the state after's dictionary:
    // one term, one form in both ([`ResultTerm`]).
    let frontier = |term: &Term| graph_after.lookup(term);
    // The state before's failures are thrown away: none is made or charged.
    let validator_before =
        Validator::discarding_failures(before, shapes, data, resources).with_frontier(&frontier);
    let validator_after = Validator::new(after, shapes, data, resources);
    let mut introduced: BTreeSet<RawResult> = BTreeSet::new();
    if !changed.is_empty() {
        let rdf_type = graph_after.iri(RDF_TYPE);
        for shape in shapes.targeted() {
            // After a stop in either state the difference means nothing: it is the outcome.
            if validator_after.stopped() || validator_before.stopped() {
                break;
            }
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
            let work = |nodes: Vec<TermId>| -> Vec<(ShapeRef, TermId)> {
                nodes.into_iter().map(|node| (shape, node)).collect()
            };
            let after_results = validator_after.validate_all(&work(focus(
                validator_after.focus_nodes(&shapes.shapes[shape]),
            )));
            if after_results.is_empty() {
                continue;
            }
            let before_results = validator_before.validate_all(&work(focus(
                validator_before.focus_nodes(&shapes.shapes[shape]),
            )));
            // Charged per result found, so counted before the set drops repeated ones.
            let before_bytes: usize = before_results.iter().map(RawResult::retained_bytes).sum();
            let before: BTreeSet<RawResult> = before_results.into_iter().collect();
            // Results kept are charged to their state's validator; those dropped here are
            // returned to it once freed.
            let mut dropped = 0;
            for result in after_results {
                if before.contains(&result) || introduced.contains(&result) {
                    dropped += result.retained_bytes();
                } else {
                    introduced.insert(result);
                }
            }
            validator_after.release(dropped);
            drop(before);
            validator_before.release(before_bytes);
        }
    }
    // A stop in either state is the outcome: what was introduced is freed before the
    // validators (and their charges) go.
    if validator_after.has_stopped() || validator_before.has_stopped() {
        introduced.clear();
    }
    // Only the state after reports its failures, which the report takes over with their
    // charge; the state before's are dropped now, with theirs.
    let (failures, held_after) = validator_after.into_failures()?;
    let (earlier_failures, held_before) = validator_before.into_failures()?;
    drop(earlier_failures);
    let raw: Vec<RawResult> = introduced.into_iter().collect();
    let report = decode(after, shapes, &raw, failures, resources);
    // The raw results are freed, then their charges returned.
    drop(raw);
    drop((held_after, held_before));
    report
}

/// The results a change of the shapes introduces in `data`: full validation against
/// `shapes_after` in `after`, less what full validation against `shapes_before` found in
/// `before` (`None`: the shapes didn't compile before, so nothing was found), with the
/// failures of the state after. Both views must share one dictionary, and each set of
/// shapes be compiled from it. Runs on `resources` as [`crate::validate`] does.
///
/// The state before is validated only to tell its results apart: not at all when the state
/// after has none, and without its failures, which are neither made nor charged. So a
/// commit that removes what failed (an unsupported `sh:entailment`, a broken constraint)
/// can't be refused by what it removes.
///
/// A result is the same finding in either compilation when its focus node, path, value,
/// shape (by node), component, source constraint, the shape's severity and the messages in
/// effect (the result's own, else the shape's) are: compared in place, nothing copied
/// ([`Key`]). The comparison's own storage is charged to `resources` before it is filled,
/// and it checks `resources` as it goes; only the results introduced are decoded.
pub fn validate_shapes_change<A: ReadView + Sync, B: ReadView + Sync>(
    before: &A,
    shapes_before: Option<&Shapes>,
    after: &B,
    shapes_after: &Shapes,
    data: Selection,
    resources: &Resources,
) -> Result<ValidationReport, QueryEvaluationError> {
    let (raw, failures, held_after) = validate::validate_raw(after, shapes_after, data, resources)?;
    // Declared after their charge, so dropped before it, whatever the outcome.
    let mut later = raw;
    if let Some(shapes_before) = shapes_before
        && !later.is_empty()
    {
        // Both states tell a query's terms stored or computed by the state after's
        // dictionary, which the state after's own ids (its shapes' paths included) are of.
        let graph_after = GraphView::new(after, data);
        let frontier = |term: &Term| graph_after.lookup(term);
        let (earlier, held_before) =
            validate::validate_raw_results(before, shapes_before, data, resources, &frontier)?;
        let kept = introduced(&earlier, shapes_before, later, shapes_after, resources);
        // The earlier results are freed, then their charge returned.
        drop(earlier);
        drop(held_before);
        later = kept?;
    }
    let report = decode(after, shapes_after, &later, failures, resources);
    // The raw results are freed, then their charge returned.
    drop(later);
    drop(held_after);
    report
}

/// A result as the same finding in any compilation of the shapes ([`validate_shapes_change`]):
/// borrowed, so comparing copies nothing.
struct Key<'r> {
    result: &'r RawResult,
    shapes: &'r Shapes,
}

impl<'r> Key<'r> {
    fn shape(&self) -> &'r Shape {
        &self.shapes.shapes[self.result.shape]
    }

    /// The messages in effect: the result's own, else its shape's.
    fn messages(&self) -> &'r [TermId] {
        self.result
            .messages
            .as_deref()
            .unwrap_or(&self.shape().messages)
    }

    fn order(&self, other: &Self) -> Ordering {
        let (a, b) = (self.result, other.result);
        a.focus
            .cmp(&b.focus)
            .then_with(|| a.component.cmp(&b.component))
            .then_with(|| self.shape().node.cmp(&other.shape().node))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.value.cmp(&b.value))
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| self.shape().severity.cmp(&other.shape().severity))
            .then_with(|| self.messages().cmp(other.messages()))
    }
}

/// The results of `later` (validated against `shapes_after`) that no result of `earlier`
/// (against `shapes_before`) is the same finding as ([`Key`]); the others are freed. The
/// earlier keys' storage is charged before it is filled; `resources` is checked every
/// [`CHECK_EVERY`] keys. A refusal or a cancellation is the outcome.
fn introduced(
    earlier: &[RawResult],
    shapes_before: &Shapes,
    mut later: Vec<RawResult>,
    shapes_after: &Shapes,
    resources: &Resources,
) -> Result<Vec<RawResult>, QueryEvaluationError> {
    let mut held = Reservation::new(Arc::new(resources.memory.part()));
    held.charge(earlier.len() * std::mem::size_of::<Key<'_>>())?;
    let mut keys: Vec<Key<'_>> = Vec::with_capacity(earlier.len());
    for (i, result) in earlier.iter().enumerate() {
        if i % CHECK_EVERY == 0 {
            resources.check()?;
        }
        keys.push(Key {
            result,
            shapes: shapes_before,
        });
    }
    keys.sort_unstable_by(Key::order);
    // In place: the later results kept stay where they are, the others are freed.
    let mut checked = 0;
    let mut stop = None;
    later.retain(|result| {
        if stop.is_some() {
            return false;
        }
        if checked % CHECK_EVERY == 0
            && let Err(error) = resources.check()
        {
            stop = Some(error);
            return false;
        }
        checked += 1;
        let key = Key {
            result,
            shapes: shapes_after,
        };
        keys.binary_search_by(|earlier| earlier.order(&key))
            .is_err()
    });
    // The keys go before their charge.
    drop(keys);
    drop(held);
    match stop {
        Some(stop) => Err(stop),
        None => Ok(later),
    }
}

/// How many keys a comparison handles between two checks of its resources.
const CHECK_EVERY: usize = 4096;

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
