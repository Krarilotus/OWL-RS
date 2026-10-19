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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, SnapshotIdentity, TermId};
use nrese_exec::{IdTable, UNDEF};
use nrese_rdf::Term;
use nrese_rdf::{NamedNodeRef, Variable};
use nrese_sparql_syntax::algebra::{Expression, Function};
use nrese_sparql_syntax::term::{NamedNodePattern, TermPattern, TriplePattern};
use rstar::primitives::{GeomWithData, Rectangle};
use rstar::{AABB, RTree};

use super::geo::{RELATIONS, Shape, holds_apart, parse, relation};
use super::{Context, GraphScope, NativeResult, Solutions, unsupported};

const GEO: &str = "http://www.opengis.net/ont/geosparql#";

/// The properties from a geometry to its literals.
const SERIALISATIONS: [&str; 3] = ["asWKT", "asGeoJSON", "asGML"];

type Rect = Rectangle<[f64; 2]>;

/// The flag of a value of [`SpatialIndex::objects`] that numbers a list of positions
/// ([`SpatialIndex::several`]), and of a literal's mark that numbers a list of graphs.
const LIST: u32 = 1 << 31;

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

/// The geometry literals, for filters over them: as is over one graph; over several, each
/// marked with the graph that gives it (its number in [`SpatialIndex::graphs`]), or the
/// list of graphs ([`LIST`]) when several do.
enum Literals {
    One(RTree<GeomWithData<Rect, u64>>),
    Several {
        tree: RTree<GeomWithData<Rect, (u64, u32)>>,
        lists: Vec<Vec<TermId>>,
    },
}

