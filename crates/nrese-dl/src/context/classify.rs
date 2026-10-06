//! Classification with the context core (docs/design/owl2-dl.md §7): one query context per
//! named class (core `A(x)`, Algorithm 1 of Bate et al. for every class in one run, as
//! Sequoia does) and one with the empty core for `owl:Thing`; the individuals' contexts
//! come from [`super::abox`]. After saturation, `⊤ → B(x)` in `A`'s context is `A ⊑ B`, and
//! `⊤ → ⊥` is `A ⊑ ⊥`.

use std::time::Instant;

use nrese_owl::{Axiom, ClassExpr, EntityKind, Ontology, ProofGraph, Term, normalise_with};

use super::abox::Individuals;
use super::atoms::{Atom, CTerm, ConceptId};
use super::compile::{Compiled, Unsupported, compile};
use super::engine::{Budget, Engine, Strategy, lock};
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
    /// What the saturation may use (unlimited by default); past it, [`Unsupported::Budget`].
    pub budget: Budget,
    /// Pred joins stop where the body so far already makes the conclusion redundant
    /// (else every combination is made and `derive` drops it).
    pub prune_pred: bool,
    /// Take unqualified at-most-one clauses (functional properties) with the Eq rule
    /// (else they are refused as equality). Off by default: on ore_ont_9724 (Full-GALEN)
    /// its conditional copies outgrow 9 GB where the part without them saturates in 25 s.
    pub equality: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            threads: 1,
            strategy: Strategy::Cautious,
            proofs: true,
            normalise: nrese_owl::Options::default(),
            budget: Budget::default(),
            prune_pred: true,
            equality: false,
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

/// A saturated context structure, with what it was built for.
pub struct Saturated {
    engine: Engine,
    classes: Vec<Term>,
    /// The query context of each named concept.
    query: Vec<ContextId>,
    top: ContextId,
    /// The individuals' contexts and the clauses on their edges found no clash.
    assertions_consistent: bool,
    profile: Profile,
}

/// Classifies `ontology`, or says why the Horn stage can't.
pub fn classify(
    ontology: &Ontology,
    options: &Options,
) -> Result<(Classification, Profile), Unsupported> {
    let saturated = saturate(ontology, options)?;
    if saturated.profile.contexts_joins_left > 0 {
        return Err(Unsupported::Budget);
    }
    if !saturated.complete() {
        return Err(Unsupported::Equality {
            axiom: saturated.unmerged_axiom(),
        });
    }
    let started = Instant::now();
    let classification = saturated.classification();
    let mut profile = saturated.profile.clone();
    profile.assemble = started.elapsed();
    Ok((classification, profile))
}

/// Normalises, compiles and saturates `ontology`.
pub fn saturate(ontology: &Ontology, options: &Options) -> Result<Saturated, Unsupported> {
    let started = Instant::now();
    let normalised = normalise_with(ontology, options.normalise);
    let normalise = started.elapsed();
    let classes = signature(ontology);
    let mut saturated = saturate_normalised(&normalised, &classes, options)?;
    saturated.profile.normalise = normalise;
    Ok(saturated)
}

