//! Metamorphic tests (docs/design/owl2-dl.md §11): renaming, reordering, redundant axioms
//! and fresh definitions leave the taxonomy as it is; so do the switches (threads,
//! strategy, proofs). On fuzzed EL ontologies and on fuzzed ALCHI ontologies with chains
//! (those the Horn stage takes).

use std::collections::HashMap;

use nrese_dl::context::{self, Classification, Options, Strategy};
use nrese_owl::Ontology;
use nrese_owl::fuzz::{self, Profile, Rng, Signature};

use super::el::{cases, sizes};
use super::support::Table;

fn run(o: &Ontology, options: Options) -> Option<Classification> {
    context::classify(o, &options).ok().map(|(c, _)| c)
}

fn defaults() -> Options {
    Options::default()
}

/// `c` with every term mapped by `f`, sorted again.
fn mapped(c: &Classification, f: &dyn Fn(u64) -> u64) -> Classification {
    let mut out = Classification {
        classes: c.classes.iter().map(|&t| f(t)).collect(),
        subsumptions: c.subsumptions.iter().map(|&(a, b)| (f(a), f(b))).collect(),
        unsatisfiable: c.unsatisfiable.iter().map(|&t| f(t)).collect(),
        top: c.top.iter().map(|&t| f(t)).collect(),
        consistent: c.consistent,
    };
    out.classes.sort_unstable();
    out.subsumptions.sort_unstable();
    out.unsatisfiable.sort_unstable();
    out.top.sort_unstable();
    out
}

/// `c` over the classes of `keep` only.
fn restricted(c: &Classification, keep: &[u64]) -> Classification {
    let k = |t: &u64| keep.contains(t);
    Classification {
        classes: c.classes.iter().copied().filter(k).collect(),
        subsumptions: c
            .subsumptions
            .iter()
            .copied()
            .filter(|(a, b)| k(a) && k(b))
            .collect(),
        unsatisfiable: c.unsatisfiable.iter().copied().filter(k).collect(),
        top: c.top.iter().copied().filter(k).collect(),
        consistent: c.consistent,
    }
}

/// A random ontology: EL, or ALCHI with chains and inverses (often not Horn), a quarter
/// of those with assertions.
fn case(rng: &mut Rng, n: u64) -> (Table, Signature, Ontology, bool) {
    let mut table = Table::default();
    let sig = Signature::new(sizes(n), &mut |name| table.intern(name));
    let el = n.is_multiple_of(2);
    let profile = if el {
        Profile {
            axioms: 6 + (n % 8) as usize,
            ..Profile::el()
        }
    } else {
        Profile {
            axioms: 4 + (n % 6) as usize,
            depth: 2,
            data: false,
            nominals: false,
            numbers: false,
            abox: n % 4 == 1,
            ..Profile::sroiq()
        }
    };
    let o = fuzz::ontology(rng, &sig, profile);
    (table, sig, o, el)
}

#[test]
fn renaming_reordering_and_redundancy_keep_the_taxonomy() {
    let n = cases(400);
    let mut rng = Rng::new(0x2026_1003_3102);
    let (mut horn, mut total) = (0, 0);
    for i in 0..n {
        let (mut table, sig, o, el) = case(&mut rng, i);
        total += 1;
        let base = run(&o, defaults());
        let Some(base) = base else {
            // Not Horn: renaming and reordering don't make it Horn either.
            assert!(
                run(&fuzz::shuffle(&o, &mut rng), defaults()).is_none(),
                "case {i}"
            );
            continue;
        };
        horn += 1;
        // Renaming: every term to a new IRI (a different order of ids, too).
        let count = table.len() as u64;
        let map: HashMap<u64, u64> = (0..count)
            .map(|t| {
                let text = table.text(t).to_owned();
                let renamed = match text.strip_suffix('>') {
                    Some(iri) => format!("<{}renamed{t}>", &iri[1..]),
                    None => format!("{text}r"),
                };
                (t, table.term(&renamed))
            })
            .collect();
        let back: HashMap<u64, u64> = map.iter().map(|(&a, &b)| (b, a)).collect();
        let renamed = fuzz::rename(&o, &|t| map[&t]);
        let got = run(&renamed, defaults()).expect("renamed: still Horn");
        assert_eq!(mapped(&got, &|t| back[&t]), base, "case {i}: renaming");
        // Reordering.
        let shuffled = fuzz::shuffle(&o, &mut rng);
        assert_eq!(
            run(&shuffled, defaults()),
            Some(base.clone()),
            "case {i}: order"
        );
        // Redundant axioms (EL forms for EL ontologies: others may leave Horn).
        let redundant = fuzz::add_redundant(&o, &mut rng, &sig, 3, el);
        if let Some(got) = run(&redundant, defaults()) {
            assert_eq!(got, base, "case {i}: redundant axioms");
        } else {
            assert!(!el, "case {i}: EL with redundant EL axioms is Horn");
        }
        // Fresh names for complex superclasses: a conservative extension.
        let mut next = 0;
        let defined = fuzz::define_fresh(&o, 2, &mut || {
            next += 1;
            table.iri_id(&format!("http://example.org/fresh#F{next}"))
        });
        // `F ≡ E` puts `E` on the left too: Horn for EL, not always beyond.
        match run(&defined, defaults()) {
            Some(got) => assert_eq!(
                restricted(&got, &base.classes),
                base,
                "case {i}: definitions"
            ),
            None => assert!(!el, "case {i}: EL definitions keep Horn"),
        }
    }
    eprintln!("{horn} of {total} ontologies Horn and checked");
    assert!(horn * 2 > total, "too few Horn cases: {horn} of {total}");
}

#[test]
fn the_switches_keep_the_taxonomy() {
    let n = cases(300);
    let mut rng = Rng::new(0x2026_1003_3103);
    for i in 0..n {
        let (_, _, o, _) = case(&mut rng, i);
        let Some(base) = run(&o, defaults()) else {
            continue;
        };
        for (threads, strategy, proofs) in [
            (4, Strategy::Cautious, true),
            (1, Strategy::Eager, true),
            (3, Strategy::Eager, false),
            (1, Strategy::Cautious, false),
        ] {
            let options = Options {
                threads,
                strategy,
                proofs,
                ..Options::default()
            };
            assert_eq!(
                run(&o, options).as_ref(),
                Some(&base),
                "case {i}: {threads} threads, {strategy:?}, proofs {proofs}"
            );
        }
    }
}
