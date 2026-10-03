//! Proofs of the context core (docs/design/owl2-dl.md §5, "Proofs"): every derived
//! subsumption and unsatisfiable class has a recorded proof in the proof IR whose leaves
//! are source axioms, and the axioms of the justification read off it (`one`) entail the
//! subsumption on their own.
//!
//! The RBox axioms are added to each justification for now: `nrese-owl` attributes the
//! clauses of a role automaton to the universal's axiom only, not to the role inclusions,
//! chains and transitivity it is built from (reported 3 October 2026; the fix makes
//! sources an OR of ANDs). Class-axiom provenance is checked strictly.

use nrese_dl::context::{self, Options};
use nrese_owl::fuzz::Rng;
use nrese_owl::{Axiom, Ontology};

use super::el::{cases, el_case};

/// Whether an axiom is about roles (see the module's note).
fn rbox(a: &Axiom) -> bool {
    matches!(
        a,
        Axiom::SubObjectPropertyOf(..)
            | Axiom::EquivalentObjectProperties(..)
            | Axiom::InverseObjectProperties(..)
            | Axiom::ObjectCharacteristic(..)
    )
}

/// `o` with only the axioms at `keep` (and every declaration and RBox axiom).
fn only(o: &Ontology, keep: &[usize]) -> Ontology {
    let mut out = o.clone();
    out.axioms = o
        .axioms
        .iter()
        .enumerate()
        .filter(|(i, a)| keep.contains(i) || matches!(a, Axiom::Declaration(..)) || rbox(a))
        .map(|(_, a)| a.clone())
        .collect();
    out.sources = vec![Vec::new(); out.axioms.len()];
    out
}

#[test]
fn every_subsumption_has_a_proof_from_a_justification() {
    let n = cases(200);
    let mut rng = Rng::new(0x2026_1003_3104);
    let mut checked = 0;
    for i in 0..n {
        let (table, o) = el_case(&mut rng, i);
        let saturated = context::saturate(&o, &Options::default()).expect("EL is Horn");
        let c = saturated.classification();
        if !c.consistent {
            continue;
        }
        let goals: Vec<(u64, Option<u64>)> = c
            .subsumptions
            .iter()
            .map(|&(a, b)| (a, Some(b)))
            .chain(c.unsatisfiable.iter().map(|&a| (a, None)))
            .take(6)
            .collect();
        for (sub, sup) in goals {
            let graph = saturated
                .explain(sub, sup)
                .unwrap_or_else(|| panic!("case {i}: no proof of {sub} ⊑ {sup:?}"));
            assert!(!graph.complete, "one derivation per clause: incomplete");
            let justification = graph
                .one()
                .unwrap_or_else(|| panic!("case {i}: no justification of {sub} ⊑ {sup:?}"));
            let alone = context::classify(&only(&o, &justification), &Options::default())
                .expect("Horn")
                .0;
            let holds = match sup {
                Some(b) => {
                    alone.subsumptions.binary_search(&(sub, b)).is_ok()
                        || alone.unsatisfiable.contains(&sub)
                }
                None => alone.unsatisfiable.contains(&sub),
            };
            if !holds {
                let names = |t: u64| table.name(t);
                let axioms: Vec<String> = o
                    .axioms
                    .iter()
                    .enumerate()
                    .map(|(k, a)| format!("{k}: {}", o.functional(a, &names)))
                    .collect();
                let steps: Vec<String> = graph
                    .inferences()
                    .iter()
                    .map(|s| format!("{s:?}"))
                    .collect();
                panic!(
                    "case {i}: {} ⊑ {:?} doesn't follow from its justification \
                     {justification:?}\n{}\n{}",
                    table.name(sub),
                    sup.map(|t| table.name(t)),
                    axioms.join("\n"),
                    steps.join("\n")
                );
            }
            checked += 1;
        }
    }
    eprintln!("{checked} proofs checked");
    assert!(checked > 100);
}
