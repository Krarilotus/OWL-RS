//! The lower bound L beyond the OWL 2 RL closure (docs/design/owl2-dl.md §8: L is the
//! union of everything NRESE derives soundly and cheaply): the class memberships the DL
//! engines' taxonomy of the TBox adds to L's. Every member of `C` in L is a member of
//! each superclass of `C` under OWL 2 DL; the RL rules miss those a superclass gets
//! through what they can't use (a union on the right, an existential on the left of a
//! subclass axiom, a universal through an inverse).
//!
//! - **The taxonomy** is the TBox's alone (the assertions left out), so its subsumptions
//!   hold whatever the assertions, and it lives as long as U1's compilation does (both
//!   change only with the schema). It is computed on the first read of a schema, never in
//!   a commit, by `nrese_dl::classify` (the context core where its Horn stage takes the
//!   TBox, so EL and Horn TBoxes cost a saturation; else the hypertableau driver) within
//!   `dl.timeout`; an incomplete one still gives only entailed subsumptions.
//!   It uses the same store-owned classification options and process-memory watch policy
//!   as an explicit classification request.
//! - **The memberships** are computed per revision from L's (one scan of each class's
//!   members) and kept in the read view, beside the engine's stacks, as U1's facts are.

use std::collections::HashMap;

use nrese_dl::classify::{self, Taxonomy};
use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId};
use nrese_owl::Ontology;
use nrese_reasoner::ir::Triple;

use crate::StoreService;

#[cfg(test)]
#[path = "lower_tests.rs"]
mod tests;

/// The taxonomy of `ontology`'s TBox (its assertions left out).
pub(crate) fn tbox_taxonomy(store: &StoreService, ontology: &Ontology) -> Taxonomy {
    let mut tbox = Ontology {
        axioms: Vec::new(),
        sources: Vec::new(),
        ..ontology.clone()
    };
    for (axiom, sources) in ontology.axioms.iter().zip(&ontology.sources) {
        if !axiom.is_assertion() {
            tbox.axioms.push(axiom.clone());
            tbox.sources.push(sources.clone());
        }
    }
    classify::classify(&tbox, &super::classification::options(store))
}

/// The memberships `taxonomy` adds to `snapshot`'s: `(a rdf:type D)` for each member `a`
/// of `C` and superclass `D` of `C`, where `snapshot` doesn't have it; sorted, each once.
/// Each class of a subsumption has its members read once, by one scan, and sorted; a pair is
/// then a sorted difference, so no point read per member and superclass (D16-712). The member
/// lists, all held at once, and the list are charged to `budget` as they grow (SYN-61); `Err`
/// where it can't take them. O(the classes' members × log, plus each pair's two lists).
pub(crate) fn memberships(
    snapshot: &Snapshot,
    taxonomy: &Taxonomy,
    rdf_type: u64,
    budget: &std::sync::Arc<nrese_exec::Budget>,
) -> Result<nrese_exec::ChargedVec<Triple>, nrese_exec::BudgetExceeded> {
    let mut supers: HashMap<u64, Vec<u64>> = HashMap::new();
    for &(sub, sup) in &taxonomy.classification.subsumptions {
        supers.entry(sub).or_default().push(sup);
    }
    let classes = supers
        .iter()
        .flat_map(|(sub, sups)| std::iter::once(sub).chain(sups));
    let mut members = HashMap::new();
    for class in classes.copied().collect::<std::collections::HashSet<u64>>() {
        let mut of = nrese_exec::ChargedVec::new(std::sync::Arc::clone(budget));
        for q in snapshot.quads_for_pattern_in(
            ReadModel::Materialised,
            &QuadPattern {
                subject: None,
                predicate: Some(TermId::from_raw(rdf_type)),
                object: Some(TermId::from_raw(class)),
                graph: GraphSelector::Any,
            },
        ) {
            of.push(q.subject.raw())?;
        }
        of.sort_unstable();
        members.insert(class, of);
    }
    let mut out = nrese_exec::ChargedVec::new(std::sync::Arc::clone(budget));
    for (sub, sups) in &supers {
        for &sup in sups {
            let mut typed = members[&sup].iter().peekable();
            for &a in members[sub].iter() {
                while typed.next_if(|&&t| t < a).is_some() {}
                if typed.peek() != Some(&&a) {
                    out.push([a, rdf_type, sup])?;
                }
            }
        }
    }
    super::upper::sort_dedup(&mut out);
    Ok(out)
}
