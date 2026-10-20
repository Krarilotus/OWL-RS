//! GeoSPARQL relations in triple patterns (the query-rewrite extension):
//! `?building geo:sfWithin ex:Berlin` holds between two spatial objects, features or
//! geometries, whose geometries stand in the relation.
//!
//! A geometry is a node with a `geo:asWKT`, `geo:asGeoJSON` or `geo:asGML` literal; a feature's geometries are its
//! `geo:hasDefaultGeometry`, or else its `geo:hasGeometry` ones; the relation holds if it
//! holds for some pair of their shapes (in one reference system). Relations are computed
//! from the geometries, and read from the statements that assert them: a pattern matches
//! both.
//!
//! An R-tree over the shapes' bounding boxes finds the candidates: only the disjointness
//! relations (`sfDisjoint`, `ehDisjoint`, `rcc8dc`) look at every object. The index is
//! built from the statements of all graphs at the first spatial pattern and kept while the
//! snapshot is the latest one queried; a pattern with a constant side runs first and
//! starts the basic graph pattern's joins, others run after them.
//!
//! A pattern reads the active graph alone ([`Scope`]): the dataset's default graph (one
//! graph, or the merge of its graphs) or the named graph `GRAPH` reads, as the reader's
//! access restricts them. The index keeps no graph per entry: its shapes are placements in
//! two arrays, geometries' and features', each ordered graph by graph, and an R-tree entry
//! is a placement's position, whose graph is the run it falls in ([`SpatialIndex::runs`]).
//! Only a feature's shape whose link and geometry are in different graphs, or that a
//! default geometry in another graph may hide, is kept apart with its graphs
//! ([`Crossing`]). A query takes the candidates its scope shows: a graph outside it gives
//! no geometry, feature, default geometry, stated relation or filter candidate, and so
//! moves no answer or count. Where the scope shows every graph the index holds (one graph,
//! the common case) nothing is checked. A literal is parsed once, however many statements
//! give it. `GRAPH ?g` reads spatial relations graph by graph, like paths.
//!
//! Memory: everything the index and the patterns over it allocate is charged before it is
//! allocated, and stays charged while it lives. The index to a part of the request that
//! builds it (kept with the index, in the cache and in queries still reading a replaced
//! one); a pattern's candidates, intermediate lists and result to the evaluation's budget,
//! the result handed on with its charge ([`Context::consumed`] releases it). Vectors are
//! [`ChargedVec`]s; a growth is charged before it grows, the new capacity beside the old;
//! a literal's decoder bound (from its stored key) is reserved before it is decoded, its
//! parser's (from its text) before it is parsed, both trimmed to the shape after, and so is
//! a filter constant's; the R-trees are packed arrays ([`Packed`]) sized before they are
//! filled. Computing a relation, geo's working memory is charged first, from a bound on
//! the two shapes ([`relate_bound`]); a result's header (its variables, its columns' list)
//! is charged and stays so with the evaluation. So is the engine's working memory for a
//! scan (its merge's cursors and decoded blocks) and a lookup, before each runs.

use std::collections::HashMap;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

use nrese_engine::{
    GraphSelector, QuadPattern, ReadModel, Snapshot, SnapshotIdentity, TermId, TermKind,
};
use nrese_exec::{Budget, BudgetExceeded, ChargedVec, IdTable, Reservation, UNDEF};
use nrese_rdf::{Literal, NamedNodeRef, Term, Variable};
use nrese_sparql_syntax::algebra::{Expression, Function};
use nrese_sparql_syntax::term::{NamedNodePattern, TermPattern, TriplePattern};
use rayon::prelude::*;

use super::geo::{
    RELATIONS, Shape, Size, holds_apart, parse_literal, relate_bound, relation, sized_bound,
};
use super::{
    CancellationToken, Context, GraphScope, NativeResult, PARALLEL_EXPRESSION_ROWS,
    QueryEvaluationError, Solutions, unsupported,
};

const GEO: &str = "http://www.opengis.net/ont/geosparql#";

/// The properties from a geometry to its literals.
const SERIALISATIONS: [&str; 3] = ["asWKT", "asGeoJSON", "asGML"];

/// A bounding box: least x and y, greatest x and y.
type Bbox = [f64; 4];

/// The budget an index is charged to: a part of the request envelope that built it, kept
/// (with the server's share) while the index lives.
type Held = Reservation<Arc<Budget>>;
/// A vector charged to that budget as it grows.
type Charged<T> = ChargedVec<T, Arc<Budget>>;
/// A vector charged to an evaluation's budget as it grows.
type Scratch<'b, T> = ChargedVec<T, &'b Budget>;

/// The flag of a value of [`SpatialIndex::objects`] that numbers a list of positions
/// ([`SpatialIndex::several`]), and of a literal's slot that numbers a list of graphs.
/// Positions are 64-bit, so no index that fits in memory reaches it.
const LIST: u64 = 1 << 63;

/// Entries a leaf of a [`Packed`] R-tree holds at most, and the fan-out its splits aim
/// at: a node of height `h` holds at most `NODE`^`h` entries.
const NODE: usize = 6;
/// Inner levels a [`Packed`] R-tree can have, and one: `NODE`²⁵ entries exceed 2⁶⁴.
const LEVELS: usize = 25;

/// A static R-tree, packed in two arrays charged to the index's budget: the entries, and
/// above them, level by level from the top, the inner nodes. It is built top-down, as
/// rstar's bulk load is (overlap-minimising): a node cuts its entries into slabs by `y` and
/// each slab into parts by `x`, as many of each as its subtrees need, so that a subtree's
/// entries are a run and its boxes overlap little whatever the entries' layout (it checks
/// as few boxes as rstar's tree would). Every leaf is at the same depth.
/// Its node count follows from the entry count alone, so its memory is exactly its
/// arrays', charged before they are filled; a query descends from the top without
/// allocating.
struct Packed<D> {
    entries: Charged<(Bbox, D)>,
    nodes: Charged<Node>,
    /// Where each inner level starts in `nodes`, top first; `levels[depth]` is the end.
    levels: [usize; LEVELS],
    depth: usize,
}

/// An inner node of a [`Packed`] R-tree: the box of its entries, and where its children
/// start in the level below (in the entries, for the lowest level). They end where the
/// next node's start, or with the level below.
#[derive(Debug, Clone, Copy)]
struct Node {
    bbox: Bbox,
    children: usize,
}

fn meets(a: &Bbox, b: &Bbox) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

fn union<'a>(boxes: impl Iterator<Item = &'a Bbox>) -> Bbox {
    boxes.fold(
        [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ],
        |u, b| {
            [
                u[0].min(b[0]),
                u[1].min(b[1]),
                u[2].max(b[2]),
                u[3].max(b[3]),
            ]
        },
    )
}

/// The size of the `j`th of `k` runs `len` items are cut into: ⌈`len`/`k`⌉ each, but the
/// last (which may be smaller, or empty).
fn run(len: usize, k: usize, j: usize) -> usize {
    let size = len.div_ceil(k);
    size.min(len.saturating_sub(j * size))
}

/// The slabs (and parts per slab) the `len` entries of a node of `height` are cut into:
/// enough that each part fits a child, `NODE`^(`height` − 1) entries.
fn across(len: usize, height: usize) -> usize {
    let child = NODE.checked_pow(height as u32 - 1).unwrap_or(usize::MAX);
    let parts = len.div_ceil(child);
    (1..)
        .find(|c: &usize| c * c >= parts)
        .expect("a square root")
}

/// Counts, level by level from `level`, the inner nodes under a node of `height` with
/// `len` entries: its children, and theirs while they have inner children.
fn count(len: usize, height: usize, level: usize, sizes: &mut [usize; LEVELS]) {
    let across = across(len, height);
    for slab in (0..across).map(|i| run(len, across, i)) {
        for part in (0..across).map(|j| run(slab, across, j)) {
            if part > 0 {
                sizes[level] += 1;
                if height > 2 {
                    count(part, height - 1, level + 1, sizes);
                }
            }
        }
    }
}

/// `items` reordered into `k` runs ([`run`]), each run's box centres on `axis` no greater
/// than the next run's.
fn cut<D>(items: &mut [(Bbox, D)], k: usize, axis: usize) {
    // Twice a box's centre on the axis.
    let centre = |b: &Bbox| b[axis] + b[axis + 2];
    let len = items.len();
    let mut rest = items;
    for j in 0..k - 1 {
        let size = run(len, k, j);
        if size == 0 || size >= rest.len() {
            break;
        }
        rest.select_nth_unstable_by(size, |a, b| centre(&a.0).total_cmp(&centre(&b.0)));
        rest = &mut rest[size..];
    }
}

impl<D: Copy> Packed<D> {
    /// The tree of `entries`, ordered in place; its inner nodes charged to `budget` first.
    fn pack(mut entries: Charged<(Bbox, D)>, budget: &Arc<Budget>) -> Result<Self, BudgetExceeded> {
        let n = entries.len();
        let mut height = 1;
        let mut capacity = NODE;
        while capacity < n {
            capacity = capacity.saturating_mul(NODE);
            height += 1;
        }
        // Stored are the root's children and the levels below them.
        let depth = height - 1;
        let mut sizes = [0; LEVELS];
        if depth > 0 {
            count(n, height, 0, &mut sizes);
        }
        let mut nodes = ChargedVec::with_capacity(Arc::clone(budget), sizes.iter().sum())?;
        let mut levels = [0; LEVELS];
        if depth > 0 {
            // Level by level from the root's children: a node's `children` is its first
            // entry until it is split itself, its entries running to the next node's.
            Self::split(&mut entries, 0, height, &mut nodes)?;
            levels[1] = nodes.len();
            for level in 0..depth - 1 {
                let (from, to) = (levels[level], levels[level + 1]);
                for i in from..to {
                    let first = nodes[i].children;
                    let last = if i + 1 < to { nodes[i + 1].children } else { n };
                    nodes[i].children = nodes.len() - to;
                    let height = height - 1 - level;
                    Self::split(&mut entries[first..last], first, height, &mut nodes)?;
                }
                levels[level + 2] = nodes.len();
            }
        }
        // The boxes, from the lowest level up.
        for level in (0..depth).rev() {
            let (from, to) = (levels[level], levels[level + 1]);
            let lowest = level + 1 == depth;
            let below = if lowest { n } else { levels[level + 2] - to };
            for i in from..to {
                let first = nodes[i].children;
                let last = if i + 1 < to {
                    nodes[i + 1].children
                } else {
                    below
                };
                nodes[i].bbox = match lowest {
                    true => union(entries[first..last].iter().map(|(b, _)| b)),
                    false => union(nodes[to + first..to + last].iter().map(|c| &c.bbox)),
                };
            }
        }
        Ok(Self {
            entries,
            nodes,
            levels,
            depth,
        })
    }

