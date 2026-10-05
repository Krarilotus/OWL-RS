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
//! **Exact, per class** (saturation coupling, Konclude's idea in the context core's
//! terms): where the saturation of a class never reaches a context in which a clause
//! left out of the part could fire, the model the calculus builds for the class is a
//! model of every clause, and the part's subsumers of the class are all of them: the
//! class needs no hypertableau test ([`exact_for`]). Where that holds for every class
//! and `owl:Thing`, the part's taxonomy is the terminology's, and only consistency is
//! left to decide.

use nrese_owl::{
    BodyAtom, Clause, HeadAtom, Normalised, Term, UNSUPPORTED_DATATYPE_DEFINITIONS,
    UNSUPPORTED_KEYS,
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
    /// Per class: its known subsumers are all its subsumers (and it is satisfiable unless
    /// `unsat` says otherwise), as the part's saturation of it reaches nothing left out.
    pub exact_for: Vec<bool>,
    /// The same for `owl:Thing`: `top` is complete.
    pub top_exact: bool,
    /// The part has no model: neither has the ontology.
    pub inconsistent: bool,
}

/// How many times an axiom the context core refuses is dropped before giving up.
const RETRIES: usize = 16;

/// Clauses the Horn stage can take at all (after renaming).
fn admissible(c: &Clause) -> bool {
    !c.flags.equality && !c.flags.nominal && !c.flags.datatype
}

/// The Horn part's consequences over `classes` (sorted), or `None` if the context core
/// takes none of it.
pub fn horn_lower_bound(
    normalised: &Normalised,
    classes: &[Term],
    threads: usize,
    budget: crate::context::Budget,
    strategy: crate::context::Strategy,
) -> Option<Lower> {
    let all = &normalised.clauses;
    // The clauses of the part, by index into the normalisation's.
    let kept: Vec<usize> = (0..all.len()).filter(|&i| admissible(&all[i])).collect();
    // First everything admissible (the context core renames what it can into Horn
    // clauses); then the Horn clauses alone.
    // The first, fuller part gets three quarters: where it saturates, it is the one that
    // makes classes exact.
    let most = budget.deadline.map(|d| {
        let now = std::time::Instant::now();
        now + d.saturating_duration_since(now) * 3 / 4
    });
    let first = crate::context::Budget {
        deadline: most,
        ..budget
    };
    let horn: Vec<usize> = kept
        .iter()
        .copied()
        .filter(|&i| all[i].flags.horn)
        .collect();
    let all_horn = horn.len() == kept.len();
    if let Some(l) = saturate(normalised, kept, classes, threads, first, strategy) {
        return Some(l);
    }
    if all_horn {
        return None;
    }
    saturate(normalised, horn, classes, threads, budget, strategy)
}

/// The context core on the clauses `part` (indexes) of `n`, dropping clauses it refuses a
/// few times; with which classes the part is exact for.
fn saturate(
    n: &Normalised,
    mut part: Vec<usize>,
    classes: &[Term],
    threads: usize,
    budget: crate::context::Budget,
    strategy: crate::context::Strategy,
) -> Option<Lower> {
    let options = crate::context::Options {
        threads,
        proofs: false,
        budget,
        strategy,
        ..crate::context::Options::default()
    };
    let build = |part: &[usize]| Normalised {
        clauses: part.iter().map(|&i| n.clauses[i].clone()).collect(),
        fresh: n.fresh.clone(),
        classes: n.classes.clone(),
        ranges: n.ranges.clone(),
        ..Normalised::default()
    };
    for _ in 0..RETRIES {
        let why = match crate::context::saturate_normalised(&build(&part), classes, &options) {
            Ok(saturated) => {
                let mut lower = lower(&saturated.classification(), classes);
                exact_for(&mut lower, &saturated, n, &part, classes);
                return Some(lower);
            }
            Err(why) => why,
        };
        super::trace(&format!(
            "lower bound: the context core refuses {} clauses: {why}",
            part.len()
        ));
        use crate::context::Unsupported as U;
        match why {
            U::NotHorn { .. } if part.iter().any(|&i| !n.clauses[i].flags.horn) => {
                // No renaming makes it Horn: the Horn clauses alone.
                part.retain(|&i| n.clauses[i].flags.horn);
            }
            U::NotHorn { axiom: Some(a) }
            | U::Equality { axiom: a }
            | U::Nominals { axiom: a }
            | U::Datatypes { axiom: a }
            | U::ClauseShape { axiom: a }
            | U::Normalisation { axiom: a, .. } => {
                let before = part.len();
                part.retain(|&i| !n.clauses[i].sources.iter().any(|set| set.contains(&a)));
                if part.len() == before {
                    return None;
                }
            }
            // Out of budget, too large: no lower bound from this part.
            _ => return None,
        }
    }
    None
}

