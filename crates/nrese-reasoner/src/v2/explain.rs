//! Why an inferred fact holds: a derivation of it from asserted facts, step by step, each
//! step naming the rule and the premises it used (G3 of the gap plan; GraphDB's and
//! Stardog's "explain inference").
//!
//! The rules are grounded on the materialised state ([`super::delta::program`]), and the
//! one-step derivations of the fact are collected backwards ([`GroundProgram::named_derivations`]),
//! then those of their inferred premises, up to a budget. A forward pass then proves facts
//! from the asserted ones: a fact is proved by the first derivation whose premises are all
//! proved, so the proof is well-founded (no fact is its own premise) however cyclic the
//! derivations are. The fact's explanation is the proof of it, shared premises once.

use hashbrown::HashMap;

use super::delta::{Base, Rules};
use super::eval::{GroundProgram, Seg, Source};
use super::naive::Triple;

/// One step of an [`Explanation`]: a fact, how it holds, and its premises (indexes of
/// steps).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub fact: Triple,
    /// The rule that derives it; `None` for an asserted fact, `Some("axiom")` for a fact
    /// of the ruleset or a rule instance without a body.
    pub rule: Option<String>,
    pub premises: Vec<usize>,
}

/// A derivation of a fact from asserted facts: [`Step`]s, the fact first, each premise
/// after the steps that use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Explanation {
    pub steps: Vec<Step>,
}

/// Derivations collected per fact.
const BRANCHES: usize = 16;

/// The derivation of `fact` over the materialised state `base` under `rules`, if `fact`
/// holds there; facts examined at most `budget`.
pub fn explain<B: Base + ?Sized>(
    base: &B,
    rules: Rules<'_>,
    fact: Triple,
    budget: usize,
) -> Option<Explanation> {
    if !base.contains(fact) {
        return None;
    }
    let program = super::delta::program(base, rules);
    explain_with(base, &program, fact, budget)
}

/// [`explain`] with the ground program of `base` given.
pub fn explain_with<B: Base + ?Sized>(
    base: &B,
    program: &GroundProgram,
    fact: Triple,
    budget: usize,
) -> Option<Explanation> {
    let source = Whole(base);
    // Backwards: the derivations of every inferred fact reachable from `fact`.
    let mut derivations: HashMap<Triple, Vec<(String, Vec<Triple>)>> = HashMap::new();
    let mut queue = vec![fact];
    while let Some(next) = queue.pop() {
        if derivations.contains_key(&next) || base.is_asserted(next) {
            continue;
        }
        if derivations.len() >= budget {
            break;
        }
        let mut found = program.named_derivations(&source, next, BRANCHES);
        if program.bodiless.contains(&next) {
            found.insert(
                0,
                ("axiom".to_owned(), program.bodiless_premises(next).to_vec()),
            );
        }
        for (_, body) in &found {
            queue.extend(body.iter().copied().filter(|premise| *premise != next));
        }
        derivations.insert(next, found);
    }
    // Forwards, in rounds: a fact is proved in the first round where a derivation of it
    // has every premise proved in an earlier one, so its proof is as shallow as can be;
    // of the derivations usable then, the smallest proof wins (then the rule's name and
    // the premises), so the explanation is the same on every run.
    let mut proved: HashMap<Triple, Proved> = HashMap::new();
    let size = |fact: Triple, proved: &HashMap<Triple, Proved>| -> Option<usize> {
        match proved.get(&fact) {
            Some(entry) => Some(entry.size),
            None => base.is_asserted(fact).then_some(1),
        }
    };
    let mut pending: Vec<Triple> = derivations.keys().copied().collect();
    pending.sort_unstable();
    while !proved.contains_key(&fact) {
        let mut round: Vec<(Triple, Proved)> = Vec::new();
        for &next in &pending {
            let best = derivations[&next]
                .iter()
                .filter_map(|(rule, body)| {
                    let sizes = body
                        .iter()
                        .map(|&premise| match premise == next {
                            true => None,
                            false => size(premise, &proved),
                        })
                        .collect::<Option<Vec<usize>>>()?;
                    Some((1 + sizes.iter().sum::<usize>(), rule, body))
                })
                .min();
            if let Some((size, rule, body)) = best {
                let derivation = Some((rule.clone(), body.clone()));
                round.push((next, Proved { size, derivation }));
            }
        }
        if round.is_empty() {
            break;
        }
        pending.retain(|next| !round.iter().any(|(fact, _)| fact == next));
        proved.extend(round);
    }
    if !proved.contains_key(&fact) && !base.is_asserted(fact) {
        return None;
    }
    // The proof of `fact`, each fact once.
    let mut steps: Vec<Step> = Vec::new();
    let mut index: HashMap<Triple, usize> = HashMap::new();
    fn add(
        fact: Triple,
        proved: &HashMap<Triple, Proved>,
        steps: &mut Vec<Step>,
        index: &mut HashMap<Triple, usize>,
    ) -> usize {
        if let Some(&at) = index.get(&fact) {
            return at;
        }
        let at = steps.len();
        index.insert(fact, at);
        steps.push(Step {
            fact,
            rule: None,
            premises: Vec::new(),
        });
        if let Some(Proved {
            derivation: Some((rule, body)),
            ..
        }) = proved.get(&fact)
        {
            let premises: Vec<usize> = body
                .iter()
                .map(|&premise| add(premise, proved, steps, index))
                .collect();
            steps[at].rule = Some(rule.clone());
            steps[at].premises = premises;
        }
        at
    }
    add(fact, &proved, &mut steps, &mut index);
    Some(Explanation { steps })
}

/// A fact proved: the size of its proof (steps, shared ones counted per use) and how.
struct Proved {
    size: usize,
    derivation: Option<(String, Vec<Triple>)>,
}

/// A [`Base`] as a [`Source`] whose every fact is old.
struct Whole<'a, B: ?Sized>(&'a B);

impl<B: Base + ?Sized> Source for Whole<'_, B> {
    fn scan(&self, pattern: [Option<u64>; 3], seg: Seg, f: &mut dyn FnMut(Triple)) {
        if seg != Seg::Delta {
            self.0.scan(pattern, f);
        }
    }

    fn estimate(&self, pattern: [Option<u64>; 3], seg: Seg) -> usize {
        match seg {
            Seg::Delta => 0,
            _ => self.0.estimate(pattern),
        }
    }

    fn contains(&self, fact: Triple) -> bool {
        self.0.contains(fact)
    }
}