    /// The children of a node of `height` whose entries are `entries`, from position
    /// `offset`: the entries cut into slabs by `y`, each into parts by `x`, and a child
    /// pushed onto `nodes` for each part, with its first entry.
    fn split(
        entries: &mut [(Bbox, D)],
        offset: usize,
        height: usize,
        nodes: &mut Charged<Node>,
    ) -> Result<(), BudgetExceeded> {
        let len = entries.len();
        let across = across(len, height);
        cut(entries, across, 1);
        let mut start = 0;
        for i in 0..across {
            let slab = run(len, across, i);
            cut(&mut entries[start..start + slab], across, 0);
            for j in 0..across {
                let part = run(slab, across, j);
                if part > 0 {
                    nodes.push(Node {
                        bbox: [0.0; 4],
                        children: offset + start,
                    })?;
                }
                start += part;
            }
        }
        Ok(())
    }

    /// `found` for the data of every entry whose box meets `query`.
    fn search(&self, query: &Bbox, found: &mut impl FnMut(D)) {
        if self.depth == 0 {
            for (b, data) in self.entries.iter() {
                if meets(b, query) {
                    found(*data);
                }
            }
            return;
        }
        self.down(0, 0, self.levels[1], query, found);
    }

    /// [`Self::search`] in the nodes `from..to` of inner level `level`.
    fn down(&self, level: usize, from: usize, to: usize, query: &Bbox, found: &mut impl FnMut(D)) {
        let (start, end) = (self.levels[level], self.levels[level + 1]);
        let lowest = level + 1 == self.depth;
        let below = if lowest {
            self.entries.len()
        } else {
            self.levels[level + 2] - end
        };
        for i in start + from..start + to {
            let node = &self.nodes[i];
            if !meets(&node.bbox, query) {
                continue;
            }
            let last = if i + 1 < end {
                self.nodes[i + 1].children
            } else {
                below
            };
            if lowest {
                for (b, data) in &self.entries[node.children..last] {
                    if meets(b, query) {
                        found(*data);
                    }
                }
            } else {
                self.down(level + 1, node.children, last, query, found);
            }
        }
    }
}

/// One shape of an object: a geometry's from one of its literals, or a feature's through
/// one of its links.
struct Placement {
    object: u64,
    shape: Arc<Shape>,
}

/// Where a feature's shape comes from when not all from one graph: the graph of its link
/// (and whether it is `geo:hasDefaultGeometry`) and of its geometry's literal.
#[derive(Debug, Clone, Copy)]
struct Crossing {
    link: TermId,
    default: bool,
    geometry: TermId,
}

