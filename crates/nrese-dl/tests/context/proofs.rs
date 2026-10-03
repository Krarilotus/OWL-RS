//! Proofs of the context core (docs/design/owl2-dl.md §5, "Proofs"), on fuzzed EL and
//! Horn ALCHI ontologies: every derived
//! subsumption and unsatisfiable class has a recorded proof in the proof IR whose leaves
//! are source axioms, and the axioms of the justification read off it (`one`) entail the
//! subsumption on their own.
//!
//! A clause's sources are alternatives, each a set of axioms that give it together (the
//! automaton transitions of `nrese-owl` name the role inclusions they come from), so the
//! justifications are checked strictly: the axioms read off the proof alone.

use nrese_dl::context::{self, Options};
use nrese_owl::fuzz::Rng;
use nrese_owl::{Axiom, Ontology};

use super::el::cases;
use super::metamorphic::case;

/// `o` with only the axioms at `keep` (and every declaration).
fn only(o: &Ontology, keep: &[usize]) -> Ontology {
    let mut out = o.clone();
    out.axioms = o
        .axioms
        .iter()
        .enumerate()
        .filter(|(i, a)| keep.contains(i) || matches!(a, Axiom::Declaration(..)))
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
        // EL, or ALCHI with chains and inverses where it is Horn.
        let (table, _, o, _) = case(&mut rng, i);
        let Ok(saturated) = context::saturate(&o, &Options::default()) else {
            continue;
        };
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
