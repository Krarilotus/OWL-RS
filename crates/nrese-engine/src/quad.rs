//! Encoded quads, triple patterns and the permutation that answers each pattern.

use crate::term::TermId;

/// A quad in canonical `(subject, predicate, object, graph)` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EncodedQuad {
    pub subject: TermId,
    pub predicate: TermId,
    pub object: TermId,
    pub graph: TermId,
}

impl EncodedQuad {
    pub const fn new(subject: TermId, predicate: TermId, object: TermId, graph: TermId) -> Self {
        Self {
            subject,
            predicate,
            object,
            graph,
        }
    }

    pub(crate) const fn components(self) -> [u64; 4] {
        [
            self.subject.raw(),
            self.predicate.raw(),
            self.object.raw(),
            self.graph.raw(),
        ]
    }

    pub(crate) const fn from_components(c: [u64; 4]) -> Self {
        Self::new(
            TermId::from_raw(c[0]),
            TermId::from_raw(c[1]),
            TermId::from_raw(c[2]),
            TermId::from_raw(c[3]),
        )
    }
}

/// A triple: the unit the reasoner derives. Inferred triples are stored as quads in the
/// default graph ([`in_default_graph`](Self::in_default_graph)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EncodedTriple {
    pub subject: TermId,
    pub predicate: TermId,
    pub object: TermId,
}

impl EncodedTriple {
    pub const fn new(subject: TermId, predicate: TermId, object: TermId) -> Self {
        Self {
            subject,
            predicate,
            object,
        }
    }

    pub const fn in_default_graph(self) -> EncodedQuad {
        EncodedQuad::new(
            self.subject,
            self.predicate,
            self.object,
            TermId::DEFAULT_GRAPH,
        )
    }
}

impl From<EncodedQuad> for EncodedTriple {
    /// Drops the graph.
    fn from(quad: EncodedQuad) -> Self {
        Self::new(quad.subject, quad.predicate, quad.object)
    }
}

/// Which graphs a pattern ranges over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GraphSelector {
    /// Every graph, including the default graph.
    Any,
    /// Every named graph, excluding the default graph (SPARQL `GRAPH ?g`).
    AnyNamed,
    /// Exactly one graph ([`TermId::DEFAULT_GRAPH`] for the default graph).
    Exact(TermId),
}

/// A quad pattern; `None` positions are unbound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QuadPattern {
    pub subject: Option<TermId>,
    pub predicate: Option<TermId>,
    pub object: Option<TermId>,
    pub graph: GraphSelector,
}

impl QuadPattern {
    pub const fn all() -> Self {
        Self {
            subject: None,
            predicate: None,
            object: None,
            graph: GraphSelector::Any,
        }
    }

    pub const fn in_graph(graph: TermId) -> Self {
        Self {
            subject: None,
            predicate: None,
            object: None,
            graph: GraphSelector::Exact(graph),
        }
    }

    pub fn matches(&self, quad: &EncodedQuad) -> bool {
        self.subject.is_none_or(|s| s == quad.subject)
            && self.predicate.is_none_or(|p| p == quad.predicate)
            && self.object.is_none_or(|o| o == quad.object)
            && match self.graph {
                GraphSelector::Any => true,
                GraphSelector::AnyNamed => !quad.graph.is_default_graph(),
                GraphSelector::Exact(g) => g == quad.graph,
            }
    }
}

pub(crate) type Key = [u64; 4];

/// The sort orders the index maintains. Every [`QuadPattern`] is answered by a contiguous
/// key range in one of the first six (see [`AccessPlan::for_pattern`]).
///
/// The last two exist for executors that need `(?s p ?o)` sorted by subject (star joins):
/// [`Gpso`](Self::Gpso) in the asserted stack, and [`Psog`](Self::Psog) in the inferred
/// stack, whose single graph makes it sort like GPSO (see `index::Layout`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Permutation {
    Spog = 0,
    Posg = 1,
    Ospg = 2,
    Gspo = 3,
    Gpos = 4,
    Gosp = 5,
    Gpso = 6,
    Psog = 7,
}

impl Permutation {
    /// Number of permutations; sizes the per-run permutation array.
    pub const COUNT: usize = 8;

    pub const ALL: [Self; Self::COUNT] = [
        Self::Spog,
        Self::Posg,
        Self::Ospg,
        Self::Gspo,
        Self::Gpos,
        Self::Gosp,
        Self::Gpso,
        Self::Psog,
    ];

