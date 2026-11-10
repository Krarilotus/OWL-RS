//! A search of the algebra ([`GraphPattern::find`]) allocates nothing: it runs on every query
//! before evaluation, several times (the native executor's rewrite checks, its triple-term
//! check), and a microsecond query paid for a vector at every node (Harness note
//! 2026-11-10-small-query-fastpath-design.md, profile smallq-attrib-7d1e9109-1110). Counted
//! with an allocator of this test binary's own, on this thread only.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use nrese_sparql_syntax::algebra::GraphPattern;
use nrese_sparql_syntax::visit::Node;
use nrese_sparql_syntax::{Query, SparqlParser};

thread_local! {
    /// Allocations on this thread while counting; `None` while not counting.
    static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

struct Counting;

// SAFETY: every call goes to the system allocator unchanged; counting has no other effect.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.with(|n| n.set(n.get().map(|n| n + 1)));
        // SAFETY: the caller's contract for `alloc` is the system allocator's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from the system allocator with `layout`, as the caller guarantees.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The allocations `f` makes on this thread.
fn allocations(f: impl FnOnce()) -> usize {
    ALLOCATIONS.with(|n| n.set(Some(0)));
    f();
    ALLOCATIONS.with(|n| n.replace(None)).unwrap()
}

#[test]
fn a_search_of_a_query_s_algebra_allocates_nothing() {
    // Every pattern kind with nodes in it, expressions with operands, and an EXISTS pattern.
    let text = "SELECT DISTINCT ?x (SUM(?n) AS ?s) (COUNT(*) AS ?c) WHERE {
          { ?x <urn:p> ?n } UNION { GRAPH ?g { ?x <urn:q> ?n } }
          OPTIONAL { ?x <urn:r> ?m FILTER(?m > 1 && ?m < 9 || ?m = 0) }
          MINUS { ?x <urn:s> ?n }
          BIND(IF(?n IN (1, 2), COALESCE(-?n + +?n, ?n * 2 / 3), !BOUND(?m)) AS ?b)
          FILTER EXISTS { ?x <urn:v> ?n FILTER(STRLEN(STR(?x)) >= 0) }
        } GROUP BY ?x ORDER BY DESC(?s - 1) LIMIT 5";
    let Query::Select { pattern, .. } = SparqlParser::new().parse_query(text).unwrap() else {
        unreachable!()
    };
    let mut nodes = 0;
    // A search that finds nothing visits every node.
    let made = allocations(|| {
        pattern.find(&mut |_| {
            nodes += 1;
            false
        });
    });
    assert!(nodes > 30, "{nodes} nodes visited");
    assert_eq!(made, 0, "allocations in a search of {nodes} nodes");
    // And a search that stops early.
    let made = allocations(|| {
        assert!(pattern.find(&mut |node| matches!(node, Node::Pattern(GraphPattern::Bgp { .. }))));
    });
    assert_eq!(made, 0);
}
