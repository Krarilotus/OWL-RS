//! Guard of a key's scratch (`tableau::keys`; PR #48's review, F1, and its fix check): the
//! pairs a key tries are enumerated, not stored, so evaluating it holds memory linear in its
//! members, not in their pairs, and what it holds is what it is charged. Beside it, the guards
//! of what a merge of named individuals copies and reads (`tableau::merge`; I35, PR #72). Measured
//! with a counting allocator, so this file is a test binary of its own; its tests take turns
//! (`PROFILE`), since a heap profile is the process's.

use std::sync::{Mutex, PoisonError};

use nrese_dl::tableau::{Answer, Config, consistency};
use nrese_exec::heap;
use nrese_owl::{Axiom, Characteristic, ClassExpr, ExprId, ObjProp, Ontology};

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

/// Held by each test for its whole run: two at once would share one profile.
static PROFILE: Mutex<()> = Mutex::new(());

fn add(o: &mut Ontology, axiom: Axiom) {
    o.axioms.push(axiom);
    o.sources.push(Vec::new());
}

/// `HasKey(A (R) ())` with R transitive, and `members` A-individuals with R to one `z`, the
/// first two of them different: every two members share `z`'s reach name, and the key makes
/// the first two equal.
fn shared_target(members: u64) -> Ontology {
    let mut o = Ontology::default();
    let a = ExprId(o.classes.intern(ClassExpr::Class(1)));
    let r = ObjProp::Named(2);
    add(
        &mut o,
        Axiom::ObjectCharacteristic(Characteristic::Transitive, r),
    );
    add(&mut o, Axiom::HasKey(a, vec![r], Vec::new()));
    for x in 100..100 + members {
        add(&mut o, Axiom::ClassAssertion(a, x));
        add(&mut o, Axiom::ObjectPropertyAssertion(2, x, 10));
    }
    add(&mut o, Axiom::DifferentIndividuals(vec![100, 101]));
    o
}

/// The fix check's witness: `HasKey(A (R₁ … Rₖ) ())` with each Rⱼ transitive and one
/// A-individual `a` with Rⱼ(a, a), so `a` holds the one name of each Rⱼ's group, a list each.
fn self_reaching(properties: u64) -> Ontology {
    let mut o = Ontology::default();
    let a = ExprId(o.classes.intern(ClassExpr::Class(1)));
    let roles: Vec<ObjProp> = (1000..1000 + properties).map(ObjProp::Named).collect();
    for &r in &roles {
        add(
            &mut o,
            Axiom::ObjectCharacteristic(Characteristic::Transitive, r),
        );
        add(&mut o, Axiom::ObjectPropertyAssertion(r.named(), 100, 100));
    }
    add(&mut o, Axiom::HasKey(a, roles, Vec::new()));
    add(&mut o, Axiom::ClassAssertion(a, 100));
    o
}

/// One consistency check: its answer, the most one evaluation of the key held at once (its
/// step of the heap profile) and the most one was charged. The check as a whole holds more
/// (its graph and the members' merges), so its peak can't tell the key's part apart.
fn key_step(ontology: &Ontology, config: &Config) -> (Answer, usize, usize) {
    heap::start("consistency");
    let outcome = consistency(ontology, config);
    let steps = heap::steps();
    let _ = heap::finish();
    let held = steps
        .iter()
        .filter(|step| step.label == "dl: a key evaluated")
        .map(|step| step.peak)
        .max()
        .expect("the key is evaluated");
    (outcome.answer, held, outcome.telemetry.key_scratch as usize)
}

fn key_scratch(members: u64) -> usize {
    let config = Config {
        portfolio: false,
        ..Config::default()
    };
    let (answer, held, _) = key_step(&shared_target(members), &config);
    assert_eq!(
        answer,
        Answer::Inconsistent,
        "{members} members, the first two different"
    );
    held
}

