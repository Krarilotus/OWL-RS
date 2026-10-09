use super::*;

#[test]
fn lower_true_ask_evaluates_once_and_delivers_that_result() {
    use crate::{
        MutationCommand, MutationPipeline, MutationTicket, Requester, SparqlUpdateRequest,
    };
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    let store = Arc::new(StoreService::new(crate::StoreConfig::in_memory()).unwrap());
    let pipeline = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    );
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(
                "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#> \
         INSERT DATA { :a a [ owl:unionOf ( :B :C ) ] . :b a :B . }",
            )),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap();
    let prepared = store
        .prepare_query(&crate::SparqlQueryRequest::all("ASK { ?x a <urn:test:B> }"))
        .unwrap();
    crate::query_executor::BOUND_EVALUATIONS.with(|n| n.set(0));
    let outcome = answer(
        &store,
        &prepared,
        &CancellationToken::new(),
        DlAnswers::Exact,
    )
    .unwrap();
    let Outcome::Answers(answers @ Answers::Boolean(true), status, detail) = outcome else {
        panic!("retained lower truth")
    };
    assert!(status.complete);
    assert!(detail.paths.contains(&"lower-true-ask"));
    let mut out = Vec::new();
    crate::query_executor::write_answers(&prepared, answers, &mut out).unwrap();
    assert!(String::from_utf8(out).unwrap().contains("true"));
    crate::query_executor::BOUND_EVALUATIONS
        .with(|n| assert_eq!(n.get(), 1, "upper was never evaluated"));
}

#[test]
fn cancelled_consistency_is_not_published_or_reused_by_another_request() {
    let store = StoreService::new(crate::StoreConfig::in_memory()).unwrap();
    let snapshot = store.read_snapshot(None);
    let deadline = Instant::now() + store.config().dl.timeout;
    let view = unproved_view(&snapshot);
    let token = CancellationToken::new();
    let flag = token.flag();
    // Hold the existing ontology-cache lock so the consistency path is in flight
    // when cancelled. Its shared flag handle proves it passed the entry check.
    let cache = store.dl().query_ontology.0.lock().unwrap();
    std::thread::scope(|scope| {
        let checking = scope.spawn(|| consistency_at(&store, &snapshot, &view, &token, deadline));
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while Arc::strong_count(&flag) == 2 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let entered = Arc::strong_count(&flag) > 2;
        token.cancel();
        drop(cache);
        let verdict = checking.join().unwrap();
        assert!(
            entered,
            "the request must enter the nested consistency path"
        );
        assert_eq!(verdict, Verdict::Unknown("cancelled".to_owned()));
    });
    assert!(
        store.dl().status().is_none(),
        "cancellation is request-local"
    );
    let fresh = CancellationToken::new();
    assert_eq!(
        consistency_at(&store, &snapshot, &view, &fresh, deadline),
        Verdict::Consistent
    );
    let recorded = store.dl().status().unwrap();
    assert_eq!(recorded.consistency.verdict, Verdict::Consistent);
    assert_eq!(
        consistency_at(&store, &snapshot, &view, &token, deadline),
        Verdict::Unknown("cancelled".to_owned())
    );
    assert_eq!(
        store.dl().status().unwrap().consistency,
        recorded.consistency
    );
}

fn unproved_view(snapshot: &Snapshot) -> View {
    View {
        revision: snapshot.revision(),
        lower: snapshot.clone(),
        lower_facts: 0,
        upper: None,
        unavailable: None,
        gap_classes: HashSet::new(),
        gap_predicates: HashSet::new(),
        named_gap_classes: HashSet::new(),
        named_gap_predicates: HashSet::new(),
        internal: HashSet::new(),
        facts: 0,
        proves_consistency: false,
        rules: false,
    }
}

#[test]
fn expired_consistency_keeps_request_timeout_out_of_shared_status() {
    let store = StoreService::new(crate::StoreConfig::in_memory()).unwrap();
    let snapshot = store.read_snapshot(None);
    let view = unproved_view(&snapshot);
    let fresh = CancellationToken::new();
    let expired = Instant::now();
    assert_eq!(
        consistency_at(&store, &snapshot, &view, &fresh, expired),
        Verdict::Unknown("past dl.timeout".to_owned())
    );
    assert!(store.dl().status().is_none());
    assert!(store.dl().query_ontology.0.lock().unwrap().is_none());
    let deadline = Instant::now() + store.config().dl.timeout;
    assert_eq!(
        consistency_at(&store, &snapshot, &view, &fresh, deadline),
        Verdict::Consistent
    );
    let recorded = store.dl().status().unwrap();
    // A cached proof remains usable after a new request's reasoning budget expires.
    assert_eq!(
        consistency_at(&store, &snapshot, &view, &fresh, expired),
        Verdict::Consistent
    );
    fresh.cancel();
    assert_eq!(
        consistency_at(&store, &snapshot, &view, &fresh, expired),
        Verdict::Unknown("cancelled".to_owned())
    );
    assert_eq!(
        store.dl().status().unwrap().consistency,
        recorded.consistency
    );
}

#[test]
fn an_upper_bound_proof_survives_expiry_but_not_request_cancellation() {
    let store = StoreService::new(crate::StoreConfig::in_memory()).unwrap();
    let snapshot = store.read_snapshot(None);
    let mut view = unproved_view(&snapshot);
    view.proves_consistency = true;
    let token = CancellationToken::new();
    let expired = Instant::now();
    assert_eq!(
        consistency_at(&store, &snapshot, &view, &token, expired),
        Verdict::Consistent
    );
    token.cancel();
    assert_eq!(
        consistency_at(&store, &snapshot, &view, &token, expired),
        Verdict::Unknown("cancelled".to_owned())
    );
    assert!(store.dl().status().is_none());
}

#[test]
fn cancellation_takes_precedence_over_partial_answers_and_incompleteness() {
    let store = StoreService::new(crate::StoreConfig::in_memory()).unwrap();
    let prepared = PreparedQuery::parse(&crate::SparqlQueryRequest::all(
        "SELECT ?s FROM <http://example.org/g> WHERE { ?s ?p ?o }",
    ))
    .unwrap();
    for mode in [
        DlAnswers::Sound,
        DlAnswers::CertainWhereComplete,
        DlAnswers::Exact,
    ] {
        let token = CancellationToken::new();
        let outcome = decide_answers(&store, &prepared, &token, mode).unwrap();
        assert!(matches!(outcome, Outcome::Stream(ref status, _, _) if !status.complete));
        token.cancel();
        assert!(matches!(
            answer(&store, &prepared, &token, mode),
            Err(StoreError::SparqlEvaluation(
                nrese_sparql::QueryEvaluationError::Cancelled
            ))
        ));
    }
}
