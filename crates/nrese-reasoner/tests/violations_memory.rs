//! Guard of the consistency report's memory (P2-C4): every violation recorded is charged
//! to the report's budget before it is allocated, its sort and compaction allocate
//! nothing, a budget without room ends the list without making the state look consistent,
//! and duplicates are compacted away before the list refuses. Measured against the budget
//! (`nrese_exec::measure`), so this file is a test binary of its own.

use std::sync::Arc;

use nrese_exec::{Budget, measure};
use nrese_reasoner::ir::Violations;

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

/// `records` distinct violations of a rule with `stride` variables, each recorded `times`
/// times, the copies in reverse order (so sorting has work to do); then finished.
fn fire(violations: &mut Violations, records: u64, stride: usize, times: u64) {
    let group = violations
        .group("user-check", stride)
        .expect("room for the group");
    for _ in 0..times {
        for i in (0..records).rev() {
            violations.record(group, (0..stride as u64).map(|w| i * 1_000 + w));
        }
    }
    violations.finish();
}

/// The list holds nothing it didn't charge first, at any size and width, duplicates and
/// sorting included; and gives everything back when dropped. Its charge grows with the
/// violations.
#[test]
fn violations_hold_no_more_than_their_charge() {
    if !measure::alone() {
        return;
    }
    for (records, stride) in [(1, 1), (5_000, 1), (1, 64), (5_000, 64)] {
        let budget = Arc::new(Budget::unlimited());
        let ((), excess) = measure::measure_within(&budget, || {
            let mut violations = Violations::new(Arc::clone(&budget));
            fire(&mut violations, records, stride, 2);
            assert_eq!(
                violations.at_least() as u64,
                records,
                "{records} × {stride}"
            );
            assert!(violations.complete());
            // Borrowed: `first()`'s owned copy (one per rejected commit, for its
            // explanation, bounded by the program) is no part of the list's charge.
            let (_, first) = violations.iter().next().expect("a violation");
            assert_eq!(first[0], 0, "sorted: the smallest first");
            assert!(
                budget.used() >= records as usize * stride * 8,
                "{records} × {stride}: {} charged",
                budget.used()
            );
            drop(violations);
        });
        assert_eq!(excess, 0, "{records} violations of {stride} words");
        assert_eq!(budget.used(), 0, "{records} violations of {stride} words");
    }
}

/// A budget without room for the list: it stops, says it isn't complete, still makes the
/// state inconsistent (at least one violation, even with none listed), and never holds
/// beyond the budget.
#[test]
fn a_budget_without_room_ends_the_list_and_keeps_the_state_inconsistent() {
    for limit in [0, 512, 4_096] {
        let budget = Arc::new(Budget::new(limit));
        let mut violations = Violations::new(Arc::clone(&budget));
        match violations.group("user-check", 8) {
            Some(group) => {
                for i in 0..10_000u64 {
                    violations.record(group, (0..8).map(|w| i * 10 + w));
                }
            }
            None => assert!(!violations.complete(), "limit {limit}: no group, refused"),
        }
        violations.finish();
        assert!(!violations.complete(), "limit {limit}");
        assert!(!violations.consistent(), "limit {limit}");
        assert!(violations.at_least() >= 1, "limit {limit}");
        assert!(
            budget.peak() <= limit,
            "limit {limit}: peak {}",
            budget.peak()
        );
    }
}

/// A rule whose same violations fire many times (one per derivation): the copies are
/// compacted away when the buffer can't double, so a budget the distinct list fits in
/// lists them all; without compaction the eight copies would need eight times the room.
#[test]
fn duplicates_are_compacted_before_the_list_refuses() {
    let (records, stride, times) = (1_000u64, 4, 8);
    // Room for the distinct list's buffer doubled once (8,192 words) beside its last
    // smaller one, not for the next doubling.
    let limit = (8_192 + 4_096) * 8 + 1_024;
    let budget = Arc::new(Budget::new(limit));
    let mut violations = Violations::new(Arc::clone(&budget));
    fire(&mut violations, records, stride, times);
    assert!(violations.complete(), "the distinct list fits");
    assert_eq!(violations.at_least() as u64, records);
    assert!(budget.peak() <= limit);
}

/// Width 0 (a rule without variables): every firing is the same violation.
#[test]
fn a_rule_without_variables_is_one_violation() {
    let mut violations = Violations::unlimited();
    fire(&mut violations, 50, 0, 3);
    assert_eq!(violations.at_least(), 1);
    assert_eq!(violations.to_vec().len(), 1);
}

/// Two rules of one name and different widths keep their own lists; a later section
/// (the datatype checks) follows an earlier one whatever their names.
#[test]
fn groups_by_width_and_sections_keep_their_order() {
    let mut violations = Violations::unlimited();
    let wide = violations.group("same-name", 3).unwrap();
    let narrow = violations.group("same-name", 1).unwrap();
    violations.record(wide, [7, 8, 9]);
    violations.record(narrow, [5]);
    violations.finish();
    let mut later = Violations::new(Arc::clone(violations.budget()));
    let early_name = later.group("aaa-first-by-name", 1).unwrap();
    later.record(early_name, [1]);
    later.finish();
    violations.append(later);
    let listed: Vec<(String, Vec<u64>)> = violations
        .iter()
        .map(|(rule, bindings)| (rule.to_owned(), bindings.to_vec()))
        .collect();
    assert_eq!(
        listed,
        [
            ("same-name".to_owned(), vec![5]),
            ("same-name".to_owned(), vec![7, 8, 9]),
            ("aaa-first-by-name".to_owned(), vec![1]),
        ]
    );
    assert_eq!(violations.at_least(), 3);
}
