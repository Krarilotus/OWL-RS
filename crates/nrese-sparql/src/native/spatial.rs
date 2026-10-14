//! GeoSPARQL's spatial questions answered through indexes.
//!
//! **Filters against a constant geometry** (`FILTER(geof:sfWithin(?g, "POLYGON(…)"^^geo:wktLiteral))`,
//! the constant a literal or an expression of constants such as `geof:buffer(…)`): the
//! store's geometry index ([`nrese_geo::GeometryIndex`], every literal of a geometry
//! datatype whichever property states it, GeoSPARQL 1.1 §8.5) gives the literals whose
//! box can stand in the relation. Where they are fewer than the statements of the
//! smallest pattern that binds the variable, they start the basic graph pattern's joins
//! ([`Context::spatial_seed`]); the filter itself runs compiled ([`ConstantRelation`] in
//! `fast`), its boxes deciding most rows without reading a geometry.
//!
//! **Relations in triple patterns** (the query-rewrite extension):
//! `?building geo:sfWithin ex:Berlin` holds between two spatial objects, features or
//! geometries, whose geometries stand in the relation.
//!
//! A geometry is a node with a serialisation: a `geo:hasSerialization`, `geo:asWKT`,
//! `geo:asGeoJSON` or `geo:asGML` literal. A feature's geometries are its
//! `geo:hasDefaultGeometry`, or else its `geo:hasGeometry` ones; the relation holds if it
//! holds for some pair of their shapes (in one reference system). Relations are computed
//! from the geometries, and read from the statements that assert them: a pattern matches
//! both. A property the data declares a sub-property of one of these
//! (`geonames:asWKT rdfs:subPropertyOf geo:asWKT`) counts where reasoning has derived its
//! statements with the super-property (the RDFS entailment regime); the GeoSPARQL
//! compliance benchmark's answers leave such features out of the queries it runs
//! without entailment.
//!
//! An R-tree over the shapes' bounding boxes finds the candidates: only the disjointness
//! relations (`sfDisjoint`, `ehDisjoint`, `rcc8dc`) look at every object. The index is
//! built from the statements of all graphs at the first spatial pattern and kept while the
//! snapshot is the latest one queried. A pattern with a constant side runs first and
//! starts the basic graph pattern's joins; others run after them.

use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::sync::{Arc, Mutex};

use geo::Rect;
use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId, TermView};
use nrese_exec::{Budget, IdTable, Reservation, UNDEF};
use nrese_geo::{Boxes, GeometryIndex, Shape, boxes, decided_by_boxes};
use nrese_rdf::{NamedNodeRef, Term, Variable};
use nrese_sparql_syntax::algebra::{Expression, Function};
use nrese_sparql_syntax::term::{NamedNodePattern, TermPattern, TriplePattern};
use rstar::primitives::{GeomWithData, Rectangle};
use rstar::{AABB, RTree};

use super::fast::Tri;
use super::geo::{GEOF, RELATIONS, holds_apart, parse, relation};
use super::{Context, NativeResult, Solutions};

const GEO: &str = nrese_geo::GEO;

/// The properties from a geometry to its literals.
const SERIALISATIONS: [&str; 4] = ["hasSerialization", "asWKT", "asGeoJSON", "asGML"];

type Entry = GeomWithData<Rectangle<[f64; 2]>, u64>;

