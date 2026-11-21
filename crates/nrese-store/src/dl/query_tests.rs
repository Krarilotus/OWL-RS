use super::*;
use nrese_dl::limits::Deadline;

fn lower_true_store() -> Arc<StoreService> {
    lower_true_store_with(crate::StoreConfig::in_memory(), "")
}

/// [`lower_true_store`] under `config`, `data` committed with it.
fn lower_true_store_with(config: crate::StoreConfig, data: &str) -> Arc<StoreService> {
    use crate::{
        MutationCommand, MutationPipeline, MutationTicket, Requester, SparqlUpdateRequest,
    };
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    let store = Arc::new(StoreService::new(config).unwrap());
    let pipeline = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    );
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!(
                "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#> \
         INSERT DATA {{ :a a [ owl:unionOf ( :B :C ) ] . :b a :B . {data} }}"
            ))),
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
        &store.engine().snapshot(),
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
    let result = store.run_query_reporting(&prepared, &token, &mut bytes, |status, _| {
        assert!(status.unwrap().complete);
        reported = true;
        token.cancel();
        None
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
            &crate::dl::bounds::view(&store),
            (&token, Deadline::after(store.config().dl.timeout)),
            &nrese_exec::Envelope::unlimited(),
            mode,
        )
        .unwrap();
        assert!(matches!(outcome, Outcome::Stream(ref status, _, _) if !status.complete));
        token.cancel();
        assert!(matches!(
            answer(
                &store,
                &store.engine().snapshot(),
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

/// A read cancelled while U1's element for every element is posed (`owl:Thing ⊑ C`, every name
/// in C) ends as the cancellation it is, in each answer mode that poses it (`sound` reads L
/// alone, never `complete`): never an answer whose status the cut short question made
/// incomplete, nor an exact read refused for it.
#[test]
fn a_cancellation_while_the_element_is_posed_is_the_reads() {
    let store = lower_true_store_with(
        crate::StoreConfig::in_memory(),
        "owl:Thing <http://www.w3.org/2000/01/rdf-schema#subClassOf> :C .",
    );
    let prepared = PreparedQuery::parse(&crate::SparqlQueryRequest::all(
        "SELECT ?x { ?x a <urn:test:C> }",
    ))
    .unwrap();
    for mode in [DlAnswers::CertainWhereComplete, DlAnswers::Exact] {
        let token = CancellationToken::new();
        let cancel = token.clone();
        crate::service::hooks::BEFORE_STAND_IN.set(Some(Box::new(move || cancel.cancel())));
        let outcome = answer(
            &store,
            &store.engine().snapshot(),
            &prepared,
            &token,
            &nrese_exec::Envelope::unlimited(),
            mode,
        )
        .err();
        assert!(
            matches!(
                outcome,
                Some(StoreError::SparqlEvaluation(
                    nrese_sparql::QueryEvaluationError::Cancelled
                ))
            ),
            "{mode:?}: {outcome:?}"
        );
        assert!(
            token.is_cancelled(),
            "{mode:?}: the element was never posed"
        );
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

/// A cold build of U1 begins with no older revision's ontology that only the cache keeps:
/// its charge would hold the room the build's own reading needs, and
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
        .run_query_reporting(&prepared, &CancellationToken::new(), &mut out, |s, _| {
            status = s;
            None
        })
        .unwrap();
    assert!(
        ran.get(),
        "the commit ran between the decision and the status"
    );
    (String::from_utf8(out).unwrap(), status.expect("a status"))
}

/// One way: at the decided revision an annotation property's domain puts `:x a :C`
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

/// The other way: at the decided revision an ordinary property's domain entails
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

/// A query naming its dataset: its rows are read at the revision its status was
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

/// A read of the inferred statements alone (`FROM onto:implicit`): run, EXPLAIN and
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
                .run_query_reporting(&prepared, &cancel, &mut answer, |s, _| {
                    status = s;
                    None
                })
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

/// The ontology read the status needs, refused at its budget (nothing cached for the
/// revision, neither its ontology nor its mark, and the bounds' memory spent, between the
/// decision and the status): whether
/// OWL 2 DL reads the data otherwise than the rules isn't known, so the answers are
/// `unsound` with that reason, and an exact read is refused. The control: the same exact
/// read with the memory free is answered. The store holds a term equal to another, so the
/// status needs the ontology: its indexes alone don't decide the mark ([`by_index`]).
#[test]
fn an_ontology_read_refused_at_its_budget_leaves_the_answers_unsound() {
    let store = lower_true_store_with(
        crate::StoreConfig {
            total_query_memory_bytes: 64 << 20,
            ..crate::StoreConfig::in_memory()
        },
        ":b owl:sameAs :d .",
    );
    let mut request = crate::SparqlQueryRequest::all("ASK { ?x a <urn:test:B> }");
    request.dl_answers = Some(crate::DlAnswers::Exact);
    let prepared = store.prepare_query(&request).unwrap();
    let run = || {
        store.run_query_reporting(
            &prepared,
            &CancellationToken::new(),
            &mut Vec::new(),
            |_, _| None,
        )
    };
    assert!(run().is_ok(), "the control: {:?}", run().map(|_| ()));
    let kept = Arc::clone(store.dl().bounds.kept());
    let spent = std::rc::Rc::new(std::cell::Cell::new(0));
    let (cache, budget, held) = (
        Arc::clone(&store),
        Arc::clone(&kept),
        std::rc::Rc::clone(&spent),
    );
    crate::service::hooks::AFTER_DECIDED.set(Some(Box::new(move || {
        drop(cache.dl().query_ontology.0.lock().unwrap().take());
        // Nor the mark the control's read kept for the revision.
        drop(cache.dl().query_ontology.1.lock().unwrap().take());
        let rest = budget.remaining();
        budget.charge(rest).unwrap();
        held.set(rest);
    })));
    let refused = run();
    kept.release(spent.get());
    assert!(spent.get() > 0, "the hook spent the bounds' memory");
    let answer = match refused {
        Err(crate::StoreError::Incomplete(answer)) => answer,
        other => panic!(
            "expected the exact read refused, got {:?}",
            other.map(|_| ())
        ),
    };
    let reasons: Vec<&str> = answer
        .status
        .reasons
        .iter()
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(answer.status.as_str(), "unsound", "{reasons:?}");
    assert!(
        reasons.iter().any(|r| r.contains("isn't known")),
        "{reasons:?}"
    );
}

thread_local! {
    /// How often this thread's marks went to the whole ontology, read or cached
    /// ([`reads_differently`]).
    pub(super) static ONTOLOGY_PASSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Data a read leaves the rules route for: OWL 2 RL has no rule for a union as a superclass.
const OFF_ROUTE: &str = ":Pet rdfs:subClassOf [ owl:unionOf ( :Dog :Cat ) ] . :tib a :Pet . ";

/// `ASK { :tib a :Pet }` over a store under `owl2-dl` (with `config`) with [`OFF_ROUTE`] and
/// `data` committed, read committed and then inside a transaction adding `pending`: for each
/// read, the mark's reasons and how often its mark went to the whole ontology; then the mark
/// the whole committed ontology gives ([`by_ontology`]).
fn marks_and_passes(
    config: crate::StoreConfig,
    data: &str,
    pending: &str,
) -> (Vec<(Vec<String>, usize)>, Option<String>) {
    use crate::{SparqlUpdateRequest, StatementOp, StatementsRequest};
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    let store = Arc::new(StoreService::new(config).unwrap());
    let pipeline = crate::MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    );
    commit(&pipeline, &format!("INSERT DATA {{ {OFF_ROUTE}{data} }}"));
    let prepared = store
        .prepare_query(&crate::SparqlQueryRequest::all(format!(
            "{MARK_PREFIXES}ASK {{ :tib a :Pet }}"
        )))
        .unwrap();
    let update = format!("{MARK_PREFIXES}INSERT DATA {{ {pending} }}");
    let mut reads = Vec::new();
    for inside in [false, true] {
        ONTOLOGY_PASSES.set(0);
        let (mut status, cancel) = (None, CancellationToken::new());
        let out = Vec::<u8>::new();
        match inside {
            false => store.run_query_reporting(&prepared, &cancel, out, |s, _| {
                status = s;
                None
            }),
            true => store.run_query_pending_reporting(
                &StatementsRequest::new(vec![StatementOp::Update(SparqlUpdateRequest::new(
                    update.clone(),
                ))]),
                &prepared,
                &cancel,
                out,
                |s, _| {
                    status = s;
                    None
                },
            ),
        }
        .unwrap();
        let marks = status
            .expect("a status")
            .reasons
            .into_iter()
            .filter(|r| r.text.contains(ONLY))
            .map(|r| r.text)
            .collect();
        reads.push((marks, ONTOLOGY_PASSES.get()));
    }
    let snapshot = store.engine().snapshot();
    let whole = read_ontology(&store, &snapshot).expect("the ontology");
    (reads, by_ontology(&snapshot, whole.value()))
}

/// A read off the rules route of a store with neither an annotation-property axiom nor an
/// equality is told no mark, committed and inside a transaction. Neither read's mark goes to
/// the whole ontology: the store's indexes and the commit's record show there is neither, and
/// the pending change, a class assertion over the base's signature, keeps the record, as its
/// commit would (`Bounds::keeps_record`).
#[test]
fn a_store_without_annotation_axioms_or_equality_is_marked_without_the_ontology() {
    let (reads, _) = marks_and_passes(
        crate::StoreConfig::in_memory(),
        ":tib :owner :ann . :ann rdfs:label \"Ann\" .",
        ":rex a :Pet .",
    );
    assert_eq!(
        reads,
        [(vec![], 0), (vec![], 0)],
        "[committed, pending]: (the mark's reasons, passes over the whole ontology)"
    );
}

/// Spec (a pending read takes its base's record of what the reader left out only where its
/// changes keep it, by the tests their commit applies, `Bounds::keeps_record`): a change that
/// names a reserved name, changes the schema, says something about the ontology's node, or
/// gives a property an axiom names a second kind (a literal on an object property) may change
/// what the reader reads. Its pending read passes once over its own ontology and is told what
/// that tells: the reserved name and the second kind a mark, the schema and the header
/// statement none. A data-only change over a base whose premise has a break is told the
/// base's break, from the base's record, without the ontology.
#[test]
fn a_pending_read_takes_its_bases_record_only_where_its_changes_keep_it() {
    let owner = ":tib :owner :ann . ";
    // (the case, the base past the route's own data, the pending change, a word of the
    // pending read's one mark (`None`: no mark), its passes over the whole ontology)
    let cases = [
        (
            "a reserved name",
            String::new(),
            ":rex :owner rdfs:Resource .",
            Some("were left out"),
            1,
        ),
        (
            "a schema change",
            String::new(),
            ":Dog rdfs:subClassOf :Pet .",
            None,
            1,
        ),
        (
            "a header statement",
            ":onto a owl:Ontology . ".to_owned(),
            ":onto :owner :ann .",
            None,
            1,
        ),
        (
            "a second kind",
            ":owner rdfs:domain :Pet . ".to_owned(),
            ":rex :owner \"Rex\" .",
            Some("two kinds"),
            1,
        ),
        (
            "a base with a break",
            ":ann :owner rdfs:Resource . ".to_owned(),
            ":rex a :Pet .",
            Some("were left out"),
            0,
        ),
    ];
    let mut wrong = Vec::new();
    for (case, base, change, word, passes) in cases {
        let data = format!("{owner}{base}");
        let (reads, _) = marks_and_passes(crate::StoreConfig::in_memory(), &data, change);
        let (marks, made) = &reads[1];
        let told = match word {
            None => marks.is_empty(),
            Some(word) => marks.len() == 1 && marks[0].contains(word),
        };
        if !told || *made != passes {
            wrong.push(format!("{case}: (marks, passes) {:?}", reads[1]));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (*Mapping to RDF* Table 17: a reification annotates its main triple, which the graph
/// states; the reader reports one whose main triple is absent, `Diagnostic::Malformed`):
/// deleting a triple that a surviving `owl:Axiom` names as its main triple leaves that node
/// annotating nothing, so the base's record no longer holds. On the rules route and off it, a
/// read inside a transaction that deletes it, and a committed read after a commit that does,
/// are each told the mark a fresh read of that data gives: a part left out.
#[test]
fn a_deleted_triple_a_surviving_reification_names_is_told_as_a_fresh_read_tells_it() {
    use crate::{SparqlUpdateRequest, StatementOp, StatementsRequest};
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    let reified = ":tib :owner :ann . [ a owl:Axiom ; owl:annotatedSource :tib ; \
                   owl:annotatedProperty :owner ; owl:annotatedTarget :ann ; \
                   rdfs:comment \"since 2020\" ] . ";
    let delete = "DELETE DATA { :tib :owner :ann . }";
    let left_out = |marks: &[String]| marks.len() == 1 && marks[0].contains("were left out");
    let mut wrong = Vec::new();
    for (route, base) in [
        ("off the route", OFF_ROUTE),
        ("on the route", ":tib a :Pet . "),
    ] {
        let store = Arc::new(StoreService::new(crate::StoreConfig::in_memory()).unwrap());
        let pipeline = crate::MutationPipeline::new(
            Arc::clone(&store),
            Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
                ReasoningMode::Owl2Dl,
            ))),
        );
        commit(&pipeline, &format!("INSERT DATA {{ {base}{reified} }}"));
        let ask = format!("{MARK_PREFIXES}ASK {{ :tib a :Pet }}");
        let prepared = || {
            let request = crate::SparqlQueryRequest::all(ask.clone());
            store.prepare_query(&request).unwrap()
        };
        let marks = |status: Option<nrese_sparql::Completeness>| -> Vec<String> {
            let reasons = status.expect("a status").reasons.into_iter();
            reasons
                .filter(|r| r.text.contains(ONLY))
                .map(|r| r.text)
                .collect()
        };
        // Inside a transaction that deletes it.
        let (mut status, cancel) = (None, CancellationToken::new());
        let update = SparqlUpdateRequest::new(format!("{MARK_PREFIXES}{delete}"));
        let ops = StatementsRequest::new(vec![StatementOp::Update(update)]);
        let out = Vec::<u8>::new();
        let read = store.run_query_pending_reporting(&ops, &prepared(), &cancel, out, |s, _| {
            status = s;
            None
        });
        read.unwrap();
        let pending = marks(status);
        // After a commit that deletes it.
        commit(&pipeline, delete);
        let mut status = None;
        let out = Vec::<u8>::new();
        let read = store.run_query_reporting(&prepared(), &cancel, out, |s, _| {
            status = s;
            None
        });
        read.unwrap();
        let committed = marks(status);
        // A fresh read of that data.
        let snapshot = store.engine().snapshot();
        let whole = read_ontology(&store, &snapshot).expect("the ontology");
        let fresh = by_ontology(&snapshot, whole.value());
        if !fresh.as_ref().is_some_and(|f| f.contains("were left out")) {
            wrong.push(format!("{route}: setup: a fresh read tells {fresh:?}"));
        }
        if !left_out(&pending) {
            wrong.push(format!("{route}: inside the transaction: {pending:?}"));
        }
        if !left_out(&committed) {
            wrong.push(format!("{route}: after the commit: {committed:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (*Mapping to RDF* Table 17, as above): a triple that a surviving `owl:Axiom` names,
/// deleted by one commit and inserted again by the next, gives the node its main triple back.
/// A committed read after the deletion is told the part left out, and after the insert what a
/// fresh read of that data tells: nothing. The property is declared an object property, so
/// the insert only asserts something over U1's signature, and the consistency check is off,
/// so no check reads the data again. Starting off the rules route and on it: the deletion
/// leaves a part out, so the insert's commit starts off the route either way.
#[test]
fn a_reinserted_triple_a_surviving_reification_names_is_told_as_a_fresh_read_tells_it() {
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    let reified = ":owner a owl:ObjectProperty . :tib :owner :ann . [ a owl:Axiom ; \
                   owl:annotatedSource :tib ; owl:annotatedProperty :owner ; \
                   owl:annotatedTarget :ann ; rdfs:comment \"since 2020\" ] . ";
    let mut wrong = Vec::new();
    for (route, base) in [
        ("off the route", OFF_ROUTE),
        ("on the route", ":tib a :Pet . "),
    ] {
        let config = crate::StoreConfig {
            dl: crate::DlConfig {
                consistency: crate::DlConsistency::Off,
                ..crate::DlConfig::default()
            },
            ..crate::StoreConfig::in_memory()
        };
        let store = Arc::new(StoreService::new(config).unwrap());
        let pipeline = crate::MutationPipeline::new(
            Arc::clone(&store),
            Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
                ReasoningMode::Owl2Dl,
            ))),
        );
        commit(&pipeline, &format!("INSERT DATA {{ {base}{reified} }}"));
        let marks = || -> Vec<String> {
            let request =
                crate::SparqlQueryRequest::all(format!("{MARK_PREFIXES}ASK {{ :tib a :Pet }}"));
            let prepared = store.prepare_query(&request).unwrap();
            let (mut status, out) = (None, Vec::<u8>::new());
            let read =
                store.run_query_reporting(&prepared, &CancellationToken::new(), out, |s, _| {
                    status = s;
                    None
                });
            read.unwrap();
            let reasons = status.expect("a status").reasons.into_iter();
            reasons
                .filter(|r| r.text.contains(ONLY))
                .map(|r| r.text)
                .collect()
        };
        commit(&pipeline, "DELETE DATA { :tib :owner :ann . }");
        let deleted = marks();
        if !(deleted.len() == 1 && deleted[0].contains("were left out")) {
            wrong.push(format!("{route}: setup: after the deletion: {deleted:?}"));
        }
        commit(&pipeline, "INSERT DATA { :tib :owner :ann . }");
        let snapshot = store.engine().snapshot();
        let whole = read_ontology(&store, &snapshot).expect("the ontology");
        let fresh = by_ontology(&snapshot, whole.value());
        let inserted = marks();
        if fresh.is_some() {
            wrong.push(format!(
                "{route}: setup: a fresh read after the insert tells {fresh:?}"
            ));
        }
        if !inserted.is_empty() {
            wrong.push(format!("{route}: after the insert: {inserted:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A store under `owl2-dl` with `base` committed and the consistency check off, so no check
/// reads the data again: a commit's record is the one it carries over or reads.
fn unchecked(base: &str) -> (Arc<StoreService>, crate::MutationPipeline) {
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    let config = crate::StoreConfig {
        dl: crate::DlConfig {
            consistency: crate::DlConsistency::Off,
            ..crate::DlConfig::default()
        },
        ..crate::StoreConfig::in_memory()
    };
    let store = Arc::new(StoreService::new(config).unwrap());
    let pipeline = crate::MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    );
    commit(&pipeline, &format!("INSERT DATA {{ {base} }}"));
    (store, pipeline)
}

/// Spec (the reader types an undeclared property by its uses, every statement of it counted:
/// a data property while its values are literals only, `nrese_owl::mapping`): `:p` has an IRI
/// value and, on the ontology's node, a literal one, so it is an object property (and, by the
/// header statement, an annotation property: a name of two kinds), whose range is a class's
/// complement. Deleting the IRI value leaves it a data property by the literal, whose range
/// is then no data range, which the reader leaves out. With no consistency check reading the
/// data again, the commit's record is what a fresh read of that data tells: the part left
/// out. (A read inside the deleting transaction is told its base's record, which names the
/// two kinds: a pending read takes a base's record that tells something whatever its
/// changes, `reads_differently`.)
#[test]
fn a_deleted_value_that_typed_an_undeclared_property_is_told_as_a_fresh_read_tells_it() {
    let (store, pipeline) = unchecked(&format!(
        "{OFF_ROUTE}:p rdfs:range [ owl:complementOf :C ] . :s :p :o . \
         :ont a owl:Ontology ; :p \"v\" . "
    ));
    let route = store.dl_bounds().last;
    commit(&pipeline, "DELETE DATA { :s :p :o . }");
    let snapshot = store.engine().snapshot();
    let whole = read_ontology(&store, &snapshot).expect("the ontology");
    let fresh = by_ontology(&snapshot, whole.value());
    assert!(
        fresh.as_ref().is_some_and(|f| f.contains("were left out")),
        "setup: a fresh read tells {fresh:?}"
    );
    let told = reads_differently(&store, Revision::Committed(&snapshot));
    assert_eq!(
        first_named(&told),
        first_named(&fresh),
        "from {route}: the commit's record, and a fresh read"
    );
}

/// Spec (*Mapping to RDF* Table 17: `owl:annotatedSource` is a reification's role, which the
/// reader reads only as one; a statement of it between two names is no OWL 2 DL, and is left
/// out): a store whose restriction on `owl:annotatedSource` the reader leaves out (a reserved
/// name), then a commit of `:x owl:annotatedSource :y`, with no consistency check reading the
/// data again. The commit's record is what a fresh read of that data tells: the parts it
/// leaves out, the new statement among them (which part it names first follows the read's
/// order, so it isn't compared).
#[test]
fn a_statement_of_a_reification_role_is_told_as_a_fresh_read_tells_it() {
    let (store, pipeline) = unchecked(&format!(
        "{OFF_ROUTE}:Pet rdfs:subClassOf [ a owl:Restriction ; \
         owl:onProperty owl:annotatedSource ; owl:someValuesFrom :Dog ] . "
    ));
    let route = store.dl_bounds().last;
    commit(&pipeline, "INSERT DATA { :x owl:annotatedSource :y . }");
    let snapshot = store.engine().snapshot();
    let whole = read_ontology(&store, &snapshot).expect("the ontology");
    let fresh = by_ontology(&snapshot, whole.value());
    assert!(
        fresh.as_ref().is_some_and(|f| f.contains("were left out")),
        "setup: a fresh read tells {fresh:?}"
    );
    let told = reads_differently(&store, Revision::Committed(&snapshot));
    assert_eq!(
        first_named(&told),
        first_named(&fresh),
        "from {route}: the commit's record, and a fresh read"
    );
}

/// A revision with no record of what the reader left out (as a reopened store has none; a
/// commit after a delta takes its base's). Committed reads at that revision (a plan, EXPLAIN,
/// the inferred statements alone), one after another, are each told the same mark, the
/// reader's, and their marks pass over the whole ontology at most once between them, not
/// once per read. (Two that miss at once may each pass; both publish the same mark.)
#[test]
fn committed_reads_without_a_record_pass_over_the_ontology_at_most_once() {
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    let value = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#value>";
    let stores = [
        ("nothing left out", ":tib :owner :ann .".to_owned(), false),
        (
            "a reserved name",
            format!("{value} rdfs:domain :Pet . :ann {value} :tib ."),
            true,
        ),
    ];
    let mut wrong = Vec::new();
    for (name, data, left_out) in stores {
        let config = crate::StoreConfig {
            dl: crate::DlConfig {
                consistency: crate::DlConsistency::Off,
                ..crate::DlConfig::default()
            },
            ..crate::StoreConfig::in_memory()
        };
        let store = Arc::new(StoreService::new(config).unwrap());
        let reasoner = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl);
        let pipeline = crate::MutationPipeline::new(
            Arc::clone(&store),
            Arc::new(ReasonerService::new(reasoner)),
        );
        commit(&pipeline, &format!("INSERT DATA {{ {OFF_ROUTE}{data} }}"));
        commit(&pipeline, "INSERT DATA { :rex a :Pet . }");
        *store.dl().status.lock().unwrap() = None;
        let snapshot = store.engine().snapshot();
        let reads: Vec<(bool, usize)> = (0..3)
            .map(|_| {
                ONTOLOGY_PASSES.set(0);
                let told = reads_differently(&store, Revision::Committed(&snapshot));
                (told.is_some(), ONTOLOGY_PASSES.get())
            })
            .collect();
        let passes: usize = reads.iter().map(|&(_, p)| p).sum();
        if passes > 1 || reads.iter().any(|&(told, _)| told != left_out) {
            wrong.push(format!("{name}: (told, passes) {reads:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Off the route, `:p` has a domain and is given a literal and an IRI: a name of two kinds,
/// which breaks the premise of the rules' closure. With no record of the revision (as a
/// reopened store has none), the consistency check a read runs records what the premise
/// tells, never "nothing left out", and a committed read is told it.
#[test]
fn a_consistency_check_records_what_the_premise_tells() {
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    let store = Arc::new(StoreService::new(crate::StoreConfig::in_memory()).unwrap());
    let pipeline = crate::MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    );
    let two_kinds = ":C a owl:Class . :p rdfs:domain :C . :a :p \"x\" . :b :p :c .";
    commit(
        &pipeline,
        &format!("INSERT DATA {{ {OFF_ROUTE}{two_kinds} }}"),
    );
    *store.dl().status.lock().unwrap() = None;
    let snapshot = store.engine().snapshot();
    let _ = consistency_at(
        &store,
        &snapshot,
        &unproved_view(&snapshot),
        &CancellationToken::new(),
        Deadline::after(store.config().dl.timeout),
        &nrese_exec::Envelope::unlimited(),
    );
    let recorded = store.dl().status().expect("a record").left_out;
    assert!(
        matches!(&recorded, Some(Some(why)) if why.contains("two kinds")),
        "the record names the two kinds: {recorded:?}"
    );
    let told = reads_differently(&store, Revision::Committed(&snapshot));
    assert!(told.is_some(), "a committed read is told it");
}

const PREF_LABEL: &str = "<http://www.w3.org/2004/02/skos/core#prefLabel>";

/// A store with an annotation-property axiom of each kind (a domain, a range, a
/// super-property: SKOS declares its labels annotation properties under `rdfs:label`), or
/// with two annotation properties that have axioms, and no term equal to another. A read off
/// the rules route is told the mark the whole ontology gives, naming the property, committed
/// and inside a transaction, and its mark takes it from the store's indexes, without going
/// to the whole ontology. A blank node declared an annotation property names none: the reader
/// leaves its axiom out, and the store is told that part left out (the commit's record, which
/// names the part where the whole ontology alone doesn't: compared without it, [`first_named`]).
#[test]
fn an_annotation_property_axiom_marks_the_read_without_the_ontology() {
    let property = |p: &str| format!("{p} a owl:AnnotationProperty ; ");
    let stores: [(&str, String, &[&str]); 5] = [
        (
            "domain only",
            property(":d") + "rdfs:domain :Pet .",
            &["<urn:test:d>"],
        ),
        (
            "range only",
            property(":r") + "rdfs:range :Pet .",
            &["<urn:test:r>"],
        ),
        (
            "sub-property only",
            property(PREF_LABEL)
                + &format!("rdfs:subPropertyOf rdfs:label . :tib {PREF_LABEL} \"Tib\" ."),
            &[PREF_LABEL],
        ),
        (
            "two properties",
            property(":m") + "rdfs:domain :Pet . " + &property(":n") + "rdfs:range :Pet .",
            &["<urn:test:m>", "<urn:test:n>"],
        ),
        ("a blank node", property("[]") + "rdfs:domain :Pet .", &[]),
    ];
    let mut wrong = Vec::new();
    for (store, data, names) in stores {
        let (reads, whole) =
            marks_and_passes(crate::StoreConfig::in_memory(), &data, ":rex a :Pet .");
        eprintln!("{store}: [committed, pending] {reads:?}, the whole ontology {whole:?}");
        let named = |w: &String| {
            let name = |n: &&str| w.starts_with(&format!("{n} has annotation-property axioms"));
            let left_out = "1 part(s) of the data aren't OWL 2 DL and were left out";
            names.iter().any(name) || names.is_empty() && w.starts_with(left_out)
        };
        let unnamed = |w: &String| first_named(&Some(w.clone()));
        let told: Vec<_> = whole.iter().map(unnamed).collect();
        let right = whole.as_ref().is_some_and(named)
            && reads.len() == 2
            && reads.iter().all(|(r, passes)| {
                (r.iter().map(unnamed).collect::<Vec<_>>(), *passes) == (told.clone(), 0)
            });
        if !right {
            wrong.push(store);
        }
    }
    assert!(
        wrong.is_empty(),
        "wrong: {wrong:?}; each store's (the mark's reasons, passes over the whole ontology) above"
    );
}

/// The control: a class name that is also an individual equal to another (under compact
/// equality) is a case only the ontology decides. Each read is told the mark naming it,
/// committed and inside a transaction, and its mark goes to the whole ontology once.
#[test]
fn a_punned_name_equal_to_another_is_marked_by_one_pass_over_the_ontology() {
    let config = crate::StoreConfig {
        equality_by_representatives: true,
        equality_compact: true,
        ..crate::StoreConfig::in_memory()
    };
    let (reads, _) = marks_and_passes(
        config,
        ":Dog a owl:Class , :Species ; owl:sameAs :Canis . :rex a :Dog .",
        ":fido a :Dog .",
    );
    let right = |(marks, passes): &(Vec<String>, usize)| {
        *passes == 1
            && marks.len() == 1
            && marks[0].starts_with("<urn:test:Dog> is a class or property name")
    };
    assert!(
        reads.len() == 2 && reads.iter().all(right),
        "[committed, pending]: (the mark's reasons, passes over the whole ontology): {reads:#?}"
    );
}

/// A small deterministic generator (the tests need no randomness crate).
struct Draw(u64);

impl Draw {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % n as u64) as usize
    }

    fn pick<T: Clone>(&mut self, from: &[T]) -> T {
        from[self.below(from.len())].clone()
    }
}

/// A reason without the part left out it names first (`left_out`): the commit's read and a
/// later one may meet the parts in another order.
fn first_named(reason: &Option<String>) -> Option<String> {
    let reason = reason.as_ref()?;
    Some(
        match (reason.find("; first: "), reason.find(", whose statements")) {
            (Some(a), Some(b)) if a < b => format!("{}{}", &reason[..a], &reason[b..]),
            _ => reason.clone(),
        },
    )
}

/// What the store's indexes decide of the mark is what the whole ontology decides. Random
/// stores under `owl2-dl`, with and without compact equality: declarations (OWL 1's
/// `owl:OntologyProperty` among them), schema
/// statements and blank-node class expressions (whole or broken) on names, on blank nodes and
/// on the nine built-in annotation properties, as subjects and as super-properties; property
/// assertions with names and
/// literals (a property's kind flips with its use); `owl:sameAs`, asserted and through
/// functional properties; over the default and two named graphs. Each is read committed and
/// with random pending additions and deletions: the mark equals the whole ontology's, reason
/// for reason, but for the parts left out: a committed read is told what the commit recorded
/// (which must be the whole's), a pending one the base's record; a part left out that no
/// record tells comes after the indexes' decision. Which part left out a reason names first
/// follows the read's order, so it isn't compared. And wherever the indexes decide, their
/// answer is the whole ontology's past what it left out (most random stores leave a part out,
/// whose reason comes first). `NRESE_FUZZ_SEED` (default 0) varies the stores; each run prints
/// how often the indexes decided and how often the whole ontology found each reason.
#[test]
fn the_mark_by_the_indexes_is_the_mark_of_the_whole_ontology_on_random_stores() {
    use crate::{MutationCommand, MutationTicket, Requester, SparqlUpdateRequest};
    use nrese_rdf::{BlankNode, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term};
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    const CASES: usize = 40;
    let iri = |ns: &str, local: &str| NamedNode::new_unchecked(format!("{ns}{local}"));
    let rdf = |l: &str| iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#", l);
    let rdfs = |l: &str| iri("http://www.w3.org/2000/01/rdf-schema#", l);
    let owl = |l: &str| iri("http://www.w3.org/2002/07/owl#", l);
    let mut names: Vec<NamedNode> = ["a", "b", "c", "A", "B", "p", "q"]
        .iter()
        .map(|n| iri("urn:test:", n))
        .collect();
    names.extend([
        iri("http://www.w3.org/2004/02/skos/core#", "prefLabel"),
        rdfs("label"),
    ]);
    // The other eight built-in annotation properties (rdfs:label is a name), as subjects and
    // as objects (a property's super-property), never declared.
    let builtins: Vec<NamedNode> = nrese_owl::BUILTIN_ANNOTATIONS[1..]
        .iter()
        .map(|i| NamedNode::new_unchecked(*i))
        .collect();
    let blanks: Vec<BlankNode> = (0..4)
        .map(|i| BlankNode::new_unchecked(format!("x{i}")))
        .collect();
    let mut types: Vec<NamedNode> = [
        "Class",
        "ObjectProperty",
        "DatatypeProperty",
        "AnnotationProperty",
        "OntologyProperty",
        "NamedIndividual",
        "Restriction",
        "FunctionalProperty",
    ]
    .into_iter()
    .map(owl)
    .collect();
    types.extend(names.iter().cloned());
    let mut schema: Vec<NamedNode> = ["subClassOf", "domain", "range", "subPropertyOf"]
        .into_iter()
        .map(rdfs)
        .collect();
    schema.extend(
        ["equivalentClass", "onProperty", "someValuesFrom", "unionOf"]
            .into_iter()
            .map(owl),
    );
    schema.extend([rdf("first"), rdf("rest"), rdfs("comment")]);
    let properties = [
        names[5].clone(),
        names[6].clone(),
        names[7].clone(),
        rdfs("label"),
    ];
    let mut objects: Vec<Term> = names.iter().cloned().map(Term::from).collect();
    objects.extend(blanks.iter().cloned().map(Term::from));
    objects.extend(builtins.iter().cloned().map(Term::from));
    objects.extend([
        Literal::new_simple_literal("l").into(),
        rdf("nil").into(),
        owl("Class").into(),
        owl("Thing").into(),
    ]);
    let graphs = [
        GraphName::DefaultGraph,
        iri("urn:test:", "g1").into(),
        iri("urn:test:", "g2").into(),
    ];
    let statement = |r: &mut Draw| {
        let subject: NamedOrBlankNode = match r.below(5) {
            0 => r.pick(&blanks).into(),
            1 => r.pick(&builtins).into(),
            _ => r.pick(&names).into(),
        };
        let (predicate, object): (NamedNode, Term) = match r.below(6) {
            0 | 1 => (rdf("type"), r.pick(&types).into()),
            2 | 3 => (r.pick(&schema), r.pick(&objects)),
            4 => (r.pick(&properties), r.pick(&objects)),
            _ => (owl("sameAs"), r.pick(&names).into()),
        };
        Quad::new(subject, predicate, object, r.pick(&graphs))
    };
    let sparql = |q: &Quad| match &q.graph_name {
        GraphName::DefaultGraph => format!("{} {} {} . ", q.subject, q.predicate, q.object),
        g => format!(
            "GRAPH {g} {{ {} {} {} . }} ",
            q.subject, q.predicate, q.object
        ),
    };
    let fuzz: u64 = std::env::var("NRESE_FUZZ_SEED")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    let mut r = Draw(fuzz.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x6d61_726b);
    let (mut wrong, mut refused, mut keeping) = (Vec::new(), 0, 0);
    // By the indexes: a reason, none, the ontology's to decide; by the whole ontology: an
    // annotation reason, a part left out, a punning one.
    let (mut by_indexes, mut found) = ([0usize; 3], [0usize; 3]);
    // Past what the whole ontology left out: an annotation reason, a punning one.
    let mut past_found = [0usize; 2];
    for case in 0..CASES {
        let compact = r.below(2) == 0;
        let config = crate::StoreConfig {
            equality_by_representatives: compact,
            equality_compact: compact,
            ..crate::StoreConfig::in_memory()
        };
        let store = Arc::new(StoreService::new(config).unwrap());
        let pipeline = crate::MutationPipeline::new(
            Arc::clone(&store),
            Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
                ReasoningMode::Owl2Dl,
            ))),
        );
        let data: String = (0..4 + r.below(14))
            .map(|_| sparql(&statement(&mut r)))
            .collect();
        let insert = SparqlUpdateRequest::new(format!("INSERT DATA {{ {data} }}"));
        let committed = pipeline.apply(
            MutationCommand::Update(insert),
            &Requester::all(),
            &MutationTicket::new(),
        );
        if committed.is_err() {
            refused += 1;
            continue;
        }
        let snapshot = store.engine().snapshot();
        let mut tx = store
            .engine()
            .speculative(Arc::new(nrese_exec::Budget::unlimited()));
        let added: Vec<String> = (0..r.below(5))
            .map(|_| {
                let quad = statement(&mut r);
                tx.try_insert(quad.as_ref()).unwrap();
                quad.to_string()
            })
            .collect();
        let asserted: Vec<_> = snapshot
            .quads_for_pattern_in(ReadModel::Asserted, &nrese_engine::QuadPattern::all())
            .collect();
        let removed: Vec<String> = (0..r.below(3))
            .filter(|_| !asserted.is_empty())
            .map(|_| {
                let quad = asserted[r.below(asserted.len())];
                tx.try_remove_encoded(quad).unwrap();
                format!("{:?}", snapshot.decode_quad(quad))
            })
            .collect();
        let pending = tx.pending_snapshot().unwrap();
        let keeps_record = store.dl().bounds.keeps_record(&tx);
        keeping += usize::from(keeps_record);
        let at = Revision::Pending {
            snapshot: &pending,
            deletes: true,
            keeps_record,
        };
        let reads = [
            ("committed", Revision::Committed(&snapshot), &snapshot),
            ("pending", at, &pending),
        ];
        // What the commit recorded left out; a committed read checks it against the whole.
        let record = store
            .dl()
            .status()
            .filter(|s| s.revision == snapshot.revision())
            .and_then(|s| s.left_out);
        for (of, at, read) in reads {
            let Ok(whole) = read_ontology(&store, read) else {
                refused += 1;
                continue;
            };
            let full = by_ontology(read, whole.value());
            let left = full.as_ref().is_some_and(|f| f.contains("were left out"));
            let expected = match (of, &record) {
                // Pending: the base's record, where it has one.
                ("pending", Some(Some(why))) => Some(why.clone()),
                ("committed", Some(_)) => full.clone(),
                // The limit: a part left out that no record tells (a revision the store
                // didn't check) comes after the indexes' decision. A pending read takes a
                // clean record from its base only where its changes keep it: the mark is then
                // still the whole pending ontology's.
                _ if left => match by_index(read) {
                    Some(Some(why)) => Some(why),
                    _ => full.clone(),
                },
                _ => full.clone(),
            };
            let mark = reads_differently(&store, at);
            by_indexes[match by_index(read) {
                Some(Some(_)) => 0,
                Some(None) => 1,
                None => 2,
            }] += 1;
            // The indexes decide annotation-property axioms and punning, which the whole
            // ontology tells after what it left out and what the rules read otherwise (a name
            // of two kinds, a structure no axiom uses): the indexes never see those, and
            // decide only beside a record that tells them.
            let mut kept = nrese_owl::Ontology::clone(whole.value());
            kept.diagnostics
                .retain(|d| !d.leaves_out() && !d.reads_otherwise());
            let past = by_ontology(read, &kept);
            if let Some(reason) = &past {
                past_found[usize::from(!reason.contains("annotation-property axioms"))] += 1;
            }
            if let Some(decided) = by_index(read)
                && decided != past
            {
                wrong.push(format!(
                    "case {case} ({of}, compact equality {compact}): the indexes {decided:?}, \
                     the whole ontology past what it left out {past:?}\n  committed: {data}\n  \
                     pending: + {added:?} - {removed:?}"
                ));
            }
            if let Some(reason) = &full {
                found[if reason.contains("annotation-property axioms") {
                    0
                } else if left {
                    1
                } else {
                    2
                }] += 1;
            }
            if first_named(&mark) != first_named(&expected) {
                wrong.push(format!(
                    "case {case} ({of}, compact equality {compact}): the mark {mark:?}, \
                     expected {expected:?}, the whole ontology {full:?}\n  committed: \
                     {data}\n  pending: + {added:?} - {removed:?}"
                ));
            }
        }
    }
    eprintln!(
        "NRESE_FUZZ_SEED={fuzz}: {CASES} stores, {refused} refused, {keeping} transactions that \
         keep their base's record; by the indexes: {} reasons, {} none, {} to the ontology; by \
         the whole ontology: {} annotation, {} left-out and {} punning reasons; past what it \
         left out: {} annotation and {} punning",
        by_indexes[0],
        by_indexes[1],
        by_indexes[2],
        found[0],
        found[1],
        found[2],
        past_found[0],
        past_found[1]
    );
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert!(
        refused * 4 < CASES,
        "{refused} of {CASES} stores or reads refused"
    );
}

/// Spec (a pending read takes its base's record of what the reader left out only where its
/// changes keep it, as their commit would: `Bounds::keeps_record`): on random stores off the
/// rules route over a small vocabulary (declared classes, an object property with a domain, a
/// data property, an undeclared property with a range), and random transactions of
/// statements about its individuals that may add or delete (a class membership, a name of the
/// vocabulary as an individual, a property's value of either kind, an equality), the mark a
/// pending read is told is the mark it is told where it takes no record and reads its own
/// ontology. Each store also has `:a :p :b`, sometimes with a reification of it, and besides a
/// random transaction three fixed ones: one adding `:c a :A`, one deleting `:a :p :b`, one
/// adding `:b owl:sameAs :c`; the first store has nothing random, so each seed has a record
/// taken over by an insert and by a delete, and kept by the equality (whose read the indexes
/// leave to the ontology). `NRESE_FUZZ_SEED` (default 0) varies them; each run prints, per kind of
/// transaction, how many kept the record and how many of those the base's clean record
/// answered, and how many deletions a reification kept from it.
#[test]
fn a_kept_record_marks_a_pending_read_as_its_own_ontology_does() {
    use crate::{MutationCommand, MutationTicket, Requester, SparqlUpdateRequest};
    use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    const CASES: usize = 40;
    let ex = |local: &str| NamedNode::new_unchecked(format!("urn:test:{local}"));
    let schema = ":A a owl:Class . :B a owl:Class . :p a owl:ObjectProperty ; rdfs:domain :A . \
                  :d a owl:DatatypeProperty . :q rdfs:range :B . ";
    let individuals: Vec<NamedNode> = ["a", "b", "c", "tib"].map(ex).into();
    // Classes, and names of the vocabulary as an individual's class or its equal.
    let types: Vec<NamedNode> = ["A", "B", "Pet", "Dog", "p"].map(ex).into();
    let properties: Vec<NamedNode> = ["p", "d", "q", "owner"].map(ex).into();
    let same_as = NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs");
    let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    // `equal`: whether an equality may be drawn (the base has none: U1 then has no classes).
    let statement = |r: &mut Draw, equal: bool| {
        let s = r.pick(&individuals);
        let (p, o): (NamedNode, Term) = match r.below(if equal { 5 } else { 4 }) {
            0 => (rdf_type.clone(), r.pick(&types).into()),
            1 => (r.pick(&properties), r.pick(&individuals).into()),
            2 => (r.pick(&properties), Literal::new_simple_literal("v").into()),
            3 => (r.pick(&properties), r.pick(&types).into()),
            _ => (same_as.clone(), r.pick(&individuals).into()),
        };
        Quad::new(s, p, o, GraphName::DefaultGraph)
    };
    let fuzz: u64 = std::env::var("NRESE_FUZZ_SEED")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    let mut r = Draw(fuzz.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x6b65_6570);
    // `:a :p :b`, which every store has and the fixed deletion removes, and its reification.
    let anchor = Quad::new(ex("a"), ex("p"), ex("b"), GraphName::DefaultGraph);
    let reification = "[ a owl:Axiom ; owl:annotatedSource :a ; owl:annotatedProperty :p ; \
                       owl:annotatedTarget :b ; rdfs:comment \"r\" ] . ";
    let member = Quad::new(ex("c"), rdf_type.clone(), ex("A"), GraphName::DefaultGraph);
    let equal = Quad::new(ex("b"), same_as.clone(), ex("c"), GraphName::DefaultGraph);
    const KINDS: [&str; 4] = ["random", "insert", "delete", "same-as"];
    let (mut wrong, mut refused, mut reified_kept_out) = (Vec::new(), 0, 0);
    // Per kind of transaction: how many kept the record, how many the clean record answered.
    let (mut keeping, mut answered) = ([0usize; 4], [0usize; 4]);
    for case in 0..CASES {
        let store = Arc::new(StoreService::new(crate::StoreConfig::in_memory()).unwrap());
        let pipeline = crate::MutationPipeline::new(
            Arc::clone(&store),
            Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
                ReasoningMode::Owl2Dl,
            ))),
        );
        let random = if case == 0 { 0 } else { 2 + r.below(6) };
        let reified = case > 0 && r.below(3) == 0;
        let mut data: String = (0..random)
            .map(|_| {
                let q = statement(&mut r, false);
                format!("{} {} {} . ", q.subject, q.predicate, q.object)
            })
            .collect();
        data += ":a :p :b . ";
        if reified {
            data += reification;
        }
        let insert = format!("{MARK_PREFIXES}INSERT DATA {{ {OFF_ROUTE}{schema}{data} }}");
        let committed = pipeline.apply(
            MutationCommand::Update(SparqlUpdateRequest::new(insert)),
            &Requester::all(),
            &MutationTicket::new(),
        );
        if committed.is_err() {
            refused += 1;
            continue;
        }
        let snapshot = store.engine().snapshot();
        let record = (store.dl().status())
            .filter(|s| s.revision == snapshot.revision())
            .and_then(|s| s.left_out);
        for (kind, name) in KINDS.iter().enumerate() {
            let mut tx = store
                .engine()
                .speculative(Arc::new(nrese_exec::Budget::unlimited()));
            let (mut added, mut removed) = (Vec::new(), Vec::new());
            match kind {
                0 => {
                    for _ in 0..1 + r.below(3) {
                        let quad = statement(&mut r, true);
                        tx.try_insert(quad.as_ref()).unwrap();
                        added.push(quad.to_string());
                    }
                    let asserted: Vec<_> = snapshot
                        .quads_for_pattern_in(
                            ReadModel::Asserted,
                            &nrese_engine::QuadPattern::all(),
                        )
                        .collect();
                    for _ in 0..r.below(2) {
                        let quad = asserted[r.below(asserted.len())];
                        tx.try_remove_encoded(quad).unwrap();
                        removed.push(format!("{:?}", snapshot.decode_quad(quad)));
                    }
                }
                1 => {
                    tx.try_insert(member.as_ref()).unwrap();
                    added.push(member.to_string());
                }
                2 => {
                    tx.remove(anchor.as_ref()).unwrap();
                    removed.push(anchor.to_string());
                }
                _ => {
                    tx.try_insert(equal.as_ref()).unwrap();
                    added.push(equal.to_string());
                }
            }
            let pending = tx.pending_snapshot().unwrap();
            let keeps_record = store.dl().bounds.keeps_record(&tx);
            let at = |keeps_record| Revision::Pending {
                snapshot: &pending,
                deletes: !removed.is_empty(),
                keeps_record,
            };
            let told = reads_differently(&store, at(keeps_record));
            let own = reads_differently(&store, at(false));
            keeping[kind] += usize::from(keeps_record);
            // The base's clean record and the indexes decided it, without the ontology.
            let by_record =
                keeps_record && record == Some(None) && by_index(&pending) == Some(None);
            answered[kind] += usize::from(by_record);
            reified_kept_out += usize::from(kind == 2 && reified && !keeps_record);
            if told != own {
                wrong.push(format!(
                    "case {case}, {name}: told {told:?}, its own ontology {own:?}\n  \
                     committed: {data}\n  pending: + {added:?} - {removed:?}"
                ));
            }
        }
    }
    eprintln!(
        "NRESE_FUZZ_SEED={fuzz}: {CASES} stores, {refused} refused; kept the record (random, \
         insert, delete, same-as): {keeping:?}; answered clean by it: {answered:?}; deletions \
         of a reified triple that took no record: {reified_kept_out}"
    );
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert!(refused * 4 < CASES, "{refused} of {CASES} stores refused");
    assert!(
        answered[1] > 0 && answered[2] > 0 && keeping[3] > 0,
        "no insert or no delete took its base's clean record, or no equality kept it: \
         {keeping:?}, {answered:?}"
    );
}

/// A question naming an individual the data doesn't name is decided only where the
/// ontology has no key. Where the ontology can't be read within its budget (nothing cached
/// for the revision and the bounds' memory spent, between the decision and the status), it
/// isn't known whether it has one, so the answer is `sound-only`; it is still given, evaluated
/// afresh with the memory back (the cached one was `complete`'s). On the rules route, whose
/// status reads no ontology otherwise. The control: the same read with the memory free, over
/// an ontology without keys, is decided.
#[test]
fn a_question_naming_an_individual_the_data_doesnt_is_not_complete_when_the_ontology_isnt_read() {
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    let store = Arc::new(
        StoreService::new(crate::StoreConfig {
            total_query_memory_bytes: 64 << 20,
            ..crate::StoreConfig::in_memory()
        })
        .unwrap(),
    );
    let pipeline = crate::MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    );
    commit(
        &pipeline,
        "INSERT DATA { :A a owl:Class . :a a :A . :r a owl:ObjectProperty . :a :r :x . }",
    );
    assert_eq!(store.dl_bounds().last, "rules", "the rules route");
    let prepared = store
        .prepare_query(&crate::SparqlQueryRequest::all(format!(
            "{MARK_PREFIXES}ASK {{ :b a :A }}"
        )))
        .unwrap();
    // The run's answer and status. The status is told before the answers are evaluated, and
    // `told` runs then: a status other than the cached answer's evaluates them afresh, so
    // the memory the hook spent is given back there.
    let run = |told: &dyn Fn()| {
        let (mut status, mut out) = (None, Vec::new());
        store
            .run_query_reporting(&prepared, &CancellationToken::new(), &mut out, |s, _| {
                status = s;
                told();
                None
            })
            .unwrap();
        (String::from_utf8(out).unwrap(), status.expect("a status"))
    };
    let (answer, free) = run(&|| {});
    assert!(
        free.complete && answer.contains("false"),
        "the control: {answer} {free:?}"
    );
    let kept = Arc::clone(store.dl().bounds.kept());
    let spent = std::rc::Rc::new(std::cell::Cell::new(0));
    let (cache, budget, held) = (
        Arc::clone(&store),
        Arc::clone(&kept),
        std::rc::Rc::clone(&spent),
    );
    crate::service::hooks::AFTER_DECIDED.set(Some(Box::new(move || {
        drop(cache.dl().query_ontology.0.lock().unwrap().take());
        let rest = budget.remaining();
        budget.charge(rest).unwrap();
        held.set(rest);
    })));
    let given = std::cell::Cell::new(0);
    let (answer, refused) = run(&|| {
        given.set(spent.take());
        kept.release(given.get());
    });
    assert!(given.get() > 0, "the hook spent the bounds' memory");
    assert!(
        refused.sound && !refused.complete,
        "with the ontology unread: {refused:?}"
    );
    assert!(
        answer.contains("false"),
        "the answer, evaluated afresh: {answer}"
    );
}

/// The unions store of `tests/it/dl/model_labels.rs` with `n` individuals, the `k`-th with
/// the value `v(k mod 4)`, a candidate limit of 4, and the model's refutations on or off.
fn unions_store(model_labels: bool, n: usize) -> Arc<StoreService> {
    padded_unions_store(model_labels, n, 0)
}

/// [`unions_store`], the names of the individuals with `v2` and `v3` (the gap of `?x a :U0`)
/// longer by `pad` bytes.
fn padded_unions_store(model_labels: bool, n: usize, pad: usize) -> Arc<StoreService> {
    let dl = crate::DlConfig {
        max_candidates: Some(4),
        model_labels,
        ..crate::DlConfig::default()
    };
    unions_store_with(dl, n, pad)
}

/// [`padded_unions_store`] under the DL settings `dl`.
fn unions_store_with(dl: crate::DlConfig, n: usize, pad: usize) -> Arc<StoreService> {
    let mut data = String::new();
    for k in 0..n {
        let pad = "x".repeat(if k % 4 < 2 { 0 } else { pad });
        data.push_str(&format!(":i{k}{pad} :val :v{} .\n", k % 4));
    }
    unions_store_of(dl, &data)
}

/// The unions of `tests/it/dl/model_labels_unions.ttl` with the individuals `data` states,
/// committed in that order, under the DL settings `dl`.
fn unions_store_of(dl: crate::DlConfig, data: &str) -> Arc<StoreService> {
    let data = format!(
        "{}{data}",
        include_str!("../../tests/it/dl/model_labels_unions.ttl")
    );
    let config = crate::StoreConfig {
        dl,
        ..crate::StoreConfig::in_memory()
    };
    lower_true_store_with(config, &data)
}

/// A read's answers' bytes and its status with what decided it.
type Read = (
    Vec<u8>,
    Option<(nrese_sparql::Completeness, crate::dl::DlDetail)>,
);

/// `SELECT ?x { ?x a :U0 }` over `store` within `memory` bytes.
fn unions_read(store: &StoreService, memory: Option<usize>) -> crate::StoreResult<Read> {
    unions_read_of(store, "?x a <urn:test:U0>", memory)
}

/// `SELECT ?x { pattern }` over `store` within `memory` bytes.
fn unions_read_of(
    store: &StoreService,
    pattern: &str,
    memory: Option<usize>,
) -> crate::StoreResult<Read> {
    let mut request = crate::SparqlQueryRequest::all(format!("SELECT ?x {{ {pattern} }}"));
    request.memory_limit = memory;
    let prepared = store.prepare_query(&request)?;
    let mut out = Vec::new();
    let status = store.run_query_dl(&prepared, &CancellationToken::new(), &mut out)?;
    Ok((out, status))
}

/// Spec: a setting turns the model's refutations on, off by default, and off a read is decided
/// by each candidate's own test as before: over the 24 individuals with a candidate limit of 4,
/// the 4 admitted refuted, 8 unresolved, the read not `complete`, none refuted by labels. On,
/// the model refutes all 12 (*Direct Semantics* §2.5: one model with the candidate outside the
/// class), the read is `complete`, and its rows are the same 12.
#[test]
fn without_model_labels_a_read_is_decided_by_its_tests_alone() {
    let default = crate::DlConfig::default().model_labels;
    assert!(!default, "off by default");
    let (off, on) = (unions_store(default, 24), unions_store(true, 24));
    let (off_rows, off_status) = unions_read(&off, None).unwrap();
    let (on_rows, on_status) = unions_read(&on, None).unwrap();
    let ((off_status, off_detail), (on_status, on_detail)) =
        (off_status.unwrap(), on_status.unwrap());
    println!("off {off_status:?} {off_detail:?}; on {on_status:?} {on_detail:?}");
    let bounds =
        |s: &nrese_sparql::Completeness| s.bounds.map(|b| (b.lower, b.upper, b.unresolved));
    assert_eq!(bounds(&off_status), Some((12, 24, 8)));
    assert_eq!(
        (
            off_detail.proved,
            off_detail.refuted,
            off_detail.refuted_by_labels
        ),
        (0, 4, 0)
    );
    assert!(!off_status.complete);
    assert_eq!(off_rows, on_rows, "the same rows");
    assert_eq!(bounds(&on_status), Some((12, 24, 0)));
    assert_eq!(
        (
            on_detail.proved,
            on_detail.refuted,
            on_detail.refuted_by_labels
        ),
        (0, 12, 12)
    );
    assert!(on_status.complete, "{:?}", on_status.reasons);
}

/// Spec: the model's refutations need the whole gap selected, but never fail a read: under
/// each request memory limit from 16 KiB to 64 MiB (steps of a tenth), over 400 individuals,
/// a read answered with the refutations off is answered with them on. And the fallback is
/// reached: at the least limit the read is answered under with them off (found to 256 bytes),
/// or within 16 KiB above it, the whole gap (400 rows) has no room beside what the capped
/// selection needs, and the read with them on says so (`capped-gap`) and is answered. Every
/// read, on both sides, is the first of a store just committed: a store's first read holds
/// more than its later ones, so only first reads compare like with like.
#[test]
fn a_read_answered_without_model_labels_is_answered_with_them_under_every_memory_limit() {
    let first = |labels: bool, limit: usize| unions_read(&unions_store(labels, 400), Some(limit));
    let (mut limit, mut answered, mut failed) = (16usize << 10, 0, 0);
    while limit <= 64 << 20 {
        match first(false, limit) {
            Ok(_) => {
                answered += 1;
                let with = first(true, limit);
                assert!(
                    with.is_ok(),
                    "{limit} bytes: off answered, on {:?}",
                    with.err()
                );
            }
            Err(_) => failed += 1,
        }
        limit += limit / 10;
    }
    println!("off answered under {answered} limits and failed under {failed}");
    assert!(
        answered > 0 && failed > 0,
        "the limits cross the read's need"
    );
    let (mut low, mut high) = (16usize << 10, 64usize << 20);
    while high - low > 256 {
        let middle = low + (high - low) / 2;
        match first(false, middle) {
            Ok(_) => high = middle,
            Err(_) => low = middle,
        }
    }
    let capped = |limit: usize| match first(true, limit) {
        Ok((_, Some((_, detail)))) => detail.paths.contains(&"capped-gap"),
        _ => false,
    };
    let fallback = (0..64).map(|k| high + k * 256).find(|&limit| capped(limit));
    println!("off answered from {high} bytes; on fell back to the capped gap at {fallback:?}");
    assert!(
        fallback.is_some(),
        "no fallback within 16 KiB of {high} bytes"
    );
}

/// Spec: where the whole gap's selection fits but the labels' own run has no room, the read
/// with them on refutes none by labels, and it needs no more than the read with them off. With
/// the gap's 200 candidates named by 8000-byte IRIs, the decoded rows of a batch, not the
/// selection's fixed size per term, set a read's need. Under each request memory limit from
/// 16 KiB to 4 MiB (steps of a fiftieth), and in steps of 256 bytes over the 16 KiB from the
/// least limit the read is answered under with them off (found to 256 bytes), every read the
/// first of a store just committed, a read answered with the refutations off is answered with
/// them on. And the case is reached: under some limit the read with them on selects the whole
/// gap (no `capped-gap`), refutes none by labels, and is answered; and under the largest limit
/// the same store's read refutes all 200 by labels, so no room is why none were.
#[test]
fn a_read_whose_labels_have_no_room_needs_no_more_memory_than_without_them() {
    let first = |labels: bool, limit: usize| {
        unions_read(&padded_unions_store(labels, 400, 8000), Some(limit))
    };
    let (mut low, mut high) = (16usize << 10, 4usize << 20);
    while high - low > 256 {
        let middle = low + (high - low) / 2;
        match first(false, middle) {
            Ok(_) => high = middle,
            Err(_) => low = middle,
        }
    }
    let mut limits: Vec<usize> = (0..64).map(|k| high + k * 256).collect();
    let mut limit = 16usize << 10;
    while limit <= 4 << 20 {
        limits.push(limit);
        limit += limit / 50;
    }
    let largest = *limits.iter().max().unwrap();
    let mut reached = Vec::new();
    for limit in limits {
        if first(false, limit).is_ok() {
            let with = first(true, limit);
            assert!(
                with.is_ok(),
                "{limit} bytes: off answered, on {:?}",
                with.err()
            );
            if let Ok((_, Some((_, detail)))) = &with
                && !detail.paths.contains(&"capped-gap")
                && detail.refuted_by_labels == 0
            {
                reached.push(limit);
            }
        }
    }
    println!(
        "off answered from {high} bytes; the whole gap selected, none refuted by labels, answered under {reached:?}"
    );
    assert!(!reached.is_empty(), "no limit reaches the case");
    let (_, status) = first(true, largest).unwrap();
    let detail = status.expect("a status").1;
    println!("under {largest} bytes: {detail:?}");
    assert_eq!(detail.refuted_by_labels, 200, "with room, all 200");
}

/// Spec: the labels' quarter is taken from what the read has left, never from a fresh clock: a
/// read whose deadline has passed when it begins gives them no time, so no model is run. With
/// `dl.timeout` an hour (a fresh quarter: 15 minutes) and a deadline passed before the read,
/// over the 24 individuals with a candidate limit of 4: the read with the labels on is decided
/// exactly as with them off, by its tests alone, which the deadline leaves undecided: none
/// proved or refuted, none by labels, 12 unresolved, not `complete`, the same status and
/// detail. And the labels are live: the same read under the hour refutes all 12 by labels.
/// Nothing waits; no time is measured.
// By reading only: the labels' deadline is fixed before `gap::select`, and `run.deadline` is set.
#[test]
fn a_read_whose_deadline_has_passed_gives_the_labels_no_time() {
    let read = |model_labels: bool, deadline: Deadline| {
        let dl = crate::DlConfig {
            max_candidates: Some(4),
            model_labels,
            timeout: Some(std::time::Duration::from_secs(3600)),
            ..crate::DlConfig::default()
        };
        let store = unions_store_with(dl, 24, 0);
        let request = crate::SparqlQueryRequest::all("SELECT ?x { ?x a <urn:test:U0> }");
        let prepared = PreparedQuery::parse(&request).unwrap();
        let outcome = decide_answers(
            &store,
            &prepared,
            &analyse(prepared.query()),
            &crate::dl::bounds::view(&store),
            (&CancellationToken::new(), deadline),
            &nrese_exec::Envelope::unlimited(),
            DlAnswers::default(),
        )
        .unwrap();
        let (Outcome::Stream(status, _, detail) | Outcome::Answers(_, status, detail)) = outcome;
        println!("labels {model_labels}: {status:?} {detail:?}");
        (status, detail)
    };
    let passed = Deadline::at(std::time::Instant::now());
    let (off, off_detail) = read(false, passed);
    let bounds = off.bounds.map(|b| (b.lower, b.upper, b.unresolved));
    assert_eq!(bounds, Some((12, 24, 12)));
    let counts = |d: &crate::dl::DlDetail| (d.proved, d.refuted, d.refuted_by_labels);
    assert_eq!((counts(&off_detail), off.complete), ((0, 0, 0), false));
    let (on, on_detail) = read(true, passed);
    assert_eq!(
        format!("{on:?} {on_detail:?}"),
        format!("{off:?} {off_detail:?}"),
        "status and detail as with them off"
    );
    let hour = Deadline::after(Some(std::time::Duration::from_secs(3600)));
    assert_eq!(counts(&read(true, hour).1).2, 12, "under the hour, all 12");
}

/// The unions' store of 24 individuals with a candidate limit of 4 and no `dl.timeout`, its
/// view built.
fn capped_unions_store() -> Arc<StoreService> {
    let dl = crate::DlConfig {
        max_candidates: Some(4),
        timeout: None,
        ..crate::DlConfig::default()
    };
    let store = unions_store_with(dl, 24, 0);
    let _ = crate::dl::bounds::view(&store);
    store
}

/// `SELECT ?x { ?x a :U0 }` with the request's `deadline`.
fn request_until(deadline: Option<std::time::Instant>) -> crate::SparqlQueryRequest {
    let mut request = crate::SparqlQueryRequest::all("SELECT ?x { ?x a <urn:test:U0> }");
    request.deadline = deadline;
    request
}

/// Spec: at the request's deadline a read is told incomplete with the rows it has, never an
/// error. Over [`capped_unions_store`], a read whose request's deadline has passed returns
/// the rows the read with no deadline returns (L's), decides none of its candidates (none
/// refuted, 12 unresolved) and is not `complete`; with no deadline, 4 are refuted. Nothing
/// waits; no time is measured.
#[test]
fn a_read_past_its_requests_deadline_is_told_incomplete_with_its_rows() {
    let store = capped_unions_store();
    let read = |deadline| {
        let prepared = store.prepare_query(&request_until(deadline)).unwrap();
        let mut out = Vec::new();
        let status = store.run_query_dl(&prepared, &CancellationToken::new(), &mut out);
        let status = status.expect("an answer, not an error");
        (out, status.expect("a DL status"))
    };
    let (rows, (status, detail)) = read(Some(std::time::Instant::now()));
    let (free_rows, (free, free_detail)) = read(None);
    println!("passed {status:?} {detail:?}; no deadline {free:?} {free_detail:?}");
    let bounds =
        |s: &nrese_sparql::Completeness| s.bounds.map(|b| (b.lower, b.upper, b.unresolved));
    let counts = |d: &crate::dl::DlDetail| (d.proved, d.refuted);
    assert_eq!(
        (bounds(&status), counts(&detail), status.complete),
        (Some((12, 24, 12)), (0, 0), false)
    );
    assert_eq!(
        (bounds(&free), counts(&free_detail)),
        (Some((12, 24, 8)), (0, 4)),
        "with no deadline"
    );
    assert_eq!(rows, free_rows, "the rows in hand");
}

/// Spec: the cancellation stays the operator's stop and an error, whether the request's
/// deadline has passed or not: a read cancelled once its answers are decided ends
/// `Cancelled`, never as an incomplete answer.
#[test]
fn an_operators_cancel_is_an_error_past_the_deadline_too() {
    let store = capped_unions_store();
    for deadline in [None, Some(std::time::Instant::now())] {
        let prepared = store.prepare_query(&request_until(deadline)).unwrap();
        let token = CancellationToken::new();
        let cancel = token.clone();
        crate::service::hooks::AFTER_DECIDED.set(Some(Box::new(move || cancel.cancel())));
        let outcome = answer(
            &store,
            &store.engine().snapshot(),
            &prepared,
            &token,
            &nrese_exec::Envelope::unlimited(),
            DlAnswers::default(),
        )
        .err();
        assert!(
            token.is_cancelled(),
            "{deadline:?}: the read was never cancelled"
        );
        assert!(
            matches!(
                outcome,
                Some(StoreError::SparqlEvaluation(
                    nrese_sparql::QueryEvaluationError::Cancelled
                ))
            ),
            "{deadline:?}: {outcome:?}"
        );
    }
}

/// Spec: where the labels refute the rows the capped read would admit first, the read with
/// them on admits no later row whose batch costs more, so it never needs more memory than the
/// read with them off. Under `?x a :U0 . ?x a :U1`, the candidate limit 4: eight individuals
/// each with `v0`, `v1` and `v2`, short-named and committed first, and six with `v3`, named by
/// IRIs longer by 64000 bytes and committed last (term-id order is the admission order). The
/// rows with `v0` and `v2` keep one class assertion, which the labels refute; the six with
/// `v3` keep both. Under each request memory limit from 16 KiB to 8 MiB (steps of a fiftieth),
/// and in steps of 256 bytes over the 16 KiB from the least limit the read is answered under
/// with them off, every read the first of a store just committed, a read answered with them off
/// is answered with them on. And the case is reached: under some limit the read with them on
/// refutes all 16 by labels and is answered.
#[test]
fn rows_the_labels_promote_past_the_capped_prefix_cost_no_more_memory() {
    let mut data = String::new();
    for k in 0..24 {
        data.push_str(&format!(":i{k} :val :v{} .\n", k % 3));
    }
    let pad = "x".repeat(64000);
    for k in 0..6 {
        data.push_str(&format!(":j{k}{pad} :val :v3 .\n"));
    }
    let first = |model_labels: bool, limit: usize| {
        let dl = crate::DlConfig {
            max_candidates: Some(4),
            model_labels,
            ..crate::DlConfig::default()
        };
        let store = unions_store_of(dl, &data);
        unions_read_of(
            &store,
            "?x a <urn:test:U0> . ?x a <urn:test:U1>",
            Some(limit),
        )
    };
    let (mut low, mut high) = (16usize << 10, 8usize << 20);
    while high - low > 256 {
        let middle = low + (high - low) / 2;
        match first(false, middle) {
            Ok(_) => high = middle,
            Err(_) => low = middle,
        }
    }
    let mut limits: Vec<usize> = (0..64).map(|k| high + k * 256).collect();
    let mut limit = 16usize << 10;
    while limit <= 8 << 20 {
        limits.push(limit);
        limit += limit / 50;
    }
    let mut reached = Vec::new();
    for limit in limits {
        if first(false, limit).is_ok() {
            let with = first(true, limit);
            assert!(
                with.is_ok(),
                "{limit} bytes: off answered, on {:?}",
                with.err()
            );
            if let Ok((_, Some((_, detail)))) = &with
                && detail.refuted_by_labels == 16
            {
                reached.push(limit);
            }
        }
    }
    println!(
        "off answered from {high} bytes; all 16 refuted by labels, answered under {reached:?}"
    );
    assert!(!reached.is_empty(), "no limit reaches the case");
}

/// Spec: where the labels refute rows the capped read would admit and its batches would mix
/// with others, the read with them on puts the rows they leave in no batch that costs more than
/// the capped read's did. Under `?x a :U0 . ?x a :U1`, the default candidate limit: twice 32
/// short-named individuals with `v0`, which the labels refute, each followed by 32 with `v3`,
/// named by IRIs longer by 8000 bytes, committed in that order (term-id order is the admission
/// order). With them off each batch of 64 holds 32 of each; with them on the 64 long rows are
/// left. Under each request memory limit from 16 KiB to 4 MiB (steps of a tenth), and in
/// steps of 256 bytes over the 16 KiB from the least limit the read is answered under with them
/// off, every read the first of a store just committed, a read answered with them off is
/// answered with them on. And the case is reached: under some limit below the least one a read
/// of the 64 long rows alone (with them off: one batch) is answered under, the read with them on
/// refutes the 64 short rows by labels and is answered.
#[test]
fn rows_the_labels_leave_are_batched_no_costlier_than_the_capped_read() {
    let pad = "x".repeat(8000);
    let (mut data, mut long) = (String::new(), String::new());
    for g in 0..2 {
        for k in g * 32..(g + 1) * 32 {
            data.push_str(&format!(":a{k} :val :v0 .\n"));
        }
        for k in g * 32..(g + 1) * 32 {
            long.push_str(&format!(":b{k}{pad} :val :v3 .\n"));
            data.push_str(&format!(":b{k}{pad} :val :v3 .\n"));
        }
    }
    let first = |model_labels: bool, data: &str, limit: usize| {
        let dl = crate::DlConfig {
            model_labels,
            ..crate::DlConfig::default()
        };
        let store = unions_store_of(dl, data);
        unions_read_of(
            &store,
            "?x a <urn:test:U0> . ?x a <urn:test:U1>",
            Some(limit),
        )
    };
    let least = |data: &str| {
        let (mut low, mut high) = (16usize << 10, 4usize << 20);
        while high - low > 256 {
            let middle = low + (high - low) / 2;
            match first(false, data, middle) {
                Ok(_) => high = middle,
                Err(_) => low = middle,
            }
        }
        high
    };
    let (high, one_batch) = (least(&data), least(&long));
    let mut limits: Vec<usize> = (0..64).map(|k| high + k * 256).collect();
    let mut limit = 16usize << 10;
    while limit <= 4 << 20 {
        limits.push(limit);
        limit += limit / 10;
    }
    let mut reached = Vec::new();
    for limit in limits {
        if first(false, &data, limit).is_ok() {
            let with = first(true, &data, limit);
            assert!(
                with.is_ok(),
                "{limit} bytes: off answered, on {:?}",
                with.err()
            );
            if let Ok((_, Some((_, detail)))) = &with
                && detail.refuted_by_labels == 64
                && limit < one_batch
            {
                reached.push(limit);
            }
        }
    }
    println!(
        "off answered from {high} bytes, the 64 long rows alone from {one_batch}; all 64 short refuted by labels, answered under {reached:?}"
    );
    assert!(!reached.is_empty(), "no limit reaches the case");
}

/// The pattern of [`promoted_store`]'s and [`past_prefix_store`]'s reads.
const PROMOTED: &str = "?x a <urn:test:U0> . ?x a <urn:test:U1>";

/// The unions store under [`PROMOTED`], a candidate limit of 64 and the labels on or off, with
/// its gap in commit order (term-id order is the admission order): 60 short-named individuals
/// with `v0` (`:a{k}`, each keeping one class assertion, which the labels refute), 4 with `v3`
/// (`:b{k}`), then 100 with `v3` named a byte longer (`:c{k}x`). The capped read's first 64
/// rows are the `a`s and `b`s; the labels leave the 4 `b`s of them, then the `c`s, whose rows
/// cost more than the `a`s they follow. Each `v3` row's own test refutes it.
fn promoted_store(model_labels: bool) -> Arc<StoreService> {
    let mut data = String::new();
    for k in 0..60 {
        data.push_str(&format!(":a{k} :val :v0 .\n"));
    }
    for k in 0..4 {
        data.push_str(&format!(":b{k} :val :v3 .\n"));
    }
    for k in 0..100 {
        data.push_str(&format!(":c{k}x :val :v3 .\n"));
    }
    let dl = crate::DlConfig {
        max_candidates: Some(64),
        model_labels,
        ..crate::DlConfig::default()
    };
    unions_store_of(dl, &data)
}

/// [`promoted_store`]'s read with the labels on and no memory limit: what decided it, its status.
fn promoted_read() -> (crate::dl::DlDetail, nrese_sparql::Completeness) {
    let (_, status) = unions_read_of(&promoted_store(true), PROMOTED, None).unwrap();
    let (status, detail) = status.expect("a status");
    println!("{status:?} {detail:?}");
    (detail, status)
}

/// Spec: a read says the candidate limit stopped it only where that many candidates went to
/// their tests. In [`promoted_read`] the labels refute 60 of the capped read's first 64 rows,
/// and 104 candidates survive them: a reason names `dl.max_candidates` (64), and 64 of the
/// survivors were tested (here each refuted by its test, so refuted less refuted by labels).
#[test]
fn the_candidate_limit_is_named_only_where_that_many_were_tested() {
    let (detail, status) = promoted_read();
    assert_eq!(detail.refuted_by_labels, 60, "the 60 `a`s");
    let tested = detail.proved + detail.refuted - detail.refuted_by_labels;
    for reason in status.reasons.iter().map(|r| &r.text) {
        if reason.contains("dl.max_candidates reached") {
            assert_eq!(tested, 64, "{reason}, with {tested} tested");
        }
    }
    let named = |r: &String| r.contains("dl.max_candidates reached (64)");
    assert!(
        status.reasons.iter().map(|r| &r.text).any(named),
        "the limit named: {:?}",
        status.reasons
    );
}

/// Spec: the survivors of the labels past the capped read's first rows are tested, in batches
/// no costlier than its own, and fail no read: in [`promoted_read`] the labels leave 4 of the
/// capped read's first 64 rows, and 64 survivors are tested, those 4 and 60 past them. Under
/// each request memory limit from 16 KiB to 4 MiB (steps of a fiftieth), and in steps of 256
/// bytes over the 16 KiB from the least limit the read is answered under with the labels off
/// (found to 256 bytes), every read the first of a store just committed, a read answered with
/// the labels off is answered with them on.
#[test]
fn survivors_past_the_capped_prefix_are_tested_and_fail_no_read() {
    let (detail, _) = promoted_read();
    let tested = detail.proved + detail.refuted - detail.refuted_by_labels;
    assert_eq!(tested, 64, "the 4 left of the first 64, and 60 past them");
    let first =
        |labels: bool, limit: usize| unions_read_of(&promoted_store(labels), PROMOTED, Some(limit));
    let (mut low, mut high) = (16usize << 10, 4usize << 20);
    while high - low > 256 {
        let middle = low + (high - low) / 2;
        match first(false, middle) {
            Ok(_) => high = middle,
            Err(_) => low = middle,
        }
    }
    let mut limits: Vec<usize> = (0..64).map(|k| high + k * 256).collect();
    let mut limit = 16usize << 10;
    while limit <= 4 << 20 {
        limits.push(limit);
        limit += limit / 50;
    }
    let mut answered = 0;
    for limit in limits {
        if first(false, limit).is_ok() {
            answered += 1;
            let with = first(true, limit);
            assert!(
                with.is_ok(),
                "{limit} bytes: off answered, on {:?}",
                with.err()
            );
        }
    }
    println!("off answered from {high} bytes, under {answered} limits; on under each");
    assert!(answered > 0, "off answers under some limit");
}

/// The unions store under [`PROMOTED`] with a candidate limit of 4 and the labels on, its gap
/// in commit order: 3 individuals with `v0` (`:a{k}`, refuted by the labels) and `:b0` with
/// `v3`, the capped read's first 4 rows, then `past` with `v3` past them (`:c{k}`). The `a`s
/// are named by IRIs longer by `pads.0` bytes, the `c`s by `pads.1`: with both 100000, the
/// `c`s' rows together cost no more than the first 4 rows' batch, but each more than `:b0`'s,
/// the one row the labels leave of them.
fn past_prefix_store(pads: (usize, usize), past: usize) -> Arc<StoreService> {
    let (pad, long) = ("x".repeat(pads.0), "x".repeat(pads.1));
    let mut data = String::new();
    for k in 0..3 {
        data.push_str(&format!(":a{k}{pad} :val :v0 .\n"));
    }
    data.push_str(":b0 :val :v3 .\n");
    for k in 0..past {
        data.push_str(&format!(":c{k}{long} :val :v3 .\n"));
    }
    let dl = crate::DlConfig {
        max_candidates: Some(4),
        model_labels: true,
        ..crate::DlConfig::default()
    };
    unions_store_of(dl, &data)
}

/// Spec: rows past the capped read's first ones never fail a read answered without them: a
/// batch of them the request has no room for ends the tests there, its rows left unresolved for
/// the request's memory. Over [`past_prefix_store`] with IRIs longer by 100000 bytes, without
/// `c`s and with 3 (their one batch costing more than any one row), under each request memory
/// limit from 64 KiB to 2 MiB (steps of a fiftieth), every read the first of a store just
/// committed: where the read without the `c`s is answered, the read with them is answered. And
/// the case is reached: some such read leaves the 3 `c`s unresolved, with the 3 `a`s refuted by
/// the labels and `:b0` by its test, and a reason naming the request's memory envelope.
#[test]
fn a_batch_past_the_capped_prefix_without_room_leaves_its_rows_unresolved() {
    use nrese_dl::limits::Limit;
    let read = |past: usize, limit: usize| {
        unions_read_of(
            &past_prefix_store((100_000, 100_000), past),
            PROMOTED,
            Some(limit),
        )
    };
    let (mut limit, mut reached) = (64usize << 10, Vec::new());
    while limit <= 2 << 20 {
        if read(0, limit).is_err() {
            println!("{limit}: failed without the `c`s");
            limit += limit / 50;
            continue;
        }
        let (status, detail) = match read(3, limit) {
            Ok((_, Some(status))) => status,
            other => panic!(
                "{limit} bytes: answered without the `c`s, with them {:?}",
                other.err()
            ),
        };
        let reasons: Vec<_> = status.reasons.iter().map(|r| r.text.as_str()).collect();
        let counts = (detail.refuted_by_labels, detail.refuted, detail.proved);
        let unresolved = &detail.unresolved;
        println!(
            "{limit}: {counts:?}, {} unresolved, {reasons:?}",
            unresolved.len()
        );
        if counts == (3, 4, 0)
            && unresolved.len() == 3
            && unresolved.iter().all(|r| r.contains("urn:test:c"))
            && reasons
                .iter()
                .any(|r| Limit::named_in(r) == Some(Limit::Envelope))
        {
            reached.push(limit);
        }
        limit += limit / 50;
    }
    println!("the `c`s without room, the read answered, under {reached:?}");
    assert!(!reached.is_empty(), "no limit reaches the case");
}

/// Spec: a candidate past the capped read's first rows whose row alone costs more than its
/// costliest batch is in no batch: the tests end there, the read is answered, told incomplete,
/// and says why by name. Over [`past_prefix_store`] with short-named `a`s and one `c` named by
/// an IRI longer by 100000 bytes, with no memory limit: the 3 `a`s refuted by the labels,
/// `:b0` by its test, `:c0` unresolved, and the reason "the next candidate's row costs more
/// than" the batch's bytes.
#[test]
fn a_row_costlier_than_the_capped_batches_is_left_unresolved_and_named() {
    let store = past_prefix_store((0, 100_000), 1);
    let (_, status) = unions_read_of(&store, PROMOTED, None).unwrap();
    let (status, detail) = status.expect("a status");
    let reasons: Vec<_> = status.reasons.iter().map(|r| r.text.as_str()).collect();
    let counts = (detail.refuted_by_labels, detail.refuted, detail.proved);
    println!(
        "{counts:?}, {} unresolved, {reasons:?}",
        detail.unresolved.len()
    );
    assert_eq!(counts, (3, 4, 0), "the `a`s by labels, `:b0` by its test");
    assert!(detail.unresolved.len() == 1 && detail.unresolved[0].contains("urn:test:c0"));
    assert!(!status.complete, "told incomplete");
    let named = |r: &&str| r.contains("the next candidate's row costs more than");
    assert!(
        reasons.iter().any(named),
        "the reason by name, in {reasons:?}"
    );
}

/// Spec: where the tests past the capped read's first rows end, the read says why, beside what
/// an earlier batch said: a stop is named even where a batch before it was left unresolved for
/// a reason of its own. Over [`past_prefix_store`] with short-named `a`s and one `c` named by an
/// IRI longer by 100000 bytes, under each request memory limit from 4 KiB to 2 MiB (steps of a
/// fiftieth), every read the first of a store just committed: where the read is answered with
/// the 3 `a`s refuted by the labels and both `:b0` and `:c0` unresolved (`:b0`'s batch without
/// room for its run), its reasons name the request's memory envelope and "the next candidate's
/// row costs more than". And the case is reached.
#[test]
fn a_stop_past_the_capped_rows_is_named_beside_an_earlier_reason() {
    let (mut limit, mut reached) = (4usize << 10, Vec::new());
    while limit <= 2 << 20 {
        let store = past_prefix_store((0, 100_000), 1);
        if let Ok((_, Some((status, detail)))) = unions_read_of(&store, PROMOTED, Some(limit)) {
            let reasons: Vec<_> = status.reasons.iter().map(|r| r.text.as_str()).collect();
            let counts = (detail.refuted_by_labels, detail.refuted, detail.proved);
            println!(
                "{limit}: {counts:?}, {} unresolved, {reasons:?}",
                detail.unresolved.len()
            );
            if counts == (3, 3, 0) && detail.unresolved.len() == 2 {
                let both = |r: &&str| {
                    r.contains("budgets.query_memory")
                        && r.contains("the next candidate's row costs more than")
                };
                assert!(reasons.iter().any(both), "{limit} bytes: {reasons:?}");
                reached.push(limit);
            }
        }
        limit += limit / 50;
    }
    println!("`:b0` unresolved, then `:c0` stopped, both named, under {reached:?}");
    assert!(!reached.is_empty(), "no limit reaches the case");
}

/// Four individuals `:m0`..`:m3`, each in `:C` and with `:p` to the literal `"v{k}"`, and the
/// rows of `?x :p ?l` (`?x`, `?l`) over them.
fn labels_store() -> (Arc<StoreService>, Vec<Vec<Option<Term>>>) {
    use nrese_rdf::{Literal, NamedNode};
    let mut data = String::new();
    let mut rows = Vec::new();
    for k in 0..4 {
        data.push_str(&format!(":m{k} a :C ; :p \"v{k}\" .\n"));
        rows.push(vec![
            Some(NamedNode::new_unchecked(format!("urn:test:m{k}")).into()),
            Some(Literal::new_simple_literal(format!("v{k}")).into()),
        ]);
    }
    let store = lower_true_store_with(crate::StoreConfig::in_memory(), &data);
    (store, rows)
}

/// The model's refutations over `rows` of `SELECT * { pattern }` in `store`, under `token` and
/// `deadline`, with a fresh envelope of `limit` bytes: the mask; what the envelope held while the
/// mask the call returned lived, then once it was dropped; the most it held; and whether it was
/// marked refused.
fn labels_of(
    store: &StoreService,
    pattern: &str,
    rows: &[Vec<Option<Term>>],
    token: &CancellationToken,
    deadline: Deadline,
    limit: usize,
) -> (Vec<bool>, (usize, usize), usize, bool) {
    let request = crate::SparqlQueryRequest::all(format!("SELECT * {{ {pattern} }}"));
    let analysis = analyse(PreparedQuery::parse(&request).unwrap().query());
    let variables = analysis.shape.as_ref().unwrap().projected.clone();
    let memory = nrese_exec::Envelope::new(Some(limit), None);
    let mask = super::exact::refuted_by_labels(
        store,
        &store.engine().snapshot(),
        &analysis,
        &variables,
        rows.len(),
        &|i| rows[i].clone(),
        token,
        deadline,
        &memory,
    );
    let (alive, copy) = (memory.used(), mask.to_vec());
    drop(mask);
    (
        copy,
        (alive, memory.used()),
        memory.peak(),
        memory.refused(),
    )
}

/// Spec: the model refutes only rows whose one test is `C(a)`, so a query whose atoms aren't all
/// `?x a C` (an answer variable, a named class) gets no mask, and the premise is neither loaded
/// nor charged for it: its rows' tests could otherwise add to the premise's copy, uncharged
/// (a literal's text, an existential's expression). Over four rows of `?x :p ?l`, a fresh
/// envelope holds nothing at any time. And the measure is live: over the same store, `?x a :C`
/// charges the premise.
#[test]
fn a_query_the_labels_cant_read_loads_and_charges_nothing() {
    let (store, rows) = labels_store();
    let (token, deadline) = (CancellationToken::new(), Deadline::after(None));
    let (mask, _, peak, _) = labels_of(
        &store,
        "?x <urn:test:p> ?l",
        &rows,
        &token,
        deadline,
        1 << 30,
    );
    println!("?x :p ?l: mask {mask:?}, the envelope's peak {peak}");
    assert!(mask.is_empty(), "no mask");
    assert_eq!(peak, 0, "nothing charged");
    let typed: Vec<_> = rows.iter().map(|r| r[..1].to_vec()).collect();
    let (_, _, peak, _) = labels_of(
        &store,
        "?x a <urn:test:C>",
        &typed,
        &token,
        deadline,
        1 << 30,
    );
    println!("?x a :C: the envelope's peak {peak}");
    assert!(peak > 0, "the premise charged");
}

/// Spec: labels whose deadline has passed, or whose read is cancelled, load and charge nothing:
/// no mask, and a fresh envelope holds nothing at any time, over four rows of `?x a :C` (a shape
/// the labels read, which charges the premise when neither has happened).
#[test]
fn labels_past_their_deadline_or_cancelled_load_and_charge_nothing() {
    let (store, rows) = labels_store();
    let rows: Vec<_> = rows.iter().map(|r| r[..1].to_vec()).collect();
    let expired = Deadline::at(std::time::Instant::now());
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    for (token, deadline) in [
        (CancellationToken::new(), expired),
        (cancelled, Deadline::after(None)),
    ] {
        let (mask, _, peak, _) = labels_of(
            &store,
            "?x a <urn:test:C>",
            &rows,
            &token,
            deadline,
            1 << 30,
        );
        assert!(mask.is_empty(), "no mask");
        assert_eq!(peak, 0, "nothing charged");
    }
}

/// Spec: the mask the labels return is charged for as long as it lives, not only while it is
/// built: over the unions store's 12 gap rows of `?x a :U0` (the individuals with `v2` and
/// `v3`), all refuted by one model, the request's envelope holds at least the mask's 12 bytes
/// while the caller holds it, and nothing once the caller drops it.
#[test]
fn the_labels_mask_is_charged_while_it_lives() {
    use nrese_rdf::NamedNode;
    let store = unions_store_with(crate::DlConfig::default(), 24, 0);
    let iri = |k: usize| -> Term { NamedNode::new_unchecked(format!("urn:test:i{k}")).into() };
    let rows: Vec<_> = (0..24)
        .filter(|k| k % 4 >= 2)
        .map(|k| vec![Some(iri(k))])
        .collect();
    let (token, deadline) = (CancellationToken::new(), Deadline::after(None));
    let (mask, (alive, dropped), _, _) = labels_of(
        &store,
        "?x a <urn:test:U0>",
        &rows,
        &token,
        deadline,
        1 << 30,
    );
    println!("mask {mask:?}; the envelope held {alive} bytes with it, {dropped} without");
    assert_eq!(mask, vec![true; 12], "all 12 refuted by the model");
    assert!(alive >= 12, "the mask's 12 bytes charged while it lives");
    assert_eq!(dropped, 0, "nothing once it is dropped");
}

/// Spec: the labels' work is work the read can do without, so a charge they are refused inside
/// it marks only their own part: the envelope the caller passed is never marked refused (which
/// would keep the request's result from being published beyond it), whether the labels had
/// room or not. Over the unions store's 12 gap rows of `?x a :U0`, all refuted by one model,
/// under each limit from 64 bytes up (steps of a tenth) to the least that gives the whole mask:
/// the envelope isn't marked refused. And the case is reached: some limit gives no mask (the
/// labels refused), and the last gives all 12 refuted.
#[test]
fn labels_refused_their_memory_leave_the_request_unmarked() {
    use nrese_rdf::NamedNode;
    let store = unions_store_with(crate::DlConfig::default(), 24, 0);
    let iri = |k: usize| -> Term { NamedNode::new_unchecked(format!("urn:test:i{k}")).into() };
    let rows: Vec<_> = (0..24)
        .filter(|k| k % 4 >= 2)
        .map(|k| vec![Some(iri(k))])
        .collect();
    let (token, deadline) = (CancellationToken::new(), Deadline::after(None));
    let (mut limit, mut empty) = (64usize, 0);
    let mask = loop {
        let (mask, _, _, refused) =
            labels_of(&store, "?x a <urn:test:U0>", &rows, &token, deadline, limit);
        println!("{limit}: mask of {} rows, refused {refused}", mask.len());
        assert!(
            !refused,
            "{limit} bytes: the labels' refusal marked the caller's envelope"
        );
        if mask.len() == rows.len() || limit > 1 << 30 {
            break mask;
        }
        empty += usize::from(mask.is_empty());
        limit += limit / 10;
    };
    println!("the whole mask from {limit} bytes; {empty} limits gave none");
    assert!(empty > 0, "no limit refuses the labels");
    assert_eq!(mask, vec![true; 12], "all 12 refuted under the last limit");
}

/// The store of [`lower_true_store_with`] with `:x` in `:A` or `:E`: `:A` below `≥ 200 :r` (200
/// successors, about a megabyte with their inequalities), `:E` a class of nothing else. `:A`
/// is first in the union if `a_first.0`, and committed before `:E` if `a_first.1`.
fn at_least_or_e_store(a_first: (bool, bool)) -> Arc<StoreService> {
    let union = if a_first.0 { "( :A :E )" } else { "( :E :A )" };
    let a = ":A <http://www.w3.org/2000/01/rdf-schema#subClassOf> [ a owl:Restriction ; \
             owl:onProperty :r ; owl:minCardinality \
             \"200\"^^<http://www.w3.org/2001/XMLSchema#nonNegativeInteger> ] .";
    let classes = if a_first.1 {
        format!("{a} :E a owl:Class .")
    } else {
        format!(":E a owl:Class . {a}")
    };
    let data = format!(":r a owl:ObjectProperty . {classes} :x a [ owl:unionOf {union} ] .");
    lower_true_store_with(crate::StoreConfig::in_memory(), &data)
}

/// Spec: the labels give a mask only from a run none of their charges was refused in: a model
/// found after a branch was abandoned for want of room is no mask (the optional part's terms:
/// no call goes on past a refusal). Over [`at_least_or_e_store`] in each union and commit
/// order, the rows of `?x a :E` (`:x`), under each limit from 4 KiB to 16 MiB (steps of a
/// tenth): the labels give no mask or the one they give under 1 GiB. And the case is reached:
/// in an order whose model under 1 GiB takes the `≥ 200` branch (`:x` refuted), some limit
/// gives no mask though the labels' premise was charged (the envelope's peak above 0).
#[test]
fn labels_whose_run_abandoned_a_branch_give_no_mask() {
    use nrese_rdf::NamedNode;
    let rows = vec![vec![Some(NamedNode::new_unchecked("urn:test:x").into())]];
    let (token, deadline) = (CancellationToken::new(), Deadline::after(None));
    let mut reached = false;
    for order in [(true, true), (false, true), (true, false), (false, false)] {
        let store = at_least_or_e_store(order);
        let of = |limit| labels_of(&store, "?x a <urn:test:E>", &rows, &token, deadline, limit);
        let (whole, ..) = of(1 << 30);
        let (mut limit, mut none) = (4usize << 10, 0);
        while limit <= 16 << 20 {
            let (mask, _, peak, _) = of(limit);
            println!("order {order:?}, {limit}: mask {mask:?}, peak {peak}");
            assert!(
                mask.is_empty() || mask == whole,
                "{limit} bytes: {mask:?}, not {whole:?}, from a run past a refusal"
            );
            none += usize::from(mask.is_empty() && peak > 0);
            limit += limit / 10;
        }
        println!("order {order:?}: {whole:?} under 1 GiB, {none} gave none past the premise");
        reached |= whole == [true] && none > 0;
    }
    assert!(reached, "no order reaches the case");
}