    /// `order()[i]` is the canonical component (0=s,1=p,2=o,3=g) stored at key position `i`.
    pub const fn order(self) -> [usize; 4] {
        match self {
            Self::Spog => [0, 1, 2, 3],
            Self::Posg => [1, 2, 0, 3],
            Self::Ospg => [2, 0, 1, 3],
            Self::Gspo => [3, 0, 1, 2],
            Self::Gpos => [3, 1, 2, 0],
            Self::Gosp => [3, 2, 0, 1],
            Self::Gpso => [3, 1, 0, 2],
            Self::Psog => [1, 0, 2, 3],
        }
    }

    /// For a graph-first permutation, the permutation with the same order over subject,
    /// predicate and object and the graph last (GSPO → SPOG). Over quads that all share one
    /// graph, both sort the same way. `None` for graph-last permutations.
    pub(crate) const fn graph_last(self) -> Option<Self> {
        match self {
            Self::Gspo => Some(Self::Spog),
            Self::Gpos => Some(Self::Posg),
            Self::Gosp => Some(Self::Ospg),
            Self::Gpso => Some(Self::Psog),
            Self::Spog | Self::Posg | Self::Ospg | Self::Psog => None,
        }
    }

    #[inline]
    pub(crate) const fn to_key(self, quad: &EncodedQuad) -> Key {
        let c = quad.components();
        let o = self.order();
        [c[o[0]], c[o[1]], c[o[2]], c[o[3]]]
    }

    #[inline]
    pub(crate) const fn key_to_quad(self, key: &Key) -> EncodedQuad {
        let o = self.order();
        let mut c = [0u64; 4];
        c[o[0]] = key[0];
        c[o[1]] = key[1];
        c[o[2]] = key[2];
        c[o[3]] = key[3];
        EncodedQuad::from_components(c)
    }
}

/// How a pattern is evaluated: an inclusive key range in one permutation plus an optional
/// post-filter that excludes the default graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AccessPlan {
    pub permutation: Permutation,
    pub low: Key,
    pub high: Key,
    pub exclude_default_graph: bool,
}

impl AccessPlan {
    pub fn for_pattern(pattern: &QuadPattern) -> Self {
        let (s, p, o) = (pattern.subject, pattern.predicate, pattern.object);
        let raw = |t: Option<TermId>| t.map(TermId::raw);
        match pattern.graph {
            GraphSelector::Exact(g) => {
                let (permutation, bound) = match (s, p, o) {
                    (Some(_), _, None) | (Some(_), Some(_), Some(_)) | (None, None, None) => {
                        (Permutation::Gspo, [Some(g.raw()), raw(s), raw(p), raw(o)])
                    }
                    (None, Some(_), _) => {
                        (Permutation::Gpos, [Some(g.raw()), raw(p), raw(o), None])
                    }
                    (_, None, Some(_)) => {
                        (Permutation::Gosp, [Some(g.raw()), raw(o), raw(s), None])
                    }
                };
                Self::from_prefix(permutation, bound, false)
            }
            GraphSelector::Any | GraphSelector::AnyNamed => {
                let exclude = pattern.graph == GraphSelector::AnyNamed;
                if s.is_none() && p.is_none() && o.is_none() && exclude {
                    // All named graphs: every key in GSPO with graph >= 1.
                    return Self {
                        permutation: Permutation::Gspo,
                        low: [1, 0, 0, 0],
                        high: [u64::MAX; 4],
                        exclude_default_graph: false,
                    };
                }
                let (permutation, bound) = match (s, p, o) {
                    (Some(_), _, None) | (Some(_), Some(_), Some(_)) | (None, None, None) => {
                        (Permutation::Spog, [raw(s), raw(p), raw(o), None])
                    }
                    (None, Some(_), _) => (Permutation::Posg, [raw(p), raw(o), None, None]),
                    (_, None, Some(_)) => (Permutation::Ospg, [raw(o), raw(s), None, None]),
                };
                Self::from_prefix(permutation, bound, exclude)
            }
        }
    }

