//! The geometry literals of a store by id: each one's reference system and bounding box,
//! and an R-tree over the boxes that finds the literals a box meets or holds.
//!
//! The engine keeps one for its dictionary: every literal of a geometry datatype
//! ([`crate::DATATYPES`]) that reads, whichever property states it. Literals come in id
//! order and are never changed, so the index grows by parts: the literals added since
//! the last extension become a part of their own, and a part at least half as large as
//! the one before it is merged into it (size-tiered, as the engine's runs): O(log n)
//! parts, each literal re-sorted O(log n) times. A clone shares the parts.
//!
//! Memory: about 70 bytes a literal (its id, box, system and serialisation in the list,
//! its box again in the tree). Building reads every literal once, on every core.

use std::ops::ControlFlow;
use std::sync::Arc;

use geo::{Geometry, Point, Rect};
use rayon::prelude::*;
use rstar::primitives::{GeomWithData, Rectangle};
use rstar::{AABB, RTree, RTreeObject};

use crate::Boxes;
use crate::shape::{Serialisation, Shape, parse};

type Node = GeomWithData<Rectangle<[f64; 2]>, u64>;

/// What the index keeps of a geometry literal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Entry {
    /// Its reference system, numbered by the index ([`GeometryIndex::crs`]).
    pub crs: u32,
    /// Its bounding box; `None` for an empty geometry.
    pub rect: Option<Rect<f64>>,
    /// Whether the geometry is one point (then the box is the point).
    pub point: bool,
    /// How its literal is written: what a construction from it is written as.
    pub format: Serialisation,
}

impl Entry {
    /// The point, if the geometry is one.
    pub fn as_point(&self) -> Option<Point<f64>> {
        self.rect.filter(|_| self.point).map(|r| Point(r.min()))
    }
}

/// Literals with consecutive ids (and gaps), their entries, and the tree over their boxes.
#[derive(Debug)]
struct Part {
    ids: Vec<u64>,
    entries: Vec<Entry>,
    tree: RTree<Node>,
}

fn node(id: u64, rect: &Rect<f64>) -> Node {
    GeomWithData::new(
        Rectangle::from_corners([rect.min().x, rect.min().y], [rect.max().x, rect.max().y]),
        id,
    )
}

fn envelope(rect: &Rect<f64>) -> AABB<[f64; 2]> {
    AABB::from_corners([rect.min().x, rect.min().y], [rect.max().x, rect.max().y])
}

impl Part {
    fn new(ids: Vec<u64>, entries: Vec<Entry>) -> Self {
        let nodes = ids
            .iter()
            .zip(&entries)
            .filter_map(|(&id, entry)| entry.rect.as_ref().map(|r| node(id, r)))
            .collect();
        Self {
            ids,
            entries,
            tree: RTree::bulk_load(nodes),
        }
    }

    fn get(&self, id: u64) -> Option<&Entry> {
        self.ids.binary_search(&id).ok().map(|i| &self.entries[i])
    }
}

/// A store's geometry literals (module docs).
#[derive(Debug, Clone, Default)]
pub struct GeometryIndex {
    parts: Vec<Arc<Part>>,
    /// The reference systems' IRIs, by number.
    crs: Vec<String>,
    /// The dictionary entries looked at so far.
    covered: u64,
}

impl GeometryIndex {
    /// The dictionary entries looked at so far: those before this number.
    pub fn covered(&self) -> u64 {
        self.covered
    }

