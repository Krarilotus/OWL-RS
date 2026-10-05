//! Known subsumers from the Horn part of an ontology (docs/design/owl2-dl.md §7): the
//! context core saturates the clauses it can take, and what follows from a subset of the
//! clauses follows from all of them. So each subsumption and each unsatisfiable class it
//! finds is entailed: a lower bound the driver needn't test.
//!
//! The part: the clauses without equality, nominals or data, the individuals left out
//! (without nominals they can't change a subsumption of a consistent ontology). The
//! context core makes what it can Horn by renaming fresh names; if it can't, the
//! clauses with more than one head atom go too, and where it still refuses a clause (its
//! shapes), that axiom's clauses are dropped and it tries again, a few times.
//!
//! **Exact:** where nothing was dropped but data clauses, and no clause requires a data
//! value (so data properties may be empty and data clauses hold trivially, as the Horn
//! stage argues), the part has the ontology's subsumptions: then the lower bound *is* the
//! terminology's taxonomy, and only consistency is left to decide.

use nrese_owl::{
    Clause, HeadAtom, Normalised, Term, UNSUPPORTED_DATATYPE_DEFINITIONS, UNSUPPORTED_KEYS,
};

/// What the Horn part proves, by class index.
#[derive(Debug, Clone, Default)]
pub struct Lower {
    /// Each class's known subsumers (sorted, without the class).
    pub known: Vec<Vec<u32>>,
    pub unsat: Vec<bool>,
    /// The classes equivalent to `owl:Thing` (sorted).
    pub top: Vec<u32>,
    /// The part had every subsumption of the ontology's terminology.
    pub exact: bool,
    /// The part has no model: neither has the ontology.
    pub inconsistent: bool,
}

/// How many times an axiom the context core refuses is dropped before giving up.
const RETRIES: usize = 16;

/// Clauses the Horn stage can take at all (after renaming).
fn admissible(c: &Clause) -> bool {
    !c.flags.equality && !c.flags.nominal && !c.flags.datatype
}

/// Whether leaving out `normalised`'s data clauses keeps its subsumptions: no clause
/// requires a data value, nothing else was left out of the clauses.
fn data_droppable(normalised: &Normalised) -> bool {
    let requires_data = normalised.clauses.iter().any(|c| {
        c.head
            .iter()
            .any(|h| matches!(h, HeadAtom::DataAtLeast { .. }))
    });
    let unsupported = normalised
        .unsupported
        .iter()
        .any(|(_, r)| *r != UNSUPPORTED_DATATYPE_DEFINITIONS && *r != UNSUPPORTED_KEYS);
    !requires_data && !unsupported
}

/// The Horn part's consequences over `classes` (sorted), or `None` if the context core
/// takes none of it.
pub fn horn_lower_bound(
    normalised: &Normalised,
    classes: &[Term],
    threads: usize,
    budget: crate::context::Budget,
) -> Option<Lower> {
    let kept: Vec<Clause> = normalised
        .clauses
        .iter()
        .filter(|c| admissible(c))
        .cloned()
        .collect();
    let dropped = kept.len()
        < normalised
            .clauses
            .iter()
            .filter(|c| !c.flags.datatype)
            .count();
    let exact = !dropped && data_droppable(normalised);
    let part = |clauses: Vec<Clause>| Normalised {
        clauses,
        fresh: normalised.fresh.clone(),
        classes: normalised.classes.clone(),
        ranges: normalised.ranges.clone(),
        ..Normalised::default()
    };
    // First everything admissible (the context core renames what it can into Horn
    // clauses), with half the time; then the Horn clauses alone.
    let half = budget.deadline.map(|d| {
        let now = std::time::Instant::now();
        now + d.saturating_duration_since(now) / 2
    });
    let first = crate::context::Budget {
        deadline: half,
        ..budget
    };
    let horn: Vec<Clause> = kept.iter().filter(|c| c.flags.horn).cloned().collect();
    let all_horn = horn.len() == kept.len();
    if let Some(l) = saturate(part(kept), classes, threads, first, exact) {
        return Some(l);
    }
    if all_horn {
        return None;
    }
    saturate(part(horn), classes, threads, budget, false)
}

/// The context core on `part`, dropping clauses it refuses a few times.
fn saturate(
    mut part: Normalised,
    classes: &[Term],
    threads: usize,
    budget: crate::context::Budget,
    mut exact: bool,
) -> Option<Lower> {
    if part.clauses.is_empty() && !exact {
        return None;
    }
    let options = crate::context::Options {
        threads,
        proofs: false,
        budget,
        ..crate::context::Options::default()
    };
    for _ in 0..RETRIES {
        let why = match crate::context::saturate_normalised(&part, classes, &options) {
            Ok(saturated) => return Some(lower(&saturated.classification(), classes, exact)),
            Err(why) => why,
        };
        exact = false;
        use crate::context::Unsupported as U;
        match why {
            U::NotHorn { .. } if part.clauses.iter().any(|c| !c.flags.horn) => {
                // No renaming makes it Horn: the Horn clauses alone.
                part.clauses.retain(|c| c.flags.horn);
            }
            U::NotHorn { axiom: Some(a) }
            | U::Equality { axiom: a }
            | U::Nominals { axiom: a }
            | U::Datatypes { axiom: a }
            | U::ClauseShape { axiom: a }
            | U::Normalisation { axiom: a, .. } => {
                let before = part.clauses.len();
                part.clauses
                    .retain(|c| !c.sources.iter().any(|set| set.contains(&a)));
                if part.clauses.len() == before {
                    return None;
                }
            }
            // Out of budget, too large: no lower bound from this part.
            _ => return None,
        }
    }
    None
}

fn lower(c: &crate::context::Classification, classes: &[Term], exact: bool) -> Lower {
    let n = classes.len();
    let mut lower = Lower {
        known: vec![Vec::new(); n],
        unsat: vec![false; n],
        top: Vec::new(),
        exact,
        inconsistent: !c.consistent,
    };
    let index = |t: &Term| classes.binary_search(t).ok();
    if !c.consistent {
        // The part has no model, so neither has the ontology: every class is below
        // `owl:Nothing` (the driver's consistency test says so itself).
        lower.unsat = vec![true; n];
        return lower;
    }
    for (a, b) in &c.subsumptions {
        if let (Some(a), Some(b)) = (index(a), index(b)) {
            lower.known[a].push(b as u32);
        }
    }
    for k in &mut lower.known {
        k.sort_unstable();
        k.dedup();
    }
    for t in &c.unsatisfiable {
        if let Some(a) = index(t) {
            lower.unsat[a] = true;
        }
    }
    lower.top = c.top.iter().filter_map(index).map(|i| i as u32).collect();
    lower.top.sort_unstable();
    lower
}
