//! What the executor keeps of terms, under this test binary's counting allocator
//! ([`nrese_exec::measure`]).

use super::*;
use nrese_exec::measure::{alone, measure_within};
use nrese_rdf::{Literal, NamedNode, Quad};

/// A literal of 100 bytes, the `i`-th of `kind`.
fn long(kind: &str, i: usize) -> Literal {
    Literal::new_simple_literal(format!("{kind} {i:>94}"))
}

/// The terms a query computes (a value per row) and decodes (its cache) are charged before
/// they are kept, however many: what they hold uncharged doesn't grow with their number.
#[test]
fn computed_and_decoded_terms_hold_no_more_than_they_charge() {
    if !alone() {
        return;
    }
    let held = |terms: usize| {
        let engine = nrese_engine::Engine::new(Default::default()).unwrap();
        let mut tx = engine.transaction();
        let p = NamedNode::new_unchecked("urn:p");
        for i in 0..terms {
            let s = NamedNode::new_unchecked(format!("urn:s{i}"));
            tx.insert(
                Quad::new(
                    s,
                    p.clone(),
                    long("stored", i),
                    nrese_rdf::GraphName::DefaultGraph,
                )
                .as_ref(),
            );
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let stored: Vec<u64> = (0..terms)
            .map(|i| {
                snapshot
                    .lookup(Term::from(long("stored", i)).as_ref())
                    .unwrap()
                    .raw()
            })
            .collect();
        let fresh: Vec<Term> = (0..terms).map(|i| long("computed", i).into()).collect();
        let options = QueryOptions::default();
        let ctx = Context::new(&snapshot, &options, None, None);
        let ((), excess) = measure_within(&ctx.budget, || {
            for term in &fresh {
                ctx.id(term).ok().expect("charged");
            }
            for &id in &stored {
                drop(ctx.term(id));
            }
        });
        excess
    };
    assert_eq!(held(10), held(5_000));
}
