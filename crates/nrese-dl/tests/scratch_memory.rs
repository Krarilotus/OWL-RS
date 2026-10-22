//! Guard of the compilation scratch's estimates (`nrese_dl::memory`), which a request's
//! envelope is charged by: against the allocator, each stage's estimate is near what the
//! stage holds, neither far below (the envelope would let a request past its limit) nor
//! far above (it would refuse requests that fit). Measured with a counting allocator, so
//! this file is a test binary of its own.

use nrese_dl::memory::{normalised_bytes, ontology_bytes};
use nrese_exec::heap;
use nrese_owl::{Axiom, ClassExpr, ExprId, Ontology, Source};

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

/// An ontology as the store reads one: a small TBox with a union, and an ABox of
/// `individuals` typed and linked individuals, each axiom with its source triples.
fn ontology(individuals: u64) -> Ontology {
    let mut o = Ontology::default();
    let class = |o: &mut Ontology, c| ExprId(o.classes.intern(ClassExpr::Class(c)));
    let [a, b, c, d] = [1, 2, 3, 4].map(|n| class(&mut o, n));
    let union = ExprId(o.classes.intern(ClassExpr::Or(vec![b, c])));
    let add = |o: &mut Ontology, axiom: Axiom, triples: Vec<[u64; 3]>| {
        o.axioms.push(axiom);
        o.sources.push(vec![Source {
            graph: 0,
            triples,
            elsewhere: None,
        }]);
    };
    add(&mut o, Axiom::SubClassOf(a, union), vec![[1, 10, 11]; 3]);
    add(&mut o, Axiom::SubClassOf(b, d), vec![[2, 10, 4]]);
    add(&mut o, Axiom::SubClassOf(c, d), vec![[3, 10, 4]]);
    for i in 0..individuals {
        let x = 1_000 + i;
        add(&mut o, Axiom::ClassAssertion(a, x), vec![[x, 12, 1]]);
        add(
            &mut o,
            Axiom::ObjectPropertyAssertion(20, x, x + 1),
            vec![[x, 20, x + 1]],
        );
    }
    o
}

/// What `make` leaves live, by the allocator.
fn held<T>(make: impl FnOnce() -> T) -> (T, usize) {
    let before = heap::live();
    let made = make();
    (made, heap::live().saturating_sub(before))
}

fn near(what: &str, estimate: usize, held: usize) {
    assert!(
        estimate >= held / 10 * 9 && estimate <= held / 4 * 5,
        "{what}: estimated {estimate} bytes, the allocator counts {held}"
    );
}

#[test]
fn the_compilation_scratch_estimates_are_near_what_each_stage_holds() {
    let source = ontology(5_000);
    let (copy, bytes) = held(|| source.clone());
    near("the premise's copy", ontology_bytes(&copy), bytes);
    let (normalised, bytes) = held(|| nrese_owl::normalise(&copy));
    near("the clauses", normalised_bytes(&normalised), bytes);
    let (program, bytes) = held(|| nrese_dl::tableau::Prepared::new(&copy, &normalised, &[4], &[]));
    near("the program", program.bytes(), bytes);
}
