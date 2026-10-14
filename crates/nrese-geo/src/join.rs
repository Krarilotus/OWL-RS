//! The spatial join: the pairs of two sets of geometries that a relation holds for.
//!
//! Filter and refine (Bast, Brosi, Kalmbach, *Efficient Spatial Joins on Large Geometry
//! Sets*, SIGSPATIAL 2025, §2): the candidates are the pairs whose bounding boxes pass
//! what the relation asks of them ([`crate::boxes`]: meet, or one in the other for the
//! containment relations); an R-tree over the smaller side's boxes, bulk-loaded, finds
//! them for each box of the larger side, as PostGIS's index join does. The boxes decide
//! many candidates outright ([`crate::decided_by_boxes`]); the rest are compared exactly
//! through [`crate::relation_of`], the one definition the filters use too.
//!
//! The larger side is taken in chunks, ordered along a Z-curve of the boxes' centres so
//! that a chunk meets few of the smaller side's geometries, on every core. Each geometry
//! is read once: the smaller side's before the comparisons (those any undecided candidate
//! needs), the larger side's in its chunk. A geometry of the smaller side that a chunk
//! compares several times is prepared there (`geo`'s `PreparedGeometry`: its edges noded
//! and indexed once, not for every comparison). Points need no reading: a point is its box.
//!
//! Cost: O((n + m) log n + k) for the candidates (n the smaller side, m the larger, k the
//! candidate pairs), plus the exact comparisons of the candidates the boxes leave open.

use geo::{CoordsIter, Geometry, Point, PreparedGeometry, Rect, Relate};
use nrese_exec::{Budget, BudgetExceeded, Reservation};
use rayon::prelude::*;
use rstar::primitives::{GeomWithData, Rectangle};
use rstar::{AABB, RTree};

use crate::relation::{boxes, boxes_pass, decided_by_boxes, relation_of};

/// The larger side's geometries per chunk.
const CHUNK: usize = 1024;

/// Comparisons with one geometry of the smaller side in a chunk from which it is
/// prepared: preparing costs about as much as one plain comparison.
const PREPARE_FROM: usize = 3;

/// One geometry of a side.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Item {
    /// Its bounding box; `None` for an empty geometry.
    pub rect: Option<Rect<f64>>,
    /// Its reference system, numbered by the caller: geometries in different systems are
    /// never related.
    pub crs: u32,
    /// Whether it is one point (then the box is the point and nothing need be read).
    pub point: bool,
    /// Estimated capacity of a parsed geometry (including its inline representation).
    /// The caller reserves this before reading; opaque parser scratch is cooperative.
    pub bytes: usize,
}

/// Which side of a join.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The relation's first argument.
    Left,
    /// Its second.
    Right,
}

/// What a join did, for EXPLAIN.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JoinStats {
    /// Pairs whose boxes passed.
    pub candidates: usize,
    /// Of those, the ones compared exactly (the boxes left them open).
    pub compared: usize,
    /// Geometries prepared for repeated comparisons.
    pub prepared: usize,
    /// Pairs the relation holds for.
    pub pairs: usize,
}

type Node = GeomWithData<Rectangle<[f64; 2]>, usize>;

/// Pairs of positions, (left, right) or (smaller side, larger side).
type Pairs = Vec<(usize, usize)>;

/// A chunk's candidates: the pairs the boxes decide hold, those they leave open, and how
/// many passed the boxes.
type Candidates<'b> = (Pairs, Pairs, usize, Reservation<&'b Budget>);

/// Unsupported relations, cooperative interruption and capacity refusal are distinct.
#[derive(Debug)]
pub enum JoinError {
    Unsupported,
    Cancelled,
    Memory(BudgetExceeded),
}

impl From<BudgetExceeded> for JoinError {
    fn from(error: BudgetExceeded) -> Self {
        Self::Memory(error)
    }
}

