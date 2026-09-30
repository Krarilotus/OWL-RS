//! GeoSPARQL relations in triple patterns (the query-rewrite extension):
//! `?building geo:sfWithin ex:Berlin` holds between two spatial objects, features or
//! geometries, whose geometries stand in the relation.
//!
//! A geometry is a node with a `geo:asWKT`, `geo:asGeoJSON` or `geo:asGML` literal; a feature's geometries are its
//! `geo:hasDefaultGeometry`, or else its `geo:hasGeometry` ones; the relation holds if it
//! holds for some pair of their shapes (in one reference system). Relations are computed
//! from the data, not read from statements.
//!
//! An R-tree over the shapes' bounding boxes finds the candidates: only the disjointness
//! relations (`sfDisjoint`, `ehDisjoint`, `rcc8dc`) look at every object. The index is
//! built from the statements of all graphs at the first spatial pattern and kept while the
//! snapshot is the latest one queried. A pattern with a constant side runs first and
//! starts the basic graph pattern's joins; others run after them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId};
use nrese_exec::{IdTable, UNDEF};
use oxrdf::Term;
use oxrdf::{NamedNodeRef, Variable};
use rstar::primitives::{GeomWithData, Rectangle};
use rstar::{AABB, RTree};
use spargebra::algebra::{Expression, Function};
use spargebra::term::{NamedNodePattern, TermPattern, TriplePattern};

use super::geo::{RELATIONS, Shape, holds_apart, parse, relation};
use super::{Context, NativeResult, Solutions};

const GEO: &str = "http://www.opengis.net/ont/geosparql#";

/// The properties from a geometry to its literals.
const SERIALISATIONS: [&str; 3] = ["asWKT", "asGeoJSON", "asGML"];

type Entry = GeomWithData<Rectangle<[f64; 2]>, u64>;

pub(super) struct SpatialIndex {
    shapes: HashMap<u64, Vec<Arc<Shape>>>,
    tree: RTree<Entry>,
    /// The geometry literals themselves, for filters over them.
    literals: RTree<Entry>,
}

type Cached = (Snapshot, ReadModel, Arc<SpatialIndex>);
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

