//! Exact query tests over one compiled program. A fresh selector implies each probe's
//! assumption; only that selector is asserted by Prepared::probe. Unselected selectors
//! may be empty, so their definitions are a conservative extension of the premise.
//! Search state is created by the existing probe engine and is never shared.

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
}

impl Batch {
    /// Normalises/compiles at most once, regardless of the number of eligible tests.
    /// The caller already owns its local expression interner; the premise isn't cloned.
    pub(crate) fn new(ontology: &mut Ontology, tests: &[Test]) -> Self {
        if tests.is_empty() {
            return Self {
                prepared: None,
                probes: Vec::new(),
                left_out: None,
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
        let prepared = if classes.is_empty() {
            None
        } else {
            #[cfg(test)]
            COMPILATIONS.set(COMPILATIONS.get() + 1);
            let prepared = tableau::prepared(ontology);
            let normalised = nrese_owl::normalise_with(
                &prepared,
                nrese_owl::Options {
                    exact_provenance: false,
                    ..nrese_owl::Options::default()
                },
            );
            Some(Prepared::new(
                &prepared,
                &normalised,
                &classes,
                &individuals,
            ))
        };
        ontology.axioms.truncate(base);
        ontology.sources.truncate(sources);
        Self {
            prepared,
            probes,
            left_out: consistency::left_out(ontology),
        }
    }

    pub(crate) fn check(&self, index: usize, budget: &Budget) -> Option<Entailed> {
        let started = std::time::Instant::now();
        let check = || self.check_admitted(index, budget, started);
        match &budget.workers {
            Some(workers) => workers.install(check),
            None => check(),
        }
    }

    fn check_admitted(
        &self,
        index: usize,
        budget: &Budget,
        started: std::time::Instant,
    ) -> Option<Entailed> {
        let (class, at) = self.probes[index]?;
        if budget
            .cancel
            .as_ref()
            .is_some_and(tableau::Cancel::is_cancelled)
        {
            return Some(Entailed::Unknown("cancelled".to_owned()));
        }
        let remaining = budget.timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Some(Entailed::Unknown("past dl.timeout".to_owned()));
        }
        let config = tableau::Config {
            timeout: Some(remaining),
            max_memory: budget.memory_bytes,
            max_nodes: budget.max_nodes,
            max_branch_points: Some(budget.max_branch_points),
            cancel: budget.cancel.clone(),
            workers: budget.workers.as_ref().map(|workers| workers.limited(1)),
            portfolio: false,
            ..tableau::Config::default()
        };
        let outcome = self.prepared.as_ref()?.probe(
            &Probe {
                at,
                positive: &[class],
                negative: &[],
            },
            &config,
            Want::default(),
        );
        Some(match outcome.answer {
            tableau::Answer::Inconsistent => Entailed::Yes,
            tableau::Answer::Consistent => match &self.left_out {
                Some(why) => Entailed::Unknown(why.clone()),
                None => Entailed::No,
            },
            tableau::Answer::Unsupported(_) => return None,
            tableau::Answer::GaveUp(why) => Entailed::Unknown(format!("gave up: {why}")),
        })
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
