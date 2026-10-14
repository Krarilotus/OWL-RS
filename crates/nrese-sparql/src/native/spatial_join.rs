//! Spatial joins: `FILTER(geof:sfIntersects(?a, ?b))` (any topological relation but the
//! disjointness ones) between parts of a pattern that share no variable, `?a` bound by
//! one and `?b` by the other, in every row. Geographica's joins (Q18–27) relate the
//! geometries of two datasets so; evaluated as written they were the cross product,
//! filtered pair by pair (157–284 s each, or the memory budget at once).
//!
//! The parts are a basic graph pattern's connected components (two triple patterns are
//! connected by a shared variable or blank node) with the conjuncts that read only their
//! variables, or the inputs of a join under the filter (whose own conjuncts pushdown has
//! moved into them). Parts that share variables form a group; a group linked to another by
//! such a conjunct joins it through the relation:
//!
//! 1. The group with the smaller estimate ([`Context::estimate`]) is evaluated first.
//! 2. Its geometries' boxes find, in the store's geometry index, the literals the other
//!    side's variable could take. Fewer than the other group's estimate over the probe
//!    factor, they start that group's evaluation (an index nested-loop join: the group is
//!    evaluated from them, [`Context::eval_from`]); else the group is evaluated alone.
//! 3. The two sides' geometries are joined by [`nrese_geo::join`]: an R-tree over the
//!    smaller side's boxes, the candidates refined by the relation itself, each geometry
//!    read once (filter and refine; Bast et al., SIGSPATIAL 2025). The rows are those of
//!    the cross product the conjunct passes, so the conjunct is done.
//!
//! Further groups the conjuncts link join the same way; the rest are joined as before
//! (a cross product) and the other conjuncts filter the result.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::time::Instant;

use geo::Geometry;
use nrese_engine::{TermId, TermView};
use nrese_exec::{Budget, IdTable, Reservation, UNDEF, computed_index};
use nrese_geo::{Boxes, GeometryIndex, Item, JoinError, Side};
use nrese_rdf::Variable;
use nrese_sparql_syntax::algebra::{Expression, Function, GraphPattern};
use nrese_sparql_syntax::term::{NamedNodePattern, TriplePattern};

use super::exists::mentioned;
use super::geo::{GEOF, RELATIONS, holds_apart, parse};
use super::{
    Context, NativeResult, PROBE_FACTOR, QueryEvaluationError, Solutions, expression_variables,
    pushdown, triple_variables,
};

/// The relation and its two variables, if `conjunct` is `geof:R(?a, ?b)` for a relation
/// whose pairs boxes narrow.
fn link(conjunct: &Expression) -> Option<(&'static str, Variable, Variable)> {
    let Expression::FunctionCall(Function::Custom(name), args) = conjunct else {
        return None;
    };
    let name = name.as_str().strip_prefix(GEOF)?;
    let local = *RELATIONS.iter().find(|r| **r == name)?;
    if holds_apart(local) {
        return None;
    }
    match args.as_slice() {
        [Expression::Variable(a), Expression::Variable(b)] if a != b => {
            Some((local, a.clone(), b.clone()))
        }
        _ => None,
    }
}

/// A part: its pattern, the variables it mentions and those it binds in every row.
struct Part {
    pattern: GraphPattern,
    mentioned: Vec<Variable>,
    certain: Vec<Variable>,
}

impl Part {
    fn new(pattern: GraphPattern) -> Self {
        Self {
            mentioned: mentioned(&pattern),
            certain: pushdown::certain(&pattern),
            pattern,
        }
    }
}

/// The parts that share variables, transitively, as groups of their positions.
fn groups(mentioned: &[Vec<Variable>]) -> Vec<Vec<usize>> {
    let mut group: Vec<usize> = (0..mentioned.len()).collect();
    fn root(group: &mut [usize], mut i: usize) -> usize {
        while group[i] != i {
            group[i] = group[group[i]];
            i = group[i];
        }
        i
    }
    for i in 0..mentioned.len() {
        for j in i + 1..mentioned.len() {
            if mentioned[i].iter().any(|v| mentioned[j].contains(v)) {
                let (a, b) = (root(&mut group, i), root(&mut group, j));
                group[a] = b;
            }
        }
    }
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut at: HashMap<usize, usize> = HashMap::new();
    for i in 0..mentioned.len() {
        let r = root(&mut group, i);
        let slot = *at.entry(r).or_insert_with(|| {
            out.push(Vec::new());
            out.len() - 1
        });
        out[slot].push(i);
    }
    out
}

