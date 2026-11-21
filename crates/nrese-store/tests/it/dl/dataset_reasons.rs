//! Under `owl2-dl`, a read whose dataset (`FROM`) leaves out the store's default graph, where
//! the inferred statements are kept: what each reader is told, with the bounds' view of the
//! revision not built (the request's time was up first) and built, and inside a transaction.

use std::sync::Arc;
use std::time::Instant;

use nrese_sparql::GraphAccess;
use nrese_store::{
    CancellationToken, MutationPipeline, ReadScope, SparqlQueryRequest, StatementOp,
    StatementsRequest, StoreService,
};

use super::{PREFIXES, insert, pipeline};

const EX: &str = "http://example.com/";

/// The reason a read whose dataset leaves out the store's default graph is told.
const OMITTED: &str = "this read's dataset leaves out the store's default graph from its \
                       default graph, where inferred statements are stored; default-graph \
                       patterns see only the selected graphs' asserted statements";

/// The beginning of the reason a read is told when its time was up before the bounds' view.
const TIME_UP: &str = "the request's time was up";

/// What a read without the bounds' view answers over, where it reads the closure.
const OVER_THE_CLOSURE: &str = "answers over the RL closure";

/// The pending reason's end for a read of the committed closure, and for one leaving it out.
const PENDING_CLOSURE: &str =
    "aren't reasoned over before the commit (the committed closure, no DL bounds)";
const PENDING_NO_BOUNDS: &str = "aren't reasoned over before the commit (no DL bounds)";

/// `:tom a :Animal` follows from `:g1`'s axiom and `:g2`'s fact, into the default graph.
fn store() -> MutationPipeline {
    let dl = pipeline();
    insert(
        &dl,
        "GRAPH :g1 { :Cat rdfs:subClassOf :Animal } GRAPH :g2 { :tom a :Cat }",
    )
    .expect("data");
    dl
}

/// The `NRESE-Completeness` header `scope` is told for `query` (over [`PREFIXES`]), its
/// request's time up from the start where `time_up`; a refusal as `refused: …`.
fn told(store: &StoreService, scope: &ReadScope, query: &str, time_up: bool) -> String {
    let mut request = SparqlQueryRequest::new(format!("{PREFIXES}{query}"), scope.clone());
    if time_up {
        request.deadline = Some(Instant::now());
    }
    match store.execute_query(&request) {
        Ok(result) => result
            .completeness
            .map_or_else(|| "none".to_owned(), |status| status.header()),
        Err(error) => format!("refused: {error}"),
    }
}

const EVERY: &str = "SELECT ?x WHERE { ?x a :Animal }";
const FROM_G2: &str = "SELECT ?x FROM :g2 WHERE { ?x a :Animal }";

/// Spec: under `owl2-dl`, a reader of every graph whose `FROM` leaves out the store's default
/// graph is told `sound-only` with the reason that names the omitted default graph, with the
/// bounds' view of the revision built and without it; without it, beside the reason it has
/// none (the request's time was up), which isn't replaced, and which says nothing of an RL
/// closure the read doesn't reach. A reader of some graphs never
/// reads the bounds and is told `not-certified` both times: under `owl2-dl` its status is
/// withheld, as every graph's axioms decide it. Every read here has its time up from the
/// start; a read with time to wait builds the view between the two rounds, and a read of
/// every graph without a dataset proves which round had it.
#[test]
fn a_read_leaving_out_the_default_graph_is_told_so_with_the_bounds_and_without() {
    let dl = store();
    let store = dl.store();
    let reader = ReadScope::Graphs(Arc::new(GraphAccess {
        graphs: vec![format!("{EX}g1"), format!("{EX}g2")],
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..GraphAccess::default()
    }));
    let mut seen = Vec::new();
    for built in [false, true] {
        if built {
            told(store, &ReadScope::All, EVERY, false);
        }
        let probe = told(store, &ReadScope::All, EVERY, true);
        let all = told(store, &ReadScope::All, FROM_G2, true);
        let some = told(store, &reader, FROM_G2, true);
        println!("DATASET built={built} probe: {probe}");
        println!("DATASET built={built} every graph, FROM :g2: {all}");
        println!("DATASET built={built} some graphs, FROM :g2: {some}");
        seen.push((built, probe, all, some));
    }
    let (_, probe_unbuilt, _, _) = &seen[0];
    let (_, probe_built, _, _) = &seen[1];
    assert!(
        probe_unbuilt.contains(TIME_UP) && probe_built == "complete; regime=owl2-dl",
        "setup: the view was there in the first round, or not in the second: {seen:#?}"
    );
    for (built, _, all, some) in &seen {
        assert!(
            all.starts_with("sound-only; regime=owl2-dl;")
                && all.contains(OMITTED)
                && all.contains(TIME_UP) != *built
                && !all.contains(OVER_THE_CLOSURE),
            "every graph, built={built}: {all}"
        );
        assert_eq!(some, "not-certified", "some graphs, built={built}");
    }
}

