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

use nrese_owl::ProofGraph;

use super::delta::{Base, Rules};
use super::eval::{GroundProgram, Seg, Source};
use super::ir::Triple;

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

/// One-step derivations collected per fact for a proof graph: enough for every
/// justification of ordinary ontologies; past it the graph says it isn't complete.
const DERIVATIONS: usize = 256;

/// The derivation hypergraph of `fact` over the materialised state `base` (the proof IR of
/// docs/design/owl2-dl.md §10): every one-step derivation of every fact reachable
/// backwards from `fact`, with every set of schema facts each rule was grounded on. The
/// axioms are the asserted facts (and the ruleset's): an asserted fact is an inference of
/// its own (`asserted`) besides its derivations, so a justification may avoid it.
/// Justifications, cores, unions and proofs are computed on it ([`nrese_owl::proof`]).
/// `None` if `fact` doesn't hold; at most `budget` facts examined, past which the graph
/// is marked incomplete.
pub fn proof_graph<B: Base + ?Sized>(
    base: &B,
    program: &GroundProgram,
    fact: Triple,
    budget: usize,
) -> Option<ProofGraph<Triple, Triple>> {
    if !base.contains(fact) {
        return None;
    }
    let source = Whole(base);
    let mut graph = ProofGraph::new(fact);
    let mut seen: hashbrown::HashSet<Triple> = hashbrown::HashSet::new();
    let mut queue = vec![fact];
    while let Some(next) = queue.pop() {
        if !seen.insert(next) {
            continue;
        }
        if seen.len() > budget {
            graph.complete = false;
            break;
        }
        if base.is_asserted(next) {
            graph.add("asserted", &[], &[next], next);
        }
        let mut found = program.every_named_derivation(&source, next, DERIVATIONS);
        if found.len() >= DERIVATIONS {
            graph.complete = false;
        }
        if program.bodiless.contains(&next) {
            for premises in program.premise_alternatives(super::eval::Grounding::Bodiless(next)) {
                found.push(("axiom".to_owned(), premises.to_vec()));
            }
        }
        for (rule, body) in found {
            // Facts of the ruleset hold without premises: axioms as well.
            graph.add(rule, &body, &[], next);
            queue.extend(body.iter().copied().filter(|premise| *premise != next));
        }
    }
    Some(graph)
}

/// Whether `inference` is a valid step over `base`: an asserted fact taken as itself, or
/// an instance of its rule whose body (and schema facts) is exactly its premises,
/// re-derived from the rules (the proof checker's check of a step).
pub fn valid_step<B: Base + ?Sized>(
    base: &B,
    program: &GroundProgram,
    inference: &nrese_owl::Inference<Triple, Triple>,
) -> bool {
    if inference.rule == "asserted" {
        return inference.premises.is_empty()
            && inference.axioms == [inference.conclusion]
            && base.is_asserted(inference.conclusion);
    }
    let same = |body: &[Triple]| {
        let mut body = body.to_vec();
        body.sort_unstable();
        body.dedup();
        body.retain(|premise| *premise != inference.conclusion);
        body == inference.premises
    };
    if inference.rule == "axiom" {
        return program.bodiless.contains(&inference.conclusion)
            && program
                .premise_alternatives(super::eval::Grounding::Bodiless(inference.conclusion))
                .iter()
                .any(|premises| same(premises));
    }
    program
        .every_named_derivation(&Whole(base), inference.conclusion, usize::MAX)
        .iter()
        .any(|(rule, body)| *rule == inference.rule && same(body))
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