#[test]
fn a_key_holds_memory_linear_in_its_members_and_is_charged_what_it_holds() {
    let _turn = PROFILE.lock().unwrap_or_else(PoisonError::into_inner);
    let small = key_scratch(100);
    let large = key_scratch(400);
    // Four times the members may hold four times the memory, twice that for slack; their
    // pairs are 16 times as many.
    assert!(
        large <= small * 8,
        "the key held {small} bytes at most for 100 members, {large} for 400"
    );
    // What it is charged is near what it holds (as `scratch_memory.rs` holds the compilation's
    // estimates): every list by its capacity, a one-name list's four slots included. The
    // model is kept so that the tableau runs, not the counting shortcut.
    let config = Config {
        portfolio: false,
        keep_model: true,
        ..Config::default()
    };
    let (answer, held, charged) = key_step(&self_reaching(200), &config);
    assert_eq!(answer, Answer::Consistent, "one individual reaching itself");
    assert!(
        charged >= held / 10 * 9 && charged <= held / 4 * 5,
        "the key was charged {charged} bytes and held {held}"
    );
}

/// The whole check (I35): the key's members, each merged into the last, had the whole class
/// copied at every merge, so the check held memory and asserted facts quadratic in them
/// (5.1 GB at 8,000 members). Of two named individuals without successors, the one copying fewer
/// facts goes into the other.
#[test]
fn the_check_of_a_key_s_members_holds_memory_near_linear_in_them() {
    let _turn = PROFILE.lock().unwrap_or_else(PoisonError::into_inner);
    let check = |members: u64| {
        let config = Config {
            portfolio: false,
            ..Config::default()
        };
        heap::start("consistency");
        let outcome = consistency(&shared_target(members), &config);
        let peak = heap::finish().iter().map(|phase| phase.peak).max();
        assert_eq!(
            outcome.answer,
            Answer::Inconsistent,
            "{members} members, the first two different"
        );
        (peak.unwrap_or(0), outcome.telemetry.facts)
    };
    let (small, small_facts) = check(400);
    let (large, large_facts) = check(800);
    // Twice the members: about twice of each, four times if quadratic.
    assert!(
        large_facts < small_facts * 3,
        "SPEC I35: {small_facts} facts asserted for 400 members, {large_facts} for 800"
    );
    assert!(
        large < small * 3,
        "SPEC I35: {small} bytes held at most for 400 members, {large} for 800"
    );
}

/// PR #72 r1 F1: of two named individuals, the smaller class isn't the side with fewer facts to
/// copy. `c` holding N labels joined `a ≈ b`'s class of two and had all N copied. What a merge
/// copies is counted in the facts asserted.
#[test]
fn a_merge_of_named_individuals_copies_the_side_with_fewer_facts() {
    let _turn = PROFILE.lock().unwrap_or_else(PoisonError::into_inner);
    let asserted = |labels: u64| {
        let mut o = Ontology::default();
        let (a, b, c) = (100, 101, 102);
        for k in 0..labels {
            let class = ExprId(o.classes.intern(ClassExpr::Class(1000 + k)));
            add(&mut o, Axiom::ClassAssertion(class, c));
        }
        add(&mut o, Axiom::SameIndividual(vec![a, b]));
        add(&mut o, Axiom::SameIndividual(vec![b, c]));
        let config = Config {
            portfolio: false,
            keep_model: true,
            ..Config::default()
        };
        let outcome = consistency(&o, &config);
        assert_eq!(outcome.answer, Answer::Consistent, "{labels} labels on c");
        outcome.telemetry.facts
    };
    let (few, many) = (asserted(200), asserted(400));
    // 200 more labels on c assert 200 more facts; copied to another node as well, 400.
    assert!(
        many - few <= 250,
        "SPEC merge-copies: {few} facts asserted for 200 labels on c, {many} for 400"
    );
}

/// The model kept, so that the tableau runs, not the counting shortcut.
fn facts_asserted(o: &Ontology, what: &str) -> u64 {
    let config = Config {
        portfolio: false,
        keep_model: true,
        ..Config::default()
    };
    let outcome = consistency(o, &config);
    assert_eq!(outcome.answer, Answer::Consistent, "{what}");
    outcome.telemetry.facts
}

