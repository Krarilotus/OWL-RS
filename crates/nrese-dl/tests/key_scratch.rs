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
/// (5.1 GB at 8,000 members). Of two named individuals the one holding fewer of the facts a merge
/// copies goes into the other.
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

/// PR #72's fix, Sol r2 S2: `a : ≥n R.C` makes n successors and their inequalities, and
/// `∃R.C ⊑ {b}` then merges `a` with `b`, which holds 64 labels. Moving `a` prunes its successors
/// and `b` builds them again from the copied `≥n R.C`; moving `b` copies its 64 labels. The merge
/// adds about those labels to what the check holds without it.
#[test]
fn a_merge_keeps_the_side_whose_successors_it_would_build_again() {
    let _turn = PROFILE.lock().unwrap_or_else(PoisonError::into_inner);
    let asserted = |merged: bool| {
        let mut o = Ontology::default();
        let (a, b) = (100, 101);
        let r = ObjProp::Named(2);
        let c = ExprId(o.classes.intern(ClassExpr::Class(3)));
        let min = ExprId(o.classes.intern(ClassExpr::Min(256, r, c)));
        add(&mut o, Axiom::ClassAssertion(min, a));
        if merged {
            let some = ExprId(o.classes.intern(ClassExpr::Some(r, c)));
            let only_b = ExprId(o.classes.intern(ClassExpr::OneOf(vec![b])));
            add(&mut o, Axiom::SubClassOf(some, only_b));
        }
        for k in 0..64 {
            let class = ExprId(o.classes.intern(ClassExpr::Class(1000 + k)));
            add(&mut o, Axiom::ClassAssertion(class, b));
        }
        let config = Config {
            portfolio: false,
            keep_model: true,
            ..Config::default()
        };
        let outcome = consistency(&o, &config);
        assert_eq!(outcome.answer, Answer::Consistent, "merged: {merged}");
        outcome.telemetry.facts
    };
    let (apart, merged) = (asserted(false), asserted(true));
    // a's 256 successors and their inequalities, built again, are about 33,000 facts.
    assert!(
        merged.saturating_sub(apart) <= 1000,
        "SPEC merge-storage: {apart} facts without the merge, {merged} with it"
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
