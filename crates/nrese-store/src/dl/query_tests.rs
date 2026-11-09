use super::*;
use nrese_dl::limits::Deadline;

fn lower_true_store() -> Arc<StoreService> {
    use crate::{
        MutationCommand, MutationPipeline, MutationTicket, Requester, SparqlUpdateRequest,
    };
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    let store = Arc::new(
        StoreService::new(crate::StoreConfig {
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
    store
}

#[test]
fn lower_true_ask_evaluates_once_and_delivers_that_result() {
    let store = lower_true_store();
    let prepared = store
        .prepare_query(&crate::SparqlQueryRequest::all("ASK { ?x a <urn:test:B> }"))
        .unwrap();
    crate::query_executor::BOUND_EVALUATIONS.with(|n| n.set(0));
    let outcome = answer(
        &store,
        &prepared,
        &CancellationToken::new(),
        &nrese_exec::Envelope::unlimited(),
        DlAnswers::Exact,
    )
    .unwrap();
    let Outcome::Answers(answers @ Answers::Boolean(true), status, detail) = outcome else {
        panic!("retained lower truth")
    };
    assert!(status.complete);
    assert!(detail.paths.contains(&"lower-true-ask"));
    let mut out = Vec::new();
    crate::query_executor::write_answers(&prepared, answers, &CancellationToken::new(), &mut out)
        .unwrap();
    assert!(String::from_utf8(out).unwrap().contains("true"));
    crate::query_executor::BOUND_EVALUATIONS
        .with(|n| assert_eq!(n.get(), 1, "upper was never evaluated"));
}

#[test]
fn retained_dl_answers_honor_reporting_cancellation_before_writing() {
    let store = lower_true_store();
    let mut request = crate::SparqlQueryRequest::all("ASK { ?x a <urn:test:B> }");
    request.dl_answers = Some(DlAnswers::Exact);
    let prepared = store.prepare_query(&request).unwrap();
    let token = CancellationToken::new();
    let mut reported = false;
    let mut bytes = Vec::new();
    let result = store.run_query_reporting(&prepared, &token, &mut bytes, |status| {
        assert!(status.unwrap().complete);
        reported = true;
        token.cancel();
    });
    assert!(reported);
    assert!(matches!(
        result,
        Err(StoreError::SparqlEvaluation(
            nrese_sparql::QueryEvaluationError::Cancelled
        ))
    ));
    assert!(
        bytes.is_empty(),
        "report cancellation precedes retained bytes"
    );
}

#[test]
fn cancelled_consistency_is_not_published_or_reused_by_another_request() {
    let store = StoreService::new(crate::StoreConfig::in_memory()).unwrap();
    let snapshot = store.read_snapshot(None).unwrap();
    let deadline = Deadline::after(store.config().dl.timeout);
    let view = unproved_view(&snapshot);
    let token = CancellationToken::new();
    let flag = token.flag();
    // Hold the existing ontology-cache lock so the consistency path is in flight
    // when cancelled. Its shared flag handle proves it passed the entry check.
    let cache = store.dl().query_ontology.0.lock().unwrap();
    std::thread::scope(|scope| {
        let checking = scope.spawn(|| {
            consistency_at(
                &store,
                &snapshot,
                &view,
                &token,
                deadline,
                &nrese_exec::Envelope::unlimited(),
            )
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while Arc::strong_count(&flag) == 2 && std::time::Instant::now() < deadline {
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
        consistency_at(
            &store,
            &snapshot,
            &view,
            &fresh,
            deadline,
            &nrese_exec::Envelope::unlimited()
        ),
        Verdict::Consistent
    );
    let recorded = store.dl().status().unwrap();
    assert_eq!(recorded.consistency.verdict, Verdict::Consistent);
    assert_eq!(
        consistency_at(
            &store,
            &snapshot,
            &view,
            &token,
            deadline,
            &nrese_exec::Envelope::unlimited()
        ),
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
    let snapshot = store.read_snapshot(None).unwrap();
    let view = unproved_view(&snapshot);
    let fresh = CancellationToken::new();
    let expired = Deadline::at(std::time::Instant::now());
    assert_eq!(
        consistency_at(
            &store,
            &snapshot,
            &view,
            &fresh,
            expired,
            &nrese_exec::Envelope::unlimited()
        ),
        Verdict::Unknown(nrese_dl::limits::Limit::Time.reached("the deadline passed"))
    );
    assert!(store.dl().status().is_none());
    assert!(store.dl().query_ontology.0.lock().unwrap().is_none());
    let deadline = Deadline::after(store.config().dl.timeout);
    assert_eq!(
        consistency_at(
            &store,
            &snapshot,
            &view,
            &fresh,
            deadline,
            &nrese_exec::Envelope::unlimited()
        ),
        Verdict::Consistent
    );
    let recorded = store.dl().status().unwrap();
    // A cached proof remains usable after a new request's reasoning budget expires.
    assert_eq!(
        consistency_at(
            &store,
            &snapshot,
            &view,
            &fresh,
            expired,
            &nrese_exec::Envelope::unlimited()
        ),
        Verdict::Consistent
    );
    fresh.cancel();
    assert_eq!(
        consistency_at(
            &store,
            &snapshot,
            &view,
            &fresh,
            expired,
            &nrese_exec::Envelope::unlimited()
        ),
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
    let snapshot = store.read_snapshot(None).unwrap();
    let mut view = unproved_view(&snapshot);
    view.proves_consistency = true;
    let token = CancellationToken::new();
    let expired = Deadline::at(std::time::Instant::now());
    assert_eq!(
        consistency_at(
            &store,
            &snapshot,
            &view,
            &token,
            expired,
            &nrese_exec::Envelope::unlimited()
        ),
        Verdict::Consistent
    );
    token.cancel();
    assert_eq!(
        consistency_at(
            &store,
            &snapshot,
            &view,
            &token,
            expired,
            &nrese_exec::Envelope::unlimited()
        ),
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
        let outcome = decide_answers(
            &store,
            &prepared,
            &analyse(prepared.query()),
            &token,
            &nrese_exec::Envelope::unlimited(),
            mode,
        )
        .unwrap();
        assert!(matches!(outcome, Outcome::Stream(ref status, _, _) if !status.complete));
        token.cancel();
        assert!(matches!(
            answer(
                &store,
                &prepared,
                &token,
                &nrese_exec::Envelope::unlimited(),
                mode,
            ),
            Err(StoreError::SparqlEvaluation(
                nrese_sparql::QueryEvaluationError::Cancelled
            ))
        ));
    }
}

/// A store whose U1 takes the existential route (not OWL 2 RL's), written past the pipeline:
/// its first read builds U1 cold, and that build's taxonomy fills the query-ontology cache.
fn existential_store() -> Arc<StoreService> {
    let store = Arc::new(StoreService::new(crate::StoreConfig::in_memory()).unwrap());
    store
        .update_str_ungated(
            "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
             PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA { :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ;
                 owl:someValuesFrom :B ] . :a a :A . }",
        )
        .unwrap();
    store
}

/// The revision of the query-ontology cache's entry.
fn cached(store: &StoreService) -> Option<u64> {
    let cache = store.dl().query_ontology.0.lock().unwrap();
    cache.as_ref().map(|(revision, _)| *revision)
}

/// What the cache holds as U1's next cold build begins (`None`: no build began).
fn cached_as_a_build_begins(
    store: &Arc<StoreService>,
) -> Arc<std::sync::Mutex<Option<Option<u64>>>> {
    use super::super::bounds::seam;
    let seen = Arc::new(std::sync::Mutex::new(None));
    let (at, into) = (Arc::clone(store), Arc::clone(&seen));
    seam::set(&store.dl().bounds, seam::Point::Build, move || {
        *into.lock().unwrap() = Some(cached(&at));
    });
    seen
}

/// A cold build of U1 begins with no older revision's ontology that only the cache keeps
/// (SYN-61 review r1 N1): its charge would hold the room the build's own reading needs, and
/// the cache lets it go only when the build's taxonomy reads the new one, after.
#[test]
fn a_cold_build_begins_with_no_older_cached_ontology() {
    let store = existential_store();
    let first = super::super::bounds::report(&store);
    assert_eq!(first.unavailable, None, "{first:?}");
    assert_eq!(
        cached(&store),
        Some(first.revision),
        "the first build's taxonomy"
    );
    store
        .update_str_ungated("PREFIX : <urn:test:> INSERT DATA { :b a :A . }")
        .unwrap();
    let seen = cached_as_a_build_begins(&store);
    let after = super::super::bounds::report(&store);
    assert_eq!(
        *seen.lock().unwrap(),
        Some(None),
        "the cache as revision {}'s build began",
        after.revision
    );
}

/// The cache's entry for the revision a cold build is of stays (N1): a reader that read it
/// first keeps it for the build's taxonomy, so it isn't read again.
#[test]
fn a_cold_build_keeps_the_cached_ontology_of_its_own_revision() {
    let store = existential_store();
    let first = super::super::bounds::report(&store);
    assert_eq!(first.unavailable, None, "{first:?}");
    store
        .update_str_ungated("PREFIX : <urn:test:> INSERT DATA { :b a :A . }")
        .unwrap();
    let snapshot = store.engine().snapshot();
    drop(ontology_at(&store, &snapshot).unwrap());
    let seen = cached_as_a_build_begins(&store);
    let after = super::super::bounds::report(&store);
    assert_eq!(after.revision, snapshot.revision());
    assert_eq!(
        *seen.lock().unwrap(),
        Some(Some(after.revision)),
        "the cache as revision {}'s build began",
        after.revision
    );
}
