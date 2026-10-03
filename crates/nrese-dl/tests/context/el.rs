//! The EL gate (owl2-dl-performance.md §6, package 3.1): on EL ontologies the context
//! core's taxonomy is the EL classifier's (`nrese_reasoner::classify`), which agrees with
//! ELK on the ORE EL subset. Random EL ontologies from `nrese_owl::fuzz`, at least 1,000
//! (`NRESE_FUZZ_CASES` changes the count).

use std::collections::BTreeSet;

use nrese_owl::fuzz::{self, Profile, Rng, Signature, Sizes};
use nrese_owl::{Axiom, Characteristic, ObjProp, Ontology, Term};

use super::support::{Table, classify};

pub fn cases(default: u64) -> u64 {
    std::env::var("NRESE_FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

pub fn sizes(case: u64) -> Sizes {
    Sizes {
        classes: 4 + (case % 4) as u32,
        object_properties: 3 + (case % 3) as u32,
        simple: 2,
        data_properties: 0,
        individuals: 0,
        literals: 0,
    }
}

/// A random EL ontology of case `case` and the table of its terms.
pub fn el_case(rng: &mut Rng, case: u64) -> (Table, Ontology) {
    let mut table = Table::default();
    let sig = Signature::new(sizes(case), &mut |n| table.intern(n));
    let profile = Profile {
        axioms: 6 + (case % 10) as usize,
        depth: 1 + (case % 3) as u32,
        ..Profile::el()
    };
    let o = fuzz::ontology(rng, &sig, profile);
    (table, o)
}

/// Whether `o` breaks the OWL 2 EL profile's restriction on ranges (OWL 2 Profiles §2.2.6):
/// a range on a superproperty of a chain (or a transitive property) must be implied for
/// the chain's last property. The EL classifier (and ELK) handle ranges completely only
/// under it; the fuzzer's EL profile doesn't keep it. Checked conservatively: any range on
/// a property at or above a chain's superproperty.
pub fn breaks_el_ranges(o: &Ontology) -> bool {
    let mut ups: Vec<(Term, Term)> = Vec::new();
    let mut chained: BTreeSet<Term> = BTreeSet::new();
    let mut ranged: BTreeSet<Term> = BTreeSet::new();
    for a in &o.axioms {
        match a {
            Axiom::SubObjectPropertyOf(chain, sup) if chain.len() == 1 => {
                ups.push((chain[0].named(), sup.named()))
            }
            Axiom::SubObjectPropertyOf(_, sup) => {
                chained.insert(sup.named());
            }
            Axiom::ObjectCharacteristic(Characteristic::Transitive, p) => {
                chained.insert(p.named());
            }
            Axiom::EquivalentObjectProperties(ps) => {
                for &a in ps {
                    for &b in ps {
                        ups.push((a.named(), b.named()));
                    }
                }
            }
            Axiom::ObjectPropertyRange(ObjProp::Named(p) | ObjProp::Inverse(p), _) => {
                ranged.insert(*p);
            }
            _ => {}
        }
    }
    let mut above = chained.clone();
    loop {
        let more: Vec<Term> = ups
            .iter()
            .filter(|(a, b)| above.contains(a) && !above.contains(b))
            .map(|&(_, b)| b)
            .collect();
        if more.is_empty() {
            break;
        }
        above.extend(more);
    }
    above.iter().any(|p| ranged.contains(p))
}

fn render(table: &Table, o: &Ontology) -> String {
    let names = |t: u64| table.name(t);
    o.axioms
        .iter()
        .map(|a| o.functional(a, &names))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn equals_the_el_classifier_on_fuzzed_el_ontologies() {
    let n = cases(1000);
    let mut rng = Rng::new(0x2026_1003_3101);
    let (mut compared, mut subsumptions, mut outside) = (0, 0, 0);
    let mut case = 0;
    while compared < n {
        let (mut table, o) = el_case(&mut rng, case);
        case += 1;
        if breaks_el_ranges(&o) {
            outside += 1;
            continue;
        }
        let triples = table.triples(&o);
        let snapshot = table.clone();
        let el = nrese_reasoner::classify::classify(&triples, &mut table, &|id| {
            snapshot.text(id).starts_with('<')
        });
        assert!(
            el.skipped.is_empty(),
            "case {case}: the EL classifier skipped {:?}\n{}",
            el.skipped,
            render(&table, &o)
        );
        // Half through the RDF round trip, as the store would give it.
        let input = if case % 2 == 0 {
            o.clone()
        } else {
            table.round_trip(&o)
        };
        let ours = classify(&input, 1)
            .unwrap_or_else(|e| panic!("case {case}: {e}\n{}", render(&table, &o)));
        let show = |pairs: &[(u64, u64)]| -> Vec<String> {
            pairs
                .iter()
                .map(|&(a, b)| format!("{} ⊑ {}", table.name(a), table.name(b)))
                .collect()
        };
        // An inconsistent ontology: every class unsatisfiable (the EL classifier also
        // lists every class as equivalent to owl:Thing then).
        let el_top = if ours.consistent { &el.top } else { &ours.top };
        assert_eq!(
            (show(&ours.subsumptions), &ours.unsatisfiable, &ours.top),
            (show(&el.subsumptions), &el.unsatisfiable, el_top),
            "case {case}: (context core, EL classifier)\n{}",
            render(&table, &o)
        );
        compared += 1;
        subsumptions += ours.subsumptions.len();
    }
    eprintln!(
        "{compared} EL ontologies equal, {subsumptions} subsumptions; {outside} left out \
         (outside the EL profile's range restriction)"
    );
}
