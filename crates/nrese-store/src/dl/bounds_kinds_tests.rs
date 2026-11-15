//! The RL route's short cut under `owl2-dl`: a commit of assertions keeps the route without
//! reading the ontology again only if it gives no term a kind the reader didn't give it at
//! the last read. The reader types terms by use, assertions included (punning; a property's
//! kind by its objects), so such a commit can change what it reads and so the route. Each
//! test starts from an RL ontology on the route; whether a commit read the ontology again is
//! seen at [`Point::Read`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::seam::{self, Point};
use crate::{
    MutationCommand, MutationPipeline, MutationTicket, Requester, SparqlUpdateRequest, StoreConfig,
    StoreService,
};
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};

const PREFIXES: &str = "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
     PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> ";

/// A store under `owl2-dl` with `data` committed, on the RL route.
fn on_the_route(data: &str) -> (Arc<StoreService>, MutationPipeline) {
    let store = Arc::new(StoreService::new(StoreConfig::in_memory()).unwrap());
    let pipeline = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    );
    commit(&store, &pipeline, data);
    assert_eq!(
        store.dl_bounds().last,
        "rules",
        "the first commit is in OWL 2 RL"
    );
    (store, pipeline)
}

/// Commits `INSERT DATA { data }`: whether it read the ontology again.
fn commit(store: &StoreService, pipeline: &MutationPipeline, data: &str) -> bool {
    update(store, pipeline, &format!("INSERT DATA {{ {data} }}"))
}

/// Commits the update `text`: whether it read the ontology again.
fn update(store: &StoreService, pipeline: &MutationPipeline, text: &str) -> bool {
    let read = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&read);
    seam::set(&store.dl().bounds, Point::Read, move || {
        seen.store(true, Ordering::SeqCst)
    });
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}{text}"))),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .expect("the commit is consistent");
    let read = read.load(Ordering::SeqCst);
    // An unfired hook would fire at a later commit's read.
    seam::set(&store.dl().bounds, Point::Read, || {});
    read
}

/// Two property names, then an equality of them: it makes them individuals too.
#[test]
fn a_later_equality_of_property_names_reads_again() {
    let (store, pipeline) = on_the_route(
        ":p a owl:ObjectProperty , owl:FunctionalProperty . :q a owl:ObjectProperty . \
         :x :q :a , :b .",
    );
    assert!(commit(&store, &pipeline, ":p owl:sameAs :q ."));
}

/// Two properties declared and never used, then an equality of them: it makes them
/// individuals too.
#[test]
fn a_later_equality_of_declared_properties_reads_again() {
    let (store, pipeline) =
        on_the_route(":p a owl:ObjectProperty . :q a owl:DatatypeProperty . :x a :C .");
    assert!(commit(&store, &pipeline, ":p owl:sameAs :q ."));
}

/// Two equal individuals, then a class use of one of them: it makes it a class too.
#[test]
fn a_later_class_use_of_an_equated_individual_reads_again() {
    let (store, pipeline) =
        on_the_route(":a a owl:NamedIndividual ; owl:sameAs :b . :b a owl:NamedIndividual .");
    assert!(commit(&store, &pipeline, ":x a :a ."));
}

/// A class, then a class assertion about it: it makes it an individual too.
#[test]
fn a_later_assertion_that_puns_a_class_reads_again() {
    let (store, pipeline) = on_the_route(":Dog a owl:Class . :rex a :Dog .");
    assert!(commit(&store, &pipeline, ":Dog a :Species ."));
}

/// A class, then a known property's statement about it: it makes it an individual too.
#[test]
fn a_later_statement_about_a_class_reads_again() {
    let (store, pipeline) =
        on_the_route(":Dog a owl:Class . :owns a owl:ObjectProperty . :ann :owns :rex .");
    assert!(commit(&store, &pipeline, ":Dog :owns :rex ."));
}

/// A class, then a known property's value that is the class: it makes it an individual too.
#[test]
fn a_later_value_that_is_a_class_reads_again() {
    let (store, pipeline) =
        on_the_route(":Dog a owl:Class . :owns a owl:ObjectProperty . :ann :owns :rex .");
    assert!(commit(&store, &pipeline, ":ann :owns :Dog ."));
}

/// A key on an undeclared property, then literal values of it: they make it a data
/// property, so the key compares data values, which the RL rules don't do by value
/// (`nrese_owl::ql::reads_data_values`): the ontology leaves the route.
#[test]
fn a_later_literal_of_an_undeclared_key_property_leaves_the_route() {
    let (store, pipeline) = on_the_route(":P owl:hasKey ( :ssn ) . :a a :P . :b a :P .");
    assert!(commit(
        &store,
        &pipeline,
        ":a :ssn \"1\"^^xsd:integer . :b :ssn \"01\"^^xsd:integer .",
    ));
    assert_ne!(store.dl_bounds().last, "rules");
}

/// An undeclared property, data only by its one literal, with a datatype range; then that
/// literal deleted: the range's property is then read as an object property, whose range
/// can't be a datatype (a reserved name, not read), so the ontology leaves the route.
#[test]
fn deleting_the_literal_that_makes_a_property_data_valued_leaves_the_route() {
    let (store, pipeline) = on_the_route(":p rdfs:range xsd:integer . :a :p 1 .");
    assert!(update(&store, &pipeline, "DELETE DATA { :a :p 1 . }"));
    assert_ne!(store.dl_bounds().last, "rules");
}

/// What keeps the short cut: an assertion with a known class, a known property of its
/// object's kind, and plain individuals.
#[test]
fn an_ordinary_assertion_keeps_the_short_cut() {
    let (store, pipeline) = on_the_route(
        ":Dog rdfs:subClassOf :Animal . :owns a owl:ObjectProperty . :age a owl:DatatypeProperty . \
         :ann :owns :rex ; :age 30 . :rex a :Dog .",
    );
    assert!(!commit(
        &store,
        &pipeline,
        ":tom a :Dog . :ann :owns :tom ; :age 31 . :tom owl:sameAs :tommy ."
    ));
    assert_eq!(store.dl_bounds().last, "rules");
}

/// A property's first use reads again once; after it, the signature knows the property.
#[test]
fn the_commit_after_a_property_s_first_use_keeps_the_short_cut() {
    let (store, pipeline) = on_the_route(":Dog rdfs:subClassOf :Animal . :rex a :Dog .");
    assert!(commit(&store, &pipeline, ":rex :likes :bone ."));
    assert!(!commit(
        &store,
        &pipeline,
        ":rex :likes :ball . :tom :likes :bone ."
    ));
    assert_eq!(store.dl_bounds().last, "rules");
}

/// An annotation gives its terms no kind under the Direct Semantics (the reader reads
/// past it), so a known annotation property never reads again: built in (`rdfs:label`) or
/// declared, about an individual, a class or a property.
#[test]
fn annotation_assertions_keep_the_short_cut() {
    let (store, pipeline) = on_the_route(
        ":note a owl:AnnotationProperty . :Dog a owl:Class . :owns a owl:ObjectProperty . \
         :rex a :Dog ; rdfs:label \"rex\" .",
    );
    assert!(!commit(
        &store,
        &pipeline,
        ":x rdfs:label \"x\" . :Dog rdfs:label \"Dog\" ; :note \"a class\" . \
         :owns :note :Dog . :rex :note \"a dog\" ."
    ));
    assert_eq!(store.dl_bounds().last, "rules");
}