/// Grow a local buffer geometrically, charging only at growth, including the old/new
/// allocation overlap. Its reservation travels with the buffer across parallel phases.
fn room<T>(
    buffer: &mut Vec<T>,
    held: &mut Reservation<&Budget>,
    more: usize,
) -> Result<(), JoinError> {
    let wanted = buffer
        .len()
        .checked_add(more)
        .ok_or(JoinError::Memory(overflow()))?;
    if wanted > buffer.capacity() {
        let capacity = wanted.max(buffer.capacity().saturating_mul(2)).max(4);
        let bytes = capacity
            .checked_mul(size_of::<T>())
            .ok_or(JoinError::Memory(overflow()))?;
        held.charge(bytes)?;
        let old = buffer.capacity() * size_of::<T>();
        buffer.reserve_exact(capacity - buffer.len());
        held.release(old);
    }
    Ok(())
}

fn overflow() -> BudgetExceeded {
    BudgetExceeded {
        limit: usize::MAX,
        requested: usize::MAX,
        used: 0,
        shared: false,
    }
}

fn envelope(rect: &Rect<f64>) -> AABB<[f64; 2]> {
    AABB::from_corners([rect.min().x, rect.min().y], [rect.max().x, rect.max().y])
}

/// The position of a box's centre on a Z-curve over `extent` (16 bits a coordinate).
fn z_order(rect: &Rect<f64>, extent: &Rect<f64>) -> u32 {
    let scale = |v: f64, lo: f64, hi: f64| -> u32 {
        if hi > lo {
            (((v - lo) / (hi - lo)) * 65535.0).clamp(0.0, 65535.0) as u32
        } else {
            0
        }
    };
    let centre = rect.center();
    let x = scale(centre.x, extent.min().x, extent.max().x);
    let y = scale(centre.y, extent.min().y, extent.max().y);
    let spread = |mut v: u32| {
        v = (v | (v << 8)) & 0x00FF_00FF;
        v = (v | (v << 4)) & 0x0F0F_0F0F;
        v = (v | (v << 2)) & 0x3333_3333;
        (v | (v << 1)) & 0x5555_5555
    };
    spread(x) | (spread(y) << 1)
}

/// Whether `local` holds, the geometries in argument order (either may be prepared).
fn holds(local: &str, a: &impl Relate<f64>, b: &impl Relate<f64>) -> bool {
    relation_of(local, a, b, a.is_empty() && b.is_empty()).unwrap_or(false)
}

/// A geometry of the smaller side as a chunk compares it: as read, or prepared.
enum Ready<'g> {
    Plain(&'g Geometry<f64>),
    Prepared(Box<PreparedGeometry<'g, &'g Geometry<f64>>>),
}