pub(super) struct SpatialIndex {
    shapes: HashMap<u64, Vec<Arc<Shape>>>,
    tree: RTree<Entry>,
    /// Relation statements in the data: per relation, subject to objects and back.
    asserted: HashMap<(&'static str, u64), Vec<u64>>,
    asserted_back: HashMap<(&'static str, u64), Vec<u64>>,
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
    let rect = shape.rect()?;
    Some(AABB::from_corners(
        [rect.min().x, rect.min().y],
        [rect.max().x, rect.max().y],
    ))
}

/// The value of an expression of constants that a filter computes the same in every row:
/// a constant, or a GeoSPARQL function of such (`geof:buffer(<point>, 3000, uom:metre)`).
fn constant_term(expression: &Expression) -> Option<Term> {
    match expression {
        Expression::Literal(l) => Some(l.clone().into()),
        Expression::NamedNode(n) => Some(n.clone().into()),
        Expression::FunctionCall(Function::Custom(name), args)
            if name.as_str().starts_with(GEOF) =>
        {
            let args: Option<Vec<Term>> = args.iter().map(constant_term).collect();
            super::geo::call(name.as_str(), &args?)
        }
        _ => None,
    }
}

/// A relation filter between a variable and a constant geometry,
/// `geof:sfWithin(?v, C)` or `geof:sfWithin(C, ?v)`.
pub(crate) struct ConstantRelation {
    pub(super) variable: Variable,
    local: &'static str,
    /// Whether the variable is the relation's first argument.
    first: bool,
    constant: Shape,
    rect: Option<Rect<f64>>,
}

impl ConstantRelation {
    /// The relation `conjunct` states, if it is one between a variable and a constant
    /// geometry.
    pub(super) fn of(conjunct: &Expression) -> Option<Self> {
        let Expression::FunctionCall(Function::Custom(name), args) = conjunct else {
            return None;
        };
        let name = name.as_str().strip_prefix(GEOF)?;
        let local = *RELATIONS.iter().find(|r| **r == name)?;
        let (variable, first, other) = match args.as_slice() {
            [Expression::Variable(v), other] => (v, true, other),
            [other, Expression::Variable(v)] => (v, false, other),
            _ => return None,
        };
        let constant = parse(&constant_term(other)?)?;
        Some(Self {
            variable: variable.clone(),
            local,
            first,
            rect: constant.rect(),
            constant,
        })
    }

    /// The relation between the variable's geometry and the constant, in argument order.
    fn holds(&self, geometry: &geo::Geometry<f64>) -> Option<bool> {
        if self.first {
            relation(self.local, geometry, &self.constant.geometry)
        } else {
            relation(self.local, &self.constant.geometry, geometry)
        }
    }

    /// What the boxes decide, the variable's box `rect` (`None`: empty).
    fn by_boxes(&self, rect: Option<&Rect<f64>>) -> Option<bool> {
        if self.first {
            decided_by_boxes(self.local, rect, self.rect.as_ref())
        } else {
            decided_by_boxes(self.local, self.rect.as_ref(), rect)
        }
    }

    /// The index traversal for the variable's box against the constant's box.
    fn candidate_boxes(&self) -> Option<Boxes> {
        let required = boxes(self.local)?;
        Some(match (required, self.first) {
            (Boxes::Meet, _) => Boxes::Meet,
            (Boxes::Inside | Boxes::Equal, true) | (Boxes::Contains | Boxes::Equal, false) => {
                Boxes::Inside
            }
            (Boxes::Contains, true) | (Boxes::Inside, false) => Boxes::Contains,
        })
    }
}

/// [`ConstantRelation`] compiled for a snapshot: its geometry index and the constant's
/// reference system there (module docs).
pub(crate) struct CompiledRelation {
    relation: ConstantRelation,
    index: GeometryIndex,
    crs: Option<u32>,
}

impl CompiledRelation {
    pub(crate) fn compile(conjunct: &Expression, snapshot: &Snapshot) -> Option<Self> {
        let relation = ConstantRelation::of(conjunct)?;
        let index = snapshot.geometries();
        let crs = index.crs_number(&relation.constant.crs);
        Some(Self {
            relation,
            index,
            crs,
        })
    }

    pub(crate) fn variable(&self) -> &Variable {
        &self.relation.variable
    }

    /// The filter on the stored term `id` (as `geof:` evaluates it: an error for a term
    /// that isn't a geometry, or one in another reference system): the box decides where
    /// it can, the geometry (a point from its box, else read) where it can't.
    pub(crate) fn eval<'v>(
        &self,
        id: TermId,
        view: &'v dyn Fn(TermId) -> Option<TermView<'v>>,
    ) -> Tri {
        // The index holds every literal of the dictionary that reads as a geometry.
        let Some(entry) = self.index.get(id.raw()) else {
            return Tri::Error;
        };
        if Some(entry.crs) != self.crs {
            return Tri::Error;
        }
        if let Some(decided) = self.relation.by_boxes(entry.rect.as_ref()) {
            return Tri::of(decided);
        }
        let holds = match entry.as_point() {
            Some(point) => self.relation.holds(&geo::Geometry::Point(point)),
            None => match view(id) {
                Some(TermView::Typed { value, datatype }) => nrese_geo::parse(datatype, value)
                    .and_then(|shape| self.relation.holds(&shape.geometry)),
                _ => return Tri::Unknown,
            },
        };
        holds.map_or(Tri::Error, Tri::of)
    }
}

/// The variable of `geof:envelope(?v)`, if `expression` is that call: the geometry index
/// answers it for stored literals from the box it keeps ([`envelope_of`]).
pub(super) fn envelope_argument(expression: &Expression) -> Option<&Variable> {
    let Expression::FunctionCall(Function::Custom(name), args) = expression else {
        return None;
    };
    if name.as_str().strip_prefix(GEOF) != Some("envelope") {
        return None;
    }
    match args.as_slice() {
        [Expression::Variable(v)] => Some(v),
        _ => None,
    }
}

/// `geof:envelope` of the stored term `id`, from `index` (each geometry read once, when the
/// index took it in): its box as a polygon, written as the literal is; `None`, an error,
/// for a term that isn't a geometry or an empty one, as the function gives.
pub(super) fn envelope_of(index: &GeometryIndex, id: u64) -> Option<Term> {
    let entry = index.get(id)?;
    Some(super::geo::envelope(
        entry.rect.as_ref()?,
        index.crs(entry.crs),
        entry.format,
    ))
}

/// The properties `locals` (local names in `geo:`) the store knows.
fn properties(snapshot: &Snapshot, locals: &[&str]) -> Vec<TermId> {
    locals
        .iter()
        .filter_map(|local| {
            snapshot.lookup(NamedNodeRef::new_unchecked(&format!("{GEO}{local}")).into())
        })
        .collect()
}

impl SpatialIndex {
    fn build(snapshot: &Snapshot, model: ReadModel) -> Self {
        let with = |predicates: &[TermId]| -> Vec<(u64, u64)> {
            predicates
                .iter()
                .flat_map(|&predicate| {
                    let pattern = QuadPattern {
                        subject: None,
                        predicate: Some(predicate),
                        object: None,
                        graph: GraphSelector::Any,
                    };
                    snapshot
                        .quads_for_pattern_in(model, &pattern)
                        .map(|q| (q.subject.raw(), q.object.raw()))
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        let mut shapes: HashMap<u64, Vec<Arc<Shape>>> = HashMap::new();
        let serialisations = with(&properties(snapshot, &SERIALISATIONS));
        for (geometry, literal) in serialisations {
            if let Some(shape) = snapshot
                .decode(TermId::from_raw(literal))
                .as_ref()
                .and_then(parse)
            {
                shapes.entry(geometry).or_default().push(Arc::new(shape));
            }
        }
        let mut features: HashMap<u64, Vec<Arc<Shape>>> = HashMap::new();
        let defaults = with(&properties(snapshot, &["hasDefaultGeometry"]));
        for (feature, geometry) in &defaults {
            if let Some(found) = shapes.get(geometry) {
                features
                    .entry(*feature)
                    .or_default()
                    .extend(found.iter().cloned());
            }
        }
        let with_default: HashSet<u64> = defaults.iter().map(|&(f, _)| f).collect();
        for (feature, geometry) in with(&properties(snapshot, &["hasGeometry"])) {
            if with_default.contains(&feature) {
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
        let (mut asserted, mut asserted_back): (HashMap<_, Vec<u64>>, HashMap<_, Vec<u64>>) =
            (HashMap::new(), HashMap::new());
        for &local in RELATIONS {
            for (a, b) in with(&properties(snapshot, &[local])) {
                asserted.entry((local, a)).or_default().push(b);
                asserted_back.entry((local, b)).or_default().push(a);
            }
        }
        Self {
            shapes,
            tree: RTree::bulk_load(entries),
            asserted,
            asserted_back,
        }
    }

    fn local(local: &str) -> Option<&'static str> {
        RELATIONS.iter().copied().find(|r| *r == local)
    }

    /// The objects a statement relates `a` to.
    fn asserted_objects(&self, local: &str, a: u64) -> &[u64] {
        Self::local(local)
            .and_then(|l| self.asserted.get(&(l, a)))
            .map_or(&[], Vec::as_slice)
    }

    /// The subjects a statement relates to `b`.
    fn asserted_subjects(&self, local: &str, b: u64) -> &[u64] {
        Self::local(local)
            .and_then(|l| self.asserted_back.get(&(l, b)))
            .map_or(&[], Vec::as_slice)
    }

    /// Whether the relation holds between `a` and `b`: stated, or computed.
    fn related(&self, local: &str, a: u64, b: u64) -> bool {
        self.asserted_objects(local, a).contains(&b) || self.holds(local, a, b)
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
        // A box cannot narrow a relation from an empty geometry. Use the same exact
        // candidate fallback as disjointness, including objects with no tree entry.
        if holds_apart(local)
            || self
                .shapes
                .get(&known)
                .is_some_and(|shapes| shapes.iter().any(|shape| bbox(shape).is_none()))
        {
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

impl Context<'_> {
    /// A query-owned index hit buffer: reserve geometric growth and reallocation
    /// overlap before allocation. No shared accounting in between buffer growths.
    pub(super) fn spatial_candidate(
        &self,
        found: &mut Vec<u64>,
        held: &mut Reservation<&Budget>,
        id: u64,
    ) -> NativeResult<()> {
        if found.len() == found.capacity() {
            let capacity = found
                .capacity()
                .max(2)
                .checked_mul(2)
                .ok_or_else(|| self.too_large(usize::MAX, 1))?;
            let bytes = capacity
                .checked_mul(8)
                .ok_or_else(|| self.too_large(usize::MAX, 1))?;
            held.charge(bytes)
                .map_err(super::QueryEvaluationError::MemoryLimit)?;
            let old = found.capacity() * 8;
            found.reserve_exact(capacity - found.len());
            held.release(old);
        }
        found.push(id);
        Ok(())
    }
    /// For a basic graph pattern under `conjuncts`: when one of them relates a variable
    /// the pattern binds to a constant geometry, the geometry literals whose boxes can
    /// stand in the relation, as rows to start the joins from (the conjunct still
    /// decides), if they are fewer than the statements of the smallest pattern that binds
    /// the variable.
    pub(super) fn spatial_seed(
        &self,
        conjuncts: &[&Expression],
        patterns: &[TriplePattern],
    ) -> NativeResult<Option<Solutions>> {
        if self.as_written {
            return Ok(None);
        }
        for conjunct in conjuncts {
            let start = std::time::Instant::now();
            let Some(filter) = ConstantRelation::of(conjunct) else {
                continue;
            };
            let binding =
                |t: &&TriplePattern| super::triple_variables(t).contains(&filter.variable);
            // The statements of the smallest pattern that binds the variable.
            let Some(smallest) = patterns
                .iter()
                .filter(binding)
                .filter_map(|t| self.scan_pattern(t))
                .map(|scan| self.snapshot.estimate_in(self.model, &scan.quad_pattern()))
                .min()
            else {
                continue;
            };
            let index = self.snapshot.geometries();
            let (Some(rect), Some(required)) = (filter.rect.as_ref(), filter.candidate_boxes())
            else {
                continue;
            };
            self.check()?;
            let mut candidates = Vec::new();
            let mut candidates_held = self.working(0)?;
            let crs = index.crs_number(&filter.constant.crs);
            let mut visited = 0usize;
            if let ControlFlow::Break(error) = index.visit_boxes(required, rect, |id| {
                if visited.is_multiple_of(1024)
                    && let Err(error) = self.check()
                {
                    return ControlFlow::Break(error);
                }
                visited += 1;
                if index.get(id).is_some_and(|e| {
                    Some(e.crs) == crs && filter.by_boxes(e.rect.as_ref()) != Some(false)
                }) && let Err(error) =
                    self.spatial_candidate(&mut candidates, &mut candidates_held, id)
                {
                    return ControlFlow::Break(error);
                }
                ControlFlow::Continue(())
            }) {
                return Err(error);
            }
            self.check()?;
            if candidates.len() as u64 >= smallest {
                continue;
            }
            if self.trace.is_some() {
                let detail = format!(
                    "{}: the geometry literals whose box can stand in {} (of {} indexed; \
                     the smallest pattern binding it has {smallest} statements)",
                    filter.variable,
                    filter.local,
                    index.len()
                );
                self.note("geometry index", detail, None, candidates.len(), start);
            }
            let seed_held = self.working(
                candidates
                    .len()
                    .checked_mul(8)
                    .ok_or_else(|| self.too_large(usize::MAX, 1))?,
            )?;
            let mut table = IdTable::with_capacity(1, candidates.len());
            for (row, id) in candidates.into_iter().enumerate() {
                if row.is_multiple_of(1024) {
                    self.check()?;
                }
                table.push_row(&[id]);
            }
            self.check()?;
            seed_held.transfer();
            return Ok(Some(Solutions {
                vars: vec![filter.variable],
                table,
                ordered: false,
            }));
        }
        Ok(None)
    }

    fn spatial_index(&self) -> Arc<SpatialIndex> {
        let mut cache = CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((snapshot, model, index)) = cache.as_ref()
            && snapshot.same_version(&self.snapshot)
            && *model == self.model
        {
            return Arc::clone(index);
        }
        let index = Arc::new(SpatialIndex::build(&self.snapshot, self.model));
        *cache = Some(((*self.snapshot).clone(), self.model, Arc::clone(&index)));
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
        let same = matches!((&s, &o), (Side::Free(a), Side::Free(b)) if a == b)
            || matches!((&s, &o), (Side::Bound(a), Side::Bound(b)) if a == b);
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
            let mut pairs: Vec<(u64, u64)> = match (a, b) {
                (Some(a), Some(b)) => {
                    if index.related(local, a, b) {
                        vec![(a, b)]
                    } else {
                        Vec::new()
                    }
                }
                (Some(a), None) => index
                    .candidates(local, a)
                    .into_iter()
                    .filter(|&b| index.holds(local, a, b))
                    .chain(index.asserted_objects(local, a).iter().copied())
                    .map(|b| (a, b))
                    .collect(),
                (None, Some(b)) => index
                    .candidates(local, b)
                    .into_iter()
                    .filter(|&a| index.holds(local, a, b))
                    .chain(index.asserted_subjects(local, b).iter().copied())
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
                    if let Some(l) = SpatialIndex::local(local) {
                        for ((relation, a), objects) in &index.asserted {
                            if *relation == l {
                                pairs.extend(objects.iter().map(|&b| (*a, b)));
                            }
                        }
                    }
                    pairs
                }
            };
            // A pair both stated and computed counts once.
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
        self.consumed(&solutions);
        self.produced(Solutions {
            vars,
            table,
            ordered: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QueryOptions;
    use nrese_engine::{Engine, EngineConfig};
    use nrese_rdf::{GraphName, Literal, NamedNode, Quad};

    /// VALUES ?x { UNDEF }; ?x geo:sfEquals ?x must retain the diagonal: two
    /// identical geometry nodes yield two rows, never the four-pair cross product.
    /// Exercise both the fresh-variable and existing-UNDEF kernel entry paths.
    #[test]
    fn values_undef_same_variable_equals_fresh_spatial_evaluation() {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for name in ["urn:a", "urn:b"] {
            tx.insert(
                Quad::new(
                    NamedNode::new_unchecked(name),
                    NamedNode::new_unchecked(format!("{GEO}asWKT")),
                    Literal::new_typed_literal(
                        "POINT(0 0)",
                        NamedNode::new_unchecked(nrese_geo::WKT_LITERAL),
                    ),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            );
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let x = Variable::new_unchecked("x");
        let triple = TriplePattern {
            subject: TermPattern::Variable(x.clone()),
            predicate: NamedNodePattern::NamedNode(NamedNode::new_unchecked(format!(
                "{GEO}sfEquals"
            ))),
            object: TermPattern::Variable(x.clone()),
        };
        for as_written in [false, true] {
            let options = QueryOptions {
                as_written,
                ..QueryOptions::default()
            };
            let rows = |seeded| {
                let ctx = Context::new(&snapshot, &options, None, None);
                let seed = if seeded {
                    ctx.eval(&nrese_sparql_syntax::algebra::GraphPattern::Values {
                        variables: vec![x.clone()],
                        bindings: vec![vec![None]],
                    })
                    .unwrap_or_else(|_| panic!("VALUES with UNDEF must evaluate"))
                } else {
                    Solutions::unit()
                };
                let found = ctx
                    .spatial_join(seed, &triple)
                    .unwrap_or_else(|_| panic!("same-variable spatial pattern must evaluate"));
                let mut rows = found.table.column(0).to_vec();
                rows.sort_unstable();
                rows
            };
            let expected = rows(false);
            assert_eq!(expected.len(), 2);
            assert_eq!(rows(true), expected);
        }
    }
}
