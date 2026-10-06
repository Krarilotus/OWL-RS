//! Guards of the context core's costs (docs/design/performance.md §0): Pred's pruning and
//! the split expansion strategy, on a Horn-SHI pattern where inverse roles make the
//! successors' fillers uncertain.

use nrese_dl::context::{self, Classification, Options, Strategy};
use nrese_owl::{Axiom, Ontology};

use crate::support::{Build, Table};

/// `Aᵢ ⊑ ∃r.Cᵢ`, `∃s.Aᵢ ⊑ Pᵢ` with `s = r⁻` (a successor's `Pᵢ` depends on its
/// predecessor), `Pᵢ ⊑ ∃r.Eᵢ` (so the next successor's filler is uncertain where it is
/// made), `Eᵢ ⊓ ∃s.Pⱼ ⊑ Qᵢⱼ` for a few `j` (conditional knowledge about predecessors).
fn pattern(table: &mut Table, n: usize) -> Ontology {
    let mut b = Build::new(table);
    let r = b.r("r");
    let s = b.r("s");
    b.axiom(Axiom::InverseObjectProperties(r, s));
    for i in 0..n {
        let a = b.c(&format!("A{i}"));
        let c = b.c(&format!("C{i}"));
        let p = b.c(&format!("P{i}"));
        let e = b.c(&format!("E{i}"));
        let to_c = b.some(r, c);
        b.sub(a, to_c);
        let from_a = b.some(s, a);
        b.sub(from_a, p);
        let to_e = b.some(r, e);
        b.sub(p, to_e);
        for j in 0..4.min(n) {
            let pj = b.c(&format!("P{}", (i + j) % n));
            let from_pj = b.some(s, pj);
            let both = b.and(&[e, from_pj]);
            let q = b.c(&format!("Q{i}_{j}"));
            b.sub(both, q);
        }
    }
    b.done()
}

fn run(o: &Ontology, strategy: Strategy, prune: bool) -> (Classification, context::Profile) {
    let options = Options {
        strategy,
        prune_pred: prune,
        proofs: false,
        ..Options::default()
    };
    context::classify(o, &options).expect("Horn")
}

/// Guard (split strategy): the successors whose filler isn't certain share one
/// empty-core context under the cautious strategy, which then holds a clause per
/// predecessor's condition (ore_ont_9835: 3.0 M of 3.0 M clauses); split gives each
/// Skolem function its own. Guard (Pred's pruning): a join stops where its body so far is
/// already subsumed (ore_ont_9835: 18.6 M Pred conclusions -> 0.2 M). Same taxonomy
/// under all four.
#[test]
fn no_hub_context_and_pruned_pred_joins() {
    let mut table = Table::default();
    let o = pattern(&mut table, 40);
    let (base, cautious) = run(&o, Strategy::Cautious, false);
    let (c1, cautious_pruned) = run(&o, Strategy::Cautious, true);
    let (c2, split) = run(&o, Strategy::Split, false);
    let (c3, split_pruned) = run(&o, Strategy::Split, true);
    assert_eq!(c1, base);
    assert_eq!(c2, base);
    assert_eq!(c3, base);
    assert!(cautious.largest_context >= 200, "{}", cautious.line());
    assert!(split.largest_context <= 16, "{}", split.line());
    assert!(
        cautious_pruned.pred_inferences * 2 <= cautious.pred_inferences,
        "{} / {}",
        cautious_pruned.line(),
        cautious.line()
    );
    assert!(
        split_pruned.pred_inferences * 2 <= split.pred_inferences,
        "{}",
        split_pruned.line()
    );
}

/// Guard (memory): past `Budget::max_memory` the saturation stops with
/// `Unsupported::Budget` instead of allocating on (ore_ont_9724 with the Eq rule took a
/// 12 GB process down). One byte is always exceeded.
#[test]
fn a_memory_limit_stops_the_saturation() {
    let mut table = Table::default();
    let o = pattern(&mut table, 20);
    let options = Options {
        proofs: false,
        budget: context::Budget {
            max_memory: Some(1),
            ..context::Budget::default()
        },
        ..Options::default()
    };
    assert!(matches!(
        context::classify(&o, &options),
        Err(context::Unsupported::Budget)
    ));
}
