//! Guard of U1's run memory (SYN-61). Building, advancing and compacting the stack and
//! eligible sets must stay within their charge. Pinned readers keep their runs charged
//! until they drop them. This test binary measures allocations against the budget.

use std::sync::Arc;

use nrese_engine::{CompactionPolicy, TripleVersion};
use nrese_exec::{Budget, measure};

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

/// Facts over `predicates` predicates, in shuffled subject order.
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

/// Moves every other fact from the stack to the eligible set and back, holding the
/// intermediate publication across compaction. The last drop returns the charge to zero.
#[test]
fn the_runs_hold_nothing_beyond_their_charge() {
    if !measure::alone() {
        return;
    }
    let mut failed = Vec::new();
    let policy = CompactionPolicy {
        // Only the two-run cap can force an inline compaction with this limit.
        inline_entry_limit: 0,
        ..CompactionPolicy::default()
    };
    for (n, predicates) in [(1, 1), (40, 3), (2_000, 17), (12_000, 600)] {
        let facts = facts(n, predicates);
        let mut moved: Vec<_> = facts.iter().step_by(2).copied().collect();
        moved.sort_unstable();
        let budget = Arc::new(Budget::unlimited());
        let (held_charge, excess) = measure::measure_within(&budget, || {
            let mut stack = TripleVersion::build(facts.iter().copied(), &budget).unwrap();
            let mut eligible = TripleVersion::build(std::iter::empty(), &budget).unwrap();
            stack = stack.successor(&[], &moved).unwrap();
            eligible = eligible.successor(&moved, &[]).unwrap();
            let (held_stack, held_eligible) = (stack.clone(), eligible.clone());

            stack = stack.successor(&moved, &[]).unwrap();
            eligible = eligible.successor(&[], &moved).unwrap();
            let compact = stack.compact(policy).unwrap().expect("third run");
            stack = compact(&stack).unwrap().expect("matching stack");
            let compact = eligible
                .plan_compaction(policy)
                .expect("two equal-sized eligible runs")
                .build()
                .unwrap();
            eligible = compact(&eligible).unwrap().expect("matching eligible set");

            assert_eq!(stack.len(), facts.len());
            assert!(eligible.is_empty());
            // Both components also compact a window whose output still holds facts.
            let keep = CompactionPolicy {
                fanout: u64::MAX,
                ..policy
            };
            let extra = [4 * n + 1, 0, 0];
            for base in [&stack, &held_eligible] {
                let staged = base.successor(&[extra], &[]).unwrap();
                let compact = staged
                    .plan_compaction(keep)
                    .expect("a nonempty merge")
                    .build()
                    .unwrap();
                let compacted = compact(&staged).unwrap().expect("matching runs");
                assert_eq!(compacted.len(), base.len() + 1);
                assert!(compacted.contains(extra));
            }
            drop((stack, eligible));
            assert_eq!(held_stack.len(), facts.len() - moved.len());
            assert_eq!(held_eligible.len(), moved.len());
            let held_charge = budget.used();
            drop((held_stack, held_eligible));
            held_charge
        });
        let remaining = budget.used();
        let row = format!(
            "{n} facts: {excess} bytes beyond the charge, {held_charge} charged while held, {remaining} left"
        );
        eprintln!("{row}");
        if excess != 0 || held_charge == 0 || remaining != 0 {
            failed.push(row);
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
    let mut stack = TripleVersion::build(std::iter::empty(), &budget).unwrap();
    let mut inserted = 0;
    for t in facts(1_000, 7) {
        let charged = budget.used();
        match stack.successor(&[t], &[]) {
            Ok(next) => {
                stack = next;
                inserted += 1;
            }
            Err(_) => {
                assert!(!stack.contains(t));
                assert_eq!(budget.used(), charged);
                break;
            }
        }
    }
    assert!(inserted > 0 && inserted < 1_000, "{inserted} inserted");
    assert_eq!(stack.len(), inserted);
    drop(stack);
    assert_eq!(budget.used(), 0);
}