/// The pairs `(i, j)` of `left[i]` and `right[j]` that relation `local` holds for, `left`
/// its first argument (module docs), with what the join did. `geometry(side, i)` reads
/// the geometry of a side's item that isn't a point; `None` (it doesn't read) relates to
/// nothing. Checks surround opaque library calls and run periodically in pair loops.
/// Capacity estimates cover retained structures; they are not a strict heap ceiling.
/// The returned reservation owns the pairs until their caller consumes them.
pub fn join<'b>(
    local: &str,
    left: &[Item],
    right: &[Item],
    geometry: &(dyn Fn(Side, usize) -> Option<Geometry<f64>> + Sync),
    stop: &(dyn Fn() -> bool + Sync),
    budget: &'b Budget,
) -> Result<(Pairs, JoinStats, Reservation<&'b Budget>), JoinError> {
    if !crate::RELATIONS.contains(&local) {
        return Err(JoinError::Unsupported);
    }
    let required = boxes(local).ok_or(JoinError::Unsupported)?;
    let check = || {
        if stop() {
            return Err(JoinError::Cancelled);
        }
        if nrese_exec::memory::process_limit_exceeded() {
            return Err(JoinError::Memory(BudgetExceeded {
                limit: nrese_exec::memory::process_limit().unwrap_or(0) as usize,
                requested: 0,
                used: nrese_exec::memory::process_bytes().unwrap_or(0) as usize,
                shared: true,
            }));
        }
        Ok(())
    };
    check()?;
    // The smaller side goes into the tree, the larger one probes it.
    let (small, large, small_side) = if left.len() <= right.len() {
        (left, right, Side::Left)
    } else {
        (right, left, Side::Right)
    };
    let large_side = match small_side {
        Side::Left => Side::Right,
        Side::Right => Side::Left,
    };
    // A candidate in argument order: (left item, right item) from (small, large).
    let ordered = |s: usize, l: usize| -> (usize, usize) {
        match small_side {
            Side::Left => (s, l),
            Side::Right => (l, s),
        }
    };
    let mut tree_held = Reservation::new(budget);
    // Bulk-load input/tree overlap, cached sort keys and empty ids. R-tree internals
    // are estimated as in GeometryIndex; this phase ends before geometry refinement.
    let bytes = small
        .len()
        .checked_mul(size_of::<Node>() * 3 + size_of::<usize>() * 3)
        .and_then(|n| {
            large
                .len()
                .checked_mul(size_of::<usize>() * 3)
                .and_then(|m| n.checked_add(m))
        })
        .ok_or(JoinError::Memory(overflow()))?;
    tree_held.charge(bytes)?;
    let mut parts_held = Reservation::new(budget);
    parts_held.charge(
        large
            .len()
            .div_ceil(CHUNK)
            .checked_mul(size_of::<Candidates<'_>>() * 3 + size_of::<&[usize]>())
            .ok_or(JoinError::Memory(overflow()))?,
    )?;
    let tree: RTree<Node> = RTree::bulk_load(
        small
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let r = item.rect?;
                Some(GeomWithData::new(
                    Rectangle::from_corners([r.min().x, r.min().y], [r.max().x, r.max().y]),
                    i,
                ))
            })
            .collect(),
    );
    check()?;
    // Empty geometries have no box: the equalities may hold between two of them, so they
    // are compared with every geometry of the other side in their system.
    let small_empty: Vec<usize> = (0..small.len())
        .filter(|&i| small[i].rect.is_none())
        .collect();
    let extent = large.iter().filter_map(|item| item.rect).reduce(|a, b| {
        Rect::new(
            geo::coord! { x: a.min().x.min(b.min().x), y: a.min().y.min(b.min().y) },
            geo::coord! { x: a.max().x.max(b.max().x), y: a.max().y.max(b.max().y) },
        )
    });
    let mut order: Vec<usize> = (0..large.len()).collect();
    if let Some(extent) = extent {
        order.sort_by_cached_key(|&i| large[i].rect.map_or(u32::MAX, |r| z_order(&r, &extent)));
    }
    let item = |side: Side, i: usize| match side {
        Side::Left => &left[i],
        Side::Right => &right[i],
    };
    // The candidates of a chunk of the larger side: the pairs decided by their boxes, and
    // those left open (as (small, large) indexes).
    let candidates = |chunk: &[usize]| -> Result<Candidates<'_>, JoinError> {
        check()?;
        let mut held = Reservation::new(budget);
        let (mut decided, mut open, mut count) = (Vec::new(), Vec::new(), 0usize);
        let mut visited = 0usize;
        let mut consider = |s: usize, l: usize| -> Result<(), JoinError> {
            if visited.is_multiple_of(CHUNK) {
                check()?;
            }
            visited = visited
                .checked_add(1)
                .ok_or(JoinError::Memory(overflow()))?;
            let (a, b) = ordered(s, l);
            let (x, y) = (&left[a], &right[b]);
            if x.crs != y.crs {
                return Ok(());
            }
            if let (Some(rx), Some(ry)) = (&x.rect, &y.rect)
                && !boxes_pass(required, rx, ry)
            {
                return Ok(());
            }
            count = count.checked_add(1).ok_or(JoinError::Memory(overflow()))?;
            match decided_by_boxes(local, x.rect.as_ref(), y.rect.as_ref()) {
                Some(true) => {
                    room(&mut decided, &mut held, 1)?;
                    decided.push((a, b));
                }
                Some(false) => {}
                None => {
                    room(&mut open, &mut held, 1)?;
                    open.push((s, l));
                }
            }
            Ok(())
        };
        for &l in chunk {
            match &large[l].rect {
                Some(r) => {
                    for node in tree.locate_in_envelope_intersecting(&envelope(r)) {
                        consider(node.data, l)?;
                    }
                    for &s in &small_empty {
                        consider(s, l)?;
                    }
                }
                None => {
                    for s in 0..small.len() {
                        consider(s, l)?;
                    }
                }
            }
        }
        Ok((decided, open, count, held))
    };
    let chunks: Vec<&[usize]> = order.chunks(CHUNK).collect();
    let found: Result<Vec<Candidates<'_>>, JoinError> =
        chunks.par_iter().map(|chunk| candidates(chunk)).collect();
    let mut stats = JoinStats::default();
    let mut pairs = Vec::new();
    let mut output = Reservation::new(budget);
    let found = found?;
    let mut open = Vec::with_capacity(found.len());
    for (decided, undecided, count, mut held) in found {
        check()?;
        stats.candidates = stats
            .candidates
            .checked_add(count)
            .ok_or(JoinError::Memory(overflow()))?;
        stats.compared = stats
            .compared
            .checked_add(undecided.len())
            .ok_or(JoinError::Memory(overflow()))?;
        room(&mut pairs, &mut output, decided.len())?;
        let decided_bytes = decided.capacity() * size_of::<(usize, usize)>();
        pairs.extend(decided);
        held.release(decided_bytes);
        open.push((undecided, held));
    }
    drop(chunks);
    drop(order);
    drop(small_empty);
    drop(tree);
    drop(tree_held);
    // The smaller side's geometries the comparisons need, each read once.
    let mut scratch = Reservation::new(budget);
    scratch.charge(
        small
            .len()
            .checked_mul(1 + size_of::<Option<Geometry<f64>>>() * 2)
            .ok_or(JoinError::Memory(overflow()))?,
    )?;
    let mut needed = vec![false; small.len()];
    for (undecided, _) in &open {
        for (i, &(s, _)) in undecided.iter().enumerate() {
            if i.is_multiple_of(CHUNK) {
                check()?;
            }
            needed[s] = true;
        }
    }
    let read = |side: Side, i: usize| -> Option<Geometry<f64>> {
        let it = item(side, i);
        match (it.point, it.rect) {
            (true, Some(r)) => Some(Geometry::Point(Point(r.min()))),
            _ => geometry(side, i),
        }
    };
    let mut geometry_bytes = 0usize;
    for (i, &needed) in needed.iter().enumerate() {
        if i.is_multiple_of(CHUNK) {
            check()?;
        }
        if needed {
            geometry_bytes = geometry_bytes
                .checked_add(small[i].bytes)
                .ok_or(JoinError::Memory(overflow()))?;
        }
    }
    scratch.charge(geometry_bytes)?;
    let small_geometries: Vec<Option<Geometry<f64>>> = (0..small.len())
        .into_par_iter()
        .map(|i| {
            check()?;
            let g = if needed[i] { read(small_side, i) } else { None };
            check()?;
            Ok(g)
        })
        .collect::<Result<_, JoinError>>()?;
    // Parsing may retain the WKT input vectors through in-place conversion. Reconcile
    // the retained capacities as one batch before refinement, not one shared charge
    // per geometry. The caller estimate provides the initial parsing allowance.
    let parsed_bytes = small_geometries.iter().flatten().enumerate().try_fold(
        0usize,
        |bytes, (i, geometry)| {
            if i.is_multiple_of(CHUNK) {
                check()?;
            }
            bytes
                .checked_add(crate::geometry_heap_bytes(geometry))
                .ok_or(JoinError::Memory(overflow()))
        },
    )?;
    if parsed_bytes > geometry_bytes {
        scratch.charge(parsed_bytes - geometry_bytes)?;
    }
    let compared: Result<Vec<_>, JoinError> = open
        .par_iter()
        .map(|(undecided, _)| {
            check()?;
            let mut cache_held = Reservation::new(budget);
            // Hash entries and slack for both maps; one entry per needed geometry.
            cache_held.charge(
                undecided
                    .len()
                    .min(small.len())
                    .checked_mul(160)
                    .and_then(|n| n.checked_add(256))
                    .ok_or(JoinError::Memory(overflow()))?,
            )?;
            let mut uses: std::collections::HashMap<usize, usize> =
                std::collections::HashMap::new();
            for (i, &(s, _)) in undecided.iter().enumerate() {
                if i.is_multiple_of(CHUNK) {
                    check()?;
                }
                *uses.entry(s).or_default() += 1;
            }
            let mut ready: std::collections::HashMap<usize, Ready<'_>> =
                std::collections::HashMap::new();
            let mut prepared = 0;
            for (&s, &n) in &uses {
                check()?;
                let Some(g) = &small_geometries[s] else {
                    continue;
                };
                let form = if n >= PREPARE_FROM && !matches!(g, Geometry::Point(_)) {
                    prepared += 1;
                    // geo's private noded graph/tree: an estimate for ordinary shapes,
                    // not a bound on self-intersection or relating scratch.
                    cache_held.charge(
                        g.coords_count()
                            .checked_mul(512)
                            .and_then(|n| n.checked_add(1024))
                            .ok_or(JoinError::Memory(overflow()))?,
                    )?;
                    Ready::Prepared(Box::new(PreparedGeometry::from(g)))
                } else {
                    Ready::Plain(g)
                };
                check()?;
                ready.insert(s, form);
            }
            let mut held = Reservation::new(budget);
            let mut held_pairs = Vec::new();
            let mut large_held = Reservation::new(budget);
            let mut large_geometry: Option<(usize, Option<Geometry<f64>>)> = None;
            for (i, &(s, l)) in undecided.iter().enumerate() {
                if i.is_multiple_of(CHUNK) {
                    check()?;
                }
                let Some(small_geometry) = ready.get(&s) else {
                    continue;
                };
                // The pairs of a chunk come grouped by the larger side's geometry.
                if large_geometry.as_ref().is_none_or(|(at, _)| *at != l) {
                    drop(large_geometry.take());
                    large_held.resize(large[l].bytes)?;
                    check()?;
                    large_geometry = Some((l, read(large_side, l)));
                    check()?;
                    if let Some((_, Some(geometry))) = &large_geometry {
                        large_held
                            .resize(large[l].bytes.max(crate::geometry_heap_bytes(geometry)))?;
                    }
                }
                let Some((_, Some(other))) = &large_geometry else {
                    continue;
                };
                check()?;
                let related = match (small_geometry, small_side) {
                    (Ready::Plain(g), Side::Left) => holds(local, *g, other),
                    (Ready::Plain(g), Side::Right) => holds(local, other, *g),
                    (Ready::Prepared(p), Side::Left) => holds(local, p.as_ref(), other),
                    (Ready::Prepared(p), Side::Right) => holds(local, other, p.as_ref()),
                };
                check()?;
                if related {
                    let (a, b) = ordered(s, l);
                    room(&mut held_pairs, &mut held, 1)?;
                    held_pairs.push((a, b));
                }
            }
            Ok((held_pairs, prepared, held))
        })
        .collect();
    let compared = compared?;
    drop(open);
    drop(small_geometries);
    drop(needed);
    drop(scratch);
    for (held, prepared, _reservation) in compared {
        check()?;
        stats.prepared += prepared;
        room(&mut pairs, &mut output, held.len())?;
        pairs.extend(held);
    }
    stats.pairs = pairs.len();
    check()?;
    Ok((pairs, stats, output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relation::{RELATIONS, holds_apart, relate};
    use crate::shape::{Shape, WKT_LITERAL, parse};

    fn items(shapes: &[Shape]) -> Vec<Item> {
        shapes
            .iter()
            .map(|s| Item {
                rect: s.rect(),
                crs: u32::from(s.crs != crate::CRS84),
                point: matches!(s.geometry, Geometry::Point(_)),
                bytes: s.geometry.coords_count() * 32 + size_of::<Geometry<f64>>() * 2,
            })
            .collect()
    }

    /// A pseudo-random set of points, lines and polygons (some nested, some touching,
    /// some empty, one in another system), `n` of them, from `seed`.
    fn shapes(n: usize, seed: u64) -> Vec<Shape> {
        let mut state = seed;
        let mut next = |below: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) % below
        };
        (0..n)
            .map(|i| {
                let (x, y) = (next(20) as f64, next(20) as f64);
                let (w, h) = (1 + next(6), 1 + next(6));
                let text = match i % 9 {
                    0..=2 => format!("POINT({x} {y})"),
                    3 => format!("LINESTRING({x} {y}, {} {})", x + w as f64, y + h as f64),
                    4 => format!(
                        "LINESTRING({x} {y}, {} {y}, {} {})",
                        x + w as f64,
                        x + w as f64,
                        y + h as f64
                    ),
                    5 if i % 45 == 5 => String::new(),
                    6 if i % 54 == 6 => {
                        format!("<http://www.opengis.net/def/crs/EPSG/0/3857> POINT({x} {y})")
                    }
                    _ => format!(
                        "POLYGON(({x} {y}, {} {y}, {} {}, {x} {}, {x} {y}))",
                        x + w as f64,
                        x + w as f64,
                        y + h as f64,
                        y + h as f64
                    ),
                };
                parse(WKT_LITERAL, &text).unwrap()
            })
            .collect()
    }

    /// For every relation the join takes, its pairs are those the definition gives over
    /// the whole cross product, whichever side is smaller, with geometries prepared where
    /// a chunk compares one often. The guard of the join's candidates and refinement.
    #[test]
    fn joined_pairs_are_the_definitions() {
        let (a, b) = (shapes(100, 1), shapes(250, 2));
        let mut prepared = 0;
        for local in RELATIONS.iter().filter(|l| !holds_apart(l)) {
            for (left, right) in [(&a, &b), (&b, &a), (&a, &a)] {
                let read = |side: Side, i: usize| {
                    Some(match side {
                        Side::Left => left[i].geometry.clone(),
                        Side::Right => right[i].geometry.clone(),
                    })
                };
                let budget = Budget::unlimited();
                let (mut found, stats, _held) = join(
                    local,
                    &items(left),
                    &items(right),
                    &read,
                    &|| false,
                    &budget,
                )
                .unwrap();
                found.sort_unstable();
                let mut expected = Vec::new();
                for (i, x) in left.iter().enumerate() {
                    for (j, y) in right.iter().enumerate() {
                        if relate(local, x, y) == Some(true) {
                            expected.push((i, j));
                        }
                    }
                }
                assert_eq!(found, expected, "{local}");
                assert_eq!(stats.pairs, expected.len());
                prepared += stats.prepared;
                // The boxes keep the candidates far below the product.
                assert!(
                    stats.candidates * 4 < left.len() * right.len(),
                    "{local}: {stats:?}"
                );
            }
        }
        assert!(prepared > 0, "the oracle must exercise prepared refinement");
        // Disjointness isn't joined here.
        let budget = Budget::unlimited();
        assert!(matches!(
            join(
                "sfDisjoint",
                &items(&a),
                &items(&b),
                &|_, _| None,
                &|| { false },
                &budget
            ),
            Err(JoinError::Unsupported)
        ));
        // A stop ends it.
        assert!(matches!(
            join(
                "sfIntersects",
                &items(&a),
                &items(&b),
                &|_, _| None,
                &|| true,
                &budget,
            ),
            Err(JoinError::Cancelled)
        ));
    }

    /// Dense points isolate candidate storage: no opaque parser or relation allocation
    /// can explain the refusal. Removing candidate reservations makes this succeed.
    #[test]
    fn dense_candidates_are_charged_and_every_exit_releases_scratch() {
        use nrese_exec::{Envelope, SharedBudget};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let point = Item {
            rect: Some(Rect::new(
                geo::coord! { x: 0., y: 0. },
                geo::coord! { x: 0., y: 0. },
            )),
            crs: 0,
            point: true,
            bytes: size_of::<Geometry<f64>>(),
        };
        let (a, b) = (vec![point; 128], vec![point; 256]);
        let read = |_, _| panic!("a point is its box");
        let budget = Budget::new(96 * 1024);
        assert!(
            matches!(join("sfIntersects", &a, &b, &read, &|| false, &budget), Err(JoinError::Memory(e)) if !e.shared)
        );
        assert_eq!(budget.used(), 0);
        assert!(budget.peak() <= budget.limit());

        let shared = SharedBudget::new(128 * 1024);
        let envelope = Envelope::new(None, Some(shared.clone()));
        let first = envelope.part();
        let mut sibling = Reservation::new(&first);
        sibling.charge(32 * 1024).unwrap();
        let second = envelope.part();
        assert!(
            matches!(join("sfIntersects", &a, &b, &read, &|| false, &second), Err(JoinError::Memory(e)) if e.shared)
        );
        assert_eq!(second.used(), 0);
        assert_eq!(shared.used(), sibling.bytes());
        drop(sibling);
        assert_eq!(shared.used(), 0);

        let budget = Budget::unlimited();
        let calls = AtomicUsize::new(0);
        let stop = || calls.fetch_add(1, Ordering::Relaxed) >= 5;
        assert!(matches!(
            join("sfIntersects", &a, &b, &read, &stop, &budget),
            Err(JoinError::Cancelled)
        ));
        assert_eq!(budget.used(), 0);
        let (pairs, stats, held) = join("sfIntersects", &a, &b, &read, &|| false, &budget).unwrap();
        assert_eq!(pairs.len(), a.len() * b.len());
        assert_eq!(stats.compared, pairs.len());
        assert_eq!(stats.prepared, 0);
        assert_eq!(budget.used(), held.bytes());
        assert_eq!(held.bytes(), pairs.capacity() * size_of::<(usize, usize)>());
        drop(pairs);
        drop(held);
        assert_eq!(budget.used(), 0);
    }

    /// One worker reuses a prepared hundred-vertex polygon across several chunks.
    /// Finished workers retain only pair capacity. Keeping their preparation charges
    /// until aggregation, or charging descriptors per item, refuses this envelope.
    #[test]
    fn chunk_descriptors_and_preparation_follow_their_actual_lifetimes() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let mut coordinates: Vec<_> = (0..100)
            .map(|i| {
                let angle = i as f64 * std::f64::consts::TAU / 100.;
                format!("{} {}", angle.cos(), angle.sin())
            })
            .collect();
        coordinates.push(coordinates[0].clone());
        let shape = parse(
            WKT_LITERAL,
            &format!("POLYGON(({}))", coordinates.join(",")),
        )
        .unwrap();
        let small = items(std::slice::from_ref(&shape));
        let large = vec![small[0]; CHUNK * 4];
        pool.install(|| {
            let budget = Budget::new(256 * 1024);
            let read = |_, _| Some(shape.geometry.clone());
            let (pairs, stats, held) =
                join("sfTouches", &small, &large, &read, &|| false, &budget).unwrap();
            assert!(pairs.is_empty());
            assert_eq!(stats.compared, large.len());
            assert_eq!(stats.prepared, large.len().div_ceil(CHUNK));
            assert_eq!(budget.used(), held.bytes());
            drop(pairs);
            drop(held);
            assert_eq!(budget.used(), 0);
        });
    }
}
