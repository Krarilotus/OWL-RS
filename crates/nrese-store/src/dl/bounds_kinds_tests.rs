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
     PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
     PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> ";

/// A store of `config` under `owl2-dl`, and its pipeline.
fn opened(config: StoreConfig) -> (Arc<StoreService>, MutationPipeline) {
    let store = Arc::new(StoreService::new(config).unwrap());
    let pipeline = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    );
    (store, pipeline)
}

/// A store under `owl2-dl` with `data` committed, on the RL route.
fn on_the_route(data: &str) -> (Arc<StoreService>, MutationPipeline) {
    let (store, pipeline) = opened(StoreConfig::in_memory());
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

/// `ASK { pattern }`: its answer and its status, as `"<status> <reasons>"`.
fn ask(store: &StoreService, pattern: &str) -> (bool, String) {
    let request = crate::SparqlQueryRequest::all(format!("{PREFIXES} ASK {{ {pattern} }}"));
    let prepared = store.prepare_query(&request).unwrap();
    let (mut out, mut status) = (Vec::new(), None);
    store
        .run_query_reporting(
            &prepared,
            &crate::CancellationToken::new(),
            &mut out,
            |s, _| {
                status = s;
                None
            },
        )
        .unwrap();
    let status = status.map_or("none".to_owned(), |s| {
        format!("{} {:?}", s.as_str(), s.reasons)
    });
    (String::from_utf8(out).unwrap().contains("true"), status)
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
/// (`nrese_owl::ql::reads_data_values`): `:a` and `:b`, one key value in two lexical forms,
/// are the same individual, so `:b :p :x` holds and is answered; the commit read the
/// ontology again, and the ontology left the route.
#[test]
fn a_later_literal_of_an_undeclared_key_property_leaves_the_route() {
    let (store, pipeline) = on_the_route(":Person owl:hasKey ( :ssn ) . :p a owl:ObjectProperty .");
    let read = commit(
        &store,
        &pipeline,
        ":a a :Person ; :ssn 1 ; :p :x . :b a :Person ; :ssn \"01\"^^xsd:integer .",
    );
    let (holds, status) = ask(&store, ":b :p :x");
    eprintln!("ASK {{ :b :p :x }}: {holds}, {status}");
    assert!(holds, "{status}");
    assert!(
        status.starts_with("complete") || status.starts_with("sound-only"),
        "{status}"
    );
    assert!(read, "the commit read the ontology again");
    assert_ne!(store.dl_bounds().last, "rules");
}

/// An undeclared property, data only by its one literal, with a datatype range; then that
/// literal deleted: the range's property is then read as an object property, whose range
/// can't be a datatype (a reserved name, not read), so the ontology leaves the route, and
/// the reader reports the range it left out.
#[test]
fn deleting_the_literal_that_makes_a_property_data_valued_leaves_the_route() {
    let (store, pipeline) = on_the_route(":p rdfs:range xsd:integer . :a :p 1 .");
    assert!(update(&store, &pipeline, "DELETE DATA { :a :p 1 . }"));
    assert_ne!(store.dl_bounds().last, "rules");
    let snapshot = store.read_snapshot(None).unwrap();
    let budget = Arc::new(nrese_exec::Budget::unlimited());
    let read = super::source::read_snapshot(&snapshot, budget).unwrap();
    assert!(
        read.diagnostics
            .iter()
            .any(|d| matches!(d, nrese_owl::Diagnostic::Reserved { .. })),
        "{:?}",
        read.diagnostics
    );
}

/// A name of the reserved vocabulary used as an individual is OWL 2 Full, which the reader
/// leaves out (`nrese_owl::diagnostics::reserved_name`): a commit that so uses one reads
/// again, and the ontology leaves the route. A built-in annotation property already
/// known, on either side of an equality; a class of the vocabulary; a reserved value of a
/// known property.
#[test]
fn a_later_individual_use_of_a_reserved_name_reads_again() {
    for (data, later) in [
        (
            ":a a :C ; rdfs:label \"hello\" .",
            "rdfs:label owl:sameAs :alias .",
        ),
        (
            ":a a :C ; rdfs:label \"hello\" .",
            ":alias owl:sameAs rdfs:label .",
        ),
        (":a a :C .", "owl:Class owl:sameAs :alias ."),
        (
            ":owns a owl:ObjectProperty . :ann :owns :rex .",
            ":ann :owns rdf:nil .",
        ),
    ] {
        let (store, pipeline) = on_the_route(data);
        assert!(commit(&store, &pipeline, later), "{later}");
        assert_ne!(store.dl_bounds().last, "rules", "{later}");
    }
}

/// The control: the store reopened on that data reads it afresh, off the route as the
/// commit that read again left it.
#[test]
fn a_reopened_store_reads_a_reserved_name_as_an_individual_alike() {
    let dir = tempfile::tempdir().unwrap();
    {
        let (store, pipeline) = opened(StoreConfig::on_disk(dir.path()));
        commit(&store, &pipeline, ":a a :C ; rdfs:label \"hello\" .");
        commit(&store, &pipeline, "rdfs:label owl:sameAs :alias .");
    }
    let (store, _pipeline) = opened(StoreConfig::on_disk(dir.path()));
    assert_ne!(store.dl_bounds().last, "rules");
}

/// The built-in annotation properties are the reader's vocabulary, interned at every read
/// (`source::intern_vocabulary`): a store's first `rdfs:label`, then its first
/// `rdfs:comment`, read nothing again.
#[test]
fn a_first_built_in_annotation_keeps_the_short_cut() {
    let (store, pipeline) = on_the_route(":Dog a owl:Class . :rex a :Dog .");
    assert!(!commit(&store, &pipeline, ":rex rdfs:label \"rex\" ."));
    assert!(!commit(&store, &pipeline, ":Dog rdfs:comment \"a dog\" ."));
    assert_eq!(store.dl_bounds().last, "rules");
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