/// Compiles and saturates DL-clauses for the named classes `classes` (the classification
/// driver's Horn lower bound runs it on the Horn part of an ontology's clauses).
pub fn saturate_normalised(
    normalised: &nrese_owl::Normalised,
    classes: &[Term],
    options: &Options,
) -> Result<Saturated, Unsupported> {
    let mut profile = Profile {
        threads: options.threads.max(1),
        ..Profile::default()
    };
    let started = Instant::now();
    let compiled = compile(normalised, classes, options.equality)?;
    profile.compile = started.elapsed();
    profile.compiled = compiled.stats;
    profile.dl_clauses = compiled.program.clauses.len();
    profile.concepts = compiled.program.concepts as usize;
    profile.roles = compiled.program.roles as usize;
    profile.functions = compiled.program.funcs.len();
    let started = Instant::now();
    let Compiled { program, abox, .. } = compiled;
    let named = program.named();
    let engine = Engine::new(program, options.strategy, options.proofs)
        .with_budget(options.budget)
        .with_prune_pred(options.prune_pred);
    let query: Vec<ContextId> = (0..named)
        .map(|c| engine.context_for(&[Atom::concept(c, CTerm::X)]).0)
        .collect();
    let (top, _) = engine.context_for(&[]);
    let mut seeds = query.clone();
    seeds.push(top);
    engine.run(&seeds, options.threads);
    let individuals = Individuals::saturate(&engine, &abox, options.threads);
    if engine.exhausted() {
        return Err(Unsupported::Budget);
    }
    profile.saturate = started.elapsed();
    profile.contexts_created = engine.count();
    for (_, state) in engine.states() {
        if state.started {
            profile.contexts_saturated += 1;
        }
        profile.add(&state.clauses.counters);
        profile.proof_steps += state.clauses.derivations.len() as u64;
        profile.largest_context = profile.largest_context.max(state.clauses.recs.len() as u64);
        match state.incomplete {
            Some(super::rules::Incomplete::Merge) => profile.contexts_unmerged += 1,
            Some(super::rules::Incomplete::Join) => profile.contexts_joins_left += 1,
            None => {}
        }
    }
    // The program's named concepts: the signature's classes and any the clauses add.
    let classes = engine.program.names.clone();
    Ok(Saturated {
        engine,
        classes,
        query,
        top,
        assertions_consistent: individuals.consistent,
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
        !self.unsat(self.top) && self.assertions_consistent
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

    /// The concept of `c` in the program, and whether it stands for `c`'s complement (a
    /// fresh name the renaming flipped); `None` for a name the clauses don't have.
    pub fn concept_of(&self, c: nrese_owl::Concept) -> Option<(ConceptId, bool)> {
        let p = &self.engine.program;
        match c {
            nrese_owl::Concept::Named(t) => self.concept(t).map(|id| (id, false)),
            nrese_owl::Concept::Fresh(q) => {
                let flipped = *p.flipped.get(q as usize)?;
                Some((p.order.named + q, flipped))
            }
        }
    }

    /// The role of a property, if the program has it.
    pub fn role_of(&self, property: Term) -> Option<super::atoms::RoleId> {
        self.engine.program.role_ids.get(&property).copied()
    }

    /// Saturation coupling (docs/design/owl2-dl.md §7): per named concept of the program
    /// ([`Program::names`]), and for `owl:Thing` last, whether no context its query
    /// context reaches (through successor links) has a clause with a head `B(x)` for a
    /// `B` in `triggers` (about any term: `B(f(x))` is about a successor), or a head over
    /// a role in `roles`. The model the calculus builds for the concept then has no
    /// element in a trigger, so clauses left out of the program whose bodies need one
    /// hold in it: what was derived for the concept is all that holds.
    /// Whether the saturation is a complete answer: every merge the Eq rule had to make
    /// was made and no join was left (else what it derived still holds, but may not be
    /// all).
    pub fn complete(&self) -> bool {
        self.profile.contexts_unmerged == 0 && self.profile.contexts_joins_left == 0
    }

    /// An axiom of an at-most-one clause whose merge was left out (for the error).
    fn unmerged_axiom(&self) -> usize {
        let program = &self.engine.program;
        program
            .at_most_one
            .iter()
            .find_map(|a| a.sources.first().and_then(|s| s.first()))
            .map_or(0, |&a| a as usize)
    }

    pub fn untouched(
        &self,
        triggers: &[ConceptId],
        roles: &[super::atoms::RoleId],
        local: &[Local],
    ) -> Vec<bool> {
        let triggers: std::collections::HashSet<ConceptId> = triggers.iter().copied().collect();
        let n = self.engine.count();
        let mut tainted = vec![false; n];
        let mut stack = Vec::new();
        for (id, state) in self.engine.states() {
            let c = &state.clauses;
            let role = roles
                .iter()
                .any(|r| c.out_terms.contains_key(r) || c.in_terms.contains_key(r));
            // Every head, whatever its term: a head `B(f(x))` here is about a successor
            // whose own context never hears of `B` when no clause of the part reads it
            // (only successor triggers are passed on), and the model's element has it.
            let concept = c
                .heads
                .keys()
                .any(|a| a.kind() == super::atoms::Kind::Concept && triggers.contains(&a.pred()));
            let fires = || local.iter().any(|l| l.may_fire(c));
            if state.incomplete.is_some() || role || concept || fires() {
                tainted[id as usize] = true;
                stack.push(id);
            }
        }
        while let Some(k) = stack.pop() {
            let preds: Vec<ContextId> = lock(&self.engine.context(k).state)
                .preds
                .iter()
                .map(|&(p, _)| p)
                .collect();
            for p in preds {
                if !tainted[p as usize] {
                    tainted[p as usize] = true;
                    stack.push(p);
                }
            }
        }
        self.query
            .iter()
            .chain(std::iter::once(&self.top))
            .map(|&c| !tainted[c as usize])
            .collect()
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
            let merged_by: Vec<u32> = match d.rule {
                Rule::Eq => state.merges.get(d.dl as usize)?.by.to_vec(),
                _ => Vec::new(),
            };
            drop(state);
            let rule = match d.rule {
                Rule::Core => "core",
                Rule::Hyper => "hyper",
                Rule::Pred => "pred",
                Rule::Succ => "succ",
                Rule::Eq => "eq",
            };
            let program = &self.engine.program;
            let sources = |s: &super::program::Sources| -> Vec<Vec<usize>> {
                s.iter()
                    .map(|set| set.iter().map(|&s| s as usize).collect())
                    .collect()
            };
            let alternatives: Vec<Vec<usize>> = match d.rule {
                // Every at-most-one clause of the merge: the product of their alternatives.
                Rule::Eq => merged_by.iter().fold(vec![Vec::new()], |acc, &k| {
                    let alts = sources(&program.at_most_one[k as usize].sources);
                    let alts = if alts.is_empty() {
                        vec![Vec::new()]
                    } else {
                        alts
                    };
                    acc.iter()
                        .flat_map(|set| {
                            alts.iter().map(move |alt| {
                                let mut u: Vec<usize> = set.iter().chain(alt).copied().collect();
                                u.sort_unstable();
                                u.dedup();
                                u
                            })
                        })
                        .collect()
                }),
                _ => program
                    .clauses
                    .get(d.dl as usize)
                    .map(|c| sources(&c.sources))
                    .unwrap_or_default(),
            };
            if alternatives.is_empty() {
                graph.add(rule, &premises, &[], fact);
            }
            // Each alternative set of axioms gives the DL-clause on its own.
            for set in &alternatives {
                graph.add(rule, &premises, set, fact);
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

/// A left-out clause over one element: `B₁(x) ∧ … → D₁(x) ∨ …` (its concepts in the
/// program's ids; no head is `⊥`). A context it can't fire in unsatisfied isn't touched
/// by it ([`Saturated::untouched`]): where every `Bᵢ` that occurs is `x`'s,
/// unconditionally, and either some `Bᵢ` doesn't occur or some `Dⱼ` holds of `x`
/// unconditionally, the context's model satisfies it.
#[derive(Debug, Clone)]
pub struct Local {
    pub body: Vec<ConceptId>,
    pub head: Vec<ConceptId>,
}

impl Local {
    fn may_fire(&self, c: &super::state::Clauses) -> bool {
        use super::atoms::Kind;
        if self.body.is_empty() {
            // A body concept that no context has: the clause never fires.
            return false;
        }
        let at_x = |b: ConceptId| Atom::concept(b, CTerm::X);
        // Any occurrence of a body concept about another term, or conditional at x.
        let elsewhere = c.heads.keys().any(|a| {
            a.kind() == Kind::Concept
                && self.body.contains(&a.pred())
                && (a.term() != CTerm::X || c.unconditional(*a).is_none())
        });
        if elsewhere {
            return true;
        }
        let all = self
            .body
            .iter()
            .all(|&b| c.unconditional(at_x(b)).is_some());
        all && !self
            .head
            .iter()
            .any(|&d| c.unconditional(at_x(d)).is_some())
    }
}
