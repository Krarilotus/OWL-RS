//! Under `owl2-dl`, a read whose dataset (`FROM`) leaves out the store's default graph, where
//! the inferred statements are kept: what each reader is told, with the bounds' view of the
//! revision not built (the request's time was up first) and built.

use std::sync::Arc;
use std::time::Instant;

use nrese_sparql::GraphAccess;
use nrese_store::{ReadScope, SparqlQueryRequest, StoreService};

use super::{PREFIXES, insert, pipeline};

const EX: &str = "http://example.com/";

/// The reason a read whose dataset leaves out the store's default graph is told.
const OMITTED: &str = "this read's dataset leaves out the store's default graph from its \
                       default graph, where inferred statements are stored; default-graph \
                       patterns see only the selected graphs' asserted statements";

/// The beginning of the reason a read is told when its time was up before the bounds' view.
const TIME_UP: &str = "the request's time was up";

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

/// Spec: under `owl2-dl`, a reader of every graph whose `FROM` leaves out the store's default
/// graph is told `sound-only` with the reason that names the omitted default graph, the same
/// whether the bounds' view of the revision was built or the request's time was up before it
/// (the fallback). A reader of some graphs never reads the bounds and is told `not-certified`
/// both times: under `owl2-dl` its status is withheld, as every graph's axioms decide it.
#[test]
fn a_read_leaving_out_the_default_graph_is_told_so_with_the_bounds_and_without() {
    let dl = pipeline();
    insert(
        &dl,
        "GRAPH :g1 { :Cat rdfs:subClassOf :Animal } GRAPH :g2 { :tom a :Cat }",
    )
    .expect("data");
    let store = dl.store();
    let reader = ReadScope::Graphs(Arc::new(GraphAccess {
        graphs: vec![format!("{EX}g1"), format!("{EX}g2")],
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..GraphAccess::default()
    }));
    let every = "SELECT ?x WHERE { ?x a :Animal }";
    let from = "SELECT ?x FROM :g2 WHERE { ?x a :Animal }";
    let mut seen = Vec::new();
    for built in [false, true] {
        if built {
            // A read of every graph with time to wait builds the revision's view.
            told(store, &ReadScope::All, every, false);
        }
        // Which path: a read of every graph without a dataset says whether the view was there.
        let probe = told(store, &ReadScope::All, every, !built);
        let all = told(store, &ReadScope::All, from, !built);
        let some = told(store, &reader, from, !built);
        println!("DATASET built={built} probe: {probe}");
        println!("DATASET built={built} every graph, FROM :g2: {all}");
        println!("DATASET built={built} some graphs, FROM :g2: {some}");
        seen.push((built, probe, all, some));
    }
    let (_, probe_unbuilt, _, _) = &seen[0];
    let (_, probe_built, _, _) = &seen[1];
    assert!(
        probe_unbuilt.contains(TIME_UP) && !probe_built.contains(TIME_UP),
        "setup: the first reads didn't take the fallback, or the second did: {seen:#?}"
    );
    for (built, _, all, some) in &seen {
        assert!(
            all.starts_with("sound-only; regime=owl2-dl;")
                && all.contains(OMITTED)
                && !all.contains(TIME_UP),
            "every graph, built={built}: {all}"
        );
        assert_eq!(some, "not-certified", "some graphs, built={built}");
    }
}
