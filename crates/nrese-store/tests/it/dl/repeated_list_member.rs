use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::dl::entailment::Entailed;
use nrese_store::{
    ConsistencyStatus, MutationError, MutationPipeline, QlRewritingMode, ReadScope, RejectKind,
    StoreService,
};

use super::{PREFIXES, insert, pipeline};
use crate::support::{in_memory_store_config, load_past_the_pipeline};

const DIFFERENT: &str = "[] a owl:AllDifferent ; owl:members ( :a :a :b ) .";
const CLASSES: &str = "[] a owl:AllDisjointClasses ; owl:members ( :A :A :B ) .";
const RDF: &str = "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>";

fn rl_pipeline(rewriting: QlRewritingMode) -> MutationPipeline {
    let store = StoreService::new(nrese_store::StoreConfig {
        ql_rewriting: rewriting,
        ..in_memory_store_config()
    })
    .unwrap();
    MutationPipeline::new(
        Arc::new(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    )
}

fn rebuilt(store: &StoreService, data: &str) -> (ConsistencyStatus, bool) {
    load_past_the_pipeline(store, &format!("{PREFIXES}\n{data}"));
    let program = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
        .materialised_program()
        .unwrap();
    store.rematerialise(program).expect("rebuild");
    (store.consistency(), store.is_quarantined())
}

fn refused(result: &Result<u64, MutationError>) -> bool {
    matches!(result, Err(MutationError::Rejected(reject)) if reject.kind == RejectKind::Inconsistent)
}

/// OWL 2 Structural Specification §2.3 and Direct Semantics Tables 5–7. Removing
/// duplicates leaves the distinct operands in force, both on writes and after rebuilding.
#[test]
fn direct_semantics_reads_each_distinct_operand_once() {
    let mut failures = Vec::new();
    for data in [
        DIFFERENT,
        "[] a owl:AllDifferent ; owl:distinctMembers ( :a :a :b ) .",
        "[] a owl:AllDifferent ; owl:members ( :a :b ) .",
        &format!("{CLASSES} :x a :A ."),
        "[] a owl:AllDisjointClasses ; owl:members ( :A :B ) . :x a :A .",
        ":p a owl:ObjectProperty . :q a owl:ObjectProperty .
         [] a owl:AllDisjointProperties ; owl:members ( :p :p :q ) . :x :p :y .",
        ":p a owl:ObjectProperty . :q a owl:ObjectProperty .
         [] a owl:AllDisjointProperties ; owl:members ( :p :q ) . :x :p :y .",
        ":p a owl:DatatypeProperty . :q a owl:DatatypeProperty .
         [] a owl:AllDisjointProperties ; owl:members ( :p :p :q ) . :x :p 1 .",
        ":C owl:disjointUnionOf ( :A :A :B ) . :x a :A .",
    ] {
        let write = insert(&pipeline(), data);
        let (status, quarantine) = rebuilt(pipeline().store(), data);
        eprintln!("{data} => write {write:?}, rebuild {status:?}, quarantine {quarantine}");
        if write.is_err() || matches!(status, ConsistencyStatus::Inconsistent { .. }) || quarantine
        {
            failures.push(format!(
                "{data}: write {write:?}, rebuild {status:?}, quarantine {quarantine}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "expected admission and a clean rebuild: {failures:#?}"
    );
}

#[test]
fn direct_semantics_preserves_disjointness_between_distinct_operands() {
    let mut failures = Vec::new();
    for data in [
        ":a owl:differentFrom :a .",
        "[] a owl:AllDifferent ; owl:members ( :a :b ) . :a owl:sameAs :b .",
        &format!("{DIFFERENT} :a owl:sameAs :b ."),
        &format!("{CLASSES} :x a :A, :B ."),
        ":p a owl:ObjectProperty . :q a owl:ObjectProperty .
         [] a owl:AllDisjointProperties ; owl:members ( :p :p :q ) . :x :p :y ; :q :y .",
    ] {
        let write = insert(&pipeline(), data);
        if !refused(&write) {
            failures.push(format!("{data}: {write:?}"));
        }
    }
    let dl = pipeline();
    insert(&dl, ":C owl:disjointUnionOf ( :A :A ) .").expect("an empty class can be defined");
    let write = insert(&dl, ":c a :C .");
    if !refused(&write) {
        failures.push(format!(
            "instance of the empty one-operand union: {write:?}"
        ));
    }
    assert!(
        failures.is_empty(),
        "expected an inconsistency: {failures:#?}"
    );
}

#[test]
fn direct_semantics_still_refuses_lists_with_only_one_distinct_operand() {
    for data in [
        "[] a owl:AllDifferent ; owl:members ( :a :a ) .",
        "[] a owl:AllDisjointClasses ; owl:members ( :A :A ) . :x a :A .",
        ":p a owl:ObjectProperty .
         [] a owl:AllDisjointProperties ; owl:members ( :p :p ) . :x :p :y .",
    ] {
        let write = insert(&pipeline(), data);
        assert!(refused(&write), "{data}: {write:?}");
        let (status, quarantine) = rebuilt(pipeline().store(), data);
        assert!(
            matches!(status, ConsistencyStatus::Inconsistent { .. }),
            "{data}: {status:?}"
        );
        assert!(
            quarantine,
            "{data}: a rebuild must quarantine the inconsistency"
        );
    }
}

/// Equality between distinct operands still clashes after a bulk load and rebuild.
#[test]
fn same_as_members_still_clash_after_a_rebuild() {
    let mut failures = Vec::new();
    for representatives in [true, false] {
        for data in [
            "[] a owl:AllDifferent ; owl:members ( :a :b :c ) . :a owl:sameAs :b .",
            "[] a owl:AllDifferent ; owl:members ( :a :a :b :c ) . :b owl:sameAs :c .",
        ] {
            let dl = super::pipeline_of(nrese_store::StoreConfig {
                equality_by_representatives: representatives,
                ..in_memory_store_config()
            });
            let (status, quarantine) = rebuilt(dl.store(), data);
            if !matches!(status, ConsistencyStatus::Inconsistent { .. }) || !quarantine {
                failures.push(format!(
                    "representatives={representatives}, {data}: {status:?}, quarantine {quarantine}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "sameAs operands must clash: {failures:#?}"
    );
}

/// An alias elsewhere in the ontology introduces no second operand into a list.
#[test]
fn an_alias_outside_the_list_does_not_clash_after_a_rebuild() {
    let (status, quarantine) = rebuilt(
        pipeline().store(),
        "[] a owl:AllDifferent ; owl:members ( :a :a :c ) . :a owl:sameAs :b .",
    );
    assert_eq!(status, ConsistencyStatus::Consistent);
    assert!(!quarantine);
}

#[test]
fn same_as_members_clash_past_the_walk_limit_after_a_rebuild() {
    use std::fmt::Write;

    // Every one of the 65 walks has distinct operands (:a :b :c).
    let mut data = format!(
        "{RDF}\n[] a owl:AllDifferent ; owl:members _:head .
         _:head rdf:first :a . _:end rdf:first :c ; rdf:rest rdf:nil .
         :a owl:sameAs :b ."
    );
    for i in 0..65 {
        write!(
            data,
            "_:head rdf:rest _:tail{i} . _:tail{i} rdf:first :b ; rdf:rest _:end ."
        )
        .unwrap();
    }
    let (status, quarantine) = rebuilt(pipeline().store(), &data);
    assert!(quarantine, "the :a/:b pair must clash: {status:?}");
}

#[test]
fn a_branching_list_can_still_have_a_single_operand_walk() {
    use std::fmt::Write;

    // The first walk is (:a :a). Another 64 walks read (:a :b), exceeding the
    // compiler's walk limit even though they share the same two term sequences.
    let mut data = "[] a owl:AllDifferent ; owl:members _:head .
         _:head rdf:first :a ; rdf:rest _:repeated .
         _:repeated rdf:first :a ; rdf:rest rdf:nil ."
        .to_owned();
    for i in 0..64 {
        write!(
            data,
            "_:head rdf:rest _:tail{i} . _:tail{i} rdf:first :b ; rdf:rest rdf:nil ."
        )
        .unwrap();
    }
    let write = super::update(&pipeline(), &format!("{RDF} INSERT DATA {{ {data} }}"));
    // A branching RDF list is outside the structural mapping. Pin the rule that
    // found the clash even when the public consistency verdict is Undecided.
    assert!(
        matches!(&write, Err(MutationError::Rejected(reject))
            if reject.explanation.as_ref().is_some_and(|e| e.violated_constraint == "eq-diff2")),
        "the (:a :a) walk clashes: {write:?}"
    );

    // Extending the repeated walk to (:a :a :b) makes every walk mixed.
    let data = data.replace(
        "_:repeated rdf:first :a ; rdf:rest rdf:nil",
        "_:repeated rdf:first :a ; rdf:rest _:tail0",
    );
    let (status, quarantine) = rebuilt(pipeline().store(), &format!("{RDF}\n{data}"));
    assert!(!quarantine, "no walk disjoins a from itself: {status:?}");
}

#[test]
fn direct_entailment_keeps_the_difference_between_the_surviving_individuals() {
    let dl = pipeline();
    insert(&dl, DIFFERENT).expect("distinct individuals can coexist");
    assert_eq!(
        dl.store()
            .entails_dl(&super::entailment::turtle(":a owl:differentFrom :b ."))
            .expect("direct entailment")
            .answer,
        Entailed::Yes
    );
}

#[test]
fn classification_in_owl2_rl_reads_disjointness_by_direct_semantics() {
    let rl = rl_pipeline(QlRewritingMode::Off);
    insert(&rl, &format!(":A a owl:Class . :B a owl:Class . {CLASSES}")).unwrap();
    let report = rl.store().classify(&ReadScope::All).unwrap();
    assert!(report.complete(), "{report:?}");
    assert_eq!(report.consistency, nrese_store::dl::Verdict::Consistent);
    assert!(
        !report
            .unsatisfiable
            .iter()
            .any(|iri| iri == "http://example.com/A"),
        "{report:?}"
    );
}

/// OWL 2 Profiles, Table 4 (eq-diff2): different list positions i < j are paired,
/// and a resource is equal to itself, so the RDF-Based Semantics finds a clash.
#[test]
fn owl2_rl_checks_distinct_list_positions() {
    let write = insert(&rl_pipeline(QlRewritingMode::Off), DIFFERENT);
    assert!(refused(&write), "{write:?}");
}

#[test]
fn owl2_rl_rewriting_reports_the_hazard_from_a_repeated_existential() {
    let rl = rl_pipeline(QlRewritingMode::On);
    insert(
        &rl,
        ":r a owl:ObjectProperty . :A a owl:Class . :B a owl:Class .
         :C a owl:Class . :D a owl:Class .
         :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r ; owl:someValuesFrom :D ] .
         [] a owl:AllDisjointClasses ; owl:members ( _:c _:c :B ) .
         _:c a owl:Restriction ; owl:onProperty :r ; owl:someValuesFrom :C .
         :x a :A .",
    )
    .expect("a qualified existential in the schema");
    let answer = rl
        .store()
        .execute_query_str(&format!("{PREFIXES}SELECT ?x ?y WHERE {{ ?x :r ?y }}"))
        .unwrap();
    let report = answer.ql.expect("QL rewriting under owl2-rl").completeness;
    assert!(!report.complete, "{report:?}");
    assert!(
        report
            .reasons
            .iter()
            .any(|reason| reason.text.contains("qualified someValuesFrom")),
        "{report:?}"
    );
}
