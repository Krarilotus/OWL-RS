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
use nrese_owl::{Axiom, ClassExpr, ExprId, Ontology, Term};

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
        max_nodes: 200,
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
        max_nodes: 2,
        timeout: Some(std::time::Duration::from_secs(60)),
        ..Config::default()
    };
    for clash in [false, true] {
        let answer = islands::consistency(&chain(400, clash), &config).answer;
        assert!(matches!(answer, Answer::GaveUp(_)), "{answer:?}");
    }
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
        max_nodes: 20_000,
        max_branch_points: Some(20_000),
        timeout: Some(std::time::Duration::from_secs(60)),
        ..Config::default()
    };
    let (mut split, mut compared, mut inconsistent) = (0u64, 0u64, 0u64);
    let mut failures = Vec::new();
    for case in 0..cases {
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
        "{cases} cases: {split} split, {compared} decided by both, {inconsistent} inconsistent"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // The test exercises the islands: most cases split, both verdicts occur.
    assert!(split * 2 >= cases, "only {split} of {cases} cases split");
    assert!(
        compared * 4 >= cases * 3,
        "only {compared} of {cases} decided by both"
    );
    assert!(
        inconsistent > 0 && inconsistent < compared,
        "{inconsistent} of {compared} inconsistent: both verdicts must occur"
    );
}
