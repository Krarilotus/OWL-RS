//! The ontology's consistency, cheapest engine first (docs/design/owl2-dl.md §4):
//!
//! 1. **The context core** on the clauses and the assertions, where its Horn stage takes
//!    them. Data property assertions it refuses; but where no clause and no key reads a
//!    data property, a data assertion constrains nothing but its own literal: the
//!    ontology is then consistent iff the rest is and every asserted literal has a value
//!    (a malformed `"x"^^xsd:int` has none). The literals go to the hypertableau's
//!    datatype theory alone, the rest to the context core.
//! 2. **The hypertableau** otherwise (the driver's own probe, with the model's labels).
//!
//! An inconsistency found on a part stands for the whole: the part's models include the
//! whole's.

use nrese_owl::{Normalised, Ontology, Term};

use crate::tableau::{self, Answer, At, Prepared, Probe, Want};

/// A decided consistency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Consistent,
    Inconsistent,
}

/// What reads data properties: data clauses, keys, negative data assertions.
fn reads_data(n: &Normalised) -> bool {
    n.clauses.iter().any(|c| c.flags.datatype)
        || !n.rules.is_empty()
        || !n.facts.not_data.is_empty()
}

/// The context core's verdict, `None` where it can't give one.
pub(crate) fn by_context_core(
    ontology: &Ontology,
    normalised: &Normalised,
    classes: &[Term],
    config: &tableau::Config,
    core: &crate::context::Options,
    workers: &nrese_exec::workers::Workers,
) -> Option<Verdict> {
    let has_data = !normalised.facts.data.is_empty();
    if has_data && reads_data(normalised) {
        return None;
    }
    let mut rest = normalised.clone();
    rest.facts.data.clear();
    let saturated = crate::context::classify::saturate_normalised_with_workers(
        &rest,
        classes,
        core,
        config.cancel.clone(),
        workers,
    )
    .ok()?;
    if !saturated.consistent() {
        return Some(Verdict::Inconsistent);
    }
    // A merge left out may hide a clash.
    if !saturated.complete() {
        return None;
    }
    if !has_data {
        return Some(Verdict::Consistent);
    }
    // The literals alone.
    let literals = Normalised {
        facts: nrese_owl::Facts {
            data: normalised.facts.data.clone(),
            ..nrese_owl::Facts::default()
        },
        ranges: normalised.ranges.clone(),
        classes: normalised.classes.clone(),
        ..Normalised::default()
    };
    let program = Prepared::new(ontology, &literals, &[], &[]);
    let probe = Probe {
        at: At::Nothing,
        positive: &[],
        negative: &[],
    };
    match program.probe(&probe, config, Want::default()).answer {
        Answer::Consistent => Some(Verdict::Consistent),
        Answer::Inconsistent => Some(Verdict::Inconsistent),
        _ => None,
    }
}
