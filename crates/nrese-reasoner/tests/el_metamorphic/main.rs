//! Metamorphic tests of the EL classifier (docs/design/owl2-dl.md §11, work package 2.5):
//! random EL ontologies from `nrese_owl::fuzz`, written to triples and classified, then
//! transformed in ways that keep every entailment over their signature: renamed (the
//! taxonomy renamed the same way), reordered, padded with redundant axioms, complex
//! superclasses given fresh names (the taxonomy on the original classes unchanged). The
//! classifier must also read every generated axiom (none skipped as outside EL).

use std::cell::RefCell;
use std::collections::BTreeSet;

use nrese_owl::fuzz::{self, Name, Profile, Rng, Signature, Sizes};
use nrese_owl::{Make, Ontology, Term};
use nrese_reasoner::classify::{classify, classify_parallel};
use nrese_reasoner::vocabulary::LocalVocabulary;

/// One id space for the ontology's terms, OWL's vocabulary and blank nodes.
struct Ids {
    vocabulary: RefCell<LocalVocabulary>,
    blanks: u64,
}

impl Ids {
    fn term(&self, text: &str) -> Term {
        self.vocabulary.borrow_mut().term(text)
    }
}

impl Make for Ids {
    fn blank(&mut self) -> Term {
        self.blanks += 1;
        self.term(&format!("_:w{}", self.blanks))
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> Term {
        self.term(&format!("\"{lexical}\"^^<{datatype}>"))
    }
}

/// The subsumptions and unsatisfiable classes, by IRI, among `classes`.
type Taxonomy = (BTreeSet<(String, String)>, BTreeSet<String>);

fn taxonomy(o: &Ontology, ids: &mut Ids, classes: &[Term]) -> Taxonomy {
    for (_, iri) in nrese_owl::Vocabulary::iris() {
        ids.term(&format!("<{iri}>"));
    }
    let vocabulary = {
        let v = &ids.vocabulary;
        nrese_owl::Vocabulary::new(&|iri| Some(v.borrow_mut().term(&format!("<{iri}>"))))
    };
    let triples = nrese_owl::write(o, &vocabulary, ids);
    let mut local = ids.vocabulary.borrow().clone();
    let names = local.clone();
    let result = classify(&triples, &mut local, &|id| names.text(id).starts_with('<'));
    assert!(
        result.skipped.is_empty(),
        "outside EL: {:?}",
        result.skipped
    );
    let text = |id: u64| local.text(id).to_owned();
    let keep: BTreeSet<String> = classes.iter().map(|&c| text(c)).collect();
    let subs = result
        .subsumptions
        .iter()
        .map(|&(a, b)| (text(a), text(b)))
        .filter(|(a, b)| keep.contains(a) && keep.contains(b))
        .collect();
    let unsat = result
        .unsatisfiable
        .iter()
        .map(|&c| text(c))
        .filter(|c| keep.contains(c))
        .collect();
    (subs, unsat)
}

#[test]
fn the_el_taxonomy_survives_meaning_preserving_changes() {
    let cases = std::env::var("NRESE_FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300u64);
    let mut rng = Rng::new(0x2026_1003_0251);
    let (mut subsumptions, mut unsatisfiable) = (0, 0);
    for case in 0..cases {
        let mut ids = Ids {
            vocabulary: RefCell::new(LocalVocabulary::default()),
            blanks: 0,
        };
        let sizes = Sizes {
            classes: 5,
            object_properties: 3,
            simple: 1,
            data_properties: 0,
            individuals: 0,
            literals: 0,
        };
        let sig = Signature::new(sizes, &mut |name| match name {
            Name::Iri(iri) => ids.term(&format!("<{iri}>")),
            Name::Integer(n) => ids.term(&format!(
                "\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>"
            )),
        });
        let mut profile = Profile::el();
        profile.axioms = 6 + rng.below(10) as usize;
        let o = fuzz::ontology(&mut rng, &sig, profile);
        let base = taxonomy(&o, &mut ids, &sig.classes);
        subsumptions += base.0.len();
        unsatisfiable += base.1.len();

        // Renamed: classes and properties permuted.
        let mut classes = sig.classes.clone();
        let mut props = sig.object_properties.clone();
        for v in [&mut classes, &mut props] {
            for i in (1..v.len()).rev() {
                v.swap(i, rng.below(i as u64 + 1) as usize);
            }
        }
        let map = |t: Term| {
            if let Some(i) = sig.classes.iter().position(|&c| c == t) {
                classes[i]
            } else if let Some(i) = sig.object_properties.iter().position(|&p| p == t) {
                props[i]
            } else {
                t
            }
        };
        let renamed = taxonomy(&fuzz::rename(&o, &map), &mut ids, &sig.classes);
        let text = |t: Term| ids.vocabulary.borrow().text(t).to_owned();
        let rename_text = |s: &String| {
            let t = sig
                .classes
                .iter()
                .copied()
                .find(|&c| text(c) == *s)
                .expect("a class");
            text(map(t))
        };
        let expected: Taxonomy = (
            base.0
                .iter()
                .map(|(a, b)| (rename_text(a), rename_text(b)))
                .collect(),
            base.1.iter().map(rename_text).collect(),
        );
        assert_eq!(renamed, expected, "case {case}: renamed");

        let shuffled = taxonomy(&fuzz::shuffle(&o, &mut rng), &mut ids, &sig.classes);
        assert_eq!(shuffled, base, "case {case}: shuffled");

        let redundant = fuzz::add_redundant(&o, &mut rng, &sig, 4, true);
        assert_eq!(
            taxonomy(&redundant, &mut ids, &sig.classes),
            base,
            "case {case}: redundant"
        );

        let mut fresh = 0;
        let defined = fuzz::define_fresh(&o, 3, &mut || {
            fresh += 1;
            ids.term(&format!("<{}F{fresh}>", fuzz::FUZZ))
        });
        assert_eq!(
            taxonomy(&defined, &mut ids, &sig.classes),
            base,
            "case {case}: fresh names"
        );
    }
    eprintln!(
        "{cases} ontologies: {subsumptions} subsumptions, {unsatisfiable} unsatisfiable classes"
    );
    assert!(
        subsumptions > cases as usize,
        "the generator should give taxonomies to compare"
    );
}

/// The parallel saturation reaches the sequential one's fixpoint: the same subsumptions,
/// unsatisfiable classes and classes equivalent to `owl:Thing`, on every random ontology,
/// with several workers racing over the contexts.
#[test]
fn the_parallel_classification_equals_the_sequential_one() {
    let cases = std::env::var("NRESE_FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200u64);
    let mut rng = Rng::new(0x2026_1003_1500);
    let mut subsumptions = 0;
    for case in 0..cases {
        let mut ids = Ids {
            vocabulary: RefCell::new(LocalVocabulary::default()),
            blanks: 0,
        };
        let sizes = Sizes {
            classes: 8 + rng.below(8) as u32,
            object_properties: 4,
            simple: 2,
            data_properties: 0,
            individuals: 0,
            literals: 0,
        };
        let sig = Signature::new(sizes, &mut |name| match name {
            Name::Iri(iri) => ids.term(&format!("<{iri}>")),
            Name::Integer(n) => ids.term(&format!(
                "\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>"
            )),
        });
        let mut profile = Profile::el();
        profile.axioms = 10 + rng.below(30) as usize;
        let o = fuzz::ontology(&mut rng, &sig, profile);
        for (_, iri) in nrese_owl::Vocabulary::iris() {
            ids.term(&format!("<{iri}>"));
        }
        let vocabulary = {
            let v = &ids.vocabulary;
            nrese_owl::Vocabulary::new(&|iri| Some(v.borrow_mut().term(&format!("<{iri}>"))))
        };
        let triples = nrese_owl::write(&o, &vocabulary, &mut ids);
        let names = ids.vocabulary.borrow().clone();
        let named = |id: u64| names.text(id).starts_with('<');
        let sequential = classify(&triples, &mut ids.vocabulary.borrow().clone(), &named);
        for threads in [2, 4] {
            let parallel = classify_parallel(
                &triples,
                &mut ids.vocabulary.borrow().clone(),
                &named,
                threads,
            );
            assert_eq!(parallel, sequential, "case {case}, {threads} threads");
        }
        subsumptions += sequential.subsumptions.len();
    }
    assert!(
        subsumptions > cases as usize,
        "the generator should give taxonomies to compare"
    );
}