fn class(o: &mut Ontology, id: u64) -> ExprId {
    ExprId(o.classes.intern(ClassExpr::Class(id)))
}

/// PR #72's fix check, R1-F1 at the bound: K labels on `b` and N on `c`, then
/// `SameIndividual(a b)` (or no `a`) and `b` with `c`, either order. Where the choice couldn't
/// read both sides to their end, the fix fell back on class sizes and copied c's N labels where
/// the calculus's order (`b` into `c` for `(b c)`) copies b's K. A merge copies the increment of
/// N only where the calculus's order does: `(c b)`, with b too long to read to its end
/// (`WALK` = 1,024 in merge.rs).
#[test]
fn a_merge_copies_no_more_than_the_calculus_order_at_the_read_s_bound() {
    let _turn = PROFILE.lock().unwrap_or_else(PoisonError::into_inner);
    const WALK: u64 = 1024;
    let asserted = |k: u64, n: u64, with_a: bool, c_first: bool| {
        let mut o = Ontology::default();
        let (a, b, c) = (100, 101, 102);
        for i in 0..k {
            let label = class(&mut o, 10_000 + i);
            add(&mut o, Axiom::ClassAssertion(label, b));
        }
        for i in 0..n {
            let label = class(&mut o, 1_000_000 + i);
            add(&mut o, Axiom::ClassAssertion(label, c));
        }
        if with_a {
            add(&mut o, Axiom::SameIndividual(vec![a, b]));
        }
        let pair = if c_first { vec![c, b] } else { vec![b, c] };
        add(&mut o, Axiom::SameIndividual(pair));
        facts_asserted(&o, &format!("{k} labels on b, {n} on c"))
    };
    let mut over = Vec::new();
    for k in [WALK - 1, WALK, WALK + 1] {
        for with_a in [true, false] {
            for c_first in [false, true] {
                let added = asserted(k, 8192, with_a, c_first) - asserted(k, 4096, with_a, c_first);
                // 4,096 more labels on c assert 4,096 facts; copied to b as well, 8,192.
                let may = if c_first && k >= WALK { 8192 } else { 4096 };
                if added > may + 64 {
                    over.push(format!("K = {k}, a: {with_a}, c first: {c_first}: {added}"));
                }
            }
        }
    }
    assert!(
        over.is_empty(),
        "SPEC merge-bound: facts for 4,096 more labels on c: {}",
        over.join("; ")
    );
}

/// PR #72's fix check, FC-R1-F2: one R edge from `a` to `x`, and `x` owns `≥256 S.C` (256
/// successors, 32,640 inequalities); Horn axioms then make `b ≈ a`, and `b` holds 64 labels.
/// Moving `a` prunes x's subtree and `b` builds it again from the copied existential; the
/// calculus's order moves `b` and copies its labels. The merge adds about those labels to what
/// the check holds without it.
#[test]
fn a_merge_keeps_the_subtree_under_one_edge() {
    let _turn = PROFILE.lock().unwrap_or_else(PoisonError::into_inner);
    let asserted = |merged: bool| {
        let mut o = Ontology::default();
        let (a, b) = (100, 101);
        let (r, s, t) = (ObjProp::Named(2), ObjProp::Named(3), ObjProp::Named(4));
        let (c, d, e) = (class(&mut o, 5), class(&mut o, 6), class(&mut o, 7));
        let min = ExprId(o.classes.intern(ClassExpr::Min(256, s, c)));
        let outer = ExprId(o.classes.intern(ClassExpr::Some(r, min)));
        add(&mut o, Axiom::ClassAssertion(outer, a));
        add(&mut o, Axiom::ObjectPropertyAssertion(4, b, a));
        let some_s = ExprId(o.classes.intern(ClassExpr::Some(s, c)));
        add(&mut o, Axiom::SubClassOf(some_s, d));
        let some_r = ExprId(o.classes.intern(ClassExpr::Some(r, d)));
        add(&mut o, Axiom::SubClassOf(some_r, e));
        if merged {
            let some_t = ExprId(o.classes.intern(ClassExpr::Some(t, e)));
            let only_a = ExprId(o.classes.intern(ClassExpr::OneOf(vec![a])));
            add(&mut o, Axiom::SubClassOf(some_t, only_a));
        }
        for k in 0..64 {
            let label = class(&mut o, 1000 + k);
            add(&mut o, Axiom::ClassAssertion(label, b));
        }
        facts_asserted(&o, &format!("merged: {merged}"))
    };
    let (apart, merged) = (asserted(false), asserted(true));
    // x's 256 successors and their inequalities, built again, are about 33,000 facts.
    assert!(
        merged.saturating_sub(apart) <= 1000,
        "SPEC merge-subtree: {apart} facts without the merge, {merged} with it"
    );
}