/// Which classes the part is exact for: those whose saturation reaches nothing a clause
/// left out of the part needs in its body (`Saturated::untouched`). Per left-out clause:
/// - concepts in its body: they are its triggers (a flipped one, standing for the
///   complement, could hold anywhere: then none is exact);
/// - else roles: those roles, and the roles below them by the part's role inclusions;
/// - else data atoms or nominals alone: no trigger. Data values exist only where a clause
///   with a data at-least head fires, and that clause is left out too (its body concepts
///   are triggers); individuals are in no class's model while the part has no nominal
///   (the ontology's consistency is decided apart, and a model of the individuals joins
///   the class's as a disjoint union);
/// - an empty body: it fires everywhere, none is exact.
fn exact_for(
    lower: &mut Lower,
    saturated: &crate::context::Saturated,
    n: &Normalised,
    part: &[usize],
    classes: &[Term],
) {
    // Axioms the clauses don't cover: no part of them is exact.
    if n.unsupported
        .iter()
        .any(|(_, r)| *r != UNSUPPORTED_DATATYPE_DEFINITIONS && *r != UNSUPPORTED_KEYS)
    {
        return;
    }
    let mut inside = vec![false; n.clauses.len()];
    for &i in part {
        inside[i] = true;
    }
    let mut triggers = Vec::new();
    let mut roles: Vec<Term> = Vec::new();
    for (i, c) in n.clauses.iter().enumerate() {
        if inside[i] {
            continue;
        }
        if c.body.is_empty() {
            return;
        }
        let mut concepts = false;
        for b in &c.body {
            if let BodyAtom::Concept(concept, _) = b {
                concepts = true;
                match saturated.concept_of(*concept) {
                    // Not in the part: no context has it.
                    None => {}
                    Some((_, true)) => return,
                    Some((id, false)) => triggers.push(id),
                }
            }
        }
        if !concepts {
            roles.extend(c.body.iter().filter_map(|b| match b {
                BodyAtom::Role(r, _, _) => Some(*r),
                _ => None,
            }));
        }
    }
    // The roles below a trigger role (an edge of a subrole is one of the role).
    let mut below: std::collections::HashMap<Term, Vec<Term>> = std::collections::HashMap::new();
    for &i in part {
        let c = &n.clauses[i];
        if let ([BodyAtom::Role(s, _, _)], [HeadAtom::Role(r, _, _)]) = (&c.body[..], &c.head[..]) {
            below.entry(*r).or_default().push(*s);
        }
    }
    let mut seen: std::collections::HashSet<Term> = roles.iter().copied().collect();
    let mut stack = roles.clone();
    while let Some(r) = stack.pop() {
        for &s in below.get(&r).map_or(&[][..], |v| v) {
            if seen.insert(s) {
                stack.push(s);
            }
        }
    }
    let role_ids: Vec<crate::context::atoms::RoleId> =
        seen.iter().filter_map(|&r| saturated.role_of(r)).collect();
    if triggers.is_empty() && role_ids.is_empty() && part.len() == n.clauses.len() {
        lower.exact = true;
    }
    let untouched = saturated.untouched(&triggers, &role_ids);
    let names = &saturated.program().names;
    lower.exact_for = classes
        .iter()
        .map(|t| names.binary_search(t).is_ok_and(|i| untouched[i]))
        .collect();
    lower.top_exact = untouched.last().copied().unwrap_or(false);
    if lower.exact_for.iter().all(|&e| e) && lower.top_exact {
        lower.exact = true;
    }
}

fn lower(c: &crate::context::Classification, classes: &[Term]) -> Lower {
    let n = classes.len();
    let mut lower = Lower {
        known: vec![Vec::new(); n],
        unsat: vec![false; n],
        top: Vec::new(),
        exact: false,
        exact_for: vec![false; n],
        top_exact: false,
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
