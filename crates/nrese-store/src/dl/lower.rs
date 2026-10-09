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
//! - **The memberships** are computed per revision from L's (one scan of each subclass's
//!   members) and kept in the read view, beside the engine's stacks, as U1's facts are.

use std::collections::HashMap;

use nrese_dl::classify::{self, Taxonomy};
use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId};
use nrese_owl::Ontology;
use nrese_reasoner::ir::Triple;

use crate::StoreService;

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
/// of `C` and superclass `D` of `C`, where `snapshot` doesn't have it. O(members of the
/// subclasses × their superclasses).
pub(crate) fn memberships(snapshot: &Snapshot, taxonomy: &Taxonomy, rdf_type: u64) -> Vec<Triple> {
    let mut supers: HashMap<u64, Vec<u64>> = HashMap::new();
    for &(sub, sup) in &taxonomy.classification.subsumptions {
        supers.entry(sub).or_default().push(sup);
    }
    let has = |t: Triple| {
        snapshot
            .quads_for_pattern_in(ReadModel::Materialised, &pattern(t))
            .next()
            .is_some()
    };
    let mut out = Vec::new();
    for (sub, sups) in &supers {
        let members: Vec<u64> = snapshot
            .quads_for_pattern_in(
                ReadModel::Materialised,
                &QuadPattern {
                    subject: None,
                    predicate: Some(TermId::from_raw(rdf_type)),
                    object: Some(TermId::from_raw(*sub)),
                    graph: GraphSelector::Any,
                },
            )
            .map(|q| q.subject.raw())
            .collect();
        for a in members {
            for &sup in sups {
                let fact = [a, rdf_type, sup];
                if !has(fact) {
                    out.push(fact);
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn pattern([s, p, o]: Triple) -> QuadPattern {
    QuadPattern {
        subject: Some(TermId::from_raw(s)),
        predicate: Some(TermId::from_raw(p)),
        object: Some(TermId::from_raw(o)),
        graph: GraphSelector::Any,
    }
}
