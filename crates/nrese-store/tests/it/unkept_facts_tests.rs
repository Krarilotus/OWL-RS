//! Facts the closure can't keep: a triple whose predicate is a blank node, which RDF can't
//! store, derived from an inverse property expression. A store after a history of commits
//! answers as one that took the same data at once, and as a rebuild of it.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationPipeline, MutationTicket, Requester, SparqlQueryRequest,
    SparqlUpdateRequest, StoreConfig, StoreService,
};

use crate::support::in_memory_store_config;

const PREFIXES: &str = "PREFIX : <http://example.com/> \
    PREFIX owl: <http://www.w3.org/2002/07/owl#> \
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

const MODES: [ReasoningMode; 5] = [
    ReasoningMode::Owl2Ql,
    ReasoningMode::Owl2Rl,
    ReasoningMode::Owl2Dl,
    ReasoningMode::OwlHorst,
    ReasoningMode::RdfsPlus,
];

fn store(equality: &str) -> StoreService {
    StoreService::new(StoreConfig {
        equality_by_representatives: equality != "replicate",
        equality_compact: equality == "compact",
        ..in_memory_store_config()
    })
    .expect("store")
}

fn pipeline(store: &StoreService, mode: ReasoningMode) -> MutationPipeline {
    MutationPipeline::new(
        Arc::new(store.clone()),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    )
}

fn update(pipeline: &MutationPipeline, update: &str) {
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}{update}"))),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .expect("applied");
}

/// The answer to `ASK { question }` and whether the read is told complete.
fn ask(store: &StoreService, question: &str) -> (bool, bool) {
    let request = SparqlQueryRequest::all(format!("{PREFIXES}ASK {{ {question} }}"));
    let result = store.execute_query(&request).expect("query");
    let text: String = String::from_utf8_lossy(&result.payload)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let complete = result.completeness.is_some_and(|c| c.complete);
    (text.contains("\"boolean\":true"), complete)
}

/// The store after `first` and then `second` in two commits, after both in one, and after
/// the two commits and a rebuild: what each answers to `question`, and is told.
fn three_ways(
    mode: ReasoningMode,
    equality: &str,
    first: &str,
    second: &str,
    question: &str,
) -> [(bool, bool); 3] {
    let at_once = store(equality);
    update(
        &pipeline(&at_once, mode),
        &format!("INSERT DATA {{ {first} }} ; {second}"),
    );
    let history = store(equality);
    let writer = pipeline(&history, mode);
    update(&writer, &format!("INSERT DATA {{ {first} }}"));
    update(&writer, second);
    let two = ask(&history, question);
    let program = ReasonerConfig::for_mode(mode)
        .materialised_program()
        .expect("program");
    history.rematerialise(program).expect("rebuilt");
    [ask(&at_once, question), two, ask(&history, question)]
}

fn check(first: &str, second: &str, question: &str, holds: impl Fn(ReasoningMode) -> bool) {
    let mut failures = Vec::new();
    for mode in MODES {
        for equality in ["representatives", "replicate", "compact"] {
            let label = format!("{}, {equality}", mode.as_str());
            if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
                let [once, two, rebuilt] = three_ways(mode, equality, first, second, question);
                assert_eq!(once.0, holds(mode), "at once: {once:?}");
                assert_eq!(two, once, "after two commits, against at once");
                assert_eq!(rebuilt, once, "after a rebuild, against at once");
            })) {
                let message = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .unwrap_or_else(|| "unknown panic".to_owned());
                failures.push(format!("{label}: {message}"));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Spec: OWL 2 Structural Specification §9.2.5 and §9.2.6, with the RDF mapping of
/// `ObjectInverseOf` (OWL 2 Mapping to RDF, Table 1): `_:i owl:inverseOf :p` with
/// `_:i rdfs:domain :C` is `ObjectPropertyDomain(ObjectInverseOf(:p) :C)`, so `:a :p :b`
/// gives `:b a :C`, also where the domain comes in a later commit. Every rule set here
/// with prp-inv1 and prp-dom (OWL 2 Profiles §4.3, table 5) derives it.
#[test]
fn an_inverse_expressions_domain_from_a_later_commit_reaches_its_facts() {
    check(
        "_:i owl:inverseOf :p ; rdfs:range :D . :a :p :b .",
        "INSERT { ?i rdfs:domain :C } WHERE { ?i owl:inverseOf :p }",
        ":b a :C",
        |_| true,
    );
}

/// Spec: as above, with `TransitiveObjectProperty(ObjectInverseOf(:p))` (§9.2.13), so `:p`
/// is transitive: `:e :p :a` and `:a :p :b` give `:e :p :b`, the second from a commit
/// that adds data only. OWL 2 QL has no transitivity (OWL 2 Profiles §3), so its rules
/// don't give it.
#[test]
fn a_later_commit_of_data_reaches_an_inverse_expressions_facts() {
    check(
        "_:i owl:inverseOf :p ; a owl:TransitiveProperty . :a :p :b .",
        "INSERT DATA { :e :p :a }",
        ":e :p :b",
        |mode| mode != ReasoningMode::Owl2Ql,
    );
}

/// Spec: OWL 2 Profiles §4.3, table 4, eq-rep-p: a property equal to a blank node `_:x`
/// (`:p owl:sameAs _:x`) gives `:a _:x :b` from `:a :p :b`, and prp-dom then `:a a :C`
/// from `_:x rdfs:domain :C`, also where the domain comes in a later commit. OWL 2 QL's
/// rules apply no equality.
#[test]
fn a_property_equal_to_a_blank_node_reaches_its_facts_from_a_later_commit() {
    check(
        ":p owl:sameAs _:x . :a :p :b .",
        "INSERT { ?x rdfs:domain :C } WHERE { :p owl:sameAs ?x }",
        ":a a :C",
        |mode| mode != ReasoningMode::Owl2Ql,
    );
}
