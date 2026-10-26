//! ABox islands (`nrese_dl::islands`) against the whole ABox, on random SROIQ(D) TBoxes
//! (`nrese_owl::fuzz`, nominals in a third of them) with random ABoxes over a dozen
//! individuals: class assertions (named and the TBox's own complex classes), role
//! assertions, `sameAs`, `differentFrom`, negative role assertions and data values. Where
//! both decide, the verdicts must be the same; the test also counts the cases that split,
//! so it can't pass by deciding everything whole.
//!
//! `NRESE_FUZZ_CASES` and `NRESE_FUZZ_SEED` widen or move a campaign.

use std::collections::HashMap;

use nrese_dl::islands::{self, Split};
use nrese_dl::tableau::{self, Answer, Config};
use nrese_owl::fuzz::{self, Name, Profile, Rng, Signature, Sizes};
use nrese_owl::{Axiom, Characteristic, ClassExpr, ExprId, ObjProp, Ontology, Term};

fn env(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

fn pick<T: Copy>(rng: &mut Rng, items: &[T]) -> T {
    items[rng.below(items.len() as u64) as usize]
}

/// Adds axioms that carry information along role edges, so that an island split where it
/// mustn't changes the verdict: a universal, an existential on the left, and a
/// disjointness they can meet; in some cases a functional or an asymmetric role.
fn propagation(rng: &mut Rng, o: &mut Ontology, sig: &Signature) {
    let class = |o: &mut Ontology, rng: &mut Rng| {
        ExprId(o.classes.intern(ClassExpr::Class(pick(rng, &sig.classes))))
    };
    let role = |rng: &mut Rng| nrese_owl::ObjProp::Named(pick(rng, &sig.object_properties));
    let (a, b, c, d) = (class(o, rng), class(o, rng), class(o, rng), class(o, rng));
    let all = ExprId(o.classes.intern(ClassExpr::All(role(rng), b)));
    let some = ExprId(o.classes.intern(ClassExpr::Some(role(rng), c)));
    o.axioms.push(Axiom::SubClassOf(a, all));
    o.axioms.push(Axiom::SubClassOf(some, d));
    let mut disjoint = vec![b, d];
    disjoint.sort_unstable();
    disjoint.dedup();
    if disjoint.len() == 2 {
        o.axioms.push(Axiom::DisjointClasses(disjoint));
    }
    match rng.below(4) {
        0 => o.axioms.push(Axiom::ObjectCharacteristic(
            nrese_owl::Characteristic::Functional,
            role(rng),
        )),
        1 => o.axioms.push(Axiom::ObjectCharacteristic(
            nrese_owl::Characteristic::Asymmetric,
            nrese_owl::ObjProp::Named(sig.object_properties[0]),
        )),
        // A chain over the non-simple property: with a negative assertion over it, edges
        // carry information though no clause of the TBox reads them (the tableau reads
        // the negative assertion as a universal).
        2 => {
            let r = nrese_owl::ObjProp::Named(sig.object_properties[sig.simple]);
            o.axioms.push(Axiom::SubObjectPropertyOf(vec![r, r], r));
        }
        _ => {}
    }
}

/// Adds a random ABox over `sig`'s individuals to `o`.
fn abox(rng: &mut Rng, o: &mut Ontology, sig: &Signature) {
    let complex: Vec<ExprId> = (0..o.classes.len() as u32)
        .filter(|&i| {
            !matches!(
                o.classes.get(i),
                ClassExpr::Class(_) | ClassExpr::Thing | ClassExpr::Nothing
            )
        })
        .map(ExprId)
        .collect();
    let named: Vec<ExprId> = sig
        .classes
        .iter()
        .map(|&c| ExprId(o.classes.intern(ClassExpr::Class(c))))
        .collect();
    let people = &sig.individuals;
    let pair = |rng: &mut Rng| {
        let a = pick(rng, people);
        let mut b = pick(rng, people);
        while b == a {
            b = pick(rng, people);
        }
        let mut v = vec![a, b];
        v.sort_unstable();
        v
    };
    let n = people.len() as u64;
    for _ in 0..n + rng.below(2 * n) {
        let axiom = match rng.below(20) {
            0..=4 => Axiom::ClassAssertion(pick(rng, &named), pick(rng, people)),
            5 if !complex.is_empty() => {
                Axiom::ClassAssertion(pick(rng, &complex), pick(rng, people))
            }
            5..=14 => Axiom::ObjectPropertyAssertion(
                pick(rng, &sig.object_properties),
                pick(rng, people),
                pick(rng, people),
            ),
            15 => Axiom::SameIndividual(pair(rng)),
            16 => Axiom::DifferentIndividuals(pair(rng)),
            17 => {
                let ab = pair(rng);
                Axiom::NegativeObjectPropertyAssertion(
                    pick(rng, &sig.object_properties),
                    ab[0],
                    ab[1],
                )
            }
            _ if !sig.data_properties.is_empty() && !sig.literals.is_empty() => {
                Axiom::DataPropertyAssertion(
                    pick(rng, &sig.data_properties),
                    pick(rng, people),
                    pick(rng, &sig.literals),
                )
            }
            _ => continue,
        };
        o.axioms.push(axiom);
    }
    // A path of two edges over the non-simple property and, often, a negative assertion
    // across it: inconsistent where the property is a chain of itself.
    if rng.one_in(3) {
        let r = sig.object_properties[sig.simple];
        let (a, b, c) = (people[0], people[1], people[2]);
        o.axioms.push(Axiom::ObjectPropertyAssertion(r, a, b));
        o.axioms.push(Axiom::ObjectPropertyAssertion(r, b, c));
        if rng.one_in(2) {
            o.axioms
                .push(Axiom::NegativeObjectPropertyAssertion(r, a, c));
        }
    }
    o.axioms.sort();
    o.axioms.dedup();
    o.sources = vec![Vec::new(); o.axioms.len()];
}

/// A chain of `n` individuals over a role nothing reads across, each in `A ⊑ B`; with
/// `clash`, one individual is also in `C`, disjoint from `B`.
fn chain(n: u64, clash: bool) -> Ontology {
    let (a, b, c, r) = (1, 2, 3, 10);
    let mut o = Ontology::default();
    let class = |o: &mut Ontology, t: Term| ExprId(o.classes.intern(ClassExpr::Class(t)));
    let (ca, cb, cc) = (class(&mut o, a), class(&mut o, b), class(&mut o, c));
    o.axioms.push(Axiom::SubClassOf(ca, cb));
    o.axioms.push(Axiom::DisjointClasses(vec![cb, cc]));
    for i in 0..n {
        o.axioms.push(Axiom::ClassAssertion(ca, 100 + i));
        if i + 1 < n {
            o.axioms
                .push(Axiom::ObjectPropertyAssertion(r, 100 + i, 101 + i));
        }
    }
    if clash {
        o.axioms.push(Axiom::ClassAssertion(cc, 100 + n / 2));
    }
    o.axioms.sort();
    o.axioms.dedup();
    o.sources = vec![Vec::new(); o.axioms.len()];
    o
}

/// The whole ABox first; where it gives up within its budget, the islands decide.
#[test]
fn islands_decide_where_the_whole_abox_gives_up() {
    let config = Config {
        max_nodes: Some(200),
        timeout: Some(std::time::Duration::from_secs(60)),
        ..Config::default()
    };
    for (clash, want) in [(false, Answer::Consistent), (true, Answer::Inconsistent)] {
        let o = chain(400, clash);
        let whole = tableau::consistency(&o, &config);
        assert!(
            matches!(whole.answer, Answer::GaveUp(_)),
            "400 individuals exceed 200 nodes: {:?}",
            whole.answer
        );
        assert_eq!(islands::consistency(&o, &config).answer, want);
    }
}

/// A budget neither the whole ABox nor a batch of islands fits ends undecided (the store
/// then says the data's consistency is unknown), never in a verdict.
#[test]
fn a_spent_budget_leaves_the_islands_undecided() {
    let config = Config {
        max_nodes: Some(2),
        timeout: Some(std::time::Duration::from_secs(60)),
        ..Config::default()
    };
    for clash in [false, true] {
        let answer = islands::consistency(&chain(400, clash), &config).answer;
        assert!(matches!(answer, Answer::GaveUp(_)), "{answer:?}");
    }
}

#[test]
fn shared_island_limits_preserve_verdicts_and_never_raise_tiny_memory() {
    let physical = nrese_exec::workers::Workers::new(4);
    for width in [1, 2, 4] {
        let config = Config {
            workers: Some(physical.limited(width)),
            max_nodes: Some(200),
            max_memory: None,
            keep_model: true,
            ..Config::default()
        };
        for (clash, want) in [(false, Answer::Consistent), (true, Answer::Inconsistent)] {
            let ontology = chain(400, clash);
            assert!(matches!(
                tableau::consistency(&ontology, &config).answer,
                Answer::GaveUp(_)
            ));
            assert_eq!(islands::by_islands(&ontology, &config).answer, want);
            assert_eq!(islands::consistency(&ontology, &config).answer, want);
        }
        let tiny = Config {
            max_memory: Some(1),
            ..config
        };
        let out = islands::by_islands(&chain(400, false), &tiny);
        assert!(matches!(out.answer, Answer::GaveUp(_)), "{:?}", out.answer);
    }
}

#[test]
fn stopped_island_requests_do_not_split_or_start_search() {
    let cancel = tableau::Cancel::default();
    cancel.cancel();
    for config in [
        Config {
            cancel: Some(cancel),
            ..Config::default()
        },
        Config {
            timeout: Some(std::time::Duration::ZERO),
            ..Config::default()
        },
    ] {
        for out in [
            islands::by_islands(&chain(400, false), &config),
            islands::consistency(&chain(400, false), &config),
        ] {
            assert!(matches!(out.answer, Answer::GaveUp(_)));
            assert_eq!(out.telemetry.compile, std::time::Duration::ZERO);
            assert_eq!(out.telemetry.peak_nodes, 0);
        }
    }
}

/// A key's data property `d` has values beyond its own assertions (OWL 2 Direct
/// Semantics, keys, over the property's extension): a sub-property's, also two inclusions
/// down, or one a class gives. Under `HasKey(⊤ () (d))` two individuals with the value 0
/// so are one, which `A(a)`, `B(b)` with `A`, `B` disjoint contradicts: inconsistent,
/// though each alone is consistent and fills a batch of its own. The same holds for a key
/// over a transitive `R` (PR #33 review, R1) with `R(a, a)`, `R(b, a)`: `a` is a named
/// `R`-value of both, alone or with `d`; and for a key of no property, which makes every
/// named member of its class one (`C(a)`, `C(b)`). Without `b`'s last assertion the ontology is
/// consistent (`b` has no value), so the touched islands of adding it say inconsistent or
/// leave it to the whole ABox.
#[test]
fn key_values_the_tbox_gives_keep_their_individuals_together() {
    let (a, b, d, e, f, r, zero) = (1, 2, 10, 11, 12, 13, 20);
    let mut base = Ontology::default();
    let class = |o: &mut Ontology, t: Term| ExprId(o.classes.intern(ClassExpr::Class(t)));
    let (ca, cb, cc) = (
        class(&mut base, 3),
        class(&mut base, 4),
        class(&mut base, 5),
    );
    let thing = ExprId(base.classes.intern(ClassExpr::Thing));
    let zero_of_d = ExprId(base.classes.intern(ClassExpr::DataHasValue(d, zero)));
    base.data.literals.insert(
        zero,
        nrese_owl::Literal {
            lexical: "0".into(),
            datatype: Some("http://www.w3.org/2001/XMLSchema#integer".into()),
            language: None,
        },
    );
    let data_key = Axiom::HasKey(thing, Vec::new(), vec![d]);
    let transitive = Axiom::ObjectCharacteristic(Characteristic::Transitive, ObjProp::Named(r));
    let edge = |x: Term, y: Term| Axiom::ObjectPropertyAssertion(r, x, y);
    base.axioms.extend([
        Axiom::DisjointClasses(vec![ca, cb]),
        Axiom::ClassAssertion(ca, a),
        Axiom::ClassAssertion(cb, b),
    ]);
    // Classes nothing reads, so that each individual's island fills a batch.
    let batch = islands::BATCH as Term;
    for i in 0..batch {
        let (p, q) = (
            class(&mut base, 1000 + i),
            class(&mut base, 1000 + batch + i),
        );
        base.axioms.push(Axiom::ClassAssertion(p, a));
        base.axioms.push(Axiom::ClassAssertion(q, b));
    }
    let zero_at = |p: Term, x: Term| Axiom::DataPropertyAssertion(p, x, zero);
    let config = Config {
        timeout: Some(std::time::Duration::from_secs(60)),
        ..Config::default()
    };
    let mut failures = Vec::new();
    for (case, tbox, at_a, at_b) in [
        (
            "e ⊑ d",
            vec![data_key.clone(), Axiom::SubDataPropertyOf(e, d)],
            zero_at(e, a),
            zero_at(e, b),
        ),
        (
            "f ⊑ e ⊑ d",
            vec![
                data_key.clone(),
                Axiom::SubDataPropertyOf(f, e),
                Axiom::SubDataPropertyOf(e, d),
            ],
            zero_at(f, a),
            zero_at(d, b),
        ),
        (
            "C ⊑ ∃d.{0}",
            vec![data_key.clone(), Axiom::SubClassOf(cc, zero_of_d)],
            Axiom::ClassAssertion(cc, a),
            Axiom::ClassAssertion(cc, b),
        ),
        (
            "HasKey(⊤ (R) (d)), R transitive",
            vec![
                Axiom::HasKey(thing, vec![ObjProp::Named(r)], vec![d]),
                transitive.clone(),
                edge(a, a),
                edge(b, a),
            ],
            zero_at(d, a),
            zero_at(d, b),
        ),
        (
            "HasKey(⊤ (R) ()), R transitive",
            vec![
                Axiom::HasKey(thing, vec![ObjProp::Named(r)], Vec::new()),
                transitive.clone(),
            ],
            edge(a, a),
            edge(b, a),
        ),
        (
            "HasKey(C () ())",
            vec![Axiom::HasKey(cc, Vec::new(), Vec::new())],
            Axiom::ClassAssertion(cc, a),
            Axiom::ClassAssertion(cc, b),
        ),
    ] {
        let mut o = base.clone();
        o.axioms.extend(tbox);
        o.axioms.extend([at_a, at_b]);
        o.axioms.sort();
        o.axioms.dedup();
        o.sources = vec![Vec::new(); o.axioms.len()];
        let answer = islands::by_islands(&o, &config).answer;
        if answer != Answer::Inconsistent {
            failures.push(format!("{case}: by islands {answer:?}"));
        }
        let touched = std::collections::HashSet::from([b]);
        let answer = islands::after_assertions(&o, &touched, &config).map(|out| out.answer);
        if !matches!(answer, None | Some(Answer::Inconsistent)) {
            failures.push(format!("{case}: the touched islands {answer:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn decided(answer: &Answer) -> Option<bool> {
    match answer {
        Answer::Consistent => Some(true),
        Answer::Inconsistent => Some(false),
        _ => None,
    }
}

#[test]
fn islands_decide_as_the_whole_abox_does() {
    let cases = env("NRESE_FUZZ_CASES").unwrap_or(240);
    let seed = env("NRESE_FUZZ_SEED").unwrap_or(0x2026_1006_1500);
    let config = Config {
        max_nodes: Some(20_000),
        max_branch_points: Some(20_000),
        timeout: Some(std::time::Duration::from_secs(60)),
        ..Config::default()
    };
    let (mut split, mut compared, mut inconsistent) = (0u64, 0u64, 0u64);
    let mut failures = Vec::new();
    // Most drawn cases split, three quarters are decided by both, both verdicts occur.
    let covered = |drawn: u64, split: u64, compared: u64, inconsistent: u64| {
        split * 2 >= drawn
            && compared * 4 >= drawn * 3
            && inconsistent > 0
            && inconsistent < compared
    };
    // Draw on (up to ten times as many cases) until the run covers that, so that coverage
    // never depends on the seed (the NI fuzz test's fix, a901e34).
    let mut drawn = 0;
    for case in 0..cases * 10 {
        if case >= cases && covered(drawn, split, compared, inconsistent) {
            break;
        }
        drawn += 1;
        let mut rng = Rng::new(seed.wrapping_add(case));
        let mut names: HashMap<String, Term> = HashMap::new();
        let mut intern = |n: &Name| {
            let key = format!("{n:?}");
            let len = names.len() as Term;
            *names.entry(key).or_insert(len)
        };
        let sizes = Sizes {
            classes: 4,
            object_properties: 4,
            simple: 3,
            data_properties: 1,
            individuals: 12,
            literals: 3,
        };
        let sig = Signature::new(sizes, &mut intern);
        let profile = Profile {
            axioms: 3 + (case % 6) as usize,
            nominals: case.is_multiple_of(3),
            abox: false,
            ..Profile::sroiq()
        };
        let mut o = fuzz::ontology(&mut rng, &sig, profile);
        propagation(&mut rng, &mut o, &sig);
        abox(&mut rng, &mut o, &sig);
        if let Split::Islands(_) = islands::split(&o) {
            split += 1;
        }
        let whole = tableau::consistency(&o, &config);
        let parts = islands::by_islands(&o, &config);
        if let (Some(w), Some(p)) = (decided(&whole.answer), decided(&parts.answer)) {
            compared += 1;
            inconsistent += u64::from(!w);
            if w != p {
                failures.push(format!(
                    "case {case} (seed {seed}): the whole ABox is {}, the islands say {}",
                    if w { "consistent" } else { "inconsistent" },
                    if p { "consistent" } else { "inconsistent" },
                ));
            }
        }
    }
    eprintln!(
        "{drawn} cases: {split} split, {compared} decided by both, {inconsistent} inconsistent"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // The test exercises the islands.
    assert!(
        covered(drawn, split, compared, inconsistent),
        "{drawn} cases: {split} split, {compared} decided by both, {inconsistent} inconsistent"
    );
}

/// The incremental path (docs/design/owl2-dl.md §9): after a change that removes
/// assertions and adds some about a few individuals, deciding only the islands with one
/// of them gives what a cold check of the whole ontology gives. Counts the cases the
/// incremental path decided, so the test can't pass by falling back.
///
/// A case reaches the path only from a consistent ontology, and about three quarters of
/// the generator's are not (inconsistent or over the budget): that share is the
/// generator's, not the path's. So the coverage is counted against the cases that reach
/// it (`eligible`): the path decides at least half of them (over 1,000 seeds at least 69 %,
/// the rest the declines below), every decline is one the module documents (the ABox one
/// island, a nominal in a conclusion, the universal property), and cases are drawn until
/// a fifth of all drawn were decided incrementally (up to ten times as many), so the
/// agreement rests on as many incremental cases whatever the seed (seeds 600006, 600012,
/// 600055, 600061 landed 45 to 47 of 240 short of the fixed floor of 48).
#[test]
fn touched_islands_decide_as_a_cold_check_does() {
    let cases = env("NRESE_FUZZ_CASES").unwrap_or(240);
    let seed = env("NRESE_FUZZ_SEED").unwrap_or(0x2026_1012_1700);
    let config = Config {
        max_nodes: Some(20_000),
        max_branch_points: Some(20_000),
        timeout: Some(std::time::Duration::from_secs(60)),
        ..Config::default()
    };
    let (mut incremental, mut inconsistent, mut eligible) = (0u64, 0u64, 0u64);
    let mut failures = Vec::new();
    let documented = [
        "the ABox is one island",
        "a nominal in a conclusion",
        "the ontology uses the universal property",
    ];
    let covered = |drawn: u64, incremental: u64, eligible: u64, inconsistent: u64| {
        incremental * 5 >= drawn && incremental * 2 >= eligible && inconsistent > 0
    };
    let mut drawn = 0;
    for case in 0..cases * 10 {
        if case >= cases && covered(drawn, incremental, eligible, inconsistent) {
            break;
        }
        drawn += 1;
        let mut rng = Rng::new(seed.wrapping_add(case));
        let mut names: HashMap<String, Term> = HashMap::new();
        let mut intern = |n: &Name| {
            let key = format!("{n:?}");
            let len = names.len() as Term;
            *names.entry(key).or_insert(len)
        };
        let sizes = Sizes {
            classes: 4,
            object_properties: 4,
            simple: 3,
            data_properties: 1,
            individuals: 12,
            literals: 3,
        };
        let sig = Signature::new(sizes, &mut intern);
        let profile = Profile {
            axioms: 3 + (case % 6) as usize,
            nominals: case.is_multiple_of(3),
            abox: false,
            ..Profile::sroiq()
        };
        let mut before = fuzz::ontology(&mut rng, &sig, profile);
        propagation(&mut rng, &mut before, &sig);
        abox(&mut rng, &mut before, &sig);
        if decided(&tableau::consistency(&before, &config).answer) != Some(true) {
            continue;
        }
        eligible += 1;
        // The change: some assertions removed, a few added (another ABox's).
        let mut after = before.clone();
        let assertions: Vec<usize> = (0..after.axioms.len())
            .filter(|&i| after.axioms[i].is_assertion())
            .collect();
        for &i in assertions.iter().rev() {
            if rng.one_in(4) {
                after.axioms.remove(i);
            }
        }
        let mut more = after.clone();
        more.axioms.retain(|a| !a.is_assertion());
        abox(&mut rng, &mut more, &sig);
        let added: Vec<Axiom> = more
            .axioms
            .into_iter()
            .filter(|a| a.is_assertion())
            .take(1 + rng.below(3) as usize)
            .collect();
        let mut touched = std::collections::HashSet::new();
        for a in &added {
            match a {
                Axiom::ClassAssertion(_, x)
                | Axiom::DataPropertyAssertion(_, x, _)
                | Axiom::NegativeDataPropertyAssertion(_, x, _) => {
                    touched.insert(*x);
                }
                Axiom::ObjectPropertyAssertion(_, x, y)
                | Axiom::NegativeObjectPropertyAssertion(_, x, y) => {
                    touched.extend([*x, *y]);
                }
                Axiom::SameIndividual(xs) | Axiom::DifferentIndividuals(xs) => {
                    touched.extend(xs.iter().copied());
                }
                _ => {}
            }
        }
        after.axioms.extend(added);
        after.axioms.sort();
        after.axioms.dedup();
        after.sources = vec![Vec::new(); after.axioms.len()];
        let cold = decided(&tableau::consistency(&after, &config).answer);
        let Some(outcome) = islands::after_assertions(&after, &touched, &config) else {
            // Declined: the ABox is decided whole, for a reason the module documents.
            if let islands::Split::Whole(why) = islands::split(&after)
                && !documented.iter().any(|d| why.starts_with(d))
            {
                failures.push(format!("case {case} (seed {seed}): declined: {why}"));
            }
            continue;
        };
        if let (Some(c), Some(i)) = (cold, decided(&outcome.answer)) {
            incremental += 1;
            inconsistent += u64::from(!c);
            if c != i {
                failures.push(format!(
                    "case {case} (seed {seed}): cold {c}, touched islands {i}"
                ));
            }
        }
    }
    eprintln!(
        "{drawn} cases: {eligible} from a consistent ontology, {incremental} decided \
         incrementally, {inconsistent} inconsistent"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(
        covered(drawn, incremental, eligible, inconsistent),
        "{drawn} cases: {eligible} from a consistent ontology, {incremental} decided \
         incrementally, {inconsistent} inconsistent"
    );
}
