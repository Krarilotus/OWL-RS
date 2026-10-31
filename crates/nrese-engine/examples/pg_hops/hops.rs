//! The hop operations the property-graph micro-costs measure, over a [`Snapshot`], each
//! counting its work: patterns asked, quads read, seeks and searches from the root, triple-term
//! hash probes and decoded terms. Shared by the `pg_hops` example and its reference test
//! (`tests/pg_hops_reference.rs`), which checks answers and counters against a slow walk.
//!
//! A hop is read two ways:
//! - [`Access::Scan`]: [`Snapshot::quads_for_pattern_in`] per pattern, as the path steps do
//!   (`nrese-sparql/src/native/paths.rs`): a binary search from the root of every run per
//!   pattern. Its seeks are not counted by the engine; they are `runs` per pattern by
//!   construction (`index/mod.rs`, `IndexVersion::scan`), and reported so.
//! - [`Access::Cursor`]: a [`ProbeCursor`] per pattern shape, which finger-searches from where
//!   the last probe of that shape landed; its [`SeekStats`] are the engine's own count.

use nrese_engine::{GraphSelector, ProbeCursor, QuadPattern, ReadModel, Snapshot, TermId};

/// The model every hop reads: the asserted statements (the stores measured hold no inferred
/// ones).
pub const MODEL: ReadModel = ReadModel::Asserted;

/// The pattern `(s, p, o)` in the default graph, where a property graph lives.
pub fn pattern(s: Option<TermId>, p: Option<TermId>, o: Option<TermId>) -> QuadPattern {
    QuadPattern {
        subject: s,
        predicate: p,
        object: o,
        graph: GraphSelector::Exact(TermId::DEFAULT_GRAPH),
    }
}

/// The keys one statement places at its subject's anchor and at its object's, over the store's
/// permutations (`orders`, [`Snapshot::asserted_key_orders`]): a permutation is placed by its
/// first node component, subject or object; predicate and graph never place (the storage
/// proposal's anchor rule, §3).
pub fn anchor_keys(orders: &[[usize; 4]]) -> (u64, u64) {
    let mut keys = (0, 0);
    for order in orders {
        match order.iter().find(|&&c| c == 0 || c == 2) {
            Some(0) => keys.0 += 1,
            _ => keys.1 += 1,
        }
    }
    keys
}

/// What a step cost.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Work {
    /// Patterns asked.
    pub probes: u64,
    /// Quads they matched.
    pub results: u64,
    /// Range lookups in a run (one per run and pattern).
    pub seeks: u64,
    /// Of those, searches from the root.
    pub root_searches: u64,
    /// Triple-term lookups by their components (one hash probe each), and those that found none.
    pub hash_probes: u64,
    pub hash_misses: u64,
    /// Terms decoded from the dictionary.
    pub decoded: u64,
}

impl Work {
    pub fn add(&mut self, other: Work) {
        self.probes += other.probes;
        self.results += other.results;
        self.seeks += other.seeks;
        self.root_searches += other.root_searches;
        self.hash_probes += other.hash_probes;
        self.hash_misses += other.hash_misses;
        self.decoded += other.decoded;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Scan,
    Cursor,
}

/// Reads one pattern shape after another; with [`Access::Cursor`] one cursor per shape, so
/// each keeps its permutation and fingers.
pub struct Reader<'a> {
    snapshot: &'a Snapshot,
    access: Access,
    /// Runs of the asserted stack: the seeks a scan makes per pattern.
    runs: u64,
    cursors: Vec<ProbeCursor<'a>>,
    work: Work,
}

impl<'a> Reader<'a> {
    pub fn new(snapshot: &'a Snapshot, access: Access, runs: u64) -> Self {
        Self {
            snapshot,
            access,
            runs,
            cursors: Vec::new(),
            work: Work::default(),
        }
    }

    /// Calls `f` with the subject, predicate and object of each quad matching `pattern`;
    /// `shape` names its cursor (patterns of one shape share one).
    pub fn each(&mut self, shape: usize, pattern: &QuadPattern, mut f: impl FnMut([TermId; 3])) {
        self.work.probes += 1;
        let mut results = 0;
        match self.access {
            Access::Scan => {
                self.work.seeks += self.runs;
                self.work.root_searches += self.runs;
                for quad in self.snapshot.quads_for_pattern_in(MODEL, pattern) {
                    results += 1;
                    f([quad.subject, quad.predicate, quad.object]);
                }
            }
            Access::Cursor => {
                while self.cursors.len() <= shape {
                    self.cursors.push(self.snapshot.probe_cursor(MODEL));
                }
                self.cursors[shape].for_each(pattern, |quad| {
                    results += 1;
                    f([quad.subject, quad.predicate, quad.object]);
                });
            }
        }
        self.work.results += results;
    }

    /// The work so far, the cursors' seeks included.
    pub fn work(&self) -> Work {
        let mut work = self.work;
        for cursor in &self.cursors {
            work.seeks += cursor.stats().seeks;
            work.root_searches += cursor.stats().root_searches;
        }
        work
    }
}