struct SpatialIndex {
    /// Geometry placements (positions `0..geometries.len()`), graph by graph.
    geometries: Vec<Placement>,
    /// Feature placements (the positions after the geometries'): graph by graph those
    /// of one graph, then from [`Self::crossing_from`] the crossing ones.
    features: Vec<Placement>,
    /// Where each graph's run of positions starts, in position order.
    runs: Vec<(u32, TermId)>,
    crossing_from: u32,
    /// Beside each crossing placement, its graphs.
    crossings: Vec<Crossing>,
    /// For a feature with a crossing `geo:hasGeometry` shape, the graphs of its
    /// `geo:hasDefaultGeometry` statements (which hide that shape where shown).
    defaults: HashMap<u64, Vec<TermId>>,
    /// Per object, its placement's position, or [`LIST`] and the number of its positions in
    /// [`Self::several`].
    objects: HashMap<u64, u32>,
    several: Vec<Vec<u32>>,
    /// The placements' bounding boxes, each with its position.
    tree: RTree<GeomWithData<Rect, u32>>,
    literals: Literals,
    /// Relation statements in the data: per relation, subject to objects and back, each
    /// with the graph that states it.
    asserted: HashMap<(&'static str, u64), Vec<(u64, TermId)>>,
    asserted_back: HashMap<(&'static str, u64), Vec<(u64, TermId)>>,
    /// The graphs of the statements read, sorted, each once.
    graphs: Vec<TermId>,
}

/// The graphs whose statements a spatial pattern reads: the active graph (the search's
/// corpus is scoped the same way).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Scope {
    /// One graph, every graph, or every named graph.
    Graphs(GraphSelector),
    /// The merge of these graphs, sorted and each once.
    Merge(Vec<TermId>),
}

impl Scope {
    fn merge(graphs: &[TermId]) -> Self {
        let mut graphs = graphs.to_vec();
        graphs.sort_unstable();
        graphs.dedup();
        Self::Merge(graphs)
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

fn bbox(shape: &Shape) -> Option<AABB<[f64; 2]>> {
    use geo::BoundingRect;
    let rect = shape.geometry.bounding_rect()?;
    Some(AABB::from_corners(
        [rect.min().x, rect.min().y],
        [rect.max().x, rect.max().y],
    ))
}

fn rect(b: AABB<[f64; 2]>) -> Rect {
    Rectangle::from_corners(b.lower(), b.upper())
}

fn local_of(local: &str) -> Option<&'static str> {
    RELATIONS.iter().copied().find(|r| *r == local)
}

fn position(n: usize) -> u32 {
    u32::try_from(n)
        .ok()
        .filter(|&p| p < LIST)
        .expect("fewer spatial placements than 2^31")
}

/// The graph of the run `p` falls in.
fn run_of(runs: &[(u32, TermId)], p: u32) -> TermId {
    runs[runs.partition_point(|&(start, _)| start <= p) - 1].1
}

/// Records that `object` has a placement at `p`.
fn place(objects: &mut HashMap<u64, u32>, several: &mut Vec<Vec<u32>>, object: u64, p: u32) {
    use std::collections::hash_map::Entry;
    match objects.entry(object) {
        Entry::Vacant(entry) => {
            entry.insert(p);
        }
        Entry::Occupied(mut entry) => {
            let value = *entry.get();
            if value & LIST != 0 {
                several[(value & !LIST) as usize].push(p);
            } else {
                several.push(vec![value, p]);
                *entry.get_mut() = position(several.len() - 1) | LIST;
            }
        }
    }
}

/// Orders statements graph by graph (if not so already).
fn by_graph(statements: &mut [(u64, u64, TermId)]) {
    if !statements.is_sorted_by_key(|s| s.2) {
        statements.sort_unstable_by_key(|s| s.2);
    }
}

impl SpatialIndex {
    fn build(snapshot: &Snapshot, model: ReadModel) -> Self {
        // The statements of a predicate: subject, object, graph.
        let read = |local: &str| -> Vec<(u64, u64, TermId)> {
            let name = format!("{GEO}{local}");
            let Some(predicate) = snapshot.lookup(NamedNodeRef::new_unchecked(&name).into()) else {
                return Vec::new();
            };
            let pattern = QuadPattern {
                subject: None,
                predicate: Some(predicate),
                object: None,
                graph: GraphSelector::Any,
            };
            snapshot
                .quads_for_pattern_in(model, &pattern)
                .map(|q| (q.subject.raw(), q.object.raw(), q.graph))
                .collect()
        };
        let mut statements = read(SERIALISATIONS[0]);
        for local in &SERIALISATIONS[1..] {
            statements.extend(read(local));
        }
        by_graph(&mut statements);

        // Geometries, graph by graph; each literal parsed once.
        let mut geometries: Vec<Placement> = Vec::with_capacity(statements.len());
        let mut runs: Vec<(u32, TermId)> = Vec::new();
        // Per literal, the position of its first placement (`None`: not a shape).
        let mut first: HashMap<u64, Option<u32>> = HashMap::new();
        // The literals several graphs give, with their graphs.
        let mut spread: HashMap<u64, Vec<TermId>> = HashMap::new();
        for (object, literal, graph) in statements {
            let p = position(geometries.len());
            let shape = match first.get(&literal) {
                Some(None) => continue,
                Some(&Some(earlier)) => {
                    let earlier_graph = run_of(&runs, earlier);
                    if earlier_graph != graph {
                        let graphs = spread.entry(literal).or_insert_with(|| vec![earlier_graph]);
                        if !graphs.contains(&graph) {
                            graphs.push(graph);
                        }
                    }
                    Arc::clone(&geometries[earlier as usize].shape)
                }
                None => match snapshot
                    .decode(TermId::from_raw(literal))
                    .as_ref()
                    .and_then(parse)
                {
                    None => {
                        first.insert(literal, None);
                        continue;
                    }
                    Some(shape) => {
                        first.insert(literal, Some(p));
                        Arc::new(shape)
                    }
                },
            };
            if runs.last().is_none_or(|&(_, g)| g != graph) {
                runs.push((p, graph));
            }
            geometries.push(Placement { object, shape });
        }
        let mut objects: HashMap<u64, u32> = HashMap::new();
        let mut several: Vec<Vec<u32>> = Vec::new();
        for (p, placement) in geometries.iter().enumerate() {
            place(&mut objects, &mut several, placement.object, position(p));
        }

        // Features: through their default geometries, else through their other ones.
        let mut defaults = read("hasDefaultGeometry");
        defaults.sort_unstable();
        let links = read("hasGeometry");
        let defaults_of = |feature: u64| {
            let from = defaults.partition_point(|d| d.0 < feature);
            let to = defaults.partition_point(|d| d.0 <= feature);
            &defaults[from..to]
        };
        let mut within: Vec<(TermId, Placement)> = Vec::new();
        let mut crossing: Vec<(Placement, Crossing)> = Vec::new();
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
                    within.push((link, placement));
                } else {
                    let graphs = Crossing {
                        link,
                        default,
                        geometry: from,
                    };
                    crossing.push((placement, graphs));
                    if hidden_elsewhere {
                        hidden_by
                            .entry(feature)
                            .or_insert_with(|| own.iter().map(|d| d.2).collect());
                    }
                }
            }
        }
        if !within.is_sorted_by_key(|w| w.0) {
            within.sort_unstable_by_key(|w| w.0);
        }
        let start = geometries.len();
        let mut features: Vec<Placement> = Vec::with_capacity(within.len() + crossing.len());
        let mut run = None;
        for (graph, placement) in within {
            if run != Some(graph) {
                runs.push((position(start + features.len()), graph));
                run = Some(graph);
            }
            features.push(placement);
        }
        let crossing_from = position(start + features.len());
        let mut crossings = Vec::with_capacity(crossing.len());
        for (placement, graphs) in crossing {
            features.push(placement);
            crossings.push(graphs);
        }
        for (i, placement) in features.iter().enumerate() {
            place(
                &mut objects,
                &mut several,
                placement.object,
                position(start + i),
            );
        }

