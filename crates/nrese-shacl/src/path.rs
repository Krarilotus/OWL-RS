//! Property path evaluation: the value nodes of a focus node.

use nrese_engine::TermId;
use nrese_sparql::ReadView;

use crate::graph::GraphView;
use crate::model::Path;

/// The nodes `path` reaches from `node`, as a sorted set.
pub(crate) fn values<V: ReadView>(
    graph: &GraphView<'_, V>,
    path: &Path,
    node: TermId,
) -> Vec<TermId> {
    reach(graph, path, &[node], false)
}

/// The nodes that reach any of `from` (a sorted set) along `path`: the path followed
/// backwards.
pub(crate) fn back<V: ReadView>(
    graph: &GraphView<'_, V>,
    path: &Path,
    from: &[TermId],
) -> Vec<TermId> {
    reach(graph, path, from, true)
}

fn union(mut nodes: Vec<TermId>) -> Vec<TermId> {
    nodes.sort_unstable();
    nodes.dedup();
    nodes
}

/// The nodes `path` reaches from any of `from` (a sorted set), following it backwards if
/// `inverse`.
fn reach<V: ReadView>(
    graph: &GraphView<'_, V>,
    path: &Path,
    from: &[TermId],
    inverse: bool,
) -> Vec<TermId> {
    match path {
        Path::Predicate(predicate) => union(
            from.iter()
                .flat_map(|&node| {
                    if inverse {
                        graph.subjects(*predicate, node)
                    } else {
                        graph.objects(node, *predicate)
                    }
                })
                .collect(),
        ),
        Path::Inverse(inner) => reach(graph, inner, from, !inverse),
        Path::Sequence(parts) => {
            let mut nodes = from.to_vec();
            let mut step = |part: &Path| nodes = reach(graph, part, &nodes, inverse);
            if inverse {
                parts.iter().rev().for_each(&mut step);
            } else {
                parts.iter().for_each(&mut step);
            }
            nodes
        }
        Path::Alternative(parts) => union(
            parts
                .iter()
                .flat_map(|part| reach(graph, part, from, inverse))
                .collect(),
        ),
        Path::ZeroOrMore(inner) => closure(graph, inner, from.to_vec(), inverse),
        Path::OneOrMore(inner) => {
            let first = reach(graph, inner, from, inverse);
            closure(graph, inner, first, inverse)
        }
        Path::ZeroOrOne(inner) => {
            let mut nodes = reach(graph, inner, from, inverse);
            nodes.extend_from_slice(from);
            union(nodes)
        }
    }
}

/// `start` and everything `path` reaches from it in any number of steps.
fn closure<V: ReadView>(
    graph: &GraphView<'_, V>,
    path: &Path,
    start: Vec<TermId>,
    inverse: bool,
) -> Vec<TermId> {
    let mut reached = union(start);
    let mut frontier = reached.clone();
    while !frontier.is_empty() {
        let next = reach(graph, path, &frontier, inverse);
        frontier = next
            .into_iter()
            .filter(|node| reached.binary_search(node).is_err())
            .collect();
        reached.extend_from_slice(&frontier);
        reached.sort_unstable();
    }
    reached
}
