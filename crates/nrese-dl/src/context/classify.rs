//! Classification with the context core (docs/design/owl2-dl.md §7): one query context per
//! named class (core `A(x)`, Algorithm 1 of Bate et al. for every class in one run, as
//! Sequoia does), one with the empty core for `owl:Thing`, and one per individual with
//! its asserted classes. After saturation, `⊤ → B(x)` in `A`'s context is `A ⊑ B`, and
//! `⊤ → ⊥` is `A ⊑ ⊥`.

use std::time::Instant;

use nrese_owl::{
    Axiom, Characteristic, ClassExpr, EntityKind, ExprId, Ontology, ProofGraph, Term,
    normalise_with,
};

use super::atoms::{Atom, CTerm, ConceptId};
use super::compile::{Unsupported, compile};
use super::engine::{Engine, Strategy, lock};
use super::profile::Profile;
use super::state::{ClauseRef, ContextId, Rule};

/// How to classify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Workers of the saturation; 1 runs it on the calling thread.
    pub threads: usize,
    pub strategy: Strategy,
    /// Record each clause's derivation, for [`Saturated::explain`].
    pub proofs: bool,
    /// How `nrese-owl` normalises.
    pub normalise: nrese_owl::Options,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            threads: 1,
            strategy: Strategy::Cautious,
            proofs: true,
            normalise: nrese_owl::Options::default(),
        }
    }
}

/// A classification: as the EL classifier's (`nrese_reasoner::classify`), over the
/// ontology's terms.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Classification {
    /// The named classes classified, sorted.
    pub classes: Vec<Term>,
    /// `(sub, super)` between named classes, `sub ≠ super`; equivalent classes both ways;
    /// unsatisfiable classes and `owl:Thing` as a superclass left out. Sorted.
    pub subsumptions: Vec<(Term, Term)>,
    /// Classes that can have no instance. Sorted.
    pub unsatisfiable: Vec<Term>,
    /// Classes equivalent to `owl:Thing`. Sorted.
    pub top: Vec<Term>,
    /// False if the ontology has no model: then every class is unsatisfiable.
    pub consistent: bool,
}