        let mut asserted: HashMap<_, Vec<_>> = HashMap::new();
        let mut asserted_back: HashMap<_, Vec<_>> = HashMap::new();
        let mut stated_in = Vec::new();
        for &local in RELATIONS {
            for (a, b, graph) in read(local) {
                asserted.entry((local, a)).or_default().push((b, graph));
                asserted_back
                    .entry((local, b))
                    .or_default()
                    .push((a, graph));
                stated_in.push(graph);
            }
        }
        let mut graphs: Vec<TermId> = runs
            .iter()
            .map(|&(_, g)| g)
            .chain(crossings.iter().flat_map(|c| [c.link, c.geometry]))
            .chain(hidden_by.values().flatten().copied())
            .chain(stated_in)
            .collect();
        graphs.sort_unstable();
        graphs.dedup();

        let shape_at = |p: u32| {
            let p = p as usize;
            match p < start {
                true => &geometries[p].shape,
                false => &features[p - start].shape,
            }
        };
        let total = position(start + features.len());
        let entries: Vec<GeomWithData<Rect, u32>> = (0..total)
            .filter_map(|p| Some(GeomWithData::new(rect(bbox(shape_at(p))?), p)))
            .collect();
        let literals = match graphs.len() > 1 {
            false => Literals::One(RTree::bulk_load(
                first
                    .into_iter()
                    .filter_map(|(literal, p)| {
                        let b = bbox(shape_at(p?))?;
                        Some(GeomWithData::new(rect(b), literal))
                    })
                    .collect(),
            )),
            true => {
                let mut lists = Vec::new();
                let entries = first
                    .into_iter()
                    .filter_map(|(literal, p)| {
                        let p = p?;
                        let b = bbox(shape_at(p))?;
                        let mark = match spread.remove(&literal) {
                            Some(graphs) => {
                                lists.push(graphs);
                                position(lists.len() - 1) | LIST
                            }
                            None => {
                                let graph = run_of(&runs, p);
                                position(graphs.binary_search(&graph).expect("a graph read"))
                            }
                        };
                        Some(GeomWithData::new(rect(b), (literal, mark)))
                    })
                    .collect();
                Literals::Several {
                    tree: RTree::bulk_load(entries),
                    lists,
                }
            }
        };
        Self {
            tree: RTree::bulk_load(entries),
            geometries,
            features,
            runs,
            crossing_from,
            crossings,
            defaults: hidden_by,
            objects,
            several,
            literals,
            asserted,
            asserted_back,
            graphs,
        }
    }

    fn shape_at(&self, p: u32) -> &Arc<Shape> {
        let (p, start) = (p as usize, self.geometries.len());
        match p < start {
            true => &self.geometries[p].shape,
            false => &self.features[p - start].shape,
        }
    }

    fn object_at(&self, p: u32) -> u64 {
        let (p, start) = (p as usize, self.geometries.len());
        match p < start {
            true => self.geometries[p].object,
            false => self.features[p - start].object,
        }
    }

    /// The positions of `object`'s placements.
    fn positions(&self, object: u64) -> &[u32] {
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
    /// Every graph it holds: nothing to check.
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
    /// Whether the scope shows the placement at `p`.
    fn shows(&self, p: u32) -> bool {
        let index = self.index;
        match self.shown {
            Shown::All => true,
            Shown::Nothing => false,
            Shown::Some if p < index.crossing_from => self.scope.contains(run_of(&index.runs, p)),
            Shown::Some => {
                let c = index.crossings[(p - index.crossing_from) as usize];
                self.scope.contains(c.link)
                    && self.scope.contains(c.geometry)
                    && (c.default
                        || !index
                            .defaults
                            .get(&index.object_at(p))
                            .is_some_and(|graphs| graphs.iter().any(|&g| self.scope.contains(g))))
            }
        }
    }

    /// The shapes of `object` the scope shows.
    fn shapes(&self, object: u64) -> impl Iterator<Item = &Arc<Shape>> + '_ {
        self.index
            .positions(object)
            .iter()
            .filter(|&&p| self.shows(p))
            .map(|&p| self.index.shape_at(p))
    }

    /// The objects with a shape the scope shows.
    fn objects(&self) -> Vec<u64> {
        match self.shown {
            Shown::All => self.index.objects.keys().copied().collect(),
            Shown::Nothing => Vec::new(),
            Shown::Some => self
                .index
                .objects
                .keys()
                .copied()
                .filter(|&o| self.shapes(o).next().is_some())
                .collect(),
        }
    }

    /// The geometry literals whose bounding box meets `b`.
    fn literals(&self, b: &AABB<[f64; 2]>) -> Vec<u64> {
        if self.shown == Shown::Nothing {
            return Vec::new();
        }
        match &self.index.literals {
            Literals::One(tree) => tree
                .locate_in_envelope_intersecting(b)
                .map(|entry| entry.data)
                .collect(),
            Literals::Several { tree, lists } => {
                let all = self.shown == Shown::All;
                tree.locate_in_envelope_intersecting(b)
                    .filter(|entry| {
                        let mark = entry.data.1;
                        all || match mark & LIST != 0 {
                            true => lists[(mark & !LIST) as usize]
                                .iter()
                                .any(|&g| self.scope.contains(g)),
                            false => self.scope.contains(self.index.graphs[mark as usize]),
                        }
                    })
                    .map(|entry| entry.data.0)
                    .collect()
            }
        }
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
    fn holds(&self, local: &str, a: u64, b: u64) -> bool {
        self.shapes(a).any(|x| {
            self.shapes(b).any(|y| {
                x.crs == y.crs && relation(local, &x.geometry, &y.geometry).unwrap_or(false)
            })
        })
    }

    /// The objects of the placements whose box meets the shape at `known` and for whose
    /// shape `test(known's shape, theirs)` holds. Complete for every relation but
    /// disjointness: a pair in any other relation shares a point, so their boxes meet.
    fn near(&self, known: u32, test: impl Fn(&Shape, &Shape) -> bool, out: &mut Vec<u64>) {
        let index = self.index;
        let x = index.shape_at(known);
        let Some(b) = bbox(x) else {
            return;
        };
        for entry in index.tree.locate_in_envelope_intersecting(&b) {
            let p = entry.data;
            if self.shows(p) {
                let y = index.shape_at(p);
                if x.crs == y.crs && test(x, y) {
                    out.push(index.object_at(p));
                }
            }
        }
    }

    /// The pairs `relation` relates with `a` and `b` on its sides (`None`: any object).
    fn pairs(&self, local: &str, a: Option<u64>, b: Option<u64>) -> Vec<(u64, u64)> {
        let index = self.index;
        let holds =
            |x: &Shape, y: &Shape| relation(local, &x.geometry, &y.geometry).unwrap_or(false);
        let apart = holds_apart(local);
        match (a, b) {
            (Some(a), Some(b)) => {
                let stated = self.stated(&index.asserted, local, a).any(|x| x == b);
                match stated || self.holds(local, a, b) {
                    true => vec![(a, b)],
                    false => Vec::new(),
                }
            }
            (Some(a), None) => {
                let mut found = Vec::new();
                if apart {
                    found.extend(
                        self.objects()
                            .into_iter()
                            .filter(|&b| self.holds(local, a, b)),
                    );
                } else {
                    for &p in index.positions(a) {
                        if self.shows(p) {
                            self.near(p, holds, &mut found);
                        }
                    }
                }
                found.extend(self.stated(&index.asserted, local, a));
                found.into_iter().map(|b| (a, b)).collect()
            }
            (None, Some(b)) => {
                let mut found = Vec::new();
                if apart {
                    found.extend(
                        self.objects()
                            .into_iter()
                            .filter(|&a| self.holds(local, a, b)),
                    );
                } else {
                    for &p in index.positions(b) {
                        if self.shows(p) {
                            self.near(p, |y, x| holds(x, y), &mut found);
                        }
                    }
                }
                found.extend(self.stated(&index.asserted_back, local, b));
                found.into_iter().map(|a| (a, b)).collect()
            }
            (None, None) => {
                let mut pairs = Vec::new();
                if apart {
                    let mut all = self.objects();
                    all.sort_unstable();
                    for &a in &all {
                        for &b in &all {
                            if self.holds(local, a, b) {
                                pairs.push((a, b));
                            }
                        }
                    }
                } else {
                    let mut found = Vec::new();
                    let total = position(index.geometries.len() + index.features.len());
                    for p in (0..total).filter(|&p| self.shows(p)) {
                        found.clear();
                        self.near(p, holds, &mut found);
                        let a = index.object_at(p);
                        pairs.extend(found.iter().map(|&b| (a, b)));
                    }
                }
                if let Some(l) = local_of(local) {
                    for ((relation, a), objects) in &index.asserted {
                        if *relation == l {
                            pairs.extend(
                                objects
                                    .iter()
                                    .filter(|&&(_, graph)| self.scope.contains(graph))
                                    .map(|&(b, _)| (*a, b)),
                            );
                        }
                    }
                }
                pairs
            }
        }
    }
}

