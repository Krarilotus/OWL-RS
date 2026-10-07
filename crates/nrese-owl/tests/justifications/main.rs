//! Justifications on the proof IR against a Horn MUS enumerator (docs/design/owl2-dl.md
//! §10, the gate of work package 2.4): on random derivation hypergraphs, the resolution
//! enumeration finds exactly the minimal axiom sets a brute force over every subset finds,
//! smallest first; `one` is one of them, `core` their intersection, `union` their union;
//! every proof checks, and a broken one doesn't.

use std::collections::BTreeSet;

use nrese_owl::{ProofError, ProofGraph};

fn rng(seed: u64) -> impl FnMut(u64) -> u64 {
    let mut state = seed;
    move |n: u64| {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) % n.max(1)
    }
}

/// Facts `0..facts` (the goal is 0), axioms `0..axioms`, random inferences (cycles too).
fn graph(
    next: &mut dyn FnMut(u64) -> u64,
    facts: u32,
    axioms: u32,
    count: usize,
) -> ProofGraph<u32, u32> {
    let mut g = ProofGraph::new(0u32);
    for _ in 0..count {
        let conclusion = next(facts as u64) as u32;
        let premises: Vec<u32> = (0..next(3)).map(|_| next(facts as u64) as u32).collect();
        let used: Vec<u32> = (0..next(3)).map(|_| next(axioms as u64) as u32).collect();
        g.add(format!("r{}", next(4)), &premises, &used, conclusion);
    }
    g
}

/// Every minimal axiom set deriving the goal, by brute force over subsets. Deriving is
/// monotone, so a set is minimal iff no set with one axiom fewer derives (comparing every
/// pair of deriving sets made this test take 13 s).
fn brute(g: &ProofGraph<u32, u32>, axioms: u32) -> Vec<Vec<u32>> {
    let derives: Vec<bool> = (0u32..(1 << axioms))
        .map(|mask| g.derivable(&|a| mask & (1 << a) != 0))
        .collect();
    let minimal: Vec<u32> = (0u32..(1 << axioms))
        .filter(|&m| {
            derives[m as usize]
                && (0..axioms).all(|a| m & (1 << a) == 0 || !derives[(m & !(1 << a)) as usize])
        })
        .collect();
    let mut out: Vec<Vec<u32>> = minimal
        .into_iter()
        .map(|m| (0..axioms).filter(|a| m & (1 << a) != 0).collect())
        .collect();
    out.sort();
    out
}

#[test]
fn the_enumeration_equals_a_brute_force_mus_enumerator() {
    let cases = std::env::var("NRESE_FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000u64);
    let mut next = rng(0x2026_1003_0240);
    let (mut entailed, mut total, mut most) = (0, 0usize, 0usize);
    for case in 0..cases {
        let (facts, axioms) = (2 + next(8) as u32, 1 + next(12) as u32);
        let count = 2 + next(40) as usize;
        let g = graph(&mut next, facts, axioms, count);
        let expected = brute(&g, axioms);
        let got = g.justifications(usize::MAX, usize::MAX);
        assert!(got.complete);
        // Smallest first.
        assert!(
            got.found.windows(2).all(|w| w[0].len() <= w[1].len()),
            "case {case}: {:?}",
            got.found
        );
        let mut sorted = got.found.clone();
        sorted.sort();
        assert_eq!(sorted, expected, "case {case}: {g:?}");
        total += expected.len();
        most = most.max(expected.len());
        // One, core, union.
        match g.one() {
            Some(one) => {
                entailed += 1;
                assert!(
                    expected.contains(&one),
                    "case {case}: one {one:?} of {expected:?}"
                );
            }
            None => assert!(expected.is_empty()),
        }
        let core: BTreeSet<u32> = expected
            .iter()
            .map(|j| j.iter().copied().collect::<BTreeSet<u32>>())
            .reduce(|a, b| a.intersection(&b).copied().collect())
            .unwrap_or_default();
        assert_eq!(
            g.core(),
            core.into_iter().collect::<Vec<_>>(),
            "case {case}"
        );
        let union: BTreeSet<u32> = expected.iter().flatten().copied().collect();
        let (got_union, complete) = g.union(usize::MAX);
        assert!(complete);
        assert_eq!(
            got_union,
            union.into_iter().collect::<Vec<_>>(),
            "case {case}"
        );
        // Top-k: the k smallest.
        if expected.len() > 1 {
            let top = g.justifications(1, usize::MAX);
            assert_eq!(top.found.len(), 1);
            assert_eq!(
                top.found[0].len(),
                expected.iter().map(Vec::len).min().unwrap()
            );
        }
        // Every justification has a proof from it that checks.
        for j in &expected {
            let proof = g
                .proof_from(&|a| j.contains(a))
                .expect("a justification derives the goal");
            assert_eq!(proof.check(&|_| true), Ok(()), "case {case}");
            assert!(proof.axioms().iter().all(|a| j.contains(a)));
            // Without its first step, a premise or the goal is missing.
            if proof.steps.len() > 1 {
                let mut broken = proof.clone();
                broken.steps.remove(0);
                assert!(broken.check(&|_| true).is_err());
            }
            assert!(matches!(
                proof.check(&|_| false),
                Err(ProofError::InvalidStep { step: 0 })
            ));
        }
    }
    eprintln!(
        "{cases} graphs, {entailed} entailed, {total} justifications, at most {most} for one goal"
    );
    assert!(entailed > cases / 4);
}

#[test]
fn budgets_and_limits_report_incompleteness() {
    // The goal from any one of 12 axioms, or from pairs: many justifications.
    let mut g = ProofGraph::new(0u32);
    for a in 0..12u32 {
        g.add("single", &[], &[a], 0);
    }
    let all = g.justifications(usize::MAX, usize::MAX);
    assert_eq!(all.found.len(), 12);
    assert!(all.complete);
    let some = g.justifications(3, usize::MAX);
    assert_eq!(some.found.len(), 3);
    assert!(!some.complete);
    let starved = g.justifications(usize::MAX, 4);
    assert!(!starved.complete);
    assert!(starved.found.len() < 12);
    // A graph its engine didn't finish collecting is never complete.
    g.complete = false;
    assert!(!g.justifications(usize::MAX, usize::MAX).complete);
}
