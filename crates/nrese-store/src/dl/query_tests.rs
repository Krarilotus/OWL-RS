use super::*;

#[test]
fn cancelled_consistency_is_not_published_or_reused_by_another_request() {
    let store = StoreService::new(crate::StoreConfig::in_memory()).unwrap();
    let snapshot = store.read_snapshot(None);
    let view = View {
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
    };
    let token = CancellationToken::new();
    let flag = token.flag();
    // Hold the existing ontology-cache lock so the consistency path is in flight
    // when cancelled. Its shared flag handle proves it passed the entry check.
    let cache = store.dl().query_ontology.0.lock().unwrap();
    std::thread::scope(|scope| {
        let checking = scope.spawn(|| consistency_at(&store, &snapshot, &view, &token));
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
        consistency_at(&store, &snapshot, &view, &fresh),
        Verdict::Consistent
    );
    let recorded = store.dl().status().unwrap();
    assert_eq!(recorded.consistency.verdict, Verdict::Consistent);
    assert_eq!(
        consistency_at(&store, &snapshot, &view, &token),
        Verdict::Unknown("cancelled".to_owned())
    );
    assert_eq!(
        store.dl().status().unwrap().consistency,
        recorded.consistency
    );
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
