//! The guard of the request's memory envelope (execution-core.md, "Execution and resource
//! lifetime"): a DL query that would exceed its envelope through any of its three ends,
//! the bounds' evaluations (the query budget), the exact services' compilation scratch
//! or their engines (the DL task budget), ends with its status within it: an error for
//! the bounds, unresolved candidates for the others. Everything it held returns to the
//! envelope and the catalog's budget, and the store answers the next request in full.

use super::*;
use nrese_dl::limits::Limit;
use nrese_exec::Envelope;

/// How a request ended under its envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum End {
    /// The bounds' evaluations: the query's memory limit error.
    Bounds,
    /// The exact services' compilation scratch: candidates unresolved before any search.
    Compilation,
    /// The exact services' engines: candidates unresolved, the search gave up.
    Engines,
    /// Within the envelope: the candidate proved.
    Answered,
}

/// A store under `owl2-dl` whose query `?x a :D` has one candidate the exact services
/// prove (`w`), over an ABox of `filler` more individuals that both bounds answer and
/// every probe compiles.
fn store(filler: usize) -> Arc<StoreService> {
    use crate::{
        MutationCommand, MutationPipeline, MutationTicket, Requester, SparqlUpdateRequest,
    };
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    let store = Arc::new(
        StoreService::new(crate::StoreConfig {
            total_query_memory_bytes: 1 << 30,
            ..crate::StoreConfig::in_memory()
        })
        .unwrap(),
    );
    let pipeline = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    );
    let mut data = String::from(
        ":A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] . :B rdfs:subClassOf :D . \
         :C rdfs:subClassOf :D . :y a :B . :w a [ owl:unionOf ( :B :C ) ] . ",
    );
    for i in 0..filler {
        data.push_str(&format!(":f{i} a :B ; :knows :f{} . ", i + 1));
    }
    let update = format!(
        "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#> \
         PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> INSERT DATA {{ {data} }}"
    );
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(update)),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap();
    store
}

/// Runs the query within `memory` and says how it ended; checks that it ended within the
/// envelope and returned all it held.
fn run(store: &StoreService, memory: &Envelope) -> End {
    let prepared = store
        .prepare_query(&crate::SparqlQueryRequest::all(
            "SELECT ?x { ?x a <urn:test:D> }",
        ))
        .unwrap();
    let end = match answer(
        store,
        &prepared,
        &CancellationToken::new(),
        memory,
        DlAnswers::CertainWhereComplete,
    ) {
        Err(error) => {
            assert!(error.is_memory_limit(), "{error}");
            End::Bounds
        }
        Ok(Outcome::Answers(_, status, detail)) => {
            let reasons: Vec<&str> = status.reasons.iter().map(|r| r.text.as_str()).collect();
            if detail.proved == 1 {
                assert!(status.complete, "{reasons:?}");
                End::Answered
            } else {
                // The envelope named as the budget reached: by an engine's run (it gave
                // up), or before one (compiling).
                let envelope = |r: &&&str| Limit::named_in(r) == Some(Limit::Envelope);
                let why = reasons.iter().find(envelope);
                match why {
                    Some(why) if why.contains("gave up: ") => End::Engines,
                    Some(_) => End::Compilation,
                    None => panic!("unresolved for another reason: {reasons:?}"),
                }
            }
        }
        Ok(Outcome::Stream(status, ..)) => panic!("the bounds decide this query: {status:?}"),
    };
    assert!(memory.peak() <= memory.limit(), "{end:?} past the envelope");
    assert_eq!(
        memory.used(),
        0,
        "{end:?}: the request returned what it held"
    );
    end
}

#[test]
fn a_request_past_its_envelope_through_any_end_ends_within_it() {
    let store = store(2_000);
    let catalog = store.runtime().query_memory().expect("a catalog budget");
    let unlimited = store.envelope(None);
    assert_eq!(run(&store, &unlimited), End::Answered);
    let needed = unlimited.peak();
    assert!(!unlimited.refused());
    // The premise's copy alone is larger than the bounds' rows: a limit below it leaves
    // the bounds room and the compilation none.
    let snapshot = store.engine().snapshot();
    let premise = nrese_dl::memory::ontology_bytes(&ontology_at(&store, &snapshot));
    assert!(
        premise > 64 << 10,
        "the ABox makes compiling the larger part"
    );
    let mut ends = Vec::new();
    let mut limit = 256;
    while limit < needed + needed / 4 {
        let memory = store.envelope(Some(limit));
        let end = run(&store, &memory);
        assert!(memory.refused() || end == End::Answered, "at {limit} bytes");
        assert_eq!(
            catalog.used(),
            0,
            "at {limit} bytes the catalog's budget is back"
        );
        ends.push((limit, end));
        limit += limit / 4;
    }
    // Every end is reached in order as the envelope grows (in steps of a quarter, far
    // from the few bytes parallel kernels vary by).
    let reached: Vec<End> = ends.iter().map(|&(_, end)| end).collect();
    assert!(reached.is_sorted(), "{ends:?}");
    // Near the smallest envelope that answers, the engines are refused: the last of the
    // request's parts to grow.
    let below = ends.iter().rev().find(|&&(_, end)| end != End::Answered);
    let above = ends.iter().find(|&&(_, end)| end == End::Answered);
    let (Some(&(mut low, mut low_end)), Some(&(mut high, _))) = (below, above) else {
        panic!("{ends:?}")
    };
    while high - low > 1 {
        let mid = low + (high - low) / 2;
        match run(&store, &store.envelope(Some(mid))) {
            End::Answered => high = mid,
            end => (low, low_end) = (mid, end),
        }
    }
    let mut reached = reached;
    reached.push(low_end);
    for end in [End::Bounds, End::Compilation, End::Engines, End::Answered] {
        assert!(
            reached.contains(&end),
            "{end:?} never reached: {ends:?}, {low}: {low_end:?}"
        );
    }
    // The store answers the next request in full.
    assert_eq!(run(&store, &store.envelope(None)), End::Answered);
}
