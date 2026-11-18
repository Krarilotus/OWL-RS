//! Step 6: classification and realisation through the DL engines (`nrese_dl::classify`),
//! on the store's asserted statements. Each test names the engine that must classify
//! (the dispatch is a performance choice), and the fuzzed differential checks the store's
//! path (reading the ontology over its term ids, the dispatch) against the hypertableau
//! driver run directly on the generated ontology.

use std::collections::{BTreeSet, HashMap};

use nrese_dl::classify;
use nrese_owl::fuzz::{self, Name, Profile, Rng, Signature, Sizes};
use nrese_owl::{Make, Term, Vocabulary};
use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_store::catalog::{Catalog, RepositorySettings};
use nrese_store::{ReadScope, Refusal, StoreError};

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
    assert_eq!(report.consistency, nrese_store::dl::Verdict::Consistent);
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
    insert(&dl, ":A rdfs:subClassOf :B . :x a :A .").expect("ontology");
    let restricted = ReadScope::Graphs(std::sync::Arc::new(Default::default()));
    type Operation = fn(&nrese_store::StoreService, &ReadScope) -> Result<(), StoreError>;
    let operations: [(&str, Operation); 2] = [
        ("classification", |store, scope| {
            store.classify(scope).map(drop)
        }),
        ("realisation", |store, scope| store.realise(scope).map(drop)),
    ];
    for (name, operation) in operations {
        operation(dl.store(), &ReadScope::All).expect(name);
        match operation(dl.store(), &restricted) {
            Err(StoreError::Forbidden(refusal)) => {
                assert_eq!(refusal, Refusal::ReadAll(name.to_owned()))
            }
            other => panic!("{name}: not refused: {other:?}"),
        }
    }
}

/// Term ids for a generated ontology, with their N-Triples text (the DL-oracle
/// differential's cases too).
#[derive(Default)]
pub(crate) struct Table {
    pub(crate) text: Vec<String>,
    pub(crate) ids: HashMap<String, Term>,
}

