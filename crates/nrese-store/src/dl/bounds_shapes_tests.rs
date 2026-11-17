//! The RL route's short cut with SHACL content under `owl2-dl`: the kinds the route keeps
//! remember the SHACL predicates and classes the reader read as SHACL content, so a later
//! shape write that uses only those keeps the route without reading the ontology again; a
//! first use of one, or an OWL use of one, reads it again. After each commit the route
//! agrees with a fresh read of the same revision ([`super::afresh`]). Whether a commit read
//! the ontology again is seen at [`Point::Read`].

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
     PREFIX sh: <http://www.w3.org/ns/shacl#> ";

/// An OWL 2 RL ontology the shapes stand beside.
const ONTOLOGY: &str = ":Student rdfs:subClassOf :Person . :knows a owl:ObjectProperty . \
     :ann a :Student ; :knows :bob . :bob a :Person .";

/// A store under `owl2-dl` with [`ONTOLOGY`] committed, on the RL route.
fn on_the_route() -> (Arc<StoreService>, MutationPipeline) {
    let store = Arc::new(StoreService::new(StoreConfig::in_memory()).unwrap());
    let pipeline = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    );
    update(&store, &pipeline, &format!("INSERT DATA {{ {ONTOLOGY} }}"));
    assert_eq!(
        store.dl_bounds().last,
        "rules",
        "the ontology is in OWL 2 RL"
    );
    (store, pipeline)
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

/// The route after the last commit, `rules` or another, and whether a fresh read of the same
/// revision takes the rules' route: they must agree.
fn route(store: &StoreService) -> (&'static str, bool) {
    let last = store.dl_bounds().last;
    let snapshot = store.read_snapshot(None).unwrap();
    (last, super::afresh(store, &snapshot).1.is_some())
}

/// Spec (the route's kept kinds, `Kinds::keep`: a commit that gives no term a new kind keeps
/// the route without reading the ontology again): on an OWL 2 RL ontology, a first
/// shape write reads the ontology again and keeps the route; a second write of the same SHACL
/// vocabulary, a shape without lists, keeps it without a read; so does a deletion of one of
/// its statements; an OWL axiom on a SHACL term (`sh:path` declared an object property) reads
/// again and leaves the route. Each step's route agrees with a fresh read of its revision.
#[test]
fn shape_writes_keep_the_route_and_an_axiom_on_a_shacl_term_leaves_it() {
    let (store, pipeline) = on_the_route();
    let steps = [
        (
            "first shape",
            "INSERT DATA { :S1 a sh:NodeShape ; sh:targetClass :Person ; sh:property :P1 . \
             :P1 sh:path :knows ; sh:class :Person . }",
            true,
            true,
        ),
        (
            "second shape",
            "INSERT DATA { :S2 a sh:NodeShape ; sh:targetClass :Student ; sh:property :P2 . \
             :P2 sh:path :knows ; sh:class :Person . }",
            false,
            true,
        ),
        (
            "deletion",
            "DELETE DATA { :S1 sh:targetClass :Person . }",
            false,
            true,
        ),
        (
            "axiom on sh:path",
            "INSERT DATA { sh:path a owl:ObjectProperty . }",
            true,
            false,
        ),
    ];
    let mut wrong = Vec::new();
    for (what, text, reads, routed) in steps {
        let read = update(&store, &pipeline, text);
        let (last, fresh) = route(&store);
        let line = format!("{what}: route {last}, fresh read routed {fresh}, read again {read}");
        eprintln!("{line}");
        if (last == "rules") != routed || fresh != routed || read != reads {
            wrong.push(line);
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (OWL 2 Structural Specification §5.9, punning: a SHACL term used as an individual is
/// punned; the kept kinds take SHACL terms as names, with no exemption of the namespace): with
/// shapes on the route, a later commit that makes `sh:path` an individual (`sh:path owl:sameAs :alias`)
/// reads the ontology again and leaves the route, as a fresh read of the revision does.
#[test]
fn a_later_equality_on_a_shacl_predicate_reads_again_and_leaves_the_route() {
    let (store, pipeline) = on_the_route();
    update(
        &store,
        &pipeline,
        "INSERT DATA { :S1 a sh:NodeShape ; sh:targetClass :Person ; sh:property :P1 . \
         :P1 sh:path :knows . }",
    );
    let read = update(
        &store,
        &pipeline,
        "INSERT DATA { sh:path owl:sameAs :alias . }",
    );
    let (last, fresh) = route(&store);
    eprintln!("route {last}, fresh read routed {fresh}, read again {read}");
    assert_ne!(last, "rules");
    assert!(!fresh, "a fresh read took the rules' route");
    assert!(read, "the commit read the ontology again");
}
