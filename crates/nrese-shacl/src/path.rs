//! Admitted property-path work over SHACL's selected graph.

use nrese_engine::TermId;
use nrese_exec::{Budget, ChargedVec, Ticker};
use nrese_sparql::{QueryEvaluationError, ReadView};

use crate::graph::{GraphView, sort_set, stopped};
use crate::model::Path;

type Nodes<'b> = ChargedVec<TermId, &'b Budget>;
type Result<T> = std::result::Result<T, QueryEvaluationError>;

pub(crate) fn values<'b, V: ReadView>(
    graph: &GraphView<'_, V>,
    path: &Path,
    node: TermId,
    budget: &'b Budget,
    ticker: &mut Ticker,
) -> Result<Nodes<'b>> {
    reach(graph, path, &[node], false, budget, ticker)
}

pub(crate) fn back<'b, V: ReadView>(
    graph: &GraphView<'_, V>,
    path: &Path,
    from: &[TermId],
    budget: &'b Budget,
    ticker: &mut Ticker,
) -> Result<Nodes<'b>> {
    reach(graph, path, from, true, budget, ticker)
}

fn copy<'b>(from: &[TermId], budget: &'b Budget, ticker: &mut Ticker) -> Result<Nodes<'b>> {
    ticker.check().map_err(stopped)?;
    let mut out = ChargedVec::with_capacity(budget, from.len())?;
    out.extend_from_slice(from)?;
    ticker
        .tick(u32::try_from(from.len()).unwrap_or(u32::MAX))
        .map_err(stopped)?;
    Ok(out)
}

fn union<'b>(mut nodes: Nodes<'b>, ticker: &mut Ticker) -> Result<Nodes<'b>> {
    let kept = sort_set(&mut nodes, ticker)?;
    nodes.truncate(kept);
    Ok(nodes)
}

fn reach<'b, V: ReadView>(
    graph: &GraphView<'_, V>,
    path: &Path,
    from: &[TermId],
    inverse: bool,
    budget: &'b Budget,
    ticker: &mut Ticker,
) -> Result<Nodes<'b>> {
    ticker.tick(1).map_err(stopped)?;
    match path {
        Path::Predicate(predicate) => {
            let mut out = ChargedVec::new(budget);
            for &node in from {
                ticker.tick(1).map_err(stopped)?;
                let next = if inverse {
                    graph.subjects_within(*predicate, node, budget, ticker)?
                } else {
                    graph.objects_within(node, *predicate, budget, ticker)?
                };
                out.extend_from_slice(&next)?;
            }
            union(out, ticker)
        }
        Path::Inverse(inner) => reach(graph, inner, from, !inverse, budget, ticker),
        Path::Sequence(parts) => {
            let mut nodes = copy(from, budget, ticker)?;
            if inverse {
                for part in parts.iter().rev() {
                    nodes = reach(graph, part, &nodes, inverse, budget, ticker)?;
                }
            } else {
                for part in parts {
                    nodes = reach(graph, part, &nodes, inverse, budget, ticker)?;
                }
            }
            Ok(nodes)
        }
        Path::Alternative(parts) => {
            let mut out = ChargedVec::new(budget);
            for part in parts {
                let next = reach(graph, part, from, inverse, budget, ticker)?;
                out.extend_from_slice(&next)?;
            }
            union(out, ticker)
        }
        Path::ZeroOrMore(inner) => closure(
            graph,
            inner,
            copy(from, budget, ticker)?,
            inverse,
            budget,
            ticker,
        ),
        Path::OneOrMore(inner) => {
            let first = reach(graph, inner, from, inverse, budget, ticker)?;
            closure(graph, inner, first, inverse, budget, ticker)
        }
        Path::ZeroOrOne(inner) => {
            let mut out = reach(graph, inner, from, inverse, budget, ticker)?;
            out.extend_from_slice(from)?;
            union(out, ticker)
        }
    }
}

fn closure<'b, V: ReadView>(
    graph: &GraphView<'_, V>,
    path: &Path,
    start: Nodes<'b>,
    inverse: bool,
    budget: &'b Budget,
    ticker: &mut Ticker,
) -> Result<Nodes<'b>> {
    let mut reached = union(start, ticker)?;
    let mut frontier = copy(&reached, budget, ticker)?;
    while !frontier.is_empty() {
        let mut next = reach(graph, path, &frontier, inverse, budget, ticker)?;
        drop(frontier);
        // Retain counts candidates, including the ones already reached.
        let mut kept = 0;
        for read in 0..next.len() {
            ticker.tick(1).map_err(stopped)?;
            if reached.binary_search(&next[read]).is_err() {
                next[kept] = next[read];
                kept += 1;
            }
        }
        next.truncate(kept);
        reached.extend_from_slice(&next)?;
        reached = union(reached, ticker)?;
        frontier = next;
    }
    Ok(reached)
}