/// The named classes of an ontology: declared, or used in an expression.
pub fn signature(ontology: &Ontology) -> Vec<Term> {
    let mut out: Vec<Term> = ontology
        .axioms
        .iter()
        .filter_map(|a| match a {
            Axiom::Declaration(EntityKind::Class, t) => Some(*t),
            _ => None,
        })
        .collect();
    for id in 0..ontology.classes.len() {
        if let ClassExpr::Class(t) = ontology.classes.get(id as u32) {
            out.push(*t);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// `ontology` with each `ObjectPropertyDomain(R, C)` as `∃R.⊤ ⊑ C`, if it has chains or
/// transitive properties (`None` otherwise: nothing to change).
///
/// A workaround: `nrese-owl` normalises a domain into the raw clause `R(x, y) → C(x)`,
/// while the edges a chain or transitivity implies are never explicit (only universals
/// over non-simple roles follow them, through the automata), so a domain of a non-simple
/// role misses implied edges. As an existential on the left, the domain is a universal and
/// goes through the automaton. For simple roles both give the same clause. To remove once
/// the normalisation does this itself (reported 3 October 2026).
fn domains_as_existentials(ontology: &Ontology) -> Option<Ontology> {
    let complex = ontology.axioms.iter().any(|a| {
        matches!(a, Axiom::SubObjectPropertyOf(chain, _) if chain.len() > 1)
            || matches!(
                a,
                Axiom::ObjectCharacteristic(Characteristic::Transitive, _)
            )
    });
    if !complex
        || !ontology
            .axioms
            .iter()
            .any(|a| matches!(a, Axiom::ObjectPropertyDomain(..)))
    {
        return None;
    }
    let mut out = ontology.clone();
    let thing = ExprId(out.classes.intern(ClassExpr::Thing));
    for axiom in &mut out.axioms {
        if let Axiom::ObjectPropertyDomain(r, c) = *axiom {
            let some = ExprId(out.classes.intern(ClassExpr::Some(r, thing)));
            *axiom = Axiom::SubClassOf(some, c);
        }
    }
    Some(out)
}

/// A saturated context structure, with what it was built for.
pub struct Saturated {
    engine: Engine,
    classes: Vec<Term>,
    /// The query context of each named concept.
    query: Vec<ContextId>,
    top: ContextId,
    individuals: Vec<ContextId>,
    profile: Profile,
}

/// Classifies `ontology`, or says why the Horn stage can't.
pub fn classify(
    ontology: &Ontology,
    options: &Options,
) -> Result<(Classification, Profile), Unsupported> {
    let saturated = saturate(ontology, options)?;
    let started = Instant::now();
    let classification = saturated.classification();
    let mut profile = saturated.profile.clone();
    profile.assemble = started.elapsed();
    Ok((classification, profile))
}

/// Normalises, compiles and saturates `ontology`.
pub fn saturate(ontology: &Ontology, options: &Options) -> Result<Saturated, Unsupported> {
    let mut profile = Profile {
        threads: options.threads.max(1),
        ..Profile::default()
    };
    let started = Instant::now();
    let rewritten = domains_as_existentials(ontology);
    let normalised = normalise_with(rewritten.as_ref().unwrap_or(ontology), options.normalise);
    profile.normalise = started.elapsed();
    let started = Instant::now();
    let classes = signature(ontology);
    let compiled = compile(&normalised, &classes)?;
    profile.compile = started.elapsed();
    profile.compiled = compiled.stats;
    profile.dl_clauses = compiled.program.clauses.len();
    profile.concepts = compiled.program.concepts as usize;
    profile.roles = compiled.program.roles as usize;
    profile.functions = compiled.program.funcs.len();
    let started = Instant::now();
    let named = compiled.program.named();
    let engine = Engine::new(compiled.program, options.strategy, options.proofs);
    let query: Vec<ContextId> = (0..named)
        .map(|c| engine.context_for(&[Atom::concept(c, CTerm::X)]).0)
        .collect();
    let (top, _) = engine.context_for(&[]);
    let individuals: Vec<ContextId> = compiled
        .individuals
        .iter()
        .map(|concepts| {
            let mut core: Vec<Atom> = concepts
                .iter()
                .map(|&c| Atom::concept(c, CTerm::X))
                .collect();
            core.sort_unstable();
            engine.context_for(&core).0
        })
        .collect();
    let mut seeds = query.clone();
    seeds.push(top);
    seeds.extend(&individuals);
    seeds.sort_unstable();
    seeds.dedup();
    engine.run(&seeds, options.threads);
    profile.saturate = started.elapsed();
    profile.contexts_created = engine.count();
    for (_, state) in engine.states() {
        if state.started {
            profile.contexts_saturated += 1;
        }
        profile.add(&state.clauses.counters);
        profile.proof_steps += state.clauses.derivations.len() as u64;
    }
    // The program's named concepts: the signature's classes and any the clauses add.
    let classes = engine.program.names.clone();
    Ok(Saturated {
        engine,
        classes,
        query,
        top,
        individuals,
        profile,
    })
}

impl Saturated {
    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// The compiled program (DL-clauses with their sources, concepts, functions).
    pub fn program(&self) -> &super::program::Program {
        &self.engine.program
    }

    fn unsat(&self, c: ContextId) -> bool {
        lock(&self.engine.context(c).state).clauses.unsat
    }

    /// The named subsumers of a context's core (`ConceptId`s below the named bound).
    fn subsumers(&self, c: ContextId) -> Vec<ConceptId> {
        let named = self.engine.program.named();
        let state = lock(&self.engine.context(c).state);
        let mut out: Vec<ConceptId> = state.clauses.subsumers().filter(|&s| s < named).collect();
        out.sort_unstable();
        out
    }

    /// Whether the ontology has a model.
    pub fn consistent(&self) -> bool {
        !self.unsat(self.top) && !self.individuals.iter().any(|&i| self.unsat(i))
    }

    /// The taxonomy.
    pub fn classification(&self) -> Classification {
        let names = &self.engine.program.names;
        let mut out = Classification {
            classes: self.classes.clone(),
            consistent: self.consistent(),
            ..Classification::default()
        };
        if !out.consistent {
            out.unsatisfiable = out.classes.clone();
            return out;
        }
        out.top = self
            .subsumers(self.top)
            .into_iter()
            .map(|s| names[s as usize])
            .collect();
        for (c, &context) in self.query.iter().enumerate() {
            if self.unsat(context) {
                out.unsatisfiable.push(names[c]);
                continue;
            }
            for s in self.subsumers(context) {
                if s as usize != c {
                    out.subsumptions.push((names[c], names[s as usize]));
                }
            }
        }
        out.subsumptions.sort_unstable();
        out.unsatisfiable.sort_unstable();
        out.top.sort_unstable();
        out
    }

    fn concept(&self, class: Term) -> Option<ConceptId> {
        self.engine
            .program
            .names
            .binary_search(&class)
            .ok()
            .map(|i| i as ConceptId)
    }

    /// The recorded derivations of `sub ⊑ sup` (`sup = None`: `sub ⊑ ⊥`) as a proof graph
    /// whose leaves are the ontology's axioms (indexes into its axioms). `None` if the
    /// subsumption wasn't derived or proofs weren't recorded. Only each clause's first
    /// derivation is kept, so the graph holds one proof, not every one: it is marked
    /// incomplete.
    pub fn explain(&self, sub: Term, sup: Option<Term>) -> Option<ProofGraph<ClauseRef, usize>> {
        if !self.engine.proofs {
            return None;
        }
        let context = self.query[self.concept(sub)? as usize];
        let head = match sup {
            Some(t) => Atom::concept(self.concept(t)?, CTerm::X),
            None => Atom::BOTTOM,
        };
        let clause = {
            let state = lock(&self.engine.context(context).state);
            state.clauses.unconditional(head)?
        };
        let goal = ClauseRef { context, clause };
        let mut graph = ProofGraph::new(goal);
        graph.complete = false;
        let mut seen = std::collections::HashSet::from([goal]);
        let mut todo = vec![goal];
        while let Some(fact) = todo.pop() {
            let state = lock(&self.engine.context(fact.context).state);
            let d = *state.clauses.derivations.get(fact.clause as usize)?;
            let premises: Vec<ClauseRef> =
                state.clauses.premises[d.start as usize..(d.start + d.len) as usize].to_vec();
            drop(state);
            let rule = match d.rule {
                Rule::Core => "core",
                Rule::Hyper => "hyper",
                Rule::Pred => "pred",
                Rule::Succ => "succ",
            };
            let sources: Vec<usize> = self
                .engine
                .program
                .clauses
                .get(d.dl as usize)
                .map(|c| c.sources.iter().map(|&s| s as usize).collect())
                .unwrap_or_default();
            if sources.len() > 1 {
                // Any one of the axioms gives the DL-clause.
                for &a in &sources {
                    graph.add(rule, &premises, &[a], fact);
                }
            } else {
                graph.add(rule, &premises, &sources, fact);
            }
            for p in premises {
                if seen.insert(p) {
                    todo.push(p);
                }
            }
        }
        Some(graph)
    }
}
