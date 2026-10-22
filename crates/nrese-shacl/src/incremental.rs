//! Incremental raw set difference, retaining the existing affected-node and full-shape
//! decisions. All work draws from one validation part of the request's Resources.
use crate::graph::{GraphView, Selection, contains_within, sort_set, stopped};
use crate::model::{Constraint, Path, ShapeRef, Shapes, Target};
use crate::path;
use crate::path_work::{normalised, prefixes};
use crate::report::{ValidationReport, decode};
use crate::validate::{RawBatch, RawChunk, RawResult, Validator, constrained, reserve_map};
use nrese_engine::{EncodedQuad, GraphSelector, TermId};
use nrese_exec::{Budget, ChargedVec, Reservation, Ticker};
use nrese_sparql::{QueryEvaluationError, ReadView, Resources};
use std::collections::HashMap;
use std::sync::Arc;
type Result<T> = std::result::Result<T, QueryEvaluationError>;
const MAX_AFFECTED: usize = 100_000;
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

pub fn validate_changes<A: ReadView + Sync, B: ReadView + Sync>(
    before: &A,
    after: &B,
    shapes: &Shapes,
    data: Selection,
    changed: &[EncodedQuad],
    resources: &Resources,
) -> Result<ValidationReport> {
    resources.check()?;
    let budget = Arc::new(resources.memory.part());
    let mut ticker = resources.stop().ticker().map_err(stopped)?;
    let mut selected = ChargedVec::new(&*budget);
    for quad in changed {
        ticker.tick(1).map_err(stopped)?;
        if in_selection(data, quad.graph) {
            selected.push(*quad)?;
        }
    }
    let graph_before = GraphView::new(before, data);
    let graph_after = GraphView::new(after, data);
    let sub_class_of = graph_after.iri(RDFS_SUB_CLASS_OF);
    let mut classes_moved = false;
    for quad in selected.iter() {
        ticker.tick(1).map_err(stopped)?;
        classes_moved |= Some(quad.predicate) == sub_class_of;
    }
    let validator_before = Validator::new(before, shapes, data, resources, budget.clone())?;
    let validator_after = Validator::new(after, shapes, data, resources, budget.clone())?;
    let mut introduced = ChargedVec::new(budget.clone());
    let mut chunks = ChargedVec::new(budget.clone());
    let mut seen = ChargedVec::new(&*budget);
    let mut applies = false;
    if !selected.is_empty() {
        let rdf_type = graph_after.iri(RDF_TYPE);
        for shape in shapes.targeted() {
            ticker.tick(1).map_err(stopped)?;
            let affected = if classes_moved {
                None
            } else {
                let seeds = seeds(shapes, shape, &selected, rdf_type, &budget, &mut ticker)?;
                let mut stack = ChargedVec::new(&*budget);
                affected(
                    shapes,
                    shape,
                    &seeds,
                    &graph_before,
                    &graph_after,
                    &mut stack,
                    &budget,
                    &mut ticker,
                )?
                .filter(|nodes| nodes.len() <= MAX_AFFECTED)
            };
            let after_focus = validator_after.focus_nodes(&shapes.shapes[shape], &mut ticker)?;
            let after_work = work(
                shape,
                &after_focus,
                affected.as_deref(),
                &budget,
                &mut ticker,
            )?;
            if !applies && !after_work.is_empty() {
                seen.clear();
                applies = constrained(shapes, shape, &mut seen, &mut ticker)?;
            }
            let mut after_results = validator_after.validate_all(&after_work)?;
            if after_results.len() != 0 {
                let before_focus =
                    validator_before.focus_nodes(&shapes.shapes[shape], &mut ticker)?;
                let before_work = work(
                    shape,
                    &before_focus,
                    affected.as_deref(),
                    &budget,
                    &mut ticker,
                )?;
                let before_results = validator_before.validate_all(&before_work)?;
                let mut earlier = rows(&before_results, &budget)?;
                dedup_rows(&mut earlier, &mut ticker)?;
                let mut later = rows(&after_results, &budget)?;
                dedup_rows(&mut later, &mut ticker)?;
                for row in later.iter() {
                    ticker.tick(1).map_err(stopped)?;
                    if earlier.binary_search(row).is_err() {
                        introduced.push(row.clone())?;
                    }
                }
            }
            for chunk in after_results.chunks.iter_mut() {
                chunk.discard_results();
            }
            let (parts, _slots) = after_results.chunks.into_parts();
            for part in parts {
                chunks.push(part)?;
            }
        }
    }
    dedup_rows(&mut introduced, &mut ticker)?;
    if !introduced.is_empty() {
        let (results, held) = introduced.into_parts();
        chunks.push(RawChunk {
            results,
            failures: Vec::new(),
            held,
            failure_held: None,
        })?;
    }
    ticker.finish().map_err(stopped)?;
    drop(validator_before);
    drop(validator_after);
    drop(selected);
    drop(seen);
    decode(
        after,
        shapes,
        RawBatch {
            chunks,
            budget,
            applicable: applies,
        },
        resources,
    )
}

