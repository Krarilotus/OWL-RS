//! Guard of the consistency violations' memory (N12; Sol r1): every violation an evaluation
//! records is charged to its budget before it is made, in both dimensions that grow, the
//! violations that fire and their rule's body (a user rule's may be long), and a budget
//! without room stops the list without making the state look consistent. Measured against
//! that budget (`nrese_exec::measure`), so this file is a test binary of its own.

use std::sync::Arc;

use nrese_exec::{Budget, measure};
use nrese_reasoner::ir::{Atom, Head, Rule, Term, Violations};

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

/// A consistency rule of `body` atoms over two variables, each atom its own premise.
fn rule(body: usize) -> Rule {
    Rule {
        name: "user-check".to_owned(),
        body: (0..body as u64)
            .map(|i| Atom([Term::Var(0), Term::Const(1_000 + i), Term::Var(1)]))
            .collect(),
        guards: Vec::new(),
        head: Head::Inconsistent,
    }
}

/// `firings` violations of `rule`, each recorded twice (the second a duplicate the list
/// hands back), into `violations`.
fn fire(violations: &mut Violations, rule: &Rule, firings: u64) {
    for i in 0..firings {
        for _ in 0..2 {
            violations.record(rule, &[Some(i), Some(7)], true);
        }
    }
}

/// The list holds nothing it didn't reserve first, at any size: many firings, a long body,
/// both, with duplicates; and gives everything back when dropped.
#[test]
fn violations_hold_no_more_than_their_charge() {
    if !measure::alone() {
        return;
    }
    for (firings, body) in [(1, 1), (2_000, 1), (1, 64), (2_000, 64)] {
        let budget = Arc::new(Budget::unlimited());
        let rule = rule(body);
        let ((), excess) = measure::measure_within(&budget, || {
            let mut violations = Violations::new(Arc::clone(&budget));
            fire(&mut violations, &rule, firings);
            violations.finish();
            assert_eq!(violations.len() as u64, firings);
            assert!(violations.complete());
            drop(violations);
        });
        assert_eq!(excess, 0, "{firings} firings of a {body}-atom body");
        assert_eq!(budget.used(), 0, "{firings} firings of a {body}-atom body");
    }
}

/// The charge grows with each dimension: per firing, a premise per body atom on top of the
/// rule's name and bindings.
#[test]
fn the_charge_scales_with_the_firings_and_the_body() {
    let charged = |firings: u64, body: usize| {
        let budget = Arc::new(Budget::unlimited());
        let mut violations = Violations::new(Arc::clone(&budget));
        fire(&mut violations, &rule(body), firings);
        violations.finish();
        budget.used()
    };
    let triple = std::mem::size_of::<[u64; 3]>();
    // The same list, a longer body: exactly its premises more.
    assert_eq!(charged(1_000, 64) - charged(1_000, 1), 1_000 * 63 * triple);
    // More firings: at least their own bytes more.
    assert!(charged(2_000, 8) - charged(1_000, 8) >= 1_000 * 8 * triple);
}

/// A budget with no room for one violation: the list stops, says it isn't complete, and
/// still makes the state inconsistent; it never holds beyond the budget.
#[test]
fn a_budget_without_room_stops_the_list_and_keeps_the_state_inconsistent() {
    for limit in [0, 512] {
        let budget = Arc::new(Budget::new(limit));
        let mut violations = Violations::new(Arc::clone(&budget));
        fire(&mut violations, &rule(64), 100);
        violations.finish();
        assert!(!violations.complete(), "limit {limit}");
        assert!(violations.inconsistent(), "limit {limit}");
        assert_eq!(violations.found(), violations.len().max(1), "limit {limit}");
        assert!(budget.used() <= limit, "limit {limit}");
    }
}
