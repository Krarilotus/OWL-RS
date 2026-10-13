//! Exact query tests over one compiled program. A fresh selector implies each probe's
//! assumption; only that selector is asserted by Prepared::probe. Unselected selectors
//! may be empty, so their definitions are a conservative extension of the premise.
//! Search state is created by the existing probe engine and is never shared.

use nrese_dl::limits::Limit;
use nrese_dl::tableau::{self, At, Prepared, Probe, Want};
use nrese_owl::{Axiom, ClassExpr, EntityKind, ExprId, ObjProp, Ontology};

use super::{Budget, Entailed, consistency, intern};

#[cfg(test)]
thread_local! {
    static COMPILATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Clone)]
pub(crate) enum Test {
    Axiom(Axiom),
    Nonempty(ExprId),
}

/// Immutable compiled inputs for one bounded batch. Unreduced forms use the established
/// context/tableau consistency route; no new exact-test engine or speculative trail.
pub(crate) struct Batch {
    prepared: Option<Prepared>,
    probes: Vec<Option<(u32, At)>>,
    left_out: Option<String>,
    /// The compiled program, reserved from the request's envelope while the batch lives.
    _scratch: nrese_dl::memory::Scratch,
    /// The envelope had no room to compile: every test is unknown.
    exhausted: bool,
}

impl Batch {
    /// Normalises/compiles at most once, regardless of the number of eligible tests.
    /// The caller already owns its local expression interner; the premise isn't cloned.
    /// What compiling holds is reserved from `memory` as each stage's output exists; the
    /// program stays reserved while the batch lives.
    pub(crate) fn new(
        ontology: &mut Ontology,
        tests: &[Test],
        memory: Option<&nrese_exec::Envelope>,
    ) -> Self {
        let mut scratch = nrese_dl::memory::Scratch::new(memory);
        if tests.is_empty() {
            return Self {
                prepared: None,
                probes: Vec::new(),
                left_out: None,
                _scratch: scratch,
                exhausted: false,
            };
        }
        let base = ontology.axioms.len();
        let sources = ontology.sources.len();
        let mut names: std::collections::HashSet<u64> = (0..ontology.classes.len())
            .filter_map(|i| match ontology.classes.get(i as u32) {
                ClassExpr::Class(c) => Some(*c),
                _ => None,
            })
            .collect();
        for axiom in &ontology.axioms {
            if let Axiom::Declaration(EntityKind::Class, c) | Axiom::DisjointUnion(c, _) = axiom {
                names.insert(*c);
            }
        }
        let mut classes = Vec::new();
        let mut individuals = Vec::new();
        let mut next = u64::MAX;
        let probes = tests
            .iter()
            .map(|test| {
                let (expr, individual) = assumption(ontology, test)?;
                while names.contains(&next) {
                    next -= 1;
                }
                let name = next;
                names.insert(name);
                next -= 1;
                let class = intern(ontology, ClassExpr::Class(name));
                ontology.axioms.push(Axiom::SubClassOf(class, expr));
                ontology.sources.push(Vec::new());
                let index = classes.len() as u32;
                classes.push(name);
                let at = match individual {
                    Some(a) => {
                        let index = individuals.len() as u32;
                        individuals.push(a);
                        At::Individual(index)
                    }
                    None => At::Fresh,
                };
                Some((index, at))
            })
            .collect();
        let mut compile = || {
            use nrese_dl::memory::{normalised_bytes, ontology_bytes};
            #[cfg(test)]
            COMPILATIONS.set(COMPILATIONS.get() + 1);
            let prepared = tableau::prepared(ontology);
            let mut copied = 0;
            scratch
                .charge(|| {
                    copied = match &prepared {
                        std::borrow::Cow::Owned(copy) => ontology_bytes(copy),
                        std::borrow::Cow::Borrowed(_) => 0,
                    };
                    copied
                })
                .ok()?;
            let normalised = nrese_owl::normalise_with(
                &prepared,
                nrese_owl::Options {
                    exact_provenance: false,
                    ..nrese_owl::Options::default()
                },
            );
            let mut clauses = 0;
            scratch
                .charge(|| {
                    clauses = normalised_bytes(&normalised);
                    clauses
                })
                .ok()?;
            let program = Prepared::new(&prepared, &normalised, &classes, &individuals);
            scratch.charge(|| program.bytes()).ok()?;
            // The copy and the clauses go; the program stays for the probes.
            scratch.release(copied + clauses);
            Some(program)
        };
        let (prepared, exhausted) = match classes.is_empty() {
            true => (None, false),
            false => {
                let program = compile();
                let exhausted = program.is_none();
                (program, exhausted)
            }
        };
        ontology.axioms.truncate(base);
        ontology.sources.truncate(sources);
        Self {
            prepared,
            probes,
            left_out: consistency::left_out(ontology),
            _scratch: scratch,
            exhausted,
        }
    }