    /// The literals indexed.
    pub fn len(&self) -> usize {
        self.parts.iter().map(|p| p.ids.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The IRI of reference system number `crs`.
    pub fn crs(&self, crs: u32) -> &str {
        &self.crs[crs as usize]
    }

    /// The reference systems of the indexed literals: they are numbered `0..systems()`.
    pub fn systems(&self) -> u32 {
        self.crs.len() as u32
    }

    /// The number of the reference system `iri`, if an indexed literal is in it.
    pub fn crs_number(&self, iri: &str) -> Option<u32> {
        self.crs.iter().position(|c| c == iri).map(|i| i as u32)
    }

    /// Takes in `literals` (an id, a datatype and a lexical form each, ids ascending and
    /// above any indexed one), the dictionary entries up to `covered` looked at. A literal
    /// that doesn't read as a geometry isn't indexed. O(k log k) for k literals, plus the
    /// merges (module docs).
    pub fn extend(&mut self, literals: &[(u64, &str, &str)], covered: u64) {
        let read: Vec<(u64, Shape)> = literals
            .par_iter()
            .filter_map(|&(id, datatype, text)| Some((id, parse(datatype, text)?)))
            .collect();
        let mut ids = Vec::with_capacity(read.len());
        let mut entries = Vec::with_capacity(read.len());
        for (id, shape) in read {
            let crs = match self.crs.iter().position(|c| *c == shape.crs) {
                Some(i) => i as u32,
                None => {
                    self.crs.push(shape.crs.clone());
                    (self.crs.len() - 1) as u32
                }
            };
            ids.push(id);
            entries.push(Entry {
                crs,
                rect: shape.rect(),
                point: matches!(shape.geometry, Geometry::Point(_)),
                format: shape.format,
            });
        }
        self.covered = self.covered.max(covered);
        if ids.is_empty() {
            return;
        }
        self.parts.push(Arc::new(Part::new(ids, entries)));
        while let [.., before, last] = self.parts.as_slice()
            && last.ids.len() * 2 >= before.ids.len()
        {
            let last = self.parts.pop().expect("two parts");
            let before = self.parts.pop().expect("two parts");
            let mut ids = before.ids.clone();
            ids.extend_from_slice(&last.ids);
            let mut entries = before.entries.clone();
            entries.extend_from_slice(&last.entries);
            self.parts.push(Arc::new(Part::new(ids, entries)));
        }
    }

    /// The entry of literal `id`, if it is an indexed geometry. O(log n).
    pub fn get(&self, id: u64) -> Option<&Entry> {
        // Parts hold ascending, disjoint id ranges.
        let at = self
            .parts
            .partition_point(|p| p.ids.last().is_some_and(|&last| last < id));
        self.parts.get(at)?.get(id)
    }

    /// The literals whose box meets `rect`, in no order. O(log n + k) a part.
    pub fn meeting(&self, rect: &Rect<f64>) -> Vec<u64> {
        self.collect_boxes(Boxes::Meet, rect)
    }

    /// The literals whose box lies in `rect`, in no order. O(log n + k) a part.
    pub fn inside(&self, rect: &Rect<f64>) -> Vec<u64> {
        self.collect_boxes(Boxes::Inside, rect)
    }

    /// The literals whose box holds `rect`, in no order.
    pub fn holding(&self, rect: &Rect<f64>) -> Vec<u64> {
        self.collect_boxes(Boxes::Contains, rect)
    }

    fn collect_boxes(&self, required: Boxes, rect: &Rect<f64>) -> Vec<u64> {
        let mut found = Vec::new();
        let _: ControlFlow<()> = self.visit_boxes(required, rect, |id| {
            found.push(id);
            ControlFlow::Continue(())
        });
        found
    }

    /// Visit box candidates without allocating a traversal/output buffer, stopping on
    /// the visitor's break. O(log n + k) per part. `Inside`/`Equal` retrieve boxes inside
    /// `rect` (`Equal` still needs refinement); `Contains` retrieves boxes holding it.
    /// rstar's internal visitor traverses the existing tree recursively on the stack.
    pub fn visit_boxes<B>(
        &self,
        required: Boxes,
        rect: &Rect<f64>,
        mut visit: impl FnMut(u64) -> ControlFlow<B>,
    ) -> ControlFlow<B> {
        let bounds = envelope(rect);
        let (low, high) = (bounds.lower(), bounds.upper());
        for part in &self.parts {
            match required {
                Boxes::Meet => part
                    .tree
                    .locate_in_envelope_intersecting_int(&bounds, |n| visit(n.data)),
                Boxes::Inside | Boxes::Equal => {
                    part.tree.locate_in_envelope_int(&bounds, |n| visit(n.data))
                }
                Boxes::Contains => part.tree.locate_all_at_point_int(&low, |n| {
                    let e = n.geom().envelope();
                    if e.lower()[0] <= high[0]
                        && e.lower()[1] <= high[1]
                        && high[0] <= e.upper()[0]
                        && high[1] <= e.upper()[1]
                    {
                        visit(n.data)
                    } else {
                        ControlFlow::Continue(())
                    }
                }),
            }?;
        }
        ControlFlow::Continue(())
    }

    /// The literals with no box (empty geometries), which no box query finds.
    pub fn empties(&self) -> Vec<u64> {
        self.parts
            .iter()
            .flat_map(|p| {
                p.ids
                    .iter()
                    .zip(&p.entries)
                    .filter(|(_, e)| e.rect.is_none())
                    .map(|(&id, _)| id)
            })
            .collect()
    }

    /// Bytes held: the lists and the trees (an estimate for the trees).
    pub fn memory_bytes(&self) -> usize {
        self.parts
            .iter()
            .map(|p| {
                p.ids.capacity() * size_of::<u64>()
                    + p.entries.capacity() * size_of::<Entry>()
                    + p.tree.size() * size_of::<Node>() * 3 / 2
            })
            .sum::<usize>()
            + self.crs.iter().map(String::len).sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::WKT_LITERAL;
    use geo::Coord;

    #[test]
    fn literals_are_found_by_id_and_box_across_parts() {
        let mut index = GeometryIndex::default();
        let texts: Vec<String> = (0..100).map(|i| format!("POINT({i} {i})")).collect();
        // Added in steps of different sizes: parts merge as they grow.
        let mut from = 0;
        for step in [1, 1, 2, 30, 3, 63] {
            let literals: Vec<(u64, &str, &str)> = (from..from + step)
                .map(|i| (10 * i as u64, WKT_LITERAL, texts[i].as_str()))
                .collect();
            from += step;
            index.extend(&literals, 10 * from as u64);
            assert!(index.parts.len() <= 1 + (from as f64).log2().ceil() as usize);
        }
        assert_eq!(index.len(), 100);
        assert_eq!(index.covered(), 1000);
        let entry = index.get(370).unwrap();
        assert_eq!(entry.as_point(), Some(Point::new(37.0, 37.0)));
        assert_eq!(index.crs(entry.crs), crate::CRS84);
        assert!(index.get(371).is_none());
        let rect = Rect::new(Coord { x: 9.5, y: 9.5 }, Coord { x: 12.0, y: 12.0 });
        let mut found = index.meeting(&rect);
        found.sort_unstable();
        assert_eq!(found, [100, 110, 120]);
        // Lines and polygons, an empty geometry, a literal that doesn't read.
        let line = "LINESTRING(0 0, 20 20)";
        index.extend(
            &[
                (2000, WKT_LITERAL, line),
                (2010, WKT_LITERAL, ""),
                (2020, WKT_LITERAL, "POINT(nonsense)"),
            ],
            3000,
        );
        assert_eq!(index.len(), 102);
        assert!(index.get(2020).is_none());
        assert_eq!(index.empties(), [2010]);
        let mut inside = index.inside(&rect);
        inside.sort_unstable();
        assert_eq!(inside, [100, 110, 120]);
        let small = Rect::new(Coord { x: 1.0, y: 1.0 }, Coord { x: 2.0, y: 2.0 });
        assert_eq!(index.holding(&small), [2000]);
        for (required, expected) in [
            (Boxes::Meet, index.meeting(&rect)),
            (Boxes::Inside, index.inside(&rect)),
            (Boxes::Contains, index.holding(&rect)),
        ] {
            let mut found = Vec::new();
            let result: ControlFlow<()> = index.visit_boxes(required, &rect, |id| {
                found.push(id);
                ControlFlow::Continue(())
            });
            assert_eq!(result, ControlFlow::Continue(()));
            assert_eq!(found, expected);
            let mut calls = 0;
            let result = index.visit_boxes(required, &rect, |_| {
                calls += 1;
                ControlFlow::Break("stop")
            });
            if !expected.is_empty() {
                assert_eq!(result, ControlFlow::Break("stop"));
                assert_eq!(calls, 1);
            }
        }
    }
}
