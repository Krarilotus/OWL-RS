//! Guard of U1's stack's memory (SYN-61): what the stack's trees and table hold is charged
//! before it grows, at every size, through inserts and removes alike, and nothing stays
//! charged once it goes. Measured against the budget (`nrese_exec::measure`), so this file is
//! a test binary of its own.

use std::sync::Arc;

use nrese_exec::{Budget, measure};
use nrese_store::dl::upper::Stack;

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

/// Facts over `predicates` predicates, in an order that splits nodes everywhere.
fn facts(n: u64, predicates: u64) -> Vec<[u64; 3]> {
    (0..n)
        .map(|i| {
            [
                i.wrapping_mul(0x9E37_79B9) % (4 * n + 1),
                i % predicates,
                i / 3,
            ]
        })
        .collect()
}

/// At sizes from one fact to past ten thousand, inserted, half removed, inserted again
/// (commits' churn), then dropped: nothing held beyond the charge, the charge back to 0.
/// Every row is reported.
#[test]
fn the_stack_holds_nothing_beyond_its_charge() {
    if !measure::alone() {
        return;
    }
    let mut failed = Vec::new();
    for (n, predicates) in [(1, 1), (40, 3), (2_000, 17), (12_000, 600)] {
        let facts = facts(n, predicates);
        let budget = Arc::new(Budget::unlimited());
        let (peak, excess) = measure::measure_within(&budget, || {
            let mut stack = Stack::within(&budget);
            for &t in &facts {
                stack.try_insert(t).unwrap();
            }
            for &t in facts.iter().step_by(2) {
                stack.remove_charged(t);
            }
            for &t in facts.iter().step_by(2) {
                stack.try_insert(t).unwrap();
            }
            let peak = budget.used();
            drop(stack);
            peak
        });
        if excess != 0 || peak == 0 || budget.used() != 0 {
            failed.push(format!(
                "{n} facts: {excess} bytes beyond the charge, {peak} charged, {} left",
                budget.used()
            ));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// A budget that can't take one more fact refuses it and keeps the stack as it was.
#[test]
fn a_refused_fact_is_not_inserted() {
    if !measure::alone() {
        return;
    }
    let budget = Arc::new(Budget::new(4096));
    let mut stack = Stack::within(&budget);
    let mut inserted = 0;
    for t in facts(1_000, 7) {
        match stack.try_insert(t) {
            Ok(_) => inserted += 1,
            Err(_) => break,
        }
    }
    assert!(inserted > 0 && inserted < 1_000, "{inserted} inserted");
    assert_eq!(stack.len(), inserted);
    drop(stack);
    assert_eq!(budget.used(), 0);
}