    /// Decides `tests[index]` of [`Self::new`] over its premise `ontology`: by the
    /// compiled probe, or by the scalar reduction where the test has no probe form or the
    /// probe can't express it.
    pub(crate) fn check(
        &self,
        index: usize,
        test: &Test,
        ontology: &Ontology,
        fresh: &[u64],
        budget: &Budget,
    ) -> Entailed {
        if self.exhausted {
            return Entailed::Unknown(nrese_dl::memory::EXHAUSTED.to_owned());
        }
        let (Some(prepared), Some((class, at))) = (&self.prepared, self.probes[index]) else {
            return match test {
                Test::Axiom(axiom) => super::entails(ontology, axiom, fresh, budget),
                Test::Nonempty(class) => super::nonempty(ontology, *class, budget),
            };
        };
        if budget
            .cancel
            .as_ref()
            .is_some_and(tableau::Cancel::is_cancelled)
        {
            return Entailed::Unknown("cancelled".to_owned());
        }
        if budget.spent() {
            return Entailed::Unknown(Limit::Time.reached("the deadline passed"));
        }
        let config = tableau::Config {
            timeout: budget.timeout,
            max_memory: budget.memory_bytes,
            memory: budget.memory.clone(),
            max_nodes: budget.max_nodes,
            max_branch_points: budget.max_branch_points,
            max_merges: budget.max_merges,
            cancel: budget.cancel.clone(),
            workers: Some(budget.workers.limited(1)),
            portfolio: false,
            ..tableau::Config::default()
        };
        let outcome = prepared.probe(
            &Probe {
                at,
                positive: &[class],
                negative: &[],
            },
            &config,
            Want::default(),
        );
        match outcome.answer {
            tableau::Answer::Inconsistent => Entailed::Yes,
            tableau::Answer::Consistent => match &self.left_out {
                Some(why) => Entailed::Unknown(why.clone()),
                None => Entailed::No,
            },
            // The scalar reduction can still decide what the compiled probe can't express
            // (its Horn stage, for one): slower, but it keeps every answer the store had.
            tableau::Answer::Unsupported(_) => match test {
                Test::Axiom(axiom) => super::entails(ontology, axiom, fresh, budget),
                Test::Nonempty(class) => super::nonempty(ontology, *class, budget),
            },
            tableau::Answer::GaveUp(why) => Entailed::Unknown(format!("gave up: {why}")),
        }
    }
}

/// Ground entailment as a negative class probe. Role values use nominals, including
/// non-simple roles (the normaliser's existing automata handle their complements).
/// Nonemptiness uses ∀top.¬C, exactly C ⊑ ⊥, through the existing universal-role rewrite.
fn assumption(o: &mut Ontology, test: &Test) -> Option<(ExprId, Option<u64>)> {
    let (class, individual, negate) = match test {
        Test::Nonempty(c) => {
            let top = o.builtin.top_object?;
            let not = intern(o, ClassExpr::Not(*c));
            return Some((intern(o, ClassExpr::All(ObjProp::Named(top), not)), None));
        }
        Test::Axiom(Axiom::ClassAssertion(c, a)) => (*c, *a, true),
        Test::Axiom(Axiom::ObjectPropertyAssertion(p, a, b)) => (
            intern(o, ClassExpr::HasValue(ObjProp::Named(*p), *b)),
            *a,
            true,
        ),
        Test::Axiom(Axiom::NegativeObjectPropertyAssertion(p, a, b)) => (
            intern(o, ClassExpr::HasValue(ObjProp::Named(*p), *b)),
            *a,
            false,
        ),
        Test::Axiom(Axiom::DataPropertyAssertion(p, a, v)) => {
            (intern(o, ClassExpr::DataHasValue(*p, *v)), *a, true)
        }
        Test::Axiom(Axiom::NegativeDataPropertyAssertion(p, a, v)) => {
            (intern(o, ClassExpr::DataHasValue(*p, *v)), *a, false)
        }
        Test::Axiom(Axiom::SameIndividual(v)) if v.len() == 2 => {
            (intern(o, ClassExpr::OneOf(vec![v[1]])), v[0], true)
        }
        Test::Axiom(Axiom::DifferentIndividuals(v)) if v.len() == 2 => {
            (intern(o, ClassExpr::OneOf(vec![v[1]])), v[0], false)
        }
        _ => return None,
    };
    Some((
        if negate {
            intern(o, ClassExpr::Not(class))
        } else {
            class
        },
        Some(individual),
    ))
}

#[cfg(test)]
mod tests;
