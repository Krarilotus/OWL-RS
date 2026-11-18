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
/// equality is told no mark, committed and inside a transaction, and its mark doesn't go to
/// the whole ontology: the store's indexes show there is neither.
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

/// With the consistency check off, a commit reads no ontology, so its revision has no
/// record of what the reader left out. Committed reads at that revision (a plan, EXPLAIN,
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
    let (mut wrong, mut refused) = (Vec::new(), 0);
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
        let reads = [
            ("committed", Revision::Committed(&snapshot), &snapshot),
            ("pending", Revision::Pending(&pending, true), &pending),
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
                // The limit: a part left out that no record tells (a pending addition, a
                // revision the store didn't check) comes after the indexes' decision.
                _ if left => match (by_index(read), &record) {
                    (Some(Some(why)), _) => Some(why),
                    (Some(None), Some(None)) => None,
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
            // ontology tells after what it left out.
            let mut kept = nrese_owl::Ontology::clone(whole.value());
            kept.diagnostics.retain(|d| !d.leaves_out());
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
        "NRESE_FUZZ_SEED={fuzz}: {CASES} stores, {refused} refused; by the indexes: {} \
         reasons, {} none, {} to the ontology; by the whole ontology: {} annotation, {} \
         left-out and {} punning reasons; past what it left out: {} annotation and {} punning",
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