/// One side of a spatial pattern.
enum Side {
    Constant(Option<u64>),
    Bound(usize),
    Free(Variable),
}

/// A filter conjunct `geof:relation(?v, constant)` (or the other way round) for a
/// relation the index can find candidates for: the variable and the constant's shape.
fn indexed_filter(conjunct: &Expression) -> Option<(Variable, Shape)> {
    let Expression::FunctionCall(Function::Custom(name), args) = conjunct else {
        return None;
    };
    let local = name.as_str().strip_prefix(super::geo::GEOF)?;
    if !RELATIONS.contains(&local) || holds_apart(local) {
        return None;
    }
    match args.as_slice() {
        [Expression::Variable(v), Expression::Literal(l)]
        | [Expression::Literal(l), Expression::Variable(v)] => {
            Some((v.clone(), parse(&Term::from(l.clone()))?))
        }
        _ => None,
    }
}

impl Context<'_> {
    /// For a basic graph pattern under `conjuncts`: when one of them relates a variable
    /// that is the object of a `geo:asWKT` (`asGeoJSON`, `asGML`) pattern to a constant
    /// shape, the geometry literals whose bounding box meets the shape's, as rows to start the joins from (the
    /// conjunct still decides).
    pub(super) fn spatial_seed(
        &self,
        conjuncts: &[&Expression],
        patterns: &[TriplePattern],
    ) -> Option<Solutions> {
        if self.as_written {
            return None;
        }
        for conjunct in conjuncts {
            let Some((variable, shape)) = indexed_filter(conjunct) else {
                continue;
            };
            let bound_by_wkt = patterns.iter().any(|t| {
                matches!(&t.predicate, NamedNodePattern::NamedNode(n)
                    if n.as_str().strip_prefix(GEO).is_some_and(|l| SERIALISATIONS.contains(&l)))
                    && matches!(&t.object, TermPattern::Variable(o) if *o == variable)
            });
            let Some(b) = bbox(&shape) else {
                continue;
            };
            if !bound_by_wkt {
                continue;
            }
            // `GRAPH ?g` at once: the literals of the graphs it may bind.
            let scope = self.spatial_scope().unwrap_or_else(|| match &self.named {
                Some(named) => Scope::merge(named),
                None => Scope::Graphs(GraphSelector::AnyNamed),
            });
            let index = self.spatial_index();
            let literals = index.scoped(&scope).literals(&b);
            return Some(Solutions {
                vars: vec![variable],
                table: IdTable::from_columns(vec![literals]),
                ordered: false,
            });
        }
        None
    }

    /// The scope of a spatial pattern in the active graph; `None` while `GRAPH ?g` is
    /// evaluated for all graphs at once.
    fn spatial_scope(&self) -> Option<Scope> {
        Some(match &*self.graph.borrow() {
            GraphScope::Default => Scope::Graphs(GraphSelector::Exact(TermId::DEFAULT_GRAPH)),
            GraphScope::Named(graph) => Scope::Graphs(GraphSelector::Exact(*graph)),
            GraphScope::Union => match &self.merge_set {
                Some(graphs) => Scope::merge(graphs),
                None => Scope::Graphs(GraphSelector::Any),
            },
            GraphScope::Missing => Scope::Merge(Vec::new()),
            GraphScope::Variable(_) => return None,
        })
    }

    fn spatial_index(&self) -> Arc<SpatialIndex> {
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
            return index;
        }
        // Built without the lock: no other query waits for this one's build.
        let index = Arc::new(SpatialIndex::build(&self.snapshot, model));
        let mut cache = lock();
        if let Some(built) = cached(&cache) {
            return built;
        }
        *cache = Some((identity, model, Arc::clone(&index)));
        index
    }

    fn side(&self, term: &TermPattern, solutions: &Solutions) -> Side {
        match term {
            TermPattern::NamedNode(n) => {
                Side::Constant(self.lookup_const(n.as_ref().into()).map(TermId::raw))
            }
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
        }
    }

    /// `solutions` joined with the spatial pattern `triple`.
    pub(super) fn spatial_join(
        &self,
        solutions: Solutions,
        triple: &TriplePattern,
    ) -> NativeResult<Solutions> {
        let local = relation_of(triple).expect("a spatial pattern");
        // `GRAPH ?g` sends spatial relations graph by graph (`scans_everywhere`).
        let Some(scope) = self.spatial_scope() else {
            return unsupported("a spatial relation read in all graphs at once");
        };
        let index = self.spatial_index();
        let index = index.scoped(&scope);
        let (s, o) = (
            self.side(&triple.subject, &solutions),
            self.side(&triple.object, &solutions),
        );
        let mut vars = solutions.vars.clone();
        let mut added = Vec::new();
        for side in [&s, &o] {
            if let Side::Free(v) = side
                && !added.contains(v)
            {
                added.push(v.clone());
                vars.push(v.clone());
            }
        }
        let same = matches!((&s, &o), (Side::Free(a), Side::Free(b)) if a == b);
        let mut table = IdTable::new(vars.len());
        let mut line = vec![UNDEF; vars.len()];
        let width = solutions.vars.len();
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
            let mut pairs = index.pairs(local, a, b);
            // A pair found twice (both stated and computed, or through two shapes) once.
            pairs.sort_unstable();
            pairs.dedup();
            for (a, b) in pairs {
                if same && a != b {
                    continue;
                }
                line[..width].copy_from_slice(&solutions.table.row(row));
                let mut bound_free = |side: &Side, value: u64| {
                    if let Side::Free(v) = side
                        && let Some(position) = added.iter().position(|x| x == v)
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
                table.push_row(&line);
            }
        }
        self.consumed(solutions);
        self.produced(Solutions {
            vars,
            table,
            ordered: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use nrese_engine::{Engine, EngineConfig};
    use nrese_rdf::{GraphName, Literal, NamedNode, Quad};

    use super::*;

    /// An index over geometries `urn:g{i}`, each in `graphs[i]`, and a feature linked to
    /// each in the same graph.
    fn index_of(graphs: &[GraphName]) -> SpatialIndex {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        let node = |s: String| NamedNode::new_unchecked(s);
        for (i, graph) in graphs.iter().enumerate() {
            let wkt = Literal::new_typed_literal(
                format!("POINT({i} {i})"),
                node(format!("{GEO}wktLiteral")),
            );
            let geometry = node(format!("urn:g{i}"));
            let quads = [
                Quad::new(
                    geometry.clone(),
                    node(format!("{GEO}asWKT")),
                    wkt,
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
        SpatialIndex::build(&engine.snapshot(), ReadModel::Materialised)
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
        assert!(matches!(one.literals, Literals::One(_)));
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
}