/// Spec: a read whose `FROM` leaves out the store's default graph but that reads it by
/// `GRAPH` under another store's name for it reads the closure: without the bounds' view it
/// is told why (the request's time was up) and that it answers over the RL closure, and
/// nothing of an omitted default graph.
#[test]
fn a_read_of_the_default_graph_by_graph_keeps_the_fallbacks_reason() {
    let dl = store();
    let query = "SELECT ?x FROM :g2 WHERE { GRAPH <urn:x-arq:DefaultGraph> { ?x a :Animal } }";
    let header = told(dl.store(), &ReadScope::All, query, true);
    println!("DATASET alias, no view: {header}");
    assert!(
        header.starts_with("sound-only; regime=owl2-dl;")
            && header.contains(TIME_UP)
            && header.contains(OVER_THE_CLOSURE)
            && !header.contains(OMITTED),
        "{header}"
    );
}

/// The `NRESE-Completeness` header of `query` (over [`PREFIXES`]) read inside a transaction
/// with one operation pending, an unrelated addition.
fn pending(store: &StoreService, query: &str) -> String {
    let prepared = store
        .prepare_query(&SparqlQueryRequest::all(format!("{PREFIXES}{query}")))
        .expect("prepared");
    let ops = vec![StatementOp::Add {
        data: nrese_store::RdfPayload {
            payload: b"<http://example.com/kit> <http://example.com/note> \"k\" ."
                .to_vec()
                .into(),
            format: nrese_store::GraphResultFormat::NTriples,
            base_iri: None,
        },
        contexts: Vec::new(),
    }];
    let (mut out, mut status) = (Vec::new(), None);
    store
        .run_query_pending_reporting(
            &StatementsRequest::new(ops),
            &prepared,
            &CancellationToken::new(),
            &mut out,
            |s, _| {
                status = s;
                None
            },
        )
        .expect("answered");
    status.map_or_else(|| "none".to_owned(), |s| s.header())
}

/// Spec: inside a transaction, a read whose `FROM` leaves out the store's default graph is
/// told that its pending operations aren't reasoned over and that it has no DL bounds, and
/// that it leaves out the default graph; nothing of a committed closure it doesn't read.
#[test]
fn a_pending_read_leaving_out_the_default_graph_names_no_committed_closure() {
    let dl = store();
    let header = pending(dl.store(), FROM_G2);
    println!("DATASET pending: {header}");
    assert!(
        header.contains(PENDING_NO_BOUNDS)
            && header.contains(OMITTED)
            && !header.contains("committed closure"),
        "{header}"
    );
}

/// Spec: inside a transaction, a read whose `FROM` leaves out the store's default graph but
/// that reads it by `GRAPH` under another store's name for it is told that its pending
/// operations aren't reasoned over, over the committed closure, with no DL bounds.
#[test]
fn a_pending_read_of_the_default_graph_by_graph_keeps_the_pending_cause() {
    let dl = store();
    let query = "SELECT ?x FROM :g2 WHERE { GRAPH <urn:x-arq:DefaultGraph> { ?x a :Animal } }";
    let header = pending(dl.store(), query);
    println!("DATASET pending alias: {header}");
    assert!(header.contains(PENDING_CLOSURE), "{header}");
}