fn work<'b>(
    shape: ShapeRef,
    nodes: &[TermId],
    affected: Option<&[TermId]>,
    budget: &'b Budget,
    ticker: &mut Ticker,
) -> Result<ChargedVec<(ShapeRef, TermId), &'b Budget>> {
    let mut out = ChargedVec::new(budget);
    for &node in nodes {
        ticker.tick(1).map_err(stopped)?;
        if affected.is_none_or(|a| a.binary_search(&node).is_ok()) {
            out.push((shape, node))?;
        }
    }
    Ok(out)
}
fn rows<'s>(raw: &RawBatch<'s>, budget: &Arc<Budget>) -> Result<ChargedVec<RawResult<'s>>> {
    let mut rows = ChargedVec::with_capacity(budget.clone(), raw.len())?;
    for chunk in raw.chunks.iter() {
        rows.extend_from_slice(&chunk.results)?;
    }
    Ok(rows)
}
fn dedup_rows(rows: &mut ChargedVec<RawResult<'_>>, ticker: &mut Ticker) -> Result<()> {
    ticker.check().map_err(stopped)?;
    rows.sort_unstable();
    ticker.check().map_err(stopped)?;
    let mut kept = 0;
    for read in 0..rows.len() {
        ticker.tick(1).map_err(stopped)?;
        if kept == 0 || rows[read] != rows[kept - 1] {
            rows.swap(kept, read);
            kept += 1;
        }
    }
    rows.truncate(kept);
    Ok(())
}
fn in_selection(data: Selection, graph: TermId) -> bool {
    let selected = match data.graphs {
        GraphSelector::Exact(g) => graph == g,
        GraphSelector::AnyNamed => !graph.is_default_graph(),
        _ => true,
    };
    selected && !data.excluded.contains(&Some(graph))
}
fn nested(constraint: &Constraint) -> impl Iterator<Item = ShapeRef> + '_ {
    let (first, rest): (Option<ShapeRef>, &[ShapeRef]) = match constraint {
        Constraint::Not(s) | Constraint::Node(s) | Constraint::Property(s) => (Some(*s), &[]),
        Constraint::Logical(_, shapes) => (None, shapes),
        Constraint::Qualified {
            shape, siblings, ..
        } => (Some(*shape), siblings),
        _ => (None, &[]),
    };
    first.into_iter().chain(rest.iter().copied())
}
fn insert(
    map: &mut HashMap<TermId, ()>,
    held: &mut Reservation<&Budget>,
    id: TermId,
) -> Result<bool> {
    if map.contains_key(&id) {
        return Ok(false);
    }
    reserve_map(map, held)?;
    map.insert(id, ());
    Ok(true)
}
fn seeds<'b>(
    shapes: &Shapes,
    shape: ShapeRef,
    changed: &[EncodedQuad],
    rdf_type: Option<TermId>,
    budget: &'b Budget,
    ticker: &mut Ticker,
) -> Result<ChargedVec<TermId, &'b Budget>> {
    let mut held = Reservation::new(budget);
    let mut read = HashMap::new();
    let mut seen = ChargedVec::new(budget);
    let bounded = predicates(shapes, shape, &mut read, &mut held, &mut seen, ticker)?;
    if let Some(id) = rdf_type {
        insert(&mut read, &mut held, id)?;
    }
    let mut nodes = ChargedVec::new(budget);
    for quad in changed {
        ticker.tick(1).map_err(stopped)?;
        if !bounded || read.contains_key(&quad.predicate) {
            nodes.extend_from_slice(&[quad.subject, quad.object])?;
        }
    }
    let kept = sort_set(&mut nodes, ticker)?;
    nodes.truncate(kept);
    Ok(nodes)
}
fn predicates(
    shapes: &Shapes,
    shape: ShapeRef,
    read: &mut HashMap<TermId, ()>,
    held: &mut Reservation<&Budget>,
    seen: &mut ChargedVec<ShapeRef, &Budget>,
    ticker: &mut Ticker,
) -> Result<bool> {
    ticker.tick(1).map_err(stopped)?;
    if contains_within(seen, &shape, ticker)? {
        return Ok(true);
    }
    seen.push(shape)?;
    let shape_ = &shapes.shapes[shape];
    if let Some(path) = &shape_.path {
        path_predicates(path, read, held, ticker)?;
    }
    for target in &shape_.targets {
        ticker.tick(1).map_err(stopped)?;
        if let Target::SubjectsOf(p) | Target::ObjectsOf(p) = target {
            insert(read, held, *p)?;
        }
    }
    let mut bounded = true;
    for constraint in &shape_.constraints {
        ticker.tick(1).map_err(stopped)?;
        bounded &= match constraint {
            Constraint::Closed(_) | Constraint::Sparql(_) => false,
            Constraint::Equals(p)
            | Constraint::Disjoint(p)
            | Constraint::LessThan(p)
            | Constraint::LessThanOrEquals(p) => {
                insert(read, held, *p)?;
                true
            }
            _ => {
                let mut all = true;
                for inner in nested(constraint) {
                    if !predicates(shapes, inner, read, held, seen, ticker)? {
                        all = false;
                        break;
                    }
                }
                all
            }
        };
    }
    Ok(bounded)
}
fn path_predicates(
    path: &Path,
    read: &mut HashMap<TermId, ()>,
    held: &mut Reservation<&Budget>,
    ticker: &mut Ticker,
) -> Result<()> {
    ticker.tick(1).map_err(stopped)?;
    match path {
        Path::Predicate(p) => {
            insert(read, held, *p)?;
        }
        Path::Inverse(p) | Path::ZeroOrMore(p) | Path::OneOrMore(p) | Path::ZeroOrOne(p) => {
            path_predicates(p, read, held, ticker)?
        }
        Path::Sequence(parts) | Path::Alternative(parts) => {
            for part in parts {
                path_predicates(part, read, held, ticker)?;
            }
        }
    }
    Ok(())
}
#[expect(
    clippy::too_many_arguments,
    reason = "Existing before/after affected-node walk with shared borrowed admission and stop state."
)]
fn affected<'b, A: ReadView, B: ReadView>(
    shapes: &Shapes,
    shape: ShapeRef,
    seeds: &[TermId],
    before: &GraphView<'_, A>,
    after: &GraphView<'_, B>,
    stack: &mut ChargedVec<ShapeRef, &'b Budget>,
    budget: &'b Budget,
    ticker: &mut Ticker,
) -> Result<Option<ChargedVec<TermId, &'b Budget>>> {
    ticker.tick(1).map_err(stopped)?;
    if contains_within(stack, &shape, ticker)? {
        return Ok(Some(ChargedVec::new(budget)));
    }
    stack.push(shape)?;
    let shape_ = &shapes.shapes[shape];
    let mut reached = ChargedVec::with_capacity(budget, seeds.len())?;
    reached.extend_from_slice(seeds)?;
    for constraint in &shape_.constraints {
        ticker.tick(1).map_err(stopped)?;
        if matches!(constraint, Constraint::Closed(_) | Constraint::Sparql(_)) {
            stack.truncate(stack.len() - 1);
            return Ok(None);
        }
        for inner in nested(constraint) {
            match affected(shapes, inner, seeds, before, after, stack, budget, ticker)? {
                Some(nodes) => reached.extend_from_slice(&nodes)?,
                None => {
                    stack.truncate(stack.len() - 1);
                    return Ok(None);
                }
            }
        }
    }
    stack.truncate(stack.len() - 1);
    let kept = sort_set(&mut reached, ticker)?;
    reached.truncate(kept);
    let mut held = Reservation::new(budget);
    let mut out = HashMap::new();
    for &seed in seeds {
        ticker.tick(1).map_err(stopped)?;
        insert(&mut out, &mut held, seed)?;
    }
    match &shape_.path {
        None => {
            for &node in reached.iter() {
                ticker.tick(1).map_err(stopped)?;
                insert(&mut out, &mut held, node)?;
            }
        }
        Some(path) => {
            let normalised = normalised(path, budget, ticker)?;
            let prefixes = prefixes(&normalised.path, budget, ticker)?;
            for prefix in prefixes.iter() {
                let left = path::back(before, &prefix.path, &reached, budget, ticker)?;
                let right = path::back(after, &prefix.path, &reached, budget, ticker)?;
                for nodes in [&left, &right] {
                    for &node in nodes.iter() {
                        ticker.tick(1).map_err(stopped)?;
                        insert(&mut out, &mut held, node)?;
                    }
                    if out.len() > MAX_AFFECTED {
                        return Ok(None);
                    }
                }
            }
        }
    }
    let mut nodes = ChargedVec::with_capacity(budget, out.len())?;
    for &node in out.keys() {
        nodes.push(node)?;
    }
    let kept = sort_set(&mut nodes, ticker)?;
    nodes.truncate(kept);
    Ok(Some(nodes))
}