/// Whether every row of `pattern` takes `variable` from a stored statement: it is made
/// of basic graph patterns (filtered, in constant graphs, joined) and one of them binds it.
/// Then a seed of stored ids for the variable is the same as joining the pattern's rows.
fn scanned(pattern: &GraphPattern, variable: &Variable) -> bool {
    fn plain(pattern: &GraphPattern) -> bool {
        match pattern {
            GraphPattern::Bgp { .. } => true,
            GraphPattern::Filter { inner, .. } => plain(inner),
            GraphPattern::Graph {
                name: NamedNodePattern::NamedNode(_),
                inner,
            } => plain(inner),
            GraphPattern::Join { inputs } => inputs.iter().all(plain),
            _ => false,
        }
    }
    plain(pattern) && pushdown::certain(pattern).contains(variable)
}

/// One side's geometries: its distinct ids, what the join needs of each, and the
/// geometries of the values the query computed (which no index holds).
struct Geometries<'b> {
    ids: Vec<u64>,
    items: Vec<Item>,
    computed: HashMap<u64, Geometry<f64>>,
    _held: Reservation<&'b Budget>,
}

impl Context<'_> {
    /// `FILTER(conjuncts)` over `patterns`, if the basic graph pattern falls into parts a
    /// conjunct relates spatially (module docs); `None` if it doesn't.
    pub(super) fn spatial_bgp(
        &self,
        patterns: &[TriplePattern],
        conjuncts: &[&Expression],
    ) -> NativeResult<Option<Solutions>> {
        if self.as_written || !conjuncts.iter().any(|c| link(c).is_some()) {
            return Ok(None);
        }
        let variables: Vec<Vec<Variable>> = patterns.iter().map(triple_variables).collect();
        let components = groups(&variables);
        if components.len() < 2 {
            return Ok(None);
        }
        let component_variables: Vec<Vec<Variable>> = components
            .iter()
            .map(|c| c.iter().flat_map(|&i| variables[i].clone()).collect())
            .collect();
        // A conjunct on one component's variables goes with it; the others stay.
        let mut own: Vec<Vec<Expression>> = vec![Vec::new(); components.len()];
        let mut rest: Vec<&Expression> = Vec::new();
        for &conjunct in conjuncts {
            let read = expression_variables(conjunct);
            let home = pushdown::movable(conjunct)
                .then(|| {
                    component_variables
                        .iter()
                        .position(|vars| read.iter().all(|v| vars.contains(v)))
                })
                .flatten();
            match home {
                Some(k) => own[k].push(conjunct.clone()),
                None => rest.push(conjunct),
            }
        }
        let parts: Vec<Part> = components
            .iter()
            .zip(own)
            .map(|(component, own)| {
                let bgp = GraphPattern::Bgp {
                    patterns: component.iter().map(|&i| patterns[i].clone()).collect(),
                };
                Part::new(match Expression::balanced(own, Expression::And) {
                    Some(expr) => GraphPattern::Filter {
                        expr,
                        inner: Box::new(bgp),
                    },
                    None => bgp,
                })
            })
            .collect();
        self.linked(parts, &rest)
    }

    /// `FILTER(conjuncts)` over the join of `inputs`, if a conjunct relates inputs that
    /// share no variable spatially (module docs); `None` if none does.
    pub(super) fn spatial_inputs(
        &self,
        inputs: &[GraphPattern],
        conjuncts: &[&Expression],
    ) -> NativeResult<Option<Solutions>> {
        if self.as_written || inputs.len() < 2 || !conjuncts.iter().any(|c| link(c).is_some()) {
            return Ok(None);
        }
        let parts = inputs.iter().cloned().map(Part::new).collect();
        self.linked(parts, conjuncts)
    }

    /// The parts joined: groups a conjunct links through its relation, the rest as before,
    /// then the other conjuncts; `None` if no conjunct links two groups.
    fn linked(
        &self,
        parts: Vec<Part>,
        conjuncts: &[&Expression],
    ) -> NativeResult<Option<Solutions>> {
        let mentioned: Vec<Vec<Variable>> = parts.iter().map(|p| p.mentioned.clone()).collect();
        let groups = groups(&mentioned);
        let certain_in =
            |g: usize, v: &Variable| groups[g].iter().any(|&p| parts[p].certain.contains(v));
        let links: Vec<Option<(&'static str, Variable, Variable)>> =
            conjuncts.iter().map(|c| link(c)).collect();
        // A link from the groups `joined` to one outside them: the conjunct and the group.
        let next_link = |joined: &[bool]| -> Option<(usize, usize)> {
            links.iter().enumerate().find_map(|(c, found)| {
                let (_, a, b) = found.as_ref()?;
                let inside =
                    |v: &Variable| (0..groups.len()).any(|g| joined[g] && certain_in(g, v));
                let outside =
                    |v: &Variable| (0..groups.len()).find(|&g| !joined[g] && certain_in(g, v));
                if inside(a) {
                    outside(b).map(|g| (c, g))
                } else if inside(b) {
                    outside(a).map(|g| (c, g))
                } else {
                    None
                }
            })
        };
        // The first link: between any two groups.
        let first = links.iter().enumerate().find_map(|(c, found)| {
            let (_, a, b) = found.as_ref()?;
            let ga = (0..groups.len()).find(|&g| certain_in(g, a))?;
            let gb = (0..groups.len()).find(|&g| g != ga && certain_in(g, b))?;
            Some((c, ga, gb))
        });
        let Some((c, ga, gb)) = first else {
            return Ok(None);
        };
        let pattern = |g: usize| -> GraphPattern {
            match groups[g].as_slice() {
                [only] => parts[*only].pattern.clone(),
                many => GraphPattern::Join {
                    inputs: many.iter().map(|&p| parts[p].pattern.clone()).collect(),
                },
            }
        };
        // The smaller side first.
        let (x, y) = if self.estimate(&pattern(ga)) <= self.estimate(&pattern(gb)) {
            (ga, gb)
        } else {
            (gb, ga)
        };
        let mut joined = vec![false; groups.len()];
        let mut used = vec![false; conjuncts.len()];
        joined[x] = true;
        let mut result = self.eval(&pattern(x))?;
        let mut step = Some((c, y));
        while let Some((c, g)) = step {
            let (local, a, b) = links[c].clone().expect("a link");
            result = self.spatial_step(result, &pattern(g), local, &a, &b)?;
            used[c] = true;
            joined[g] = true;
            step = next_link(&joined);
        }
        // Groups no conjunct links: joined as before.
        for (g, _) in joined.iter().enumerate().filter(|(_, done)| !**done) {
            result = self.eval_from(result, &pattern(g))?;
        }
        for (i, conjunct) in conjuncts.iter().enumerate() {
            if !used[i] {
                result = self.filter(result, conjunct)?;
            }
        }
        Ok(Some(result))
    }

    /// `rows` (which bind one of `a`, `b` in every row) joined with `pattern` (which binds
    /// the other) where `local(a, b)` holds (module docs, steps 2 and 3).
    fn spatial_step(
        &self,
        rows: Solutions,
        pattern: &GraphPattern,
        local: &'static str,
        a: &Variable,
        b: &Variable,
    ) -> NativeResult<Solutions> {
        let start = Instant::now();
        let index = self.snapshot.geometries();
        // The side the rows bind, and the other's variable.
        let (bound, other, rows_first) = if rows.column(a).is_some() {
            (a, b, true)
        } else {
            (b, a, false)
        };
        let column = rows.column(bound).expect("a linked variable is bound");
        self.check()?;
        let mut systems = HashMap::new();
        let mut systems_held = self.working(0)?;
        let known = self.geometries(
            rows.table.column(column),
            &index,
            &mut systems,
            &mut systems_held,
        )?;
        // An index nested-loop join where the index narrows the other side enough.
        let seed = if scanned(pattern, other) {
            self.index_candidates(&known, &index, local, rows_first)?
        } else {
            None
        }
        .filter(|(found, _)| (found.len() as f64) * (PROBE_FACTOR as f64) < self.estimate(pattern));
        let probed = seed.is_some();
        let found = match seed {
            Some((candidates, _held)) => {
                if self.trace.is_some() {
                    let detail = format!(
                        "{other}: the geometry literals whose box can stand in {local} with \
                         {} of {bound}'s",
                        known.ids.len()
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
                let seed = Solutions {
                    vars: vec![other.clone()],
                    table,
                    ordered: false,
                };
                seed_held.transfer();
                self.eval_from(seed, pattern)?
            }
            None => self.eval(pattern)?,
        };
        let found_column = found.column(other).expect("the pattern binds the variable");
        let found_geometries = self.geometries(
            found.table.column(found_column),
            &index,
            &mut systems,
            &mut systems_held,
        )?;
        let (left, right) = if rows_first {
            (&known, &found_geometries)
        } else {
            (&found_geometries, &known)
        };
        let snapshot: &nrese_engine::Snapshot = &self.snapshot;
        let read = |side: Side, i: usize| -> Option<Geometry<f64>> {
            let side = match side {
                Side::Left => left,
                Side::Right => right,
            };
            let id = side.ids[i];
            if let Some(g) = side.computed.get(&id) {
                return Some(g.clone());
            }
            snapshot
                .with_view(TermId::from_raw(id), |view| match view {
                    TermView::Typed { value, datatype } => nrese_geo::parse(datatype, value),
                    _ => None,
                })
                .flatten()
                .map(|shape| shape.geometry)
        };
        let token = self.cancellation.as_ref();
        let stop = || token.is_some_and(|t| t.is_cancelled());
        let (positions, stats, positions_held) =
            nrese_geo::join(local, &left.items, &right.items, &read, &stop, &self.budget).map_err(
                |error| match error {
                    JoinError::Cancelled => QueryEvaluationError::Cancelled,
                    JoinError::Memory(error) => QueryEvaluationError::MemoryLimit(error),
                    JoinError::Unsupported => {
                        QueryEvaluationError::Unsupported(format!("spatial join {local}"))
                    }
                },
            )?;
        let _pairs_held = self.working(
            positions
                .len()
                .checked_mul(16)
                .ok_or_else(|| self.too_large(usize::MAX, 2))?,
        )?;
        let mut pairs: Vec<(u64, u64)> = Vec::with_capacity(positions.len());
        for (at, &(i, j)) in positions.iter().enumerate() {
            if at.is_multiple_of(1024) {
                self.check()?;
            }
            let (x, y) = (left.ids[i], right.ids[j]);
            pairs.push(if rows_first { (x, y) } else { (y, x) });
        }
        drop(positions);
        drop(positions_held);
        let joined = self.rows_of_pairs(rows, column, found, found_column, &pairs)?;
        if self.trace.is_some() {
            let detail = format!(
                "{local}({a}, {b}): {} × {} geometries{}, {} candidate pairs by their boxes, \
                 {} compared ({} prepared), {} related",
                left.ids.len(),
                right.ids.len(),
                if probed {
                    format!(" ({other}'s from the index)")
                } else {
                    String::new()
                },
                stats.candidates,
                stats.compared,
                stats.prepared,
                stats.pairs
            );
            self.note("spatial join", detail, None, joined.table.len(), start);
        }
        Ok(joined)
    }

    /// The distinct values of `column` as geometries: stored literals from the index,
    /// values the query computed read; anything else (not a geometry) left out.
    fn geometries<'b>(
        &'b self,
        column: &[u64],
        index: &GeometryIndex,
        other_systems: &mut HashMap<String, u32>,
        systems_held: &mut Reservation<&Budget>,
    ) -> NativeResult<Geometries<'b>> {
        let _ids_held = self.working(
            column
                .len()
                .checked_mul(8)
                .ok_or_else(|| self.too_large(usize::MAX, 1))?,
        )?;
        let mut ids: Vec<u64> = Vec::with_capacity(column.len());
        ids.extend(column.iter().copied().filter(|&id| id != UNDEF));
        ids.sort_unstable();
        ids.dedup();
        let held = self.working(
            ids.len()
                .checked_mul(8 + size_of::<Item>() + 160)
                .ok_or_else(|| self.too_large(usize::MAX, 1))?,
        )?;
        let mut out = Geometries {
            ids: Vec::with_capacity(ids.len()),
            items: Vec::with_capacity(ids.len()),
            computed: HashMap::new(),
            _held: held,
        };
        // Systems no indexed literal is in are numbered after the index's.
        let base = index.systems();
        for (at, id) in ids.into_iter().enumerate() {
            if at.is_multiple_of(1024) {
                self.check()?;
            }
            if let Some(computed) = computed_index(id) {
                // Initial allowance before cloning/parsing; retained capacity is
                // reconciled below because WKT conversion can keep oversized vectors.
                let bytes = match self.computed.borrow().get(computed as usize) {
                    Some(nrese_rdf::Term::Literal(l)) => l
                        .value()
                        .len()
                        .saturating_mul(8)
                        .saturating_add(size_of::<Geometry<f64>>()),
                    _ => 0,
                };
                out._held
                    .charge(bytes)
                    .map_err(QueryEvaluationError::MemoryLimit)?;
                let term = self.term(id);
                self.check()?;
                let Some(shape) = term.as_ref().and_then(parse) else {
                    out._held.release(bytes);
                    continue;
                };
                self.check()?;
                let retained = nrese_geo::geometry_heap_bytes(&shape.geometry);
                if retained > bytes {
                    out._held
                        .charge(retained - bytes)
                        .map_err(QueryEvaluationError::MemoryLimit)?;
                }
                let bytes = bytes.max(retained);
                let crs = if let Some(crs) = index
                    .crs_number(&shape.crs)
                    .or_else(|| other_systems.get(&shape.crs).copied())
                {
                    crs
                } else {
                    let next = u32::try_from(other_systems.len())
                        .ok()
                        .and_then(|n| base.checked_add(n))
                        .ok_or_else(|| self.too_large(usize::MAX, 1))?;
                    systems_held
                        .charge(shape.crs.len().saturating_add(96))
                        .map_err(QueryEvaluationError::MemoryLimit)?;
                    other_systems.insert(shape.crs.clone(), next);
                    next
                };
                out.items.push(Item {
                    rect: shape.rect(),
                    crs,
                    point: matches!(shape.geometry, Geometry::Point(_)),
                    bytes,
                });
                out.computed.insert(id, shape.geometry);
                out.ids.push(id);
            } else if let Some(entry) = index.get(id) {
                let bytes = if entry.point {
                    size_of::<Geometry<f64>>()
                } else {
                    self.snapshot
                        .with_view(TermId::from_raw(id), |view| match view {
                            TermView::Typed { value, .. } => value
                                .len()
                                .saturating_mul(8)
                                .saturating_add(size_of::<Geometry<f64>>()),
                            _ => 0,
                        })
                        .unwrap_or(0)
                };
                out.items.push(Item {
                    rect: entry.rect,
                    crs: entry.crs,
                    point: entry.point,
                    bytes,
                });
                out.ids.push(id);
            }
        }
        self.check()?;
        Ok(out)
    }

    /// The indexed literals the other side's variable could take for `local` to hold with
    /// one of `known` (their boxes pass what it asks, in one of their systems), sorted;
    /// `None` where the index can't
    /// tell (an empty geometry, whose partners have no box either).
    fn index_candidates(
        &self,
        known: &Geometries<'_>,
        index: &GeometryIndex,
        local: &str,
        known_first: bool,
    ) -> NativeResult<Option<(Vec<u64>, Reservation<&Budget>)>> {
        let Some(required) = nrese_geo::boxes(local) else {
            return Ok(None);
        };
        if known.items.iter().any(|item| item.rect.is_none()) {
            return Ok(None);
        }
        let mut found: Vec<u64> = Vec::new();
        let mut held = self.working(0)?;
        // Accumulated hits, including duplicates, remain charged until deduplication.
        // Traversal allocates no hit vector; unrelated indexed literals cost no charge.
        let mut visited = 0usize;
        for item in &known.items {
            self.check()?;
            let Some(rect) = item.rect else {
                return Ok(None);
            };
            let probe = match (required, known_first) {
                (Boxes::Meet, _) => Boxes::Meet,
                // The other side's box lies in the known one's.
                (Boxes::Contains, true) | (Boxes::Inside, false) | (Boxes::Equal, _) => {
                    Boxes::Inside
                }
                // The other side's box holds the known one's.
                (Boxes::Inside, true) | (Boxes::Contains, false) => Boxes::Contains,
            };
            if let ControlFlow::Break(error) = index.visit_boxes(probe, &rect, |id| {
                if visited.is_multiple_of(1024)
                    && let Err(error) = self.check()
                {
                    return ControlFlow::Break(error);
                }
                visited += 1;
                if index.get(id).is_some_and(|e| e.crs == item.crs)
                    && let Err(error) = self.spatial_candidate(&mut found, &mut held, id)
                {
                    return ControlFlow::Break(error);
                }
                ControlFlow::Continue(())
            }) {
                return Err(error);
            }
        }
        found.sort_unstable();
        found.dedup();
        self.check()?;
        Ok(Some((found, held)))
    }

    /// The rows of the cross product of `left` and `right` whose values in `lc` and `rc`
    /// are one of `pairs`.
    fn rows_of_pairs(
        &self,
        left: Solutions,
        lc: usize,
        right: Solutions,
        rc: usize,
        pairs: &[(u64, u64)],
    ) -> NativeResult<Solutions> {
        let _maps_held = self.working(
            left.table
                .len()
                .checked_add(right.table.len())
                .and_then(|n| n.checked_mul(160))
                .and_then(|n| n.checked_add(512))
                .ok_or_else(|| self.too_large(usize::MAX, 1))?,
        )?;
        let by_value = |table: &IdTable, column: usize| -> NativeResult<HashMap<u64, Vec<usize>>> {
            let mut out: HashMap<u64, Vec<usize>> = HashMap::new();
            for (row, &id) in table.column(column).iter().enumerate() {
                if row.is_multiple_of(1024) {
                    self.check()?;
                }
                out.entry(id).or_default().push(row);
            }
            Ok(out)
        };
        let (lrows, rrows) = (by_value(&left.table, lc)?, by_value(&right.table, rc)?);
        fn rows_of(map: &HashMap<u64, Vec<usize>>, id: u64) -> &[usize] {
            map.get(&id).map_or(&[], Vec::as_slice)
        }
        let width = left
            .vars
            .len()
            .checked_add(right.vars.len())
            .ok_or_else(|| self.too_large(usize::MAX, 1))?;
        let mut total = 0usize;
        for (i, &(x, y)) in pairs.iter().enumerate() {
            if i.is_multiple_of(1024) {
                self.check()?;
            }
            total = rows_of(&lrows, x)
                .len()
                .checked_mul(rows_of(&rrows, y).len())
                .and_then(|n| total.checked_add(n))
                .ok_or_else(|| self.too_large(usize::MAX, width))?;
        }
        if total > self.max_rows(width) {
            return Err(self.too_large(total, width));
        }
        let output_held = self.working(
            total
                .checked_mul(width)
                .and_then(|n| n.checked_mul(8))
                .ok_or_else(|| self.too_large(usize::MAX, width))?,
        )?;
        let mut columns: Vec<Vec<u64>> = (0..width).map(|_| Vec::with_capacity(total)).collect();
        let mut emitted = 0usize;
        for &(x, y) in pairs {
            for &l in rows_of(&lrows, x) {
                for &r in rows_of(&rrows, y) {
                    if emitted.is_multiple_of(1024) {
                        self.check()?;
                    }
                    emitted += 1;
                    for (c, column) in columns[..left.vars.len()].iter_mut().enumerate() {
                        column.push(left.table.get(l, c));
                    }
                    for (c, column) in columns[left.vars.len()..].iter_mut().enumerate() {
                        column.push(right.table.get(r, c));
                    }
                }
            }
        }
        self.consumed(&left);
        self.consumed(&right);
        let mut vars = left.vars;
        vars.extend(right.vars);
        self.check()?;
        output_held.transfer();
        Ok(Solutions {
            vars,
            table: IdTable::from_columns(columns),
            ordered: false,
        })
    }
}
