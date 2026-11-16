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

/// Commits `update` (with the prefixes of [`decided_then`]) through the `owl2-dl` pipeline.
fn commit(pipeline: &crate::MutationPipeline, update: &str) {
    use crate::{MutationCommand, MutationTicket, Requester, SparqlUpdateRequest};
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!("{MARK_PREFIXES}{update}"))),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap();
}

const MARK_PREFIXES: &str = "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

/// A store under `owl2-dl` with `first` committed, `then` set to be committed once what a
/// read reads is fixed, before its status (the hook `AFTER_DECIDED`), and whether it was.
fn racing(
    first: &str,
    then: &'static str,
) -> (Arc<StoreService>, std::rc::Rc<std::cell::Cell<bool>>) {
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    let store = Arc::new(StoreService::new(crate::StoreConfig::in_memory()).unwrap());
    let pipeline = std::rc::Rc::new(crate::MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    ));
    commit(&pipeline, &format!("INSERT DATA {{ {first} }}"));
    let ran = std::rc::Rc::new(std::cell::Cell::new(false));
    let (racer, seen) = (std::rc::Rc::clone(&pipeline), std::rc::Rc::clone(&ran));
    crate::service::hooks::AFTER_DECIDED.set(Some(Box::new(move || {
        commit(&racer, then);
        seen.set(true);
    })));
    (store, ran)
}

/// `query` over [`racing`]`(first, then)`, run: the answer and its status.
fn decided_then(
    first: &str,
    then: &'static str,
    query: &str,
) -> (String, nrese_sparql::Completeness) {
    let (store, ran) = racing(first, then);
    let prepared = store
        .prepare_query(&crate::SparqlQueryRequest::all(format!(
            "{MARK_PREFIXES}{query}"
        )))
        .unwrap();
    let (mut out, mut status) = (Vec::new(), None);
    store
        .run_query_reporting(&prepared, &CancellationToken::new(), &mut out, |s| {
            status = s
        })
        .unwrap();
    assert!(
        ran.get(),
        "the commit ran between the decision and the status"
    );
    (String::from_utf8(out).unwrap(), status.expect("a status"))
}

/// R1-F2, one way: at the decided revision an annotation property's domain puts `:x a :C`
/// in L only (OWL 2 DL gives the domain no meaning for what it annotates); a commit then
/// removes the domain. The answer `true`, read at the decided revision, may only come
/// `unsound`: its status is that revision's, not the later one's.
#[test]
fn an_answer_keeps_the_mark_of_its_revision_when_a_commit_repairs_the_ontology() {
    let (answer, status) = decided_then(
        ":note a owl:AnnotationProperty ; rdfs:domain :C . :C a owl:Class . :x :note \"n\" .",
        "DELETE DATA { :note rdfs:domain :C }",
        "ASK { :x a :C }",
    );
    let line = format!("{answer} {} {:?}", status.as_str(), status.reasons);
    eprintln!("{line}");
    assert!(
        answer.contains("true"),
        "the decided revision's answer: {line}"
    );
    assert_eq!(status.as_str(), "unsound", "{line}");
}

/// R1-F2, the other way: at the decided revision an ordinary property's domain entails
/// `:x a :C` (the rules' route); a commit then adds an annotation property's domain on other
/// terms. The answer `true`, entailed at its revision, isn't marked by the later one.
#[test]
fn an_answer_takes_no_mark_from_a_later_revision() {
    let (answer, status) = decided_then(
        ":note a owl:ObjectProperty ; rdfs:domain :C . :C a owl:Class . :x :note :v .",
        "INSERT DATA { :other a owl:AnnotationProperty ; rdfs:domain :D . :D a owl:Class . }",
        "ASK { :x a :C }",
    );
    let line = format!("{answer} {} {:?}", status.as_str(), status.reasons);
    eprintln!("{line}");
    assert!(
        answer.contains("true"),
        "the decided revision's answer: {line}"
    );
    assert_ne!(status.as_str(), "unsound", "{line}");
}

/// R1-F2, a query naming its dataset: its rows are read at the revision its status was
/// decided on. A commit after the decision removes the statement it asks for; the answer
/// is still `true`.
#[test]
fn a_query_naming_its_dataset_reads_the_revision_it_was_decided_on() {
    let (answer, status) = decided_then(
        "GRAPH :g { :x a :C . :C a owl:Class . }",
        "DELETE DATA { GRAPH :g { :x a :C } }",
        "ASK FROM :g { :x a :C }",
    );
    let line = format!("{answer} {} {:?}", status.as_str(), status.reasons);
    eprintln!("{line}");
    assert!(
        answer.contains("true"),
        "the decided revision's answer: {line}"
    );
}

/// R1-F2, a read of the inferred statements alone (`FROM onto:implicit`): run, EXPLAIN and
/// the plan each give the status of the revision the read was on. There an annotation
/// property's domain puts `:x a :C` in L only; a commit between the read and its status
/// removes the domain. The status stays `unsound`.
#[test]
fn a_read_of_the_inferred_statements_alone_keeps_the_mark_of_its_revision() {
    let read = "ASK FROM <http://www.ontotext.com/implicit> { :x a :C }";
    for path in ["run", "explain", "plan"] {
        let (store, ran) = racing(
            ":note a owl:AnnotationProperty ; rdfs:domain :C . :C a owl:Class . :x :note \"n\" .",
            "DELETE DATA { :note rdfs:domain :C }",
        );
        let request = crate::SparqlQueryRequest::all(format!("{MARK_PREFIXES}{read}"));
        let (prepared, cancel) = (
            store.prepare_query(&request).unwrap(),
            CancellationToken::new(),
        );
        let (mut answer, mut status) = (Vec::new(), None);
        match path {
            "run" => store
                .run_query_reporting(&prepared, &cancel, &mut answer, |s| status = s)
                .unwrap(),
            "explain" => {
                status = store
                    .explain_query(&prepared, &cancel)
                    .unwrap()
                    .completeness
            }
            _ => status = store.plan_query(&prepared, &cancel).unwrap().completeness,
        }
        let status = status.expect("a status");
        let answer = String::from_utf8(answer).unwrap();
        let line = format!("{path}: {answer} {} {:?}", status.as_str(), status.reasons);
        eprintln!("{line}");
        assert!(
            ran.get(),
            "{line}: no commit between the read and its status"
        );
        assert!(path != "run" || answer.contains("true"), "{line}");
        assert_eq!(status.as_str(), "unsound", "{line}");
    }
}