/// The runs of the asserted stack, as a fresh cursor's first probe counts them (one search
/// from the root per run).
pub fn asserted_runs(snapshot: &Snapshot, any: TermId) -> u64 {
    let mut cursor = snapshot.probe_cursor(MODEL);
    cursor.for_each(&pattern(Some(any), None, None), |_| {});
    cursor.stats().root_searches
}

/// Which end of an edge the hop starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// From the subject: `(node, p, ?)`, the subject-first order.
    Out,
    /// From the object: `(?, p, node)`, the object-first order.
    In,
}

/// The edges `(s, o)` under `predicate` at each of `nodes`, in the order the nodes come.
pub fn neighbours(
    reader: &mut Reader<'_>,
    nodes: &[TermId],
    predicate: TermId,
    direction: Direction,
    edges: &mut Vec<(TermId, TermId)>,
) {
    for &node in nodes {
        let p = match direction {
            Direction::Out => pattern(Some(node), Some(predicate), None),
            Direction::In => pattern(None, Some(predicate), Some(node)),
        };
        reader.each(0, &p, |[s, _, o]| edges.push((s, o)));
    }
}

/// The triple term `<<( s predicate o )>>` of each edge, where the store has one: one hash
/// probe per edge ([`Snapshot::triple_term`]).
pub fn triple_terms(
    snapshot: &Snapshot,
    edges: &[(TermId, TermId)],
    predicate: TermId,
    work: &mut Work,
    terms: &mut Vec<TermId>,
) {
    for &(s, o) in edges {
        work.hash_probes += 1;
        match snapshot.triple_term(s, predicate, o) {
            Some(term) => terms.push(term),
            None => work.hash_misses += 1,
        }
    }
}

/// The subjects of `(?r, reifies, term)` for each term: the edges' reifiers.
pub fn reifiers(reader: &mut Reader<'_>, terms: &[TermId], reifies: TermId, out: &mut Vec<TermId>) {
    for &term in terms {
        reader.each(1, &pattern(None, Some(reifies), Some(term)), |[r, _, _]| {
            out.push(r)
        });
    }
}

/// The objects of `(subject, property, ?v)` for each subject.
pub fn values(
    reader: &mut Reader<'_>,
    subjects: &[TermId],
    property: TermId,
    out: &mut Vec<TermId>,
) {
    for &subject in subjects {
        reader.each(
            2,
            &pattern(Some(subject), Some(property), None),
            |[_, _, v]| out.push(v),
        );
    }
}

/// Decodes each term; the total length of their text (so the decoding isn't optimised away).
pub fn decode(snapshot: &Snapshot, ids: &[TermId], work: &mut Work) -> u64 {
    let mut length = 0;
    for &id in ids {
        work.decoded += 1;
        if let Some(term) = snapshot.decode(id) {
            length += term.to_string().len() as u64;
        }
    }
    length
}

/// The edge property hop: from each of `nodes` along `predicate` in `direction`, the values
/// of `property` on each edge's reifiers (edge, triple term, reifier, value). With `sorted`,
/// each step's inputs are sorted and deduplicated first, so a cursor reads ascending (a
/// batched executor's order); otherwise they come in the order found, as a nested-loop plan
/// reads them.
pub fn edge_property(
    reader: &mut Reader<'_>,
    snapshot: &Snapshot,
    nodes: &[TermId],
    [predicate, reifies, property]: [TermId; 3],
    direction: Direction,
    sorted: bool,
    work: &mut Work,
) -> Vec<TermId> {
    let mut edges = Vec::new();
    neighbours(reader, nodes, predicate, direction, &mut edges);
    let mut terms = Vec::new();
    triple_terms(snapshot, &edges, predicate, work, &mut terms);
    if sorted {
        terms.sort_unstable();
        terms.dedup();
    }
    let mut rs = Vec::new();
    reifiers(reader, &terms, reifies, &mut rs);
    if sorted {
        rs.sort_unstable();
        rs.dedup();
    }
    let mut out = Vec::new();
    values(reader, &rs, property, &mut out);
    out
}

/// Breadth-first search from `start` along `predicate` in both directions (SNB's `knows` is
/// undirected), `hops` levels: the size of each level (the nodes first reached at that
/// distance). With `sorted`, each frontier is read in ascending order, so a cursor gallops;
/// otherwise in the order found, as the path steps read it.
pub fn bfs(
    reader: &mut Reader<'_>,
    start: TermId,
    predicate: TermId,
    hops: usize,
    sorted: bool,
) -> Vec<u64> {
    let mut seen = hashbrown::HashSet::new();
    seen.insert(start);
    let mut frontier = vec![start];
    let mut levels = Vec::with_capacity(hops);
    for _ in 0..hops {
        if sorted {
            frontier.sort_unstable();
        }
        let mut next = Vec::new();
        for direction in [Direction::Out, Direction::In] {
            let mut edges = Vec::new();
            neighbours(reader, &frontier, predicate, direction, &mut edges);
            for (s, o) in edges {
                let other = match direction {
                    Direction::Out => o,
                    Direction::In => s,
                };
                if seen.insert(other) {
                    next.push(other);
                }
            }
        }
        levels.push(next.len() as u64);
        frontier = next;
    }
    levels
}
