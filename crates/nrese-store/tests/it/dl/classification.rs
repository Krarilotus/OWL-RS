//! Step 6: classification and realisation through the DL engines (`nrese_dl::classify`),
//! on the store's asserted statements. Each test names the engine that must classify
//! (the dispatch is a performance choice), and the fuzzed differential checks the store's
//! path (reading the ontology over its term ids, the dispatch) against the hypertableau
//! driver run directly on the generated ontology.

use std::collections::{BTreeSet, HashMap};

use nrese_dl::classify;
use nrese_owl::fuzz::{self, Name, Profile, Rng, Signature, Sizes};
use nrese_owl::{Make, Term, Vocabulary};
use nrese_store::ReadScope;

use super::{insert, pipeline};

const EX: &str = "http://example.com/";

fn has(pairs: &[(String, String)], sub: &str, sup: &str) -> bool {
    pairs.contains(&(format!("{EX}{sub}"), format!("{EX}{sup}")))
}

#[test]
fn a_subsumption_through_a_union_is_found_by_the_tableau_driver() {
    let dl = pipeline();
    insert(
        &dl,
        ":A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] . :B rdfs:subClassOf :D . \
         :C rdfs:subClassOf :D . :E rdfs:subClassOf :B . :x a :A .",
    )
    .expect("ontology");
    let report = dl
        .store()
        .classify(&ReadScope::All)
        .expect("classification");
    assert_eq!(report.engine, "tableau");
    assert!(report.complete(), "{:?}", report.incomplete);
    assert!(report.consistent);
    assert!(has(&report.subsumptions, "A", "D"));
    assert!(has(&report.subsumptions, "E", "D"));
    assert!(!has(&report.subsumptions, "A", "B"));
    // Realisation: x is a D, which the RL closure doesn't know (queries prove it).
    let inferred = dl
        .store()
        .execute_query(&nrese_store::SparqlQueryRequest {
            read_model: Some(nrese_store::ReadModel::Inferred),
            ..nrese_store::SparqlQueryRequest::all(format!("{}ASK {{ :x a :D }}", super::PREFIXES))
        })
        .expect("ask the inferred stack");
    assert!(
        String::from_utf8(inferred.payload)
            .unwrap()
            .contains("false")
    );
    assert!(super::ask(&dl, ":x a :D"));
    let real = dl.store().realise(&ReadScope::All).expect("realisation");
    assert!(real.complete(), "{:?}", real.incomplete);
    let types = &real
        .types
        .iter()
        .find(|(a, _)| a == &format!("{EX}x"))
        .expect("x is realised")
        .1;
    assert!(types.contains(&format!("{EX}D")) && types.contains(&format!("{EX}A")));
    assert!(!types.contains(&format!("{EX}B")));
}

#[test]
fn a_horn_ontology_is_classified_by_the_context_core_and_follows_commits() {
    let dl = pipeline();
    insert(
        &dl,
        ":A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r ; owl:someValuesFrom :B ] . \
         :Rb owl:equivalentClass [ a owl:Restriction ; owl:onProperty :r ; \
           owl:someValuesFrom :B ] .",
    )
    .expect("ontology");
    let report = dl
        .store()
        .classify(&ReadScope::All)
        .expect("classification");
    assert_eq!(report.engine, "context-core");
    assert!(report.complete());
    assert!(has(&report.subsumptions, "A", "Rb"));
    assert!(!has(&report.subsumptions, "Rb", "A"));
    // The result is kept per revision: a commit gives a new one.
    insert(&dl, ":B owl:disjointWith :B2 . :B rdfs:subClassOf :B2 .").expect("commit");
    let report = dl
        .store()
        .classify(&ReadScope::All)
        .expect("classification");
    let unsat: BTreeSet<&str> = report.unsatisfiable.iter().map(String::as_str).collect();
    assert!(unsat.contains("http://example.com/B") && unsat.contains("http://example.com/A"));
}