struct SpatialIndex {
    /// Geometry placements (positions `0..geometries.len()`), graph by graph.
    geometries: Charged<Placement>,
    /// Feature placements (the positions after the geometries'): graph by graph those
    /// of one graph, then from [`Self::crossing_from`] the crossing ones.
    features: Charged<Placement>,
    /// Where each graph's run of positions starts, in position order.
    runs: Charged<(u64, TermId)>,
    crossing_from: u64,
    /// Beside each crossing placement, its graphs.
    crossings: Charged<Crossing>,
    /// For a feature with a crossing `geo:hasGeometry` shape, the graphs of its
    /// `geo:hasDefaultGeometry` statements (which hide that shape where shown).
    defaults: HashMap<u64, Vec<TermId>>,
    /// Per object, its placement's position, or [`LIST`] and the number of its positions in
    /// [`Self::several`].
    objects: HashMap<u64, u64>,
    several: Charged<Vec<u64>>,
    /// The placements' bounding boxes, each with its position.
    tree: Packed<u64>,
    /// The positions of the placements without one: empty shapes.
    empty: Charged<u64>,
    /// The largest of its shapes' sizes in every part: a relation's bound with it bounds
    /// the relation with any of its shapes ([`Workspace::cover`]).
    largest: Size,
    /// The geometry literals, for filters over them: each with the position of its first
    /// placement (its shape, and its graph by the run), or, given by several graphs, [`LIST`]
    /// and the number of that position and the graphs in [`Self::spread`].
    literals: Packed<(u64, u64)>,
    spread: Charged<(u64, Vec<TermId>)>,
    /// Relation statements in the data: per relation, subject to objects and back, each
    /// with the graph that states it.
    asserted: HashMap<(&'static str, u64), Vec<(u64, TermId)>>,
    asserted_back: HashMap<(&'static str, u64), Vec<(u64, TermId)>>,
    /// The graphs of the statements read, sorted, each once.
    graphs: Charged<TermId>,
    /// What the shapes, the maps and their lists hold (the vectors hold their own).
    _held: Held,
}

/// The graphs whose statements a spatial pattern reads: the active graph (the search's
/// corpus is scoped the same way).
#[derive(Debug)]
enum Scope {
    /// One graph, every graph, or every named graph.
    Graphs(GraphSelector),
    /// The merge of these graphs, sorted and each once.
    Merge(Charged<TermId>),
}

impl Scope {
    /// The merge of `graphs`, its list charged to `budget`.
    fn merge(graphs: &[TermId], budget: &Arc<Budget>) -> Result<Self, BudgetExceeded> {
        let mut merged = ChargedVec::with_capacity(Arc::clone(budget), graphs.len())?;
        merged.extend_from_slice(graphs)?;
        merged.sort_unstable();
        dedup(&mut merged);
        Ok(Self::Merge(merged))
    }

    fn contains(&self, graph: TermId) -> bool {
        match self {
            Self::Graphs(GraphSelector::Any) => true,
            Self::Graphs(GraphSelector::AnyNamed) => graph != TermId::DEFAULT_GRAPH,
            Self::Graphs(GraphSelector::Exact(g)) => graph == *g,
            Self::Merge(graphs) => graphs.binary_search(&graph).is_ok(),
        }
    }
}

type Cached = (SnapshotIdentity, ReadModel, Arc<SpatialIndex>);
static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

/// The relation a triple pattern's predicate names, if it is one.
fn relation_of(triple: &TriplePattern) -> Option<&str> {
    let NamedNodePattern::NamedNode(n) = &triple.predicate else {
        return None;
    };
    let local = n.as_str().strip_prefix(GEO)?;
    RELATIONS.contains(&local).then_some(local)
}

pub(super) fn is_spatial(triple: &TriplePattern) -> bool {
    relation_of(triple).is_some()
}

fn constant(term: &TermPattern) -> bool {
    matches!(term, TermPattern::NamedNode(_))
}

/// The spatial patterns of `triples` with a constant side, those without, and the rest;
/// `None` if there is no spatial pattern.
pub(super) fn split(
    triples: &[TriplePattern],
) -> Option<(Vec<TriplePattern>, Vec<TriplePattern>, Vec<TriplePattern>)> {
    if !triples.iter().any(is_spatial) {
        return None;
    }
    let (mut first, mut later, mut rest) = (Vec::new(), Vec::new(), Vec::new());
    for triple in triples {
        if !is_spatial(triple) {
            rest.push(triple.clone());
        } else if constant(&triple.subject) || constant(&triple.object) {
            first.push(triple.clone());
        } else {
            later.push(triple.clone());
        }
    }
    Some((first, later, rest))
}

fn bbox(shape: &Shape) -> Option<Bbox> {
    use geo::BoundingRect;
    let rect = shape.geometry.bounding_rect()?;
    Some([rect.min().x, rect.min().y, rect.max().x, rect.max().y])
}

fn local_of(local: &str) -> Option<&'static str> {
    RELATIONS.iter().copied().find(|r| *r == local)
}

/// The position of the `n`th placement.
fn position(n: usize) -> u64 {
    n as u64
}

/// The graph of the run `p` falls in.
fn run_of(runs: &[(u64, TermId)], p: u64) -> TermId {
    runs[runs.partition_point(|&(start, _)| start <= p) - 1].1
}

/// `items` without repeats of adjacent equal ones, in place.
fn dedup<T: PartialEq + Copy, B: Deref<Target = Budget>>(items: &mut ChargedVec<T, B>) {
    let mut kept = 0;
    for i in 0..items.len() {
        if kept == 0 || items[i] != items[kept - 1] {
            items[kept] = items[i];
            kept += 1;
        }
    }
    items.truncate(kept);
}

/// The bytes a hash map holds at `capacity` (hashbrown's table, which `std`'s map is): a
/// power of two of buckets, at most 7/8 full from eight on, each an entry and a control
/// byte, a group of control bytes more, and at most a group's alignment between them.
fn map_bytes<K, V>(capacity: usize) -> usize {
    let buckets = match capacity {
        0 => return 0,
        1..4 => 4,
        4..8 => 8,
        _ => (capacity.saturating_mul(8) / 7).next_power_of_two(),
    };
    buckets * (size_of::<(K, V)>() + 1) + 32
}

/// Room in `map` for one more entry. Its next table is charged to `held` before it is
/// allocated, beside the table it replaces, which is released once the entries moved.
fn map_room<K: std::hash::Hash + Eq, V, B: Deref<Target = Budget>>(
    held: &mut Reservation<B>,
    map: &mut HashMap<K, V>,
) -> Result<(), BudgetExceeded> {
    if map.len() < map.capacity() {
        return Ok(());
    }
    let next = map.capacity().saturating_mul(2).max(3);
    held.charge(map_bytes::<K, V>(next))?;
    let old = map_bytes::<K, V>(map.capacity());
    map.reserve(next - map.len());
    held.release(old);
    Ok(())
}

/// `item` pushed onto a small list whose growth is charged to `held` first: the new
/// capacity beside the old while the items move.
fn push<T, B: Deref<Target = Budget>>(
    held: &mut Reservation<B>,
    list: &mut Vec<T>,
    item: T,
) -> Result<(), BudgetExceeded> {
    if list.len() == list.capacity() {
        let old = list.capacity();
        let next = old.saturating_mul(2).max(4);
        held.charge(next * size_of::<T>())?;
        list.reserve_exact(next - list.len());
        held.release(old * size_of::<T>());
    }
    list.push(item);
    Ok(())
}

/// The bytes a parsed shape holds: its allocation and the geometry's coordinates and
/// parts.
fn shape_bytes(shape: &Shape) -> usize {
    use geo::{Coord, Geometry, LineString, Point, Polygon};
    fn line(l: &LineString<f64>) -> usize {
        l.0.capacity() * size_of::<Coord<f64>>()
    }
    fn polygon(p: &Polygon<f64>) -> usize {
        line(p.exterior())
            + size_of_val(p.interiors())
            + p.interiors().iter().map(line).sum::<usize>()
    }
    fn geometry(g: &Geometry<f64>) -> usize {
        match g {
            Geometry::Point(_) | Geometry::Line(_) | Geometry::Rect(_) | Geometry::Triangle(_) => 0,
            Geometry::LineString(l) => line(l),
            Geometry::Polygon(p) => polygon(p),
            Geometry::MultiPoint(m) => m.0.capacity() * size_of::<Point<f64>>(),
            Geometry::MultiLineString(m) => {
                m.0.capacity() * size_of::<LineString<f64>>() + m.0.iter().map(line).sum::<usize>()
            }
            Geometry::MultiPolygon(m) => {
                m.0.capacity() * size_of::<Polygon<f64>>() + m.0.iter().map(polygon).sum::<usize>()
            }
            Geometry::GeometryCollection(c) => {
                c.0.capacity() * size_of::<Geometry<f64>>()
                    + c.0.iter().map(geometry).sum::<usize>()
            }
        }
    }
    2 * size_of::<usize>() + size_of::<Shape>() + shape.crs.capacity() + geometry(&shape.geometry)
}

/// What parsing a literal of `text` bytes may allocate, reserved before it parses: its
/// parser's tokens and tree (each value takes a character of the text), the geometry's
/// coordinates, and vectors that grow to twice what they hold.
fn shape_bound(text: usize) -> usize {
    text.saturating_mul(64).saturating_add(4096)
}

/// The shape of the literal `id` (`None`: not a shape), what decoding and parsing it
/// allocate charged to `held` before they do: the decoder's bound, then the text's and
/// the parser's ([`shape_bound`]); `held` keeps the shape's bytes.
fn decoded(
    snapshot: &Snapshot,
    held: &mut Held,
    id: TermId,
) -> Result<Option<Arc<Shape>>, BudgetExceeded> {
    // A geometry literal has a datatype, and is no inline value.
    if id.kind() != TermKind::TypedLiteral {
        return Ok(None);
    }
    let mut decoding = 0;
    let term = snapshot.decode_charged(id, |bytes| {
        decoding = bytes;
        held.charge(bytes)
    })?;
    let Some(Term::Literal(literal)) = &term else {
        held.release(decoding);
        return Ok(None);
    };
    let (value, datatype) = (literal.value(), literal.datatype().as_str());
    let bound = value.len() + datatype.len() + shape_bound(value.len());
    if let Err(refused) = held.resize(held.bytes() - decoding + bound) {
        held.release(decoding);
        return Err(refused);
    }
    let shape = parse_literal(value, datatype);
    drop(term);
    let kept = shape.as_ref().map_or(0, shape_bytes);
    held.resize(held.bytes() - bound + kept)?;
    Ok(shape.map(Arc::new))
}

/// Records that `object` has a placement at `p`.
fn place(
    held: &mut Held,
    objects: &mut HashMap<u64, u64>,
    several: &mut Charged<Vec<u64>>,
    object: u64,
    p: u64,
) -> Result<(), BudgetExceeded> {
    use std::collections::hash_map::Entry;
    map_room(held, objects)?;
    match objects.entry(object) {
        Entry::Vacant(entry) => {
            entry.insert(p);
        }
        Entry::Occupied(mut entry) => {
            let value = *entry.get();
            if value & LIST != 0 {
                push(held, &mut several[(value & !LIST) as usize], p)?;
            } else {
                let mut list = Vec::new();
                push(held, &mut list, value)?;
                push(held, &mut list, p)?;
                several.push(list)?;
                *entry.get_mut() = position(several.len() - 1) | LIST;
            }
        }
    }
    Ok(())
}

/// Orders statements graph by graph (if not so already).
fn by_graph(statements: &mut [(u64, u64, TermId)]) {
    if !statements.is_sorted_by_key(|s| s.2) {
        statements.sort_unstable_by_key(|s| s.2);
    }
}

impl SpatialIndex {
    /// The index of `snapshot`'s statements in `model`, everything it allocates (while it
    /// builds, and what it keeps) charged to `budget` first; `Err` if the budget has no
    /// room, with nothing left charged.
    fn build(
        snapshot: &Snapshot,
        model: ReadModel,
        budget: &Arc<Budget>,
    ) -> Result<Self, BudgetExceeded> {
        let mut held: Held = Reservation::new(Arc::clone(budget));
        // The statements of a predicate: subject, object, graph.
        let read = |local: &str| -> Result<Charged<(u64, u64, TermId)>, BudgetExceeded> {
            let mut statements = ChargedVec::new(Arc::clone(budget));
            let mut naming = Reservation::new(&**budget);
            naming.charge(GEO.len() + local.len())?;
            let mut name = String::with_capacity(GEO.len() + local.len());
            name.push_str(GEO);
            name.push_str(local);
            let term = NamedNodeRef::new_unchecked(&name).into();
            let Some(predicate) = snapshot.lookup_charged(term, |bytes| naming.charge(bytes))?
            else {
                return Ok(statements);
            };
            drop((name, naming));
            // The engine's merge while it reads (its cursors, a decoded block per run).
            let mut scanning = Reservation::new(&**budget);
            scanning.charge(snapshot.scan_bytes(model))?;
            let pattern = QuadPattern {
                subject: None,
                predicate: Some(predicate),
                object: None,
                graph: GraphSelector::Any,
            };
            for q in snapshot.quads_for_pattern_in(model, &pattern) {
                statements.push((q.subject.raw(), q.object.raw(), q.graph))?;
            }
            drop(scanning);
            Ok(statements)
        };
        let mut statements = read(SERIALISATIONS[0])?;
        for local in &SERIALISATIONS[1..] {
            statements.extend_from_slice(&read(local)?)?;
        }
        by_graph(&mut statements);

        // Geometries, graph by graph; each literal parsed once.
        let mut geometries: Charged<Placement> =
            ChargedVec::with_capacity(Arc::clone(budget), statements.len())?;
        let mut runs: Charged<(u64, TermId)> = ChargedVec::new(Arc::clone(budget));
        let mut largest = Size::default();
        // Per literal, the position of its first placement (`None`: not a shape).
        let mut scratch = Reservation::new(&**budget);
        scratch.charge(map_bytes::<u64, Option<u64>>(statements.len()))?;
        let mut first: HashMap<u64, Option<u64>> = HashMap::with_capacity(statements.len());
        // The literals several graphs give, with their graphs.
        let mut spread: HashMap<u64, Vec<TermId>> = HashMap::new();
        for &(object, literal, graph) in statements.iter() {
            let p = position(geometries.len());
            let shape = match first.get(&literal) {
                Some(None) => continue,
                Some(&Some(earlier)) => {
                    let earlier_graph = run_of(&runs, earlier);
                    if earlier_graph != graph {
                        if !spread.contains_key(&literal) {
                            map_room(&mut scratch, &mut spread)?;
                            let mut graphs = Vec::new();
                            push(&mut scratch, &mut graphs, earlier_graph)?;
                            spread.insert(literal, graphs);
                        }
                        let graphs = spread.get_mut(&literal).expect("inserted");
                        if !graphs.contains(&graph) {
                            push(&mut scratch, graphs, graph)?;
                        }
                    }
                    Arc::clone(&geometries[earlier as usize].shape)
                }
                None => match decoded(snapshot, &mut held, TermId::from_raw(literal))? {
                    None => {
                        first.insert(literal, None);
                        continue;
                    }
                    Some(shape) => {
                        first.insert(literal, Some(p));
                        largest = largest.largest(Size::of(&shape.geometry));
                        shape
                    }
                },
            };
            if runs.last().is_none_or(|&(_, g)| g != graph) {
                runs.push((p, graph))?;
            }
            geometries.push(Placement { object, shape })?;
        }
        drop(statements);
        let mut objects: HashMap<u64, u64> = HashMap::new();
        let mut several: Charged<Vec<u64>> = ChargedVec::new(Arc::clone(budget));
        for (p, placement) in geometries.iter().enumerate() {
            place(
                &mut held,
                &mut objects,
                &mut several,
                placement.object,
                position(p),
            )?;
        }

        // Features: through their default geometries, else through their other ones.
        let mut defaults = read("hasDefaultGeometry")?;
        defaults.sort_unstable();
        let links = read("hasGeometry")?;
        let defaults_of = |feature: u64| {
            let from = defaults.partition_point(|d| d.0 < feature);
            let to = defaults.partition_point(|d| d.0 <= feature);
            &defaults[from..to]
        };
        let mut within: Charged<(TermId, Placement)> = ChargedVec::new(Arc::clone(budget));
        let mut crossing: Charged<(Placement, Crossing)> = ChargedVec::new(Arc::clone(budget));
        let mut hidden_by: HashMap<u64, Vec<TermId>> = HashMap::new();
        let all_links = defaults
            .iter()
            .map(|&(f, g, link)| (f, g, link, true))
            .chain(links.iter().map(|&(f, g, link)| (f, g, link, false)));
        for (feature, geometry, link, default) in all_links {
            let own = defaults_of(feature);
            // A default geometry in the link's own graph hides it wherever it is shown.
            if !default && own.iter().any(|d| d.2 == link) {
                continue;
            }
            let hidden_elsewhere = !default && !own.is_empty();
            let positions = match objects.get(&geometry) {
                None => continue,
                Some(v) if v & LIST != 0 => &several[(v & !LIST) as usize][..],
                Some(v) => std::slice::from_ref(v),
            };
            for &p in positions {
                let shape = Arc::clone(&geometries[p as usize].shape);
                let from = run_of(&runs, p);
                let placement = Placement {
                    object: feature,
                    shape,
                };
                if from == link && !hidden_elsewhere {
                    within.push((link, placement))?;
                } else {
                    let graphs = Crossing {
                        link,
                        default,
                        geometry: from,
                    };
                    crossing.push((placement, graphs))?;
                    if hidden_elsewhere && !hidden_by.contains_key(&feature) {
                        map_room(&mut held, &mut hidden_by)?;
                        let mut graphs = Vec::new();
                        for d in own {
                            push(&mut held, &mut graphs, d.2)?;
                        }
                        hidden_by.insert(feature, graphs);
                    }
                }
            }
        }
        drop((links, defaults));
        if !within.is_sorted_by_key(|w| w.0) {
            within.sort_unstable_by_key(|w| w.0);
        }
        let start = geometries.len();
        let mut features: Charged<Placement> =
            ChargedVec::with_capacity(Arc::clone(budget), within.len() + crossing.len())?;
        let mut crossings: Charged<Crossing> =
            ChargedVec::with_capacity(Arc::clone(budget), crossing.len())?;
        let (within, within_held) = within.into_parts();
        let mut run = None;
        for (graph, placement) in within {
            if run != Some(graph) {
                runs.push((position(start + features.len()), graph))?;
                run = Some(graph);
            }
            features.push(placement)?;
        }
        drop(within_held);
        let crossing_from = position(start + features.len());
        let (crossing, crossing_held) = crossing.into_parts();
        for (placement, graphs) in crossing {
            features.push(placement)?;
            crossings.push(graphs)?;
        }
        drop(crossing_held);
        for (i, placement) in features.iter().enumerate() {
            place(
                &mut held,
                &mut objects,
                &mut several,
                placement.object,
                position(start + i),
            )?;
        }

        let mut asserted: HashMap<(&'static str, u64), Vec<(u64, TermId)>> = HashMap::new();
        let mut asserted_back: HashMap<(&'static str, u64), Vec<(u64, TermId)>> = HashMap::new();
        let mut graphs: Charged<TermId> = ChargedVec::new(Arc::clone(budget));
        for &local in RELATIONS {
            for &(a, b, graph) in read(local)?.iter() {
                for (map, key, other) in [(&mut asserted, a, b), (&mut asserted_back, b, a)] {
                    if !map.contains_key(&(local, key)) {
                        map_room(&mut held, map)?;
                        map.insert((local, key), Vec::new());
                    }
                    let list = map.get_mut(&(local, key)).expect("inserted");
                    push(&mut held, list, (other, graph))?;
                }
                graphs.push(graph)?;
            }
        }
        for &(_, g) in runs.iter() {
            graphs.push(g)?;
        }
        for c in crossings.iter() {
            graphs.extend_from_slice(&[c.link, c.geometry])?;
        }
        for list in hidden_by.values() {
            graphs.extend_from_slice(list)?;
        }
        graphs.sort_unstable();
        dedup(&mut graphs);

        let shape_at = |p: u64| {
            let p = p as usize;
            match p < start {
                true => &geometries[p].shape,
                false => &features[p - start].shape,
            }
        };
        let total = start + features.len();
        let mut entries: Charged<(Bbox, u64)> =
            ChargedVec::with_capacity(Arc::clone(budget), total)?;
        let mut empty = ChargedVec::new(Arc::clone(budget));
        for p in 0..position(total) {
            match bbox(shape_at(p)) {
                Some(b) => entries.push((b, p))?,
                None => empty.push(p)?,
            }
        }
        let tree = Packed::pack(entries, budget)?;

        let mut lists: Charged<(u64, Vec<TermId>)> = ChargedVec::new(Arc::clone(budget));
        let mut entries: Charged<(Bbox, (u64, u64))> =
            ChargedVec::with_capacity(Arc::clone(budget), first.len())?;
        for (&literal, &p) in &first {
            let Some(p) = p else { continue };
            let Some(b) = bbox(shape_at(p)) else { continue };
            let slot = match spread.remove(&literal) {
                Some(spread_graphs) => {
                    let mut list = Vec::new();
                    for g in spread_graphs {
                        push(&mut held, &mut list, g)?;
                    }
                    lists.push((p, list))?;
                    position(lists.len() - 1) | LIST
                }
                None => p,
            };
            entries.push((b, (literal, slot)))?;
        }
        let literals = Packed::pack(entries, budget)?;
        drop((first, spread, scratch));
        Ok(Self {
            tree,
            empty,
            largest,
            geometries,
            features,
            runs,
            crossing_from,
            crossings,
            defaults: hidden_by,
            objects,
            several,
            literals,
            spread: lists,
            asserted,
            asserted_back,
            graphs,
            _held: held,
        })
    }

    fn shape_at(&self, p: u64) -> &Arc<Shape> {
        let (p, start) = (p as usize, self.geometries.len());
        match p < start {
            true => &self.geometries[p].shape,
            false => &self.features[p - start].shape,
        }
    }

    fn object_at(&self, p: u64) -> u64 {
        let (p, start) = (p as usize, self.geometries.len());
        match p < start {
            true => self.geometries[p].object,
            false => self.features[p - start].object,
        }
    }

    /// The positions of `object`'s placements.
    fn positions(&self, object: u64) -> &[u64] {
        match self.objects.get(&object) {
            None => &[],
            Some(v) if v & LIST != 0 => &self.several[(v & !LIST) as usize],
            Some(v) => std::slice::from_ref(v),
        }
    }

    /// This index as a pattern in `scope` reads it.
    fn scoped<'a>(&'a self, scope: &'a Scope) -> Scoped<'a> {
        let graphs = &self.graphs;
        let all = match scope {
            Scope::Graphs(GraphSelector::Any) => true,
            Scope::Graphs(GraphSelector::Exact(g)) => graphs.iter().all(|x| x == g),
            Scope::Graphs(GraphSelector::AnyNamed) => {
                graphs.binary_search(&TermId::DEFAULT_GRAPH).is_err()
            }
            Scope::Merge(merged) if merged.len() < graphs.len() => false,
            scope => graphs.iter().all(|&g| scope.contains(g)),
        };
        let nothing = !all
            && match scope {
                Scope::Graphs(GraphSelector::Exact(g)) => graphs.binary_search(g).is_err(),
                _ => graphs.len() <= 1,
            };
        Scoped {
            index: self,
            scope,
            shown: match (all, nothing) {
                (true, _) => Shown::All,
                (false, true) => Shown::Nothing,
                (false, false) => Shown::Some,
            },
        }
    }
}

/// What a scope shows of an index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    /// Every graph it holds: nothing to check but the crossing placements' precedence.
    All,
    Nothing,
    /// Some of its graphs: each placement, literal and statement by its graph.
    Some,
}

/// An index as a pattern in one scope reads it ([`SpatialIndex::scoped`]).
struct Scoped<'a> {
    index: &'a SpatialIndex,
    scope: &'a Scope,
    shown: Shown,
}

impl Scoped<'_> {
    /// Whether the scope shows the placement at `p`. A crossing placement is checked
    /// whatever the scope: a `geo:hasGeometry` shape is hidden by a default geometry in
    /// any graph the scope shows, all of them included.
    fn shows(&self, p: u64) -> bool {
        let index = self.index;
        if self.shown == Shown::Nothing {
            return false;
        }
        if p < index.crossing_from {
            return self.shown == Shown::All || self.scope.contains(run_of(&index.runs, p));
        }
        let c = index.crossings[(p - index.crossing_from) as usize];
        self.scope.contains(c.link)
            && self.scope.contains(c.geometry)
            && (c.default
                || !index
                    .defaults
                    .get(&index.object_at(p))
                    .is_some_and(|graphs| graphs.iter().any(|&g| self.scope.contains(g))))
    }