impl SpatialIndex {
    fn build(snapshot: &Snapshot, model: ReadModel) -> Self {
        let id = |local: &str| {
            snapshot.lookup(NamedNodeRef::new_unchecked(&format!("{GEO}{local}")).into())
        };
        let with = |predicate: Option<TermId>| -> Vec<(u64, u64)> {
            let Some(predicate) = predicate else {
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
                .map(|q| (q.subject.raw(), q.object.raw()))
                .collect()
        };
        let mut shapes: HashMap<u64, Vec<Arc<Shape>>> = HashMap::new();
        let mut literal_entries: HashMap<u64, Entry> = HashMap::new();
        let serialisations = SERIALISATIONS.iter().flat_map(|local| with(id(local)));
        for (geometry, literal) in serialisations {
            if let Some(shape) = snapshot
                .decode(TermId::from_raw(literal))
                .as_ref()
                .and_then(parse)
            {
                if let Some(b) = bbox(&shape) {
                    literal_entries.entry(literal).or_insert_with(|| {
                        GeomWithData::new(Rectangle::from_corners(b.lower(), b.upper()), literal)
                    });
                }
                shapes.entry(geometry).or_default().push(Arc::new(shape));
            }
        }
        let mut features: HashMap<u64, Vec<Arc<Shape>>> = HashMap::new();
        let defaults = with(id("hasDefaultGeometry"));
        for (feature, geometry) in &defaults {
            if let Some(found) = shapes.get(geometry) {
                features
                    .entry(*feature)
                    .or_default()
                    .extend(found.iter().cloned());
            }
        }
        for (feature, geometry) in with(id("hasGeometry")) {
            if defaults.iter().any(|(f, _)| *f == feature) {
                continue;
            }
            if let Some(found) = shapes.get(&geometry) {
                features
                    .entry(feature)
                    .or_default()
                    .extend(found.iter().cloned());
            }
        }
        for (feature, found) in features {
            shapes.entry(feature).or_default().extend(found);
        }
        let entries: Vec<Entry> = shapes
            .iter()
            .flat_map(|(&object, found)| {
                found.iter().filter_map(move |shape| {
                    let b = bbox(shape)?;
                    Some(GeomWithData::new(
                        Rectangle::from_corners(b.lower(), b.upper()),
                        object,
                    ))
                })
            })
            .collect();
        Self {
            shapes,
            tree: RTree::bulk_load(entries),
            literals: RTree::bulk_load(literal_entries.into_values().collect()),
        }
    }

    /// Whether `relation` holds between the spatial objects `a` and `b`.
    fn holds(&self, local: &str, a: u64, b: u64) -> bool {
        let (Some(xs), Some(ys)) = (self.shapes.get(&a), self.shapes.get(&b)) else {
            return false;
        };
        xs.iter().any(|x| {
            ys.iter().any(|y| {
                x.crs == y.crs && relation(local, &x.geometry, &y.geometry).unwrap_or(false)
            })
        })
    }

    /// The objects `relation` may hold for with `known` on one side.
    fn candidates(&self, local: &str, known: u64) -> Vec<u64> {
        if holds_apart(local) {
            return self.shapes.keys().copied().collect();
        }
        let mut out: Vec<u64> = self
            .shapes
            .get(&known)
            .into_iter()
            .flatten()
            .filter_map(|shape| bbox(shape))
            .flat_map(|b| {
                self.tree
                    .locate_in_envelope_intersecting(&b)
                    .map(|entry| entry.data)
                    .collect::<Vec<_>>()
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
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
            let index = self.spatial_index();
            let mut table = IdTable::new(1);
            for entry in index.literals.locate_in_envelope_intersecting(&b) {
                table.push_row(&[entry.data]);
            }
            return Some(Solutions {
                vars: vec![variable],
                table,
                ordered: false,
            });
        }
        None
    }

    fn spatial_index(&self) -> Arc<SpatialIndex> {
        let mut cache = CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((snapshot, model, index)) = cache.as_ref()
            && snapshot.same_version(self.snapshot)
            && *model == self.model
        {
            return Arc::clone(index);
        }
        let index = Arc::new(SpatialIndex::build(self.snapshot, self.model));
        *cache = Some((self.snapshot.clone(), self.model, Arc::clone(&index)));
        index
    }

    fn side(&self, term: &TermPattern, solutions: &Solutions) -> Side {
        match term {
            TermPattern::NamedNode(n) => {
                Side::Constant(self.snapshot.lookup(n.as_ref().into()).map(TermId::raw))
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
        let index = self.spatial_index();
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
            let pairs: Vec<(u64, u64)> = match (a, b) {
                (Some(a), Some(b)) => {
                    if index.holds(local, a, b) {
                        vec![(a, b)]
                    } else {
                        Vec::new()
                    }
                }
                (Some(a), None) => index
                    .candidates(local, a)
                    .into_iter()
                    .filter(|&b| index.holds(local, a, b))
                    .map(|b| (a, b))
                    .collect(),
                (None, Some(b)) => index
                    .candidates(local, b)
                    .into_iter()
                    .filter(|&a| index.holds(local, a, b))
                    .map(|a| (a, b))
                    .collect(),
                (None, None) => {
                    let mut all: Vec<u64> = index.shapes.keys().copied().collect();
                    all.sort_unstable();
                    let mut pairs = Vec::new();
                    for a in all {
                        for b in index.candidates(local, a) {
                            if index.holds(local, a, b) {
                                pairs.push((a, b));
                            }
                        }
                    }
                    pairs
                }
            };
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
        self.consumed(&solutions);
        self.produced(Solutions {
            vars,
            table,
            ordered: false,
        })
    }
}
