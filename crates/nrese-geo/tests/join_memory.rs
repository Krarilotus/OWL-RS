//! Representative allocator checks of the join's capacity estimates, on one worker.
//! Private geo self-noding/relating scratch remains cooperative; these fixtures do not
//! establish a heap ceiling for arbitrary self-intersecting geometries.

use geo::{Geometry, PreparedGeometry};
use nrese_exec::{Budget, heap};
use nrese_geo::{Item, Side, WKT_LITERAL, geometry_heap_bytes, join, parse};

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

#[test]
fn ordinary_geometry_tree_and_preparation_estimates_cover_observed_capacity() {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    // Start the registry before counting. All nested join work stays on this worker.
    pool.install(|| {});
    for text in [
        "POINT(1 1)",
        "LINESTRING(0 0, 1 2, 2 0)",
        "POLYGON((0 0, 2 0, 2 2, 0 2, 0 0))",
        "POLYGON((0 0,10 0,10 10,0 10,0 0),(2 2,4 2,4 4,2 4,2 2))",
    ] {
        pool.install(|| {
            heap::start("parse");
            heap::begin_step();
            let before = heap::live();
            let shape = parse(WKT_LITERAL, text).unwrap();
            let retained = heap::live().saturating_sub(before);
            heap::end_step("parse");
            let peak = heap::steps()[0].peak;
            heap::finish();
            // CRS is retained by Shape; the join retains only its geometry. WKT's
            // in-place coordinate/ring conversions preserve more than lexical length
            // predicts, so compare allocator retention against the owning capacities.
            let bytes = geometry_heap_bytes(&shape.geometry) + shape.crs.capacity();
            eprintln!("{text}: parsed retained {retained}, peak {peak}, estimate {bytes}");
            assert!(
                retained <= bytes,
                "{text}: retained {retained}, estimate {bytes}"
            );
            // Parser conversion scratch is opaque; bound only these ordinary fixtures'
            // observed overshoot, not arbitrary input. The request polls after parsing.
            assert!(
                peak <= bytes + 1024,
                "{text}: parser peak {peak}, estimate {bytes}"
            );
            if !matches!(shape.geometry, Geometry::Point(_)) {
                use geo::CoordsIter;
                let prepared_bytes = shape.geometry.coords_count() * 512 + 1024;
                heap::start("prepare");
                heap::begin_step();
                let prepared = PreparedGeometry::from(&shape.geometry);
                heap::end_step("prepare");
                let peak = heap::steps()[0].peak;
                heap::finish();
                eprintln!("{text}: prepared peak {peak}, estimate {prepared_bytes}");
                assert!(
                    peak <= prepared_bytes,
                    "{text}: preparation {peak}, estimate {prepared_bytes}"
                );
                drop(prepared);
            }
        });
        let shape = parse(WKT_LITERAL, text).unwrap();
        let item = Item {
            rect: shape.rect(),
            crs: 0,
            point: matches!(shape.geometry, Geometry::Point(_)),
            bytes: geometry_heap_bytes(&shape.geometry),
        };
        let (left, right) = (vec![item; 32], vec![item; 64]);
        pool.install(|| {
            let budget = Budget::unlimited();
            let read = |_: Side, _: usize| Some(shape.geometry.clone());
            heap::start("join");
            heap::begin_step();
            let (pairs, stats, held) = join("sfIntersects", &left, &right, &read, &|| false, &budget).unwrap();
            heap::end_step("join");
            let observed = heap::steps()[0].peak;
            heap::finish();
            eprintln!("{text}: join heap {observed}, accounted {}, pairs {}, position bytes {} (former u32 pair: 8)", budget.peak(), pairs.len(), size_of::<(usize, usize)>());
            assert_eq!(pairs.len(), left.len() * right.len());
            if !item.point { assert!(stats.prepared > 0); }
            assert!(observed <= budget.peak(), "{text}: heap {observed}, accounted {}", budget.peak());
            assert_eq!(budget.used(), held.bytes());
            drop(pairs);
            drop(held);
            assert_eq!(budget.used(), 0);
        });
    }
    pool.install(|| {
        use nrese_geo::{Boxes, GeometryIndex};
        use std::ops::ControlFlow;
        let texts: Vec<_> = (0..512).map(|i| format!("POINT({i} {i})")).collect();
        let literals: Vec<_> = texts
            .iter()
            .enumerate()
            .map(|(i, text)| (i as u64, WKT_LITERAL, text.as_str()))
            .collect();
        let mut index = GeometryIndex::default();
        index.extend(&literals, 512);
        let rect = geo::Rect::new(geo::coord! { x: 1., y: 1. }, geo::coord! { x: 1., y: 1. });
        for required in [Boxes::Meet, Boxes::Inside, Boxes::Contains] {
            heap::start("visit");
            heap::begin_step();
            let mut hits = 0;
            let result: ControlFlow<()> = index.visit_boxes(required, &rect, |_| {
                hits += 1;
                ControlFlow::Continue(())
            });
            heap::end_step("visit");
            let peak = heap::steps()[0].peak;
            heap::finish();
            assert_eq!(result, ControlFlow::Continue(()));
            assert_eq!(hits, 1);
            assert_eq!(peak, 0, "index traversal must not allocate a hit buffer");
        }
    });
}