impl Table {
    pub(crate) fn id(&mut self, text: String) -> Term {
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
        crate::support::load_past_the_pipeline(dl.store(), &data);
        let report = dl
            .store()
            .classify(&ReadScope::All)
            .expect("classification");
        if !reference.complete() || !report.complete() {
            continue;
        }
        compared += 1;
        assert_eq!(
            report.consistency, reference.classification.consistency,
            "seed {seed}: consistency"
        );
        // An inconsistent ontology's report lists no class (every one is unsatisfiable).
        let listed = report.consistency != nrese_store::dl::Verdict::Inconsistent;
        let iri = |t: Term| table.text[t as usize].trim_matches(['<', '>']).to_owned();
        let expected: BTreeSet<(String, String)> = reference
            .classification
            .subsumptions
            .iter()
            .filter(|_| listed)
            .map(|&(a, b)| (iri(a), iri(b)))
            .collect();
        let got: BTreeSet<(String, String)> = report.subsumptions.iter().cloned().collect();
        assert_eq!(got, expected, "seed {seed}: subsumptions");
        let expected: BTreeSet<String> = reference
            .classification
            .unsatisfiable
            .iter()
            .filter(|_| listed)
            .map(|&c| iri(c))
            .collect();
        let got: BTreeSet<String> = report.unsatisfiable.iter().cloned().collect();
        assert_eq!(got, expected, "seed {seed}: unsatisfiable classes");
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

/// [`pipeline_without_gate`], answering outside OWL 2 DL by a proven procedure (the EL route).
fn pipeline_proven() -> nrese_store::MutationPipeline {
    super::pipeline_with(nrese_store::DlConfig {
        consistency: nrese_store::DlConsistency::Off,
        outside_profile: nrese_store::OutsideProfile::Proven,
        ..nrese_store::DlConfig::default()
    })
}

/// An EL ontology whose role hierarchy is irregular (`r1 ∘ r2 ⊑ r3` with `r3 ⊑ r1`,
/// *OWL 2 Structural Specification* §11.2): outside OWL 2 DL and OWL 2 EL, so the DL
/// engines leave its chain out; the EL classifier's completion rules (CR6) need no
/// regular hierarchy.
const IRREGULAR: &str = ":r1 a owl:ObjectProperty . :r2 a owl:ObjectProperty . \
     :r3 a owl:ObjectProperty . :r3 owl:propertyChainAxiom ( :r1 :r2 ) . \
     :r3 rdfs:subPropertyOf :r1 . \
     :A a owl:Class . :B a owl:Class . :C a owl:Class . :D a owl:Class . \
     :E a owl:Class . :F a owl:Class . :G a owl:Class . \
     :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r1 ; owl:someValuesFrom :B ] . \
     :B rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r2 ; owl:someValuesFrom :C ] . \
     :C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r2 ; owl:someValuesFrom :F ] . \
     [ a owl:Restriction ; owl:onProperty :r3 ; owl:someValuesFrom :C ] rdfs:subClassOf :D . \
     [ a owl:Restriction ; owl:onProperty :r1 ; owl:someValuesFrom :C ] rdfs:subClassOf :E . \
     [ a owl:Restriction ; owl:onProperty :r3 ; owl:someValuesFrom :F ] rdfs:subClassOf :G .";

/// The reason the DL engines give for leaving the irregular chain out.
const LEFT_OUT: &str = "axioms left out: an irregular role hierarchy";

fn irregular(extra: &str) -> nrese_store::ClassificationReport {
    let dl = pipeline_proven();
    insert(&dl, &format!("{IRREGULAR} {extra}")).expect("ontology");
    dl.store()
        .classify(&ReadScope::All)
        .expect("classification")
}

/// Where the irregular hierarchy is the only part the DL engines leave out, and the EL
/// classifier reads the whole ontology, the EL classifier's answer is the report's: complete,
/// and saying it gave it.
#[test]
fn an_irregular_el_ontology_is_answered_completely_by_the_el_classifier() {
    let report = irregular("");
    assert_eq!(report.engine, "el");
    assert!(report.complete(), "{:?}", report.incomplete);
    assert_eq!(report.consistency, nrese_store::dl::Verdict::Consistent);
    let expected: Vec<(String, String)> = [
        // A r1 B, B r2 C: the chain gives A r3 C, and ∃r3.C ⊑ D.
        ("A", "D"),
        // A r3 C with r3 ⊑ r1 is an r1 edge, and ∃r1.C ⊑ E.
        ("A", "E"),
        // That r1 edge chains again with C r2 F: A r3 F, and ∃r3.F ⊑ G (irregularity's step).
        ("A", "G"),
    ]
    .iter()
    .map(|(sub, sup)| (format!("{EX}{sub}"), format!("{EX}{sup}")))
    .collect();
    // B, C and F have no r1 edge: nothing else holds.
    assert_eq!(report.subsumptions, expected);
    assert!(
        report.unsatisfiable.is_empty(),
        "{:?}",
        report.unsatisfiable
    );
    assert!(report.equivalent_to_thing.is_empty());
}

/// Without assertions, the ontology has a model iff `owl:Thing` is satisfiable: where the
/// chain makes it unsatisfiable, the EL classifier's answer is "inconsistent", complete,
/// listing nothing (the DL engines' Horn part, without the chain, can't find it).
#[test]
fn an_irregular_el_ontology_whose_thing_is_unsatisfiable_is_inconsistent() {
    let dl = pipeline_proven();
    insert(
        &dl,
        ":r1 a owl:ObjectProperty . :r2 a owl:ObjectProperty . :r3 a owl:ObjectProperty . \
         :r3 owl:propertyChainAxiom ( :r1 :r2 ) . :r3 rdfs:subPropertyOf :r1 . \
         :B a owl:Class . :C a owl:Class . \
         owl:Thing rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r1 ; owl:someValuesFrom :B ] . \
         :B rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r2 ; owl:someValuesFrom :C ] . \
         [ a owl:Restriction ; owl:onProperty :r3 ; owl:someValuesFrom :C ] rdfs:subClassOf owl:Nothing .",
    )
    .expect("ontology");
    let report = dl
        .store()
        .classify(&ReadScope::All)
        .expect("classification");
    assert_eq!(report.engine, "el");
    assert!(report.complete(), "{:?}", report.incomplete);
    assert_eq!(report.consistency, nrese_store::dl::Verdict::Inconsistent);
    assert!(report.subsumptions.is_empty() && report.unsatisfiable.is_empty());
}

/// The answer the DL engines give when they leave the irregular chain out: their lower
/// bound, marked incomplete for that reason (the EL classifier didn't answer).
fn assert_lower_bound(report: &nrese_store::ClassificationReport) {
    assert_eq!(report.engine, "tableau");
    assert!(!report.complete());
    assert!(
        report.incomplete.contains(&format!(
            "consistency not decided (unsupported): {LEFT_OUT}"
        )),
        "{:?}",
        report.incomplete
    );
}

/// By default (`dl.outside_profile = proven`) the store answers outside OWL 2 DL by a proven
/// procedure: the irregular ontology gets the EL classifier's answer.
#[test]
fn by_default_an_irregular_el_ontology_is_answered_by_the_el_classifier() {
    let dl = pipeline_without_gate();
    insert(&dl, IRREGULAR).expect("ontology");
    let report = dl
        .store()
        .classify(&ReadScope::All)
        .expect("classification");
    assert_eq!(report.engine, "el");
    assert!(
        has(&report.subsumptions, "A", "D"),
        "{:?}",
        report.subsumptions
    );
}

/// Under `dl.outside_profile = dl-part` no procedure outside OWL 2 DL runs: the irregular
/// ontology gets the DL engines' lower bound, marked incomplete, the axioms left out named.
#[test]
fn under_dl_part_an_irregular_el_ontology_gets_the_dl_engines_lower_bound() {
    let dl = super::pipeline_with(nrese_store::DlConfig {
        consistency: nrese_store::DlConsistency::Off,
        outside_profile: nrese_store::OutsideProfile::DlPart,
        ..nrese_store::DlConfig::default()
    });
    insert(&dl, IRREGULAR).expect("ontology");
    let report = dl
        .store()
        .classify(&ReadScope::All)
        .expect("classification");
    assert!(
        !has(&report.subsumptions, "A", "D"),
        "{:?}",
        report.subsumptions
    );
    assert_lower_bound(&report);
}

/// A catalogue whose stores' commit gate is off (as [`pipeline_without_gate`]), with the
/// server's default `dl.outside_profile`.
fn catalog(dir: &std::path::Path) -> Catalog {
    let config = nrese_store::StoreConfig {
        dl: nrese_store::DlConfig {
            consistency: nrese_store::DlConsistency::Off,
            ..nrese_store::DlConfig::default()
        },
        ..nrese_store::StoreConfig::on_disk(dir)
    };
    let store = nrese_store::StoreService::new(config).expect("store");
    Catalog::open(store, ReasonerService::new(ReasonerConfig::default())).expect("catalogue")
}

/// Creates repository `id` under `owl2-dl` with `outside_profile` (`None`: the server's),
/// holding [`IRREGULAR`].
fn irregular_repository(catalog: &Catalog, id: &str, outside_profile: Option<&str>) {
    let settings = RepositorySettings {
        reasoning: Some("owl2-dl".to_owned()),
        outside_profile: outside_profile.map(str::to_owned),
        ..RepositorySettings::default()
    };
    catalog.create(id, settings).expect("created");
    insert(
        &catalog.get(id).expect("created").pipeline.read(),
        IRREGULAR,
    )
    .expect("ontology");
}

fn classify_in(catalog: &Catalog, id: &str) -> nrese_store::ClassificationReport {
    let store = std::sync::Arc::clone(catalog.get(id).expect("repository").pipeline.read().store());
    store.classify(&ReadScope::All).expect("classification")
}

/// A repository's `outside_profile` overrides the server's: `dl-part` under the server's
/// `proven` gets the DL engines' marked lower bound; a repository that names none gets the
/// EL classifier's answer.
#[test]
fn a_repository_answers_outside_owl2_dl_as_its_setting_says() {
    let dir = tempfile::tempdir().expect("temp dir");
    let catalog = catalog(dir.path());
    irregular_repository(&catalog, "own", Some("dl-part"));
    irregular_repository(&catalog, "server", None);
    assert_lower_bound(&classify_in(&catalog, "own"));
    assert_eq!(classify_in(&catalog, "server").engine, "el");
}

/// A change of a repository's `outside_profile` applies to the next classification and
/// never serves the other value's answer: the EL classifier's answer, kept for the
/// revision, is not the answer under `dl-part`; back under `proven`, it is again.
#[test]
fn a_change_of_the_outside_profile_never_serves_the_other_values_answer() {
    let dir = tempfile::tempdir().expect("temp dir");
    let catalog = catalog(dir.path());
    irregular_repository(&catalog, "r", None);
    assert_eq!(classify_in(&catalog, "r").engine, "el");
    let set = |profile: Option<&str>| {
        let profile = profile.map(str::to_owned);
        catalog
            .patch("r", |current| {
                Ok(RepositorySettings {
                    outside_profile: profile,
                    ..current.clone()
                })
            })
            .expect("changed");
    };
    set(Some("dl-part"));
    assert_lower_bound(&classify_in(&catalog, "r"));
    set(None);
    assert_eq!(classify_in(&catalog, "r").engine, "el");
}

/// A range on a property above a chain's (`r3 ⊑ r4`) needs applying on the edges the
/// chain composes, which the EL classifier doesn't (the EL++ update's condition): the
/// answer stays the DL engines' marked lower bound.
#[test]
fn an_irregular_ontology_with_a_range_above_a_chain_is_not_answered_by_the_el_classifier() {
    assert_lower_bound(&irregular(
        ":r4 a owl:ObjectProperty . :X a owl:Class . :r3 rdfs:subPropertyOf :r4 . \
         :r4 rdfs:range :X .",
    ));
}

/// The EL classifier's completeness with an object-property range is not proven outside
/// OWL 2 DL (the cited calculus has no ranges): with a range away from the chain, it
/// answers, marked incomplete, the reason naming the range's property.
#[test]
fn an_irregular_el_answer_where_the_input_states_a_range_is_marked_incomplete() {
    let report = irregular(":r4 a owl:ObjectProperty . :X a owl:Class . :r4 rdfs:range :X .");
    assert_eq!(report.engine, "el");
    assert!(
        has(&report.subsumptions, "A", "D"),
        "{:?}",
        report.subsumptions
    );
    assert!(!report.complete());
    assert!(
        report
            .incomplete
            .iter()
            .any(|why| why.contains("object-property range") && why.contains(&format!("<{EX}r4>"))),
        "{:?}",
        report.incomplete
    );
}

/// The EL classifier doesn't read assertions: with one, the answer stays the DL engines'
/// marked lower bound.
#[test]
fn an_irregular_ontology_with_an_assertion_is_not_answered_by_the_el_classifier() {
    assert_lower_bound(&irregular(":a a :A ."));
}

/// An axiom the EL classifier skips (a single-individual nominal) leaves its answer
/// incomplete: the answer stays the DL engines' marked lower bound.
#[test]
fn an_irregular_ontology_with_an_axiom_the_el_classifier_skips_is_not_answered_by_it() {
    assert_lower_bound(&irregular(":F owl:equivalentClass [ owl:oneOf ( :o ) ] ."));
}

/// [`irregular`] with `extra`, its time left out: two reports of the same ontology are equal.
fn irregular_untimed(extra: &str) -> nrese_store::ClassificationReport {
    let mut report = irregular(extra);
    report.micros = 0;
    report
}

/// Spec (Structural Specification §5.5: of the reserved vocabulary only the nine built-in
/// annotation properties are annotation properties, so `owl:bottomObjectProperty` declared one,
/// in either spelling (Mapping to RDF Graphs, Table 6), is none, and `:r4 rdfs:subPropertyOf
/// owl:bottomObjectProperty` stays a property inclusion: with `:H ⊑ ∃r4.B` it makes `:H`
/// unsatisfiable (Direct Semantics: the bottom property's extension is empty)): the irregular
/// ontology with that inclusion and the declaration classifies as one without the
/// declaration: the same engine, the same answer and the same reasons.
#[test]
fn owl_bottom_object_property_declared_an_annotation_property_classifies_as_without_the_declaration()
 {
    let rest = ":r4 a owl:ObjectProperty . :r4 rdfs:subPropertyOf owl:bottomObjectProperty . \
                :H a owl:Class . \
                :H rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r4 ; owl:someValuesFrom :B ] .";
    let without = irregular_untimed(rest);
    let mut wrong = Vec::new();
    for spelling in super::reserved_names::SPELLINGS {
        let declared =
            irregular_untimed(&format!("owl:bottomObjectProperty a {spelling} . {rest}"));
        if declared != without {
            wrong.push(format!("{spelling}: {declared:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}\nwithout: {without:?}");
}

/// Spec (Structural Specification §5.5 and §10: a user IRI declared an annotation property, in
/// either spelling (Mapping to RDF Graphs, Table 6), is one; its assertions, its range and its
/// sub-property axioms are annotation axioms, which OWL 2 Direct Semantics gives no meaning):
/// the irregular ontology with such a property, an annotation of a class, a range and a
/// sub-property of `rdfs:comment` keeps the EL classifier's answer, as without them: the same
/// engine, the same answer and the same reasons.
#[test]
fn an_irregular_el_ontology_with_a_declared_annotation_property_keeps_the_el_classifiers_answer() {
    let plain = irregular_untimed("");
    assert_eq!(plain.engine, "el");
    let mut wrong = Vec::new();
    for spelling in super::reserved_names::SPELLINGS {
        let annotated = irregular_untimed(&format!(
            ":note a {spelling} . :A :note \"a note\" . :note rdfs:range xsd:string . \
             :note rdfs:subPropertyOf rdfs:comment ."
        ));
        if annotated != plain {
            wrong.push(format!("{spelling}: {annotated:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}\nwithout: {plain:?}");
}

/// A class the EL classifier reads nothing about (named only as a data property's domain)
/// is still classified: below a class equivalent to `owl:Thing`, as every class is.
#[test]
fn an_irregular_el_answer_classifies_a_class_named_only_where_the_el_classifier_reads_nothing() {
    let report = irregular(
        ":dp a owl:DatatypeProperty ; rdfs:domain :X . :T a owl:Class . \
         owl:Thing rdfs:subClassOf :T .",
    );
    assert_eq!(report.engine, "el");
    assert!(report.complete(), "{:?}", report.incomplete);
    assert_eq!(report.equivalent_to_thing, [format!("{EX}T")]);
    assert!(
        has(&report.subsumptions, "X", "T"),
        "{:?}",
        report.subsumptions
    );
}

/// A range on a property below one equivalent to a chain's (`r5 ⊑ r4 ≡ r3`) is below the
/// chain's too: the answer stays the DL engines' marked lower bound.
#[test]
fn an_irregular_ontology_with_a_range_below_a_chains_equivalent_is_not_answered_by_the_el_classifier()
 {
    assert_lower_bound(&irregular(
        ":r4 a owl:ObjectProperty . :r5 a owl:ObjectProperty . :X a owl:Class . \
         :r3 owl:equivalentProperty :r4 . :r5 rdfs:subPropertyOf :r4 ; rdfs:range :X .",
    ));
}

/// An annotation property's domain says nothing about classes, and neither reader reads it:
/// the EL classifier answers, complete, and the domain is no class in its answer.
#[test]
fn an_irregular_el_answer_leaves_an_annotation_propertys_domain_out() {
    let report = irregular(
        ":ap a owl:AnnotationProperty ; rdfs:domain :X . :T a owl:Class . \
         owl:Thing rdfs:subClassOf :T .",
    );
    assert_eq!(report.engine, "el");
    assert!(report.complete(), "{:?}", report.incomplete);
    assert!(
        !has(&report.subsumptions, "X", "T"),
        "{:?}",
        report.subsumptions
    );
}

/// An axiom's reification (`owl:Axiom`) states no axiom for the DL engines, but the EL
/// classifier reads the class axiom its node carries (`X ⊓ Y ⊑ Z`): its answer names
/// classes the DL engines' signature lacks, so neither reading is trusted, and the answer
/// stays the DL engines' marked lower bound.
#[test]
fn an_irregular_ontology_whose_classes_the_two_readers_read_differently_is_not_answered_by_the_el_classifier()
 {
    let report = irregular(
        ":T a owl:Class . owl:Thing rdfs:subClassOf :T . \
         _:ax a owl:Axiom ; owl:intersectionOf ( :X :Y ) ; rdfs:subClassOf :Z .",
    );
    // X is no class to the DL engines: the reification asserts nothing.
    assert!(
        !has(&report.subsumptions, "X", "T"),
        "{:?}",
        report.subsumptions
    );
    assert_lower_bound(&report);
}

/// The EL classifier skips a datatype declared `owl:DataRange` where it reads a class (a
/// data range): it skipped part of the ontology, so the answer stays the DL engines' marked
/// lower bound.
#[test]
fn an_irregular_ontology_whose_data_ranges_the_el_classifier_skips_is_not_answered_by_it() {
    let report =
        irregular(":Dt a owl:DataRange . :Et a owl:DataRange . :Dt owl:equivalentClass :Et .");
    // Dt and Et are datatypes, no classes: no pair between them.
    assert!(
        !has(&report.subsumptions, "Dt", "Et") && !has(&report.subsumptions, "Et", "Dt"),
        "{:?}",
        report.subsumptions
    );
    assert_lower_bound(&report);
}

/// A sub-property of an annotation property OWL 2 builds in (`rdfs:label`) is an annotation
/// axiom, as the ontology reader reads it, a blank subject's included: it says nothing about
/// classes, and the EL classifier's answer stays complete.
#[test]
fn a_sub_property_of_a_built_in_annotation_property_leaves_the_el_answer_complete() {
    let report = irregular("_:x a owl:AnnotationProperty ; rdfs:subPropertyOf rdfs:label .");
    assert_eq!(report.engine, "el");
    assert!(report.complete(), "{:?}", report.incomplete);
}