/// A side's successors aren't what moving it costs: they may never be built again. `a` holds
/// `≥256 R.C` and its 256 successors, `∃R.C ⊑ {b}` merges it with `b`, and `b` has R to 256
/// different named individuals in C, which meet the copied `≥256 R.C`. Moving `a` copies its
/// 17 labels and builds nothing; weighing a's edges to its successors moved `b`, copying its 256
/// edges. The merge adds about a's labels to what the check holds without it.
#[test]
fn a_merge_does_not_weigh_successors_that_need_no_building_again() {
    let _turn = PROFILE.lock().unwrap_or_else(PoisonError::into_inner);
    let asserted = |merged: bool| {
        let mut o = Ontology::default();
        let (a, b) = (100, 101);
        let r = ObjProp::Named(2);
        let c = class(&mut o, 3);
        let min = ExprId(o.classes.intern(ClassExpr::Min(256, r, c)));
        add(&mut o, Axiom::ClassAssertion(min, a));
        for k in 0..16 {
            let label = class(&mut o, 2000 + k);
            add(&mut o, Axiom::ClassAssertion(label, a));
        }
        let named: Vec<u64> = (1000..1256).collect();
        for &z in &named {
            add(&mut o, Axiom::ObjectPropertyAssertion(2, b, z));
            add(&mut o, Axiom::ClassAssertion(c, z));
        }
        add(&mut o, Axiom::DifferentIndividuals(named));
        if merged {
            let some = ExprId(o.classes.intern(ClassExpr::Some(r, c)));
            let only_b = ExprId(o.classes.intern(ClassExpr::OneOf(vec![b])));
            add(&mut o, Axiom::SubClassOf(some, only_b));
        }
        facts_asserted(&o, &format!("merged: {merged}"))
    };
    let (apart, merged) = (asserted(false), asserted(true));
    assert!(
        merged.saturating_sub(apart) <= 64,
        "SPEC merge-weigh: {apart} facts without the merge, {merged} with it"
    );
}