    /// The plan that answers `pattern` in `permutation`, if the pattern's bound components
    /// form a prefix of the permutation's order. Executors use it to get a scan in a chosen
    /// sort order; [`for_pattern`](Self::for_pattern) picks one of these per pattern.
    pub(crate) fn in_permutation(pattern: &QuadPattern, permutation: Permutation) -> Option<Self> {
        let graph_first = permutation.order()[0] == 3;
        let (graph, exclude) = match pattern.graph {
            GraphSelector::Exact(g) => (Some(g.raw()), false),
            GraphSelector::Any => (None, false),
            GraphSelector::AnyNamed => {
                if graph_first {
                    // Named graphs are the keys with graph >= 1; nothing after the graph can
                    // be bound, since the graph itself is a range.
                    let unbound = pattern.subject.is_none()
                        && pattern.predicate.is_none()
                        && pattern.object.is_none();
                    return unbound.then_some(Self {
                        permutation,
                        low: [1, 0, 0, 0],
                        high: [u64::MAX; 4],
                        exclude_default_graph: false,
                    });
                }
                (None, true)
            }
        };
        let canonical = [
            pattern.subject.map(TermId::raw),
            pattern.predicate.map(TermId::raw),
            pattern.object.map(TermId::raw),
            graph,
        ];
        let bound = permutation.order().map(|component| canonical[component]);
        let prefix = bound.iter().take_while(|b| b.is_some()).count();
        bound[prefix..]
            .iter()
            .all(Option::is_none)
            .then(|| Self::from_prefix(permutation, bound, exclude))
    }

    /// `bound` lists key positions in permutation order; a bound prefix is followed only by
    /// `None`s (guaranteed by the match above, which is a test invariant).
    fn from_prefix(permutation: Permutation, bound: [Option<u64>; 4], exclude: bool) -> Self {
        let mut low = [0u64; 4];
        let mut high = [u64::MAX; 4];
        for (i, value) in bound.iter().enumerate() {
            match value {
                Some(v) => {
                    low[i] = *v;
                    high[i] = *v;
                }
                None => break,
            }
        }
        Self {
            permutation,
            low,
            high,
            exclude_default_graph: exclude,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::{TermId, TermKind};

    fn id(n: u64) -> TermId {
        TermId::new(TermKind::Iri, n)
    }

    #[test]
    fn permutation_roundtrip() {
        let quad = EncodedQuad::new(id(1), id(2), id(3), id(4));
        for permutation in Permutation::ALL {
            assert_eq!(permutation.key_to_quad(&permutation.to_key(&quad)), quad);
        }
    }

    /// Every combination of bound positions must map to a contiguous prefix range, i.e. the
    /// plan's range is exactly the set of keys matching the pattern (modulo the default-graph
    /// post-filter).
    #[test]
    fn every_pattern_is_a_prefix_range() {
        let quads: Vec<_> = (0..3)
            .flat_map(|s| {
                (0..3)
                    .flat_map(move |p| (0..3).flat_map(move |o| (0..3).map(move |g| (s, p, o, g))))
            })
            .map(|(s, p, o, g)| {
                let graph = if g == 0 {
                    TermId::DEFAULT_GRAPH
                } else {
                    id(10 + g)
                };
                EncodedQuad::new(id(s), id(p), id(o), graph)
            })
            .collect();
        let choices = |v: u64| [None, Some(id(v))];
        let graphs = [
            GraphSelector::Any,
            GraphSelector::AnyNamed,
            GraphSelector::Exact(TermId::DEFAULT_GRAPH),
            GraphSelector::Exact(id(11)),
        ];
        for s in choices(1) {
            for p in choices(2) {
                for o in choices(0) {
                    for graph in graphs {
                        let pattern = QuadPattern {
                            subject: s,
                            predicate: p,
                            object: o,
                            graph,
                        };
                        let plan = AccessPlan::for_pattern(&pattern);
                        let mut via_plan: Vec<_> = quads
                            .iter()
                            .filter(|q| {
                                let key = plan.permutation.to_key(q);
                                key >= plan.low
                                    && key <= plan.high
                                    && !(plan.exclude_default_graph && q.graph.is_default_graph())
                            })
                            .copied()
                            .collect();
                        let mut expected: Vec<_> = quads
                            .iter()
                            .filter(|q| pattern.matches(q))
                            .copied()
                            .collect();
                        via_plan.sort();
                        expected.sort();
                        assert_eq!(via_plan, expected, "pattern {pattern:?}");
                    }
                }
            }
        }
    }
}