    /// The shapes of `object` the scope shows.
    fn shapes(&self, object: u64) -> impl Iterator<Item = &Arc<Shape>> + '_ {
        self.index
            .positions(object)
            .iter()
            .filter(|&&p| self.shows(p))
            .map(|&p| self.index.shape_at(p))
    }

    /// The objects with a shape the scope shows, charged to `budget`.
    fn objects<'b>(&self, budget: &'b Budget) -> Result<Scratch<'b, u64>, BudgetExceeded> {
        let all = self.shown == Shown::All && self.index.crossings.is_empty();
        let mut objects = ChargedVec::new(budget);
        if self.shown == Shown::Nothing {
            return Ok(objects);
        }
        objects.reserve_exact(self.index.objects.len())?;
        for &o in self.index.objects.keys() {
            if all || self.shapes(o).next().is_some() {
                objects.push(o)?;
            }
        }
        Ok(objects)
    }

    /// `found` for each geometry literal whose bounding box meets `b`, with the position of
    /// a placement of its shape.
    fn literals(&self, b: &Bbox, found: &mut impl FnMut(u64, u64)) {
        let index = self.index;
        if self.shown == Shown::Nothing {
            return;
        }
        let all = self.shown == Shown::All;
        index.literals.search(b, &mut |(literal, slot)| {
            if slot & LIST == 0 {
                if all || self.scope.contains(run_of(&index.runs, slot)) {
                    found(literal, slot);
                }
                return;
            }
            let (p, graphs) = &index.spread[(slot & !LIST) as usize];
            if all || graphs.iter().any(|&g| self.scope.contains(g)) {
                found(literal, *p);
            }
        });
    }

    /// What statements the scope shows relate: `from`'s, by relation and one side.
    fn stated<'a>(
        &'a self,
        from: &'a HashMap<(&'static str, u64), Vec<(u64, TermId)>>,
        local: &str,
        known: u64,
    ) -> impl Iterator<Item = u64> + 'a {
        local_of(local)
            .and_then(|l| from.get(&(l, known)))
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter(|&&(_, graph)| self.scope.contains(graph))
            .map(|&(other, _)| other)
    }

    /// Whether `relation` holds between the spatial objects `a` and `b`.
    fn holds(
        &self,
        work: &mut Workspace<'_>,
        local: &str,
        a: u64,
        b: u64,
    ) -> Result<bool, BudgetExceeded> {
        for x in self.shapes(a) {
            for y in self.shapes(b) {
                if work.holds(local, x, y)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// The objects of the placements whose box meets the shape at `known` and for whose
    /// shape `test(known's shape, theirs)` holds, pushed onto `out`. Complete for every
    /// relation but disjointness: a pair in any other relation shares a point, so their
    /// boxes meet, or both are empty (and equal), without a box.
    fn near<B: Deref<Target = Budget>>(
        &self,
        known: u64,
        mut test: impl FnMut(&Shape, &Shape) -> Result<bool, BudgetExceeded>,
        out: &mut ChargedVec<u64, B>,
    ) -> Result<(), BudgetExceeded> {
        let index = self.index;
        let x = index.shape_at(known);
        let mut refused = Ok(());
        let mut candidate = |p: u64| {
            if refused.is_ok() && self.shows(p) {
                let y = index.shape_at(p);
                refused = match test(x, y) {
                    Ok(true) => out.push(index.object_at(p)),
                    other => other.map(|_| ()),
                };
            }
        };
        match bbox(x) {
            Some(b) => index.tree.search(&b, &mut candidate),
            None => index.empty.iter().for_each(|&p| candidate(p)),
        }
        refused
    }

    /// The pairs `relation` relates with `a` and `b` on its sides (`None`: any object),
    /// charged to `budget`.
    fn pairs<'b>(
        &self,
        local: &str,
        a: Option<u64>,
        b: Option<u64>,
        budget: &'b Budget,
    ) -> Result<Scratch<'b, (u64, u64)>, BudgetExceeded> {
        let index = self.index;
        let mut work = Workspace::new(budget);
        let apart = holds_apart(local);
        let mut pairs: Scratch<'b, (u64, u64)> = ChargedVec::new(budget);
        // The other side's objects, with one side known.
        let mut found: Scratch<'b, u64> = ChargedVec::new(budget);
        match (a, b) {
            (Some(a), Some(b)) => {
                let stated = self.stated(&index.asserted, local, a).any(|x| x == b);
                if stated || self.holds(&mut work, local, a, b)? {
                    pairs.push((a, b))?;
                }
            }
            (Some(a), None) => {
                if apart {
                    for &b in self.objects(budget)?.iter() {
                        if self.holds(&mut work, local, a, b)? {
                            found.push(b)?;
                        }
                    }
                } else {
                    for &p in index.positions(a) {
                        if self.shows(p) {
                            work.cover(index.shape_at(p), index.largest);
                            work.cover(index.shape_at(p), index.largest);
                            self.near(p, |x, y| work.holds(local, x, y), &mut found)?;
                        }
                    }
                }
                for b in self.stated(&index.asserted, local, a) {
                    found.push(b)?;
                }
                pairs.reserve_exact(found.len())?;
                for &b in found.iter() {
                    pairs.push((a, b))?;
                }
            }
            (None, Some(b)) => {
                if apart {
                    for &a in self.objects(budget)?.iter() {
                        if self.holds(&mut work, local, a, b)? {
                            found.push(a)?;
                        }
                    }
                } else {
                    for &p in index.positions(b) {
                        if self.shows(p) {
                            work.cover(index.shape_at(p), index.largest);
                            self.near(p, |y, x| work.holds(local, x, y), &mut found)?;
                        }
                    }
                }
                for a in self.stated(&index.asserted_back, local, b) {
                    found.push(a)?;
                }
                pairs.reserve_exact(found.len())?;
                for &a in found.iter() {
                    pairs.push((a, b))?;
                }
            }
            (None, None) => {
                if apart {
                    let mut all = self.objects(budget)?;
                    all.sort_unstable();
                    for &a in all.iter() {
                        for &b in all.iter() {
                            if self.holds(&mut work, local, a, b)? {
                                pairs.push((a, b))?;
                            }
                        }
                    }
                } else {
                    let total = position(index.geometries.len() + index.features.len());
                    for p in (0..total).filter(|&p| self.shows(p)) {
                        found.clear();
                        self.near(p, |x, y| work.holds(local, x, y), &mut found)?;
                        let a = index.object_at(p);
                        for &b in found.iter() {
                            pairs.push((a, b))?;
                        }
                    }
                }
                if let Some(l) = local_of(local) {
                    for ((relation, a), objects) in &index.asserted {
                        if *relation == l {
                            for &(b, graph) in objects {
                                if self.scope.contains(graph) {
                                    pairs.push((*a, b))?;
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(pairs)
    }
}

/// Room on an evaluation's budget for geo's working memory while it computes relations:
/// a pair's bound ([`relate_bound`]) is charged before geo computes it, and the room is
/// kept, grown to the largest pair, while the operation computes relations. The first
/// side's last shape is remembered with its size (one shape related to many is sized
/// once), and the last pair's sizes, whose bound is held (points related to one shape are
/// bounded once). A shape related to an index's shapes is covered once where it can be:
/// its bound with the index's largest size held, its pairs need no other.
struct Workspace<'b> {
    budget: &'b Budget,
    held: Reservation<&'b Budget>,
    sized: Option<(*const Shape, Size)>,
    bounded: Option<(Size, Size)>,
    covered: Option<*const Shape>,
}

impl<'b> Workspace<'b> {
    fn new(budget: &'b Budget) -> Self {
        Self {
            budget,
            held: Reservation::new(budget),
            sized: None,
            bounded: None,
            covered: None,
        }
    }

    /// Holds, where the budget has room, what `known` with any shape of an index whose
    /// shapes are at most `largest` takes: [`Self::holds`] then charges nothing more for
    /// `known` and such a shape.
    fn cover(&mut self, known: &Shape, largest: Size) {
        self.covered = None;
        if let Some(bound) = sized_bound(&Size::of(&known.geometry), &largest)
            && (bound <= self.held.bytes() || self.held.resize(bound).is_ok())
        {
            self.covered = Some(known);
        }
    }

    /// The size of `shape` on the first side.
    fn size(&mut self, shape: &Shape) -> Size {
        match self.sized {
            Some((last, size)) if std::ptr::eq(last, shape) => size,
            _ => {
                let size = Size::of(&shape.geometry);
                self.sized = Some((shape, size));
                size
            }
        }
    }

    /// Whether `relation` holds between the shapes `x` and `y` (in one reference system),
    /// refused before geo allocates what the budget has no room for.
    fn holds(&mut self, local: &str, x: &Shape, y: &Shape) -> Result<bool, BudgetExceeded> {
        if x.crs != y.crs {
            return Ok(false);
        }
        if self
            .covered
            .is_some_and(|known| std::ptr::eq(known, x) || std::ptr::eq(known, y))
        {
            return Ok(relation(local, &x.geometry, &y.geometry).unwrap_or(false));
        }
        let sizes = (self.size(x), Size::of(&y.geometry));
        if self.bounded != Some(sizes) {
            let (sx, sy) = sizes;
            // Where the sizes decide the bound, the next pair of these sizes has it held.
            let (bound, bounded) = match sized_bound(&sx, &sy) {
                Some(bound) => (bound, Some(sizes)),
                None => (
                    relate_bound((&x.geometry, &sx), (&y.geometry, &sy), self.budget)?,
                    None,
                ),
            };
            if bound > self.held.bytes() {
                self.held.resize(bound)?;
            }
            self.bounded = bounded;
        }
        Ok(relation(local, &x.geometry, &y.geometry).unwrap_or(false))
    }
}

/// One side of a spatial pattern.
enum Side {
    Constant(Option<u64>),
    Bound(usize),
    Free(Variable),
}

impl Side {
    fn free(&self) -> Option<&Variable> {
        match self {
            Side::Free(v) => Some(v),
            _ => None,
        }
    }
}

/// A filter conjunct `geof:relation(?v, constant)` (or the other way round) for a
/// relation the index can find candidates for: the variable, the constant, the relation,
/// and whether the constant is its first argument.
fn indexed_filter(conjunct: &Expression) -> Option<(&Variable, &Literal, &'static str, bool)> {
    let Expression::FunctionCall(Function::Custom(name), args) = conjunct else {
        return None;
    };
    let local = local_of(name.as_str().strip_prefix(super::geo::GEOF)?)?;
    if holds_apart(local) {
        return None;
    }
    let (v, l, constant_first) = match args.as_slice() {
        [Expression::Variable(v), Expression::Literal(l)] => (v, l, false),
        [Expression::Literal(l), Expression::Variable(v)] => (v, l, true),
        _ => return None,
    };
    Some((v, l, local, constant_first))
}

/// A table of charged columns, handed on with their charge: what [`Context::consumed`]
/// releases of it is what they hold ([`IdTable::memory_bytes`]). Its list of columns is
/// charged with the result's header ([`charge_header`]).
fn charged_table<'b>(columns: impl ExactSizeIterator<Item = Scratch<'b, u64>>) -> IdTable {
    let column = |c: Scratch<'b, u64>| {
        let (items, held) = c.into_parts();
        held.transfer();
        items
    };
    IdTable::from_columns(columns.map(column).collect())
}

/// Charges a result's header before it is allocated: the list of its variables `vars`
/// and their names, and its table's list of `width` columns, which the table's charge (its
/// columns' bytes) doesn't count. It stays charged while the evaluation runs.
fn charge_header<'v>(
    budget: &Budget,
    vars: impl IntoIterator<Item = &'v Variable>,
    width: usize,
) -> Result<(), BudgetExceeded> {
    let (count, names) = vars.into_iter().fold((0, 0), |(count, names), v| {
        (count + 1, names + v.as_str().len())
    });
    budget.charge(count * size_of::<Variable>() + names + width * size_of::<Vec<u64>>())
}

impl Context<'_> {
    /// For a basic graph pattern under `conjuncts`: when one of them relates a variable
    /// that is the object of a `geo:asWKT` (`asGeoJSON`, `asGML`) pattern to a constant
    /// shape, the geometry literals it holds for, as rows to start the joins from (charged
    /// to the evaluation's budget, the charge handed on with them), and the number of that
    /// conjunct, which they decide. The R-tree finds the literals whose box meets the
    /// shape's, and the relation is computed on their indexed shapes, as `geof:` computes
    /// it on a literal's ([`super::geo::call`]: the same parse, one reference system):
    /// every row's value of the variable is one of them. An index the budget has no room
    /// for gives no seed (the filter runs without it).
    pub(super) fn spatial_seed(
        &self,
        conjuncts: &[&Expression],
        patterns: &[TriplePattern],
    ) -> NativeResult<Option<(Solutions, usize)>> {
        if self.as_written {
            return Ok(None);
        }
        for (at, conjunct) in conjuncts.iter().enumerate() {
            let Some((variable, constant, local, constant_first)) = indexed_filter(conjunct) else {
                continue;
            };
            let bound_by_wkt = patterns.iter().any(|t| {
                matches!(&t.predicate, NamedNodePattern::NamedNode(n)
                    if n.as_str().strip_prefix(GEO).is_some_and(|l| SERIALISATIONS.contains(&l)))
                    && matches!(&t.object, TermPattern::Variable(o) if o == variable)
            });
            if !bound_by_wkt {
                continue;
            }
            // The constant's shape, its parser's bound charged first and then its size.
            let text = constant.value().len();
            let mut held = Reservation::new(&*self.budget);
            held.charge(shape_bound(text))
                .map_err(QueryEvaluationError::MemoryLimit)?;
            let Some(shape) = parse_literal(constant.value(), constant.datatype().as_str()) else {
                continue;
            };
            held.resize(shape_bytes(&shape))
                .map_err(QueryEvaluationError::MemoryLimit)?;
            let Some(b) = bbox(&shape) else {
                continue;
            };
            // `GRAPH ?g` at once: the literals of the graphs it may bind.
            let scope = match self.spatial_scope()? {
                Some(scope) => scope,
                None => match &self.named {
                    Some(named) => Scope::merge(named, &self.budget)
                        .map_err(QueryEvaluationError::MemoryLimit)?,
                    None => Scope::Graphs(GraphSelector::AnyNamed),
                },
            };
            let Ok(index) = self.spatial_index() else {
                return Ok(None);
            };
            let literals = self
                .seed_literals(&index.scoped(&scope), &b, &shape, |work, p| {
                    let found = index.shape_at(p);
                    match constant_first {
                        true => work.holds(local, &shape, found),
                        false => work.holds(local, found, &shape),
                    }
                })
                .map_err(QueryEvaluationError::MemoryLimit)?;
            charge_header(&self.budget, [variable], 1)
                .map_err(QueryEvaluationError::MemoryLimit)?;
            let seed = Solutions {
                vars: vec![variable.clone()],
                table: charged_table(std::iter::once(literals)),
                ordered: false,
            };
            self.check()?;
            return Ok(Some((seed, at)));
        }
        Ok(None)
    }

    /// The literals of `index` whose box meets `b` and whose shape `holds` (with `known`):
    /// the candidates, and in parallel chunks for many of them the chunks' results, charged
    /// to the evaluation's budget before they are allocated. A cancelled query stops at its
    /// next check.
    fn seed_literals(
        &self,
        index: &Scoped<'_>,
        b: &Bbox,
        known: &Shape,
        holds: impl Fn(&mut Workspace<'_>, u64) -> Result<bool, BudgetExceeded> + Sync,
    ) -> Result<Scratch<'_, u64>, BudgetExceeded> {
        let budget: &Budget = &self.budget;
        let mut found: Scratch<'_, (u64, u64)> = ChargedVec::new(budget);
        let mut refused = Ok(());
        index.literals(b, &mut |literal, p| {
            if refused.is_ok() {
                refused = found.push((literal, p));
            }
        });
        refused?;
        let keep = |chunk: &[(u64, u64)]| -> Result<Scratch<'_, u64>, BudgetExceeded> {
            let mut kept = ChargedVec::with_capacity(budget, chunk.len())?;
            let mut work = Workspace::new(budget);
            work.cover(known, index.index.largest);
            for &(literal, p) in chunk {
                if holds(&mut work, p)? {
                    kept.push(literal)?;
                }
            }
            Ok(kept)
        };
        if found.len() < 2 * PARALLEL_EXPRESSION_ROWS {
            return keep(&found);
        }
        let token = self.cancellation.as_ref();
        // The chunks' results, in room reserved for all of them first: collecting into it
        // doesn't grow it.
        let count = found.len().div_ceil(PARALLEL_EXPRESSION_ROWS);
        let (mut parts, parts_held) = ChargedVec::with_capacity(budget, count)?.into_parts();
        found
            .par_chunks(PARALLEL_EXPRESSION_ROWS)
            .map(
                |chunk| match token.is_some_and(CancellationToken::is_cancelled) {
                    true => Ok(ChargedVec::new(budget)),
                    false => keep(chunk),
                },
            )
            .collect_into_vec(&mut parts);
        let mut total = 0;
        for part in parts.iter() {
            match part {
                Ok(part) => total += part.len(),
                Err(refused) => {
                    let refused = *refused;
                    drop((parts, parts_held));
                    return Err(refused);
                }
            }
        }
        let mut literals = ChargedVec::with_capacity(budget, total)?;
        for part in parts.iter().flatten() {
            literals.extend_from_slice(part)?;
        }
        drop((parts, parts_held));
        Ok(literals)
    }

    /// The scope of a spatial pattern in the active graph; `None` while `GRAPH ?g` is
    /// evaluated for all graphs at once.
    fn spatial_scope(&self) -> NativeResult<Option<Scope>> {
        Ok(Some(match &*self.graph.borrow() {
            GraphScope::Default => Scope::Graphs(GraphSelector::Exact(TermId::DEFAULT_GRAPH)),
            GraphScope::Named(graph) => Scope::Graphs(GraphSelector::Exact(*graph)),
            GraphScope::Union => match &self.merge_set {
                Some(graphs) => {
                    Scope::merge(graphs, &self.budget).map_err(QueryEvaluationError::MemoryLimit)?
                }
                None => Scope::Graphs(GraphSelector::Any),
            },
            GraphScope::Missing => Scope::Merge(ChargedVec::new(Arc::clone(&self.budget))),
            GraphScope::Variable(_) => return Ok(None),
        }))
    }

    /// The index of this snapshot in this read model: the cached one, or one built and
    /// charged to a part of this request's envelope, which the index keeps while it lives
    /// (in the cache, and in queries still reading it after it is replaced).
    fn spatial_index(&self) -> Result<Arc<SpatialIndex>, BudgetExceeded> {
        let (identity, model) = (self.snapshot.identity(), self.model);
        let lock = || {
            CACHE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        };
        let cached = |cache: &Option<Cached>| {
            cache
                .as_ref()
                .filter(|(at, read, _)| *at == identity && *read == model)
                .map(|(_, _, index)| Arc::clone(index))
        };
        if let Some(index) = cached(&lock()) {
            return Ok(index);
        }
        // Built without the lock: no other query waits for this one's build. Builds that
        // race are each charged to their own request. The part's own allocation and the
        // index's are charged to the part before they are made (the first through the
        // request, until the part exists to hold it).
        let shared = |size: usize| 2 * size_of::<usize>() + size;
        let part = shared(size_of::<Budget>());
        self.memory.charge(part)?;
        let budget = Arc::new(self.memory.part());
        let charged = budget.charge(part);
        self.memory.release(part);
        charged?;
        let mut index = SpatialIndex::build(&self.snapshot, model, &budget)?;
        index._held.charge(shared(size_of::<SpatialIndex>()))?;
        let index = Arc::new(index);
        let mut cache = lock();
        if let Some(built) = cached(&cache) {
            return Ok(built);
        }
        *cache = Some((identity, model, Arc::clone(&index)));
        Ok(index)
    }

    fn side(
        &self,
        term: &TermPattern,
        solutions: &Solutions,
        held: &mut Reservation<&Budget>,
    ) -> Result<Side, BudgetExceeded> {
        Ok(match term {
            TermPattern::NamedNode(n) => Side::Constant(
                self.lookup_const_charged(n.as_ref().into(), held)?
                    .map(TermId::raw),
            ),
            TermPattern::Variable(v) => match solutions.column(v) {
                Some(c) => Side::Bound(c),
                None => Side::Free(v.clone()),
            },
            TermPattern::BlankNode(b) => {
                let v = Variable::new_unchecked(format!("_bnode_{}", b.as_str()));
                match solutions.column(&v) {
                    Some(c) => Side::Bound(c),
                    None => Side::Free(v),
                }
            }
            _ => Side::Constant(None),
        })
    }

    /// `solutions` joined with the spatial pattern `triple`; what it allocates charged to
    /// the evaluation's budget first, the result handed on with its charge.
    pub(super) fn spatial_join(
        &self,
        solutions: Solutions,
        triple: &TriplePattern,
    ) -> NativeResult<Solutions> {
        let local = relation_of(triple).expect("a spatial pattern");
        // `GRAPH ?g` sends spatial relations graph by graph (`scans_everywhere`).
        let Some(scope) = self.spatial_scope()? else {
            return unsupported("a spatial relation read in all graphs at once");
        };
        let index = self
            .spatial_index()
            .map_err(QueryEvaluationError::MemoryLimit)?;
        let index = index.scoped(&scope);
        let budget: &Budget = &self.budget;
        let memory = QueryEvaluationError::MemoryLimit;
        // What joining holds besides its columns, charged before it is allocated: a free
        // side's variable (a blank node's named for it), and a row's line and the list of
        // the columns, at most two wider than the rows joined.
        let named = |term: &TermPattern| match term {
            TermPattern::Variable(v) => size_of::<Variable>() + v.as_str().len(),
            TermPattern::BlankNode(b) => size_of::<Variable>() + "_bnode_".len() + b.as_str().len(),
            _ => 0,
        };
        let most = solutions.vars.len() + 2;
        let mut working = Reservation::new(budget);
        let lists = most * (size_of::<u64>() + size_of::<Scratch<'_, u64>>());
        working
            .charge(named(&triple.subject) + named(&triple.object) + lists)
            .map_err(memory)?;
        // A constant side's lookup charged before it runs, held with the join's working memory.
        let (s, o) = (
            self.side(&triple.subject, &solutions, &mut working)
                .map_err(memory)?,
            self.side(&triple.object, &solutions, &mut working)
                .map_err(memory)?,
        );
        // The free sides' variables, each once: the columns after the rows'.
        let added = [s.free(), o.free().filter(|&v| s.free() != Some(v))];
        let width = solutions.vars.len();
        let joined = width + added.iter().flatten().count();
        let header = solutions.vars.iter().chain(added.iter().flatten().copied());
        charge_header(budget, header, joined).map_err(memory)?;
        let mut vars = Vec::with_capacity(joined);
        vars.extend(solutions.vars.iter().cloned());
        vars.extend(added.iter().flatten().map(|&v| v.clone()));
        let same = matches!((&s, &o), (Side::Free(a), Side::Free(b)) if a == b);
        let mut columns: Vec<Scratch<'_, u64>> = Vec::with_capacity(joined);
        columns.extend((0..joined).map(|_| ChargedVec::new(budget)));
        let mut rows = 0;
        let mut line = vec![UNDEF; joined];
        // A value of a side in a row: a constant, the row's binding (unbound: any object).
        let value = |side: &Side, row: usize| -> Option<Option<u64>> {
            match side {
                Side::Constant(id) => Some(*id),
                Side::Bound(c) => {
                    let id = solutions.table.get(row, *c);
                    Some((id != UNDEF).then_some(id))
                }
                Side::Free(_) => Some(None),
            }
        };
        for row in 0..solutions.table.len() {
            if row % 1024 == 0 {
                self.check()?;
            }
            let (Some(a), Some(b)) = (value(&s, row), value(&o, row)) else {
                continue;
            };
            if matches!(s, Side::Constant(None)) || matches!(o, Side::Constant(None)) {
                continue;
            }
            let mut pairs = index.pairs(local, a, b, budget).map_err(memory)?;
            // A pair found twice (both stated and computed, or through two shapes) once.
            pairs.sort_unstable();
            dedup(&mut pairs);
            for &(a, b) in pairs.iter() {
                if same && a != b {
                    continue;
                }
                line[..width].copy_from_slice(&solutions.table.row(row));
                let mut bound_free = |side: &Side, value: u64| {
                    if let Some(v) = side.free()
                        && let Some(position) = added.iter().flatten().position(|&x| x == v)
                    {
                        line[width + position] = value;
                    }
                };
                bound_free(&s, a);
                bound_free(&o, b);
                // A bound column left unbound by an earlier OPTIONAL takes the value.
                if let Side::Bound(c) = s {
                    line[c] = a;
                }
                if let Side::Bound(c) = o {
                    line[c] = b;
                }
                for (column, &value) in columns.iter_mut().zip(&line) {
                    column.push(value).map_err(memory)?;
                }
                rows += 1;
            }
        }
        drop((s, o, line));
        self.consumed(solutions);
        self.check()?;
        let table = match joined {
            // No column: only the count of rows, which holds no memory.
            0 => IdTable::from_rows(0, std::iter::repeat_n(&[][..], rows)),
            _ => charged_table(columns.into_iter()),
        };
        drop(working);
        Ok(Solutions {
            vars,
            table,
            ordered: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use nrese_engine::{Engine, EngineConfig};
    use nrese_exec::Envelope;
    use nrese_rdf::{GraphName, Literal, NamedNode, Quad};

    use super::*;
    use crate::QueryOptions;

    fn node(s: String) -> NamedNode {
        NamedNode::new_unchecked(s)
    }

    /// A snapshot of geometries `urn:g{i}`, each in `graphs[i]` with the serialisation,
    /// datatype and text `literal(i)` gives, and a feature linked to each in the same graph.
    fn snapshot_with(
        graphs: &[GraphName],
        literal: impl Fn(usize) -> (&'static str, &'static str, String),
    ) -> Snapshot {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        insert(&engine, graphs, literal);
        engine.snapshot()
    }

    /// [`snapshot_with`]'s statements in a store written to a checkpoint with its keys
    /// compressed, and reopened. For a process of its own: the encoding is the process's.
    fn reopened(
        graphs: &[GraphName],
        literal: impl Fn(usize) -> (&'static str, &'static str, String),
    ) -> (tempfile::TempDir, Snapshot) {
        let dir = tempfile::tempdir().unwrap();
        let config = EngineConfig {
            background_maintenance: false,
            ..EngineConfig::default()
        };
        nrese_engine::set_vocabulary_encoding(nrese_engine::VocabularyEncoding::Fsst);
        let engine = Engine::open(dir.path(), config.clone()).unwrap();
        insert(&engine, graphs, literal);
        engine.checkpoint().unwrap();
        drop(engine);
        let snapshot = Engine::open(dir.path(), config).unwrap().snapshot();
        (dir, snapshot)
    }

    fn insert(
        engine: &Engine,
        graphs: &[GraphName],
        literal: impl Fn(usize) -> (&'static str, &'static str, String),
    ) {
        let mut tx = engine.transaction();
        for (i, graph) in graphs.iter().enumerate() {
            let (property, datatype, text) = literal(i);
            let text = Literal::new_typed_literal(text, node(format!("{GEO}{datatype}")));
            let geometry = node(format!("urn:g{i}"));
            let quads = [
                Quad::new(
                    geometry.clone(),
                    node(format!("{GEO}{property}")),
                    text,
                    graph.clone(),
                ),
                Quad::new(
                    node(format!("urn:f{i}")),
                    node(format!("{GEO}hasGeometry")),
                    geometry,
                    graph.clone(),
                ),
            ];
            for quad in &quads {
                tx.insert(quad.as_ref());
            }
        }
        tx.commit().unwrap();
    }

    fn snapshot_of(graphs: &[GraphName]) -> Snapshot {
        snapshot_with(graphs, |i| {
            ("asWKT", "wktLiteral", format!("POINT({i} {i})"))
        })
    }

    fn index_of(graphs: &[GraphName]) -> SpatialIndex {
        let budget = Arc::new(Budget::unlimited());
        SpatialIndex::build(&snapshot_of(graphs), ReadModel::Materialised, &budget).unwrap()
    }

    fn within(box_: String) -> Expression {
        Expression::FunctionCall(
            Function::Custom(NamedNode::new_unchecked(format!(
                "{}sfWithin",
                super::super::geo::GEOF
            ))),
            vec![
                Expression::Variable(Variable::new_unchecked("w")),
                Expression::Literal(Literal::new_typed_literal(
                    box_,
                    node(format!("{GEO}wktLiteral")),
                )),
            ],
        )
    }

    fn as_wkt() -> TriplePattern {
        TriplePattern {
            subject: TermPattern::Variable(Variable::new_unchecked("g")),
            predicate: NamedNodePattern::NamedNode(node(format!("{GEO}asWKT"))),
            object: TermPattern::Variable(Variable::new_unchecked("w")),
        }
    }

    /// The guard of the scoped index's cost (performance.md §0): no graph is kept per
    /// shape or R-tree entry. Over one graph the literals are as unscoped and a scope that
    /// shows the graph checks nothing; over several, a placement's graph is its run's, one
    /// run per graph and kind, and features within one graph cross nothing.
    #[test]
    fn graphs_are_runs_not_marks() {
        let named = GraphName::NamedNode(NamedNode::new_unchecked("urn:graph"));
        let default = Scope::Graphs(GraphSelector::Exact(TermId::DEFAULT_GRAPH));
        let one = index_of(&[GraphName::DefaultGraph, GraphName::DefaultGraph]);
        assert!(one.spread.is_empty());
        assert_eq!(one.scoped(&default).shown, Shown::All);
        assert_eq!(one.runs.len(), 2, "one run of geometries, one of features");
        assert!(one.several.is_empty());
        let several = index_of(&[
            named.clone(),
            GraphName::DefaultGraph,
            named,
            GraphName::DefaultGraph,
        ]);
        assert_eq!(several.runs.len(), 4, "per kind, one run per graph");
        assert!(several.crossings.is_empty() && several.several.is_empty());
        assert_eq!(several.scoped(&default).shown, Shown::Some);
        assert_eq!(
            several.scoped(&Scope::Graphs(GraphSelector::Any)).shown,
            Shown::All
        );
    }

    /// A packed R-tree finds what a scan of its entries finds, at sizes around its levels'
    /// edges, and its inner nodes fill exactly the array charged for them. Its nodes cut
    /// by both axes: a query in the gap between two bands of points (Sol's) checks the top
    /// level alone.
    #[test]
    fn a_packed_tree_finds_what_a_scan_finds() {
        let budget = Arc::new(Budget::unlimited());
        let mut state = 7u64;
        let mut next = |n: u64| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (state >> 33) % n
        };
        for n in [0, 1, 6, 7, 36, 37, 216, 217, 4_097] {
            let mut entries: Charged<(Bbox, u64)> = ChargedVec::new(Arc::clone(&budget));
            for i in 0..n {
                let (x, y) = (next(1_000) as f64, next(1_000) as f64);
                let (w, h) = (next(20) as f64, next(20) as f64);
                entries.push(([x, y, x + w, y + h], i)).unwrap();
            }
            let all: Vec<(Bbox, u64)> = entries.to_vec();
            let tree = Packed::pack(entries, &budget).unwrap();
            assert_eq!(
                tree.nodes.charged(),
                tree.nodes.len() * size_of::<Node>(),
                "{n}"
            );
            for _ in 0..50 {
                let (x, y) = (next(1_000) as f64, next(1_000) as f64);
                let query = [x, y, x + next(200) as f64, y + next(200) as f64];
                let mut found = Vec::new();
                tree.search(&query, &mut |i| found.push(i));
                found.sort_unstable();
                let mut scanned: Vec<u64> = all
                    .iter()
                    .filter(|(b, _)| meets(b, &query))
                    .map(|&(_, i)| i)
                    .collect();
                scanned.sort_unstable();
                assert_eq!(found, scanned, "{n} {query:?}");
            }
        }
        // The boxes a query checks, from the top.
        fn checked(
            tree: &Packed<u64>,
            level: usize,
            nodes: std::ops::Range<usize>,
            query: &Bbox,
        ) -> usize {
            let (start, end) = (tree.levels[level], tree.levels[level + 1]);
            let lowest = level + 1 == tree.depth;
            let below = if lowest {
                tree.entries.len()
            } else {
                tree.levels[level + 2] - end
            };
            let mut count = nodes.len();
            for i in nodes.start + start..nodes.end + start {
                let last = if i + 1 < end {
                    tree.nodes[i + 1].children
                } else {
                    below
                };
                let children = tree.nodes[i].children..last;
                count += match (meets(&tree.nodes[i].bbox, query), lowest) {
                    (false, _) => 0,
                    (true, true) => children.len(),
                    (true, false) => checked(tree, level + 1, children, query),
                };
            }
            count
        }
        let n = 65_536;
        let mut bands: Charged<(Bbox, u64)> = ChargedVec::new(Arc::clone(&budget));
        for i in 0..n {
            let (x, y) = (i as f64, (i % 2) as f64);
            bands.push(([x, y, x, y], i)).unwrap();
        }
        let tree = Packed::pack(bands, &budget).unwrap();
        let top = tree.levels[1];
        assert_eq!(checked(&tree, 0, 0..top, &[-1.0, 0.5, n as f64, 0.5]), top);
    }

    /// What a build allocates is charged before it allocates, and what the index keeps
    /// stays charged while it lives: a budget too small fails the build with nothing left
    /// charged, and dropping the index returns everything.
    #[test]
    fn the_index_is_charged_to_the_request() {
        let graphs = vec![GraphName::DefaultGraph; 2_000];
        let snapshot = snapshot_of(&graphs);
        let small = Arc::new(Budget::new(64 * 1024));
        assert!(SpatialIndex::build(&snapshot, ReadModel::Materialised, &small).is_err());
        assert_eq!(small.used(), 0);
        let budget = Arc::new(Budget::unlimited());
        let index = SpatialIndex::build(&snapshot, ReadModel::Materialised, &budget).unwrap();
        let kept = budget.used();
        // At least the placements, the shapes and the R-tree entries.
        assert!(kept > 4_000 * (size_of::<Placement>() + size_of::<(Bbox, u64)>()));
        assert!(budget.peak() >= kept);
        drop(index);
        assert_eq!(budget.used(), 0);
    }

    /// The guard of the spatial path's memory: a filter's seed and a relation pattern over
    /// a cold index and a warm one, over sizes from a handful of shapes to 17,000 (Sol's),
    /// in one graph and in several, with WKT, GeoJSON and GML literals, their keys plain and
    /// compressed (a reopened checkpoint's); Sol's literal and filter constant of 10,000
    /// points; and Sol's relation between two lines of 10,000 points under 256 bytes. None
    /// holds (counted on every thread by this test binary's allocator, at every allocation
    /// and release) a byte more than its request was charged since it started: geo's
    /// working memory for a relation is charged before geo computes it, and the engine's
    /// for a scan and a lookup before it runs. Inputs and work all run on the guard's pool,
    /// never Rayon's global one: a parallel build there leaves its workers to look
    /// for work once more, which under load can fall in a later measurement, a worker's first
    /// look allocating its crossbeam-epoch handle there (2,304 bytes, attributed 20 Oct).
    #[test]
    fn the_spatial_path_holds_no_more_than_it_charges() {
        use nrese_exec::measure::{alone, measure_within};
        if !alone() {
            return;
        }
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        pool.broadcast(|_| ());
        let side = |n: usize| (n as f64).sqrt().ceil() as usize;
        let literal = |n: usize, i: usize| {
            let (x, y) = (i % side(n), i / side(n));
            match i % 3 {
                0 => ("asWKT", "wktLiteral", format!("POINT({x} {y})")),
                1 => (
                    "asGeoJSON",
                    "geoJSONLiteral",
                    format!(
                        r#"{{"type":"Polygon","coordinates":[[[{x},{y}],[{x}.5,{y}],[{x}.5,{y}.5],[{x},{y}]]]}}"#
                    ),
                ),
                _ => (
                    "asGML",
                    "gmlLiteral",
                    format!(
                        r#"<gml:Point xmlns:gml="http://www.opengis.net/gml/3.2"><gml:pos>{x} {y}</gml:pos></gml:Point>"#
                    ),
                ),
            }
        };
        let named = |g: usize| match g {
            0 => GraphName::DefaultGraph,
            g => GraphName::NamedNode(NamedNode::new_unchecked(format!("urn:graph{g}"))),
        };
        // ?x sfIntersects ?y, with ?y bound to urn:g0 by the rows it joins.
        let relation = TriplePattern {
            subject: TermPattern::Variable(Variable::new_unchecked("x")),
            predicate: NamedNodePattern::NamedNode(node(format!("{GEO}sfIntersects"))),
            object: TermPattern::Variable(Variable::new_unchecked("y")),
        };
        let g0 = |snapshot: &Snapshot| {
            let g0 = snapshot.lookup(node("urn:g0".to_owned()).as_ref().into());
            g0.expect("urn:g0").raw()
        };
        // The rows a join reads, made in its measurement and charged as their producer
        // charges them: their header and their column.
        let at = |ctx: &Context<'_>, g0: u64| {
            let header = size_of::<Variable>() + "y".len() + size_of::<Vec<u64>>();
            ctx.budget.charge(header + size_of::<u64>()).ok()?;
            Some(Solutions {
                vars: vec![Variable::new_unchecked("y")],
                table: IdTable::from_columns(vec![vec![g0]]),
                ordered: false,
            })
        };
        let line = |points: usize| {
            let coordinates: Vec<String> = (0..points).map(|i| format!("{i} 0")).collect();
            format!("LINESTRING({})", coordinates.join(","))
        };
        let mut failures = Vec::new();
        let rows = [
            (7, 1, false),
            (100, 1, false),
            (1_000, 3, true),
            (17_000, 1, false),
            (17_000, 4, false),
        ];
        for (n, graphs, compressed) in rows {
            let all: Vec<GraphName> = (0..n).map(|i| named(i % graphs)).collect();
            let (_dir, snapshot) = match compressed {
                false => (
                    None,
                    pool.install(|| snapshot_with(&all, |i| literal(n, i))),
                ),
                true => {
                    let (dir, snapshot) = pool.install(|| reopened(&all, |i| literal(n, i)));
                    (Some(dir), snapshot)
                }
            };
            // Every shape: 17,000 candidates take the parallel chunks.
            let edge = side(n) + 1;
            let filter = within(format!(
                "POLYGON((-1 -1, {edge} -1, {edge} {edge}, -1 {edge}, -1 -1))"
            ));
            let pattern = as_wkt();
            let path = |ctx: &Context<'_>, g0: u64| {
                let (seed, _) = ctx
                    .spatial_seed(&[&filter], std::slice::from_ref(&pattern))
                    .ok()
                    .flatten()
                    .expect("a seed");
                // Each table freed, then its charge released.
                let rows = seed.table.len();
                ctx.consumed(seed);
                let joined = ctx
                    .spatial_join(at(ctx, g0).expect("room"), &relation)
                    .ok()
                    .expect("the relation's rows");
                let rows = rows + joined.table.len();
                ctx.consumed(joined);
                rows
            };
            // Cold, then warm: the index built under another request, the pattern's own
            // charges alone.
            for phase in ["cold", "warm"] {
                let envelope = Envelope::unlimited();
                let mut options = QueryOptions {
                    union_default_graph: true,
                    ..QueryOptions::default()
                };
                options.resources.memory = envelope.clone();
                let (rows, excess) = pool.install(|| {
                    let ctx = Context::new(&snapshot, &options, None, None);
                    let g0 = g0(&snapshot);
                    measure_within(&envelope, || path(&ctx, g0))
                });
                assert!(rows > n, "{n} in {graphs}");
                if excess > 0 {
                    failures.push(format!("{n} shapes in {graphs} graphs, {phase}: {excess}"));
                }
            }
        }
        // Sol's literal of 10,000 points, decoded from a plain key and a compressed one.
        for compressed in [false, true] {
            let statement = |_| ("asWKT", "wktLiteral", line(10_000));
            let (_dir, snapshot) = match compressed {
                false => (
                    None,
                    pool.install(|| snapshot_with(&[GraphName::DefaultGraph], statement)),
                ),
                true => {
                    let (dir, snapshot) =
                        pool.install(|| reopened(&[GraphName::DefaultGraph], statement));
                    (Some(dir), snapshot)
                }
            };
            let budget = Arc::new(Budget::unlimited());
            let (built, excess) = pool.install(|| {
                measure_within(&budget, || {
                    SpatialIndex::build(&snapshot, ReadModel::Materialised, &budget).is_ok()
                })
            });
            assert!(built);
            if excess > 0 {
                failures.push(format!(
                    "a literal of 10,000 points, compressed {compressed}: {excess}"
                ));
            }
        }
        // A point related to rings of 1,000 vertices: covered by the index's largest shape.
        let ring = |r: usize| {
            let points: Vec<String> = (0..=1_000)
                .map(|k| {
                    let t = (k % 1_000) as f64 / 1_000.0 * std::f64::consts::TAU;
                    format!("{} {}", r as f64 * t.cos(), r as f64 * t.sin())
                })
                .collect();
            format!("POLYGON(({}))", points.join(","))
        };
        let snapshot = pool.install(|| {
            snapshot_with(&vec![GraphName::DefaultGraph; 4], |i| match i {
                0 => ("asWKT", "wktLiteral", "POINT(0 0)".to_owned()),
                i => ("asWKT", "wktLiteral", ring(10 * i)),
            })
        });
        let envelope = Envelope::unlimited();
        let mut options = QueryOptions::default();
        options.resources.memory = envelope.clone();
        let point = g0(&snapshot);
        let (related, excess) = pool.install(|| {
            let warm = Context::new(&snapshot, &QueryOptions::default(), None, None);
            assert!(warm.spatial_index().is_ok());
            let ctx = Context::new(&snapshot, &options, None, None);
            measure_within(&envelope, || {
                let rows = at(&ctx, point).expect("room");
                ctx.spatial_join(rows, &relation)
                    .map_or(0, |s| s.table.len())
            })
        });
        if related == 0 || excess > 0 {
            failures.push(format!("a point among rings: {related} rows, {excess}"));
        }
        // Sol's relation between two equal lines of 10,000 points, under 256 bytes.
        let snapshot = pool.install(|| {
            snapshot_with(&vec![GraphName::DefaultGraph; 2], |_| {
                ("asWKT", "wktLiteral", line(10_000))
            })
        });
        let envelope = Envelope::new(Some(256), None);
        let mut options = QueryOptions::default();
        options.resources.memory = envelope.clone();
        let g0 = g0(&snapshot);
        let (related, excess) = pool.install(|| {
            let warm = Context::new(&snapshot, &QueryOptions::default(), None, None);
            assert!(warm.spatial_index().is_ok());
            let ctx = Context::new(&snapshot, &options, None, None);
            measure_within(&envelope, || {
                at(&ctx, g0).is_some_and(|rows| ctx.spatial_join(rows, &relation).is_ok())
            })
        });
        if related || excess > 0 {
            failures.push(format!(
                "lines of 10,000 points: related {related}, {excess}"
            ));
        }
        // And a filter constant of 10,000 points over a warm index, under 16 bytes.
        let snapshot = pool.install(|| snapshot_of(&vec![GraphName::DefaultGraph; 7]));
        let (filter, pattern) = (within(line(10_000)), as_wkt());
        let envelope = Envelope::new(Some(16), None);
        let mut options = QueryOptions::default();
        options.resources.memory = envelope.clone();
        let (seeded, excess) = pool.install(|| {
            let warm = Context::new(&snapshot, &QueryOptions::default(), None, None);
            assert!(warm.spatial_index().is_ok());
            let ctx = Context::new(&snapshot, &options, None, None);
            measure_within(&envelope, || {
                ctx.spatial_seed(&[&filter], std::slice::from_ref(&pattern))
                    .is_ok()
            })
        });
        if seeded || excess > 0 {
            failures.push(format!(
                "a filter constant of 10,000 points: seeded {seeded}, {excess}"
            ));
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// Sol's (round 2): a seed from a warm index under a 64 KiB envelope, room for its
    /// constant but not for its 17,000 candidates, is refused as its candidates grow past
    /// it, before they are allocated (it holds nothing uncharged), and leaves nothing
    /// charged.
    #[test]
    fn a_warm_seed_is_charged_before_it_allocates() {
        if !nrese_exec::measure::alone() {
            return;
        }
        let snapshot = snapshot_of(&vec![GraphName::DefaultGraph; 17_000]);
        let square = "POLYGON((-1 -1, 20000 -1, 20000 20000, -1 20000, -1 -1))";
        let filter = within(square.to_owned());
        let pattern = as_wkt();
        let warm = Context::new(&snapshot, &QueryOptions::default(), None, None);
        let (seed, _) = warm
            .spatial_seed(&[&filter], std::slice::from_ref(&pattern))
            .ok()
            .flatten()
            .expect("a seed");
        assert_eq!(seed.table.len(), 17_000);
        let limit = 64 << 10;
        let envelope = Envelope::new(Some(limit), None);
        let mut options = QueryOptions::default();
        options.resources.memory = envelope.clone();
        let cold = Context::new(&snapshot, &options, None, None);
        let (seeded, excess) = nrese_exec::measure::measure_within(&envelope, || {
            cold.spatial_seed(&[&filter], std::slice::from_ref(&pattern))
                .is_ok()
        });
        assert!(
            !seeded && excess == 0,
            "seeded {seeded}, {excess} bytes uncharged"
        );
        drop(cold);
        assert_eq!(envelope.used(), 0);
        // Past the constant's charge: the candidates were charged as they grew.
        assert!(envelope.peak() > shape_bound(square.len()) && envelope.peak() <= limit);
    }
}
