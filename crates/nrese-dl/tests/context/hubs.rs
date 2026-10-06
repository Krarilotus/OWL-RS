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

/// `W ⊑ ∃r.U ⊓ ∀r.Pᵢ` (i < m), `U ⊑ ∃r.V`, `Pᵢ ⊑ ∀r.Aⱼ` (j < k), `A₀ ⊓ … ⊓ Aₖ₋₁ ⊑
/// ∀r⁻.H`: in `U`'s context each `Aⱼ(f(x))` holds under each possible `Pᵢ(x)`, so the
/// Pred join of `V`'s clause meets `mᵏ` combinations, of which `m` aren't redundant.
fn products(table: &mut Table, m: usize, k: usize) -> Ontology {
    let mut b = Build::new(table);
    let r = b.r("r");
    let s = b.r("s");
    b.axiom(Axiom::InverseObjectProperties(r, s));
    let (w, u, v, h) = (b.c("W"), b.c("U"), b.c("V"), b.c("H"));
    let to_u = b.some(r, u);
    b.sub(w, to_u);
    let to_v = b.some(r, v);
    b.sub(u, to_v);
    let a: Vec<_> = (0..k).map(|j| b.c(&format!("A{j}"))).collect();
    for i in 0..m {
        let p = b.c(&format!("P{i}"));
        let all_p = b.all(r, p);
        b.sub(w, all_p);
        for &aj in &a {
            let all_a = b.all(r, aj);
            b.sub(p, all_a);
        }
    }
    let all_a = b.and(&a);
    let back = b.all(s, h);
    b.sub(all_a, back);
    b.done()
}

/// Guard (Pred's pruning within a batch): the conclusions of one join are derived after
/// it, so a body found earlier in the join must prune the later ones that contain it
/// (one join on ore_ont_9835 went past a million conclusions; Pred conclusions there
/// 361 k -> 129 k, on ore_ont_7914 323 k -> 58 k).
#[test]
fn pred_joins_prune_by_the_bodies_they_found() {
    let (m, k) = (4, 3);
    let mut table = Table::default();
    let o = products(&mut table, m, k);
    for strategy in [Strategy::Cautious, Strategy::Split] {
        let (all, unpruned) = run(&o, strategy, false);
        let (pruned_taxonomy, pruned) = run(&o, strategy, true);
        assert_eq!(pruned_taxonomy, all);
        // The join meets every combination: mᵏ = 64.
        assert!(unpruned.pred_inferences >= 64, "{}", unpruned.line());
        assert!(pruned.pred_inferences <= m as u64, "{}", pruned.line());
    }
}

/// Guard (never refuse): a join past `Budget::max_join_steps` is left and its context
/// marked incomplete, while the run goes on (before, one exploding join ended the whole
/// lower bound): the saturation finishes, isn't complete, and isn't a complete answer.
#[test]
fn a_join_past_its_steps_leaves_its_context_incomplete() {
    let mut table = Table::default();
    let o = products(&mut table, 4, 3);
    let options = Options {
        strategy: Strategy::Split,
        proofs: false,
        budget: context::Budget {
            max_join_steps: Some(2),
            ..context::Budget::default()
        },
        ..Options::default()
    };
    let saturated = context::saturate(&o, &options).expect("left joins don't end the run");
    assert!(!saturated.complete());
    assert!(saturated.profile().contexts_joins_left > 0);
    assert!(matches!(
        context::classify(&o, &options),
        Err(context::Unsupported::Budget)
    ));
}