/// PR #72 r2 sol-loop S1: a fact both sides hold keeps one side's dependencies. `t` holds an
/// asserted `C`, `s` holds `C` by choosing it from `C ⊔ B`; `u`'s 256 successors then make
/// `s ≈ t` (by nothing chosen), and `K(s) ⊓ L(t)` gives the merged node `¬C`. Moving `s` into
/// `t` keeps the asserted `C`: the clash depends on nothing, inconsistent at once. Moving `t`
/// into `s` kept the chosen one: the clash went back to the choice, cut u's successors and
/// built them again. The check builds them once.
#[test]
fn a_merge_keeps_the_dependencies_of_a_fact_both_sides_hold() {
    let _turn = PROFILE.lock().unwrap_or_else(PoisonError::into_inner);
    let mut o = Ontology::default();
    let (s, t, u) = (100, 101, 102);
    let (q, r) = (ObjProp::Named(3), ObjProp::Named(4));
    let c = class(&mut o, 5);
    let b = class(&mut o, 6);
    let (k, l, d, e) = (
        class(&mut o, 7),
        class(&mut o, 8),
        class(&mut o, 9),
        class(&mut o, 10),
    );
    add(&mut o, Axiom::ClassAssertion(c, t));
    add(&mut o, Axiom::ClassAssertion(l, t));
    let either = ExprId(o.classes.intern(ClassExpr::Or(vec![c, b])));
    add(&mut o, Axiom::ClassAssertion(either, s));
    add(&mut o, Axiom::ClassAssertion(k, s));
    for i in 0..16 {
        let label = class(&mut o, 2000 + i);
        add(&mut o, Axiom::ClassAssertion(label, s));
    }
    let min = ExprId(o.classes.intern(ClassExpr::Min(256, r, d)));
    add(&mut o, Axiom::ClassAssertion(min, u));
    add(&mut o, Axiom::ObjectPropertyAssertion(3, s, u));
    let some_r = ExprId(o.classes.intern(ClassExpr::Some(r, d)));
    add(&mut o, Axiom::SubClassOf(some_r, e));
    let some_q = ExprId(o.classes.intern(ClassExpr::Some(q, e)));
    let only_t = ExprId(o.classes.intern(ClassExpr::OneOf(vec![t])));
    add(&mut o, Axiom::SubClassOf(some_q, only_t));
    let both = ExprId(o.classes.intern(ClassExpr::And(vec![k, l])));
    let not_c = ExprId(o.classes.intern(ClassExpr::Not(c)));
    add(&mut o, Axiom::SubClassOf(both, not_c));
    let config = Config {
        portfolio: false,
        keep_model: true,
        counting: false,
        disjunctions_first: true,
        disjunct_learning: false,
        ..Config::default()
    };
    let outcome = consistency(&o, &config);
    assert_eq!(outcome.answer, Answer::Inconsistent, "C and ¬C on s ≈ t");
    let built = outcome.telemetry.nodes_created;
    // Three named nodes and u's 256 successors, once; again after a backtrack, 515.
    assert!(
        built < 400,
        "SPEC merge-deps: {built} nodes created for u's 256 successors"
    );
}

/// PR #72's fix, Sol r2 S1: choosing a merge's direction read each side up to its next copied
/// fact, so the entries before it that aren't copied were read at every merge: Θ(N·M) for M
/// merges into a node holding N such entries. `t` holds `R(t, t)` and then `R(t, bᵢ)` for N
/// named individuals, each merged into it, which leaves N edges to merged nodes ahead of its
/// one live edge; then M individuals without facts join it, half named first, half second. What
/// those M merges read doesn't grow with N.
#[test]
fn choosing_a_merge_s_direction_reads_no_more_for_a_longer_list() {
    let _turn = PROFILE.lock().unwrap_or_else(PoisonError::into_inner);
    let read = |n: u64, m: u64| {
        let mut o = Ontology::default();
        let t = 10;
        add(&mut o, Axiom::ObjectPropertyAssertion(2, t, t));
        for b in 1000..1000 + n {
            add(&mut o, Axiom::ObjectPropertyAssertion(2, t, b));
            add(&mut o, Axiom::SameIndividual(vec![b, t]));
        }
        for s in 100_000..100_000 + m {
            let pair = if s % 2 == 0 { vec![s, t] } else { vec![t, s] };
            add(&mut o, Axiom::SameIndividual(pair));
        }
        let config = Config {
            portfolio: false,
            keep_model: true,
            ..Config::default()
        };
        let outcome = consistency(&o, &config);
        assert_eq!(outcome.answer, Answer::Consistent, "{n} edges, {m} joining");
        outcome.telemetry.merge_walk
    };
    let joining = |n: u64| read(n, 64) - read(n, 0);
    let (short, long) = (joining(2048), joining(8192));
    // Read up to each copied fact, the joining merges read about N each: four times as much.
    assert!(
        long <= short + short / 2,
        "SPEC merge-read: 64 merges read {short} entries beside 2048 edges, {long} beside 8192"
    );
}