#[test]
fn classification_needs_a_scope_that_reads_everything() {
    let dl = pipeline();
    let restricted = ReadScope::Graphs(std::sync::Arc::new(Default::default()));
    assert!(dl.store().classify(&restricted).is_err());
    assert!(dl.store().realise(&restricted).is_err());
}

/// Term ids for a generated ontology, with their N-Triples text.
#[derive(Default)]
struct Table {
    text: Vec<String>,
    ids: HashMap<String, Term>,
}

impl Table {
    fn id(&mut self, text: String) -> Term {
        if let Some(&id) = self.ids.get(&text) {
            return id;
        }
        let id = self.text.len() as Term;
        self.ids.insert(text.clone(), id);
        self.text.push(text);
        id
    }
}

impl Make for Table {
    fn blank(&mut self) -> Term {
        let n = self.text.len();
        self.id(format!("_:w{n}"))
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> Term {
        self.id(format!("\"{lexical}\"^^<{datatype}>"))
    }
}

/// The store's classification against the tableau driver on the same ontology, over
/// random ALCHI-with-ABox ontologies.
#[test]
fn the_store_classifies_as_the_tableau_driver_on_random_ontologies() {
    let mut compared = 0;
    for seed in 0..40u64 {
        let mut table = Table::default();
        for (_, iri) in Vocabulary::iris() {
            table.id(format!("<{iri}>"));
        }
        let sig = Signature::new(Sizes::default(), &mut |name| match name {
            Name::Iri(iri) => table.id(format!("<{iri}>")),
            Name::Integer(i) => table.id(format!(
                "\"{i}\"^^<http://www.w3.org/2001/XMLSchema#integer>"
            )),
        });
        let profile = Profile {
            data: false,
            nominals: false,
            numbers: false,
            chains: false,
            ..Profile::sroiq()
        };
        let mut ontology = fuzz::ontology(&mut Rng::new(seed), &sig, profile);
        ontology.axioms.extend(sig.declarations());
        ontology.axioms.sort();
        ontology.axioms.dedup();
        ontology.sources = vec![Vec::new(); ontology.axioms.len()];
        let vocabulary = Vocabulary::new(&|iri| table.ids.get(&format!("<{iri}>")).copied());
        let triples = nrese_owl::write(&ontology, &vocabulary, &mut table);
        let reference = classify::classify(
            &ontology,
            &classify::Options {
                context_core: false,
                ..classify::Options::default()
            },
        );
        let dl = pipeline_without_gate();
        let data: String = triples
            .iter()
            .map(|t| t.map(|x| table.text[x as usize].clone()).join(" ") + " .\n")
            .collect();
        dl.store()
            .execute_update_str(&format!("INSERT DATA {{ {data} }}"))
            .expect("load");
        let report = dl
            .store()
            .classify(&ReadScope::All)
            .expect("classification");
        if !reference.complete() || !report.complete() {
            continue;
        }
        compared += 1;
        let iri = |t: Term| table.text[t as usize].trim_matches(['<', '>']).to_owned();
        let expected: BTreeSet<(String, String)> = reference
            .classification
            .subsumptions
            .iter()
            .map(|&(a, b)| (iri(a), iri(b)))
            .collect();
        let got: BTreeSet<(String, String)> = report.subsumptions.iter().cloned().collect();
        assert_eq!(got, expected, "seed {seed}: subsumptions");
        let expected: BTreeSet<String> = reference
            .classification
            .unsatisfiable
            .iter()
            .map(|&c| iri(c))
            .collect();
        let got: BTreeSet<String> = report.unsatisfiable.iter().cloned().collect();
        assert_eq!(got, expected, "seed {seed}: unsatisfiable classes");
        assert_eq!(
            report.consistent, reference.classification.consistent,
            "seed {seed}: consistency"
        );
    }
    assert!(compared >= 30, "only {compared} seeds compared");
}

/// A store in the mode whose commit gate is off (random ontologies may be inconsistent).
fn pipeline_without_gate() -> nrese_store::MutationPipeline {
    super::pipeline_with(nrese_store::DlConfig {
        consistency: nrese_store::DlConsistency::Off,
        ..nrese_store::DlConfig::default()
    })
}
