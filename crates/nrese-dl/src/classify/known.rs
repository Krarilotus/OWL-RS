//! Known subsumers from the Horn part of an ontology (docs/design/owl2-dl.md §7): the
//! context core saturates the clauses it can take, and what follows from a subset of the
//! clauses follows from all of them. So each subsumption and each unsatisfiable class it
//! finds is entailed: a lower bound the driver needn't test.
//!
//! The Horn part: clauses with at most one head atom and without equality, nominals or
//! data (the Horn stage's fragment). The individuals are left out (no consequence is
//! lost that a subset could prove: assertions only add consequences for classes through
//! nominals, which are out). Where the context core still refuses a clause (its shapes),
//! that axiom's clauses are dropped and it tries again, a few times.

use nrese_owl::{Clause, Normalised, Term};

/// What the Horn part proves, by class index.
#[derive(Debug, Clone, Default)]
pub struct Lower {
    /// Each class's known subsumers (sorted, without the class).
    pub known: Vec<Vec<u32>>,
    pub unsat: Vec<bool>,
}

/// How many times an axiom the context core refuses is dropped before giving up.
const RETRIES: usize = 16;

fn horn(c: &Clause) -> bool {
    c.flags.horn && !c.flags.equality && !c.flags.nominal && !c.flags.datatype
}

/// The Horn part's consequences over `classes` (sorted), or `None` if the context core
/// takes none of it.
pub fn horn_lower_bound(
    normalised: &Normalised,
    classes: &[Term],
    threads: usize,
) -> Option<Lower> {
    let mut part = Normalised {
        clauses: normalised
            .clauses
            .iter()
            .filter(|c| horn(c))
            .cloned()
            .collect(),
        fresh: normalised.fresh.clone(),
        classes: normalised.classes.clone(),
        ranges: normalised.ranges.clone(),
        ..Normalised::default()
    };
    if part.clauses.is_empty() {
        return None;
    }
    let options = crate::context::Options {
        threads,
        proofs: false,
        ..crate::context::Options::default()
    };
    for _ in 0..RETRIES {
        match crate::context::saturate_normalised(&part, classes, &options) {
            Ok(saturated) => {
                let c = saturated.classification();
                let mut lower = Lower {
                    known: vec![Vec::new(); classes.len()],
                    unsat: vec![false; classes.len()],
                };
                let index = |t: &Term| classes.binary_search(t).ok();
                for (a, b) in &c.subsumptions {
                    if let (Some(a), Some(b)) = (index(a), index(b)) {
                        lower.known[a].push(b as u32);
                    }
                }
                for k in &mut lower.known {
                    k.sort_unstable();
                    k.dedup();
                }
                let all = !c.consistent;
                for t in &c.unsatisfiable {
                    if let Some(a) = index(t) {
                        lower.unsat[a] = true;
                    }
                }
                if all {
                    // The Horn part has no model: neither has the ontology; the driver's
                    // consistency test says so itself.
                    return None;
                }
                return Some(lower);
            }
            Err(why) => {
                let axiom = match why {
                    crate::context::Unsupported::NotHorn { axiom: Some(a) }
                    | crate::context::Unsupported::Equality { axiom: a }
                    | crate::context::Unsupported::Nominals { axiom: a }
                    | crate::context::Unsupported::Datatypes { axiom: a }
                    | crate::context::Unsupported::ClauseShape { axiom: a }
                    | crate::context::Unsupported::Normalisation { axiom: a, .. } => a,
                    _ => return None,
                };
                let before = part.clauses.len();
                part.clauses
                    .retain(|c| !c.sources.iter().any(|set| set.contains(&axiom)));
                if part.clauses.len() == before {
                    return None;
                }
            }
        }
    }
    None
}
