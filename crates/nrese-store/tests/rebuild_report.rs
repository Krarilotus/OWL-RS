//! A commit that maintenance answers with a rebuild is judged by that rebuild's own assessment.
//!
//! Spec: README ("with reasoning on, it infers with every write and refuses a write found to
//! make the data inconsistent") and OWL 2 RL's consistency rules (`cax-adc`, `cax-dw`,
//! `prp-pdw`, OWL 2 Profiles §4.3): a write whose result breaks one is refused, and the store is
//! left as it was; a write that brings no clash commits, and in quarantine one that brings no
//! new clash.
//!
//! Each case's commit is maintained by a rebuild (a split over a compact stack, a store with an
//! inverse property expression, whose facts with a blank predicate no store holds, or a split
//! the policy rebuilds, a first write), where the delta's own report misses a clash the rebuild
//! finds, or holds one the change removed. "Left as it was" is the published
//! state: the revision, the asserted and inferred counts, the consistency, and the facts named.
//! It covers the write itself: a reassessment before it (of a store an earlier build assessed)
//! may advance the state, and none runs here, as each store is assessed by this build.

use std::path::PathBuf;
use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    BulkLoadRequest, ConsistencyStatus, GraphTarget, MutationCommand, MutationError,
    MutationPipeline, MutationTicket, Requester, SparqlUpdateRequest, StoreConfig, StoreService,
};

const PREFIXES: &str = "PREFIX ex: <http://example.org/> \
     PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#>";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";
const EX: &str = "http://example.org/";

fn store(compact: bool) -> Arc<StoreService> {
    let mut config = StoreConfig::in_memory();
    config.equality_compact = compact;
    Arc::new(StoreService::new(config).expect("store"))
}

fn pipeline(store: &Arc<StoreService>) -> MutationPipeline {
    MutationPipeline::new(
        Arc::clone(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    )
}

fn apply(pipeline: &MutationPipeline, body: &str) -> Result<(), MutationError> {
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}\n{body}"))),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .map(drop)
}

/// A store holding `nt`, loaded in bulk (no checks) and then rematerialised.
fn loaded(compact: bool, name: &str, nt: &str) -> Arc<StoreService> {
    let file = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.nt"));
    std::fs::write(&file, nt).expect("write the data");
    let store = store(compact);
    store
        .bulk_load(&BulkLoadRequest {
            files: vec![file.clone()],
            replace: false,
            graph: GraphTarget::DefaultGraph,
            invalid: None,
        })
        .expect("load");
    std::fs::remove_file(&file).expect("remove the data");
    let program = ReasonerConfig::for_mode(ReasoningMode::Owl2Rl)
        .materialised_program()
        .expect("a reasoning mode");
    store.rematerialise(program).expect("rematerialise");
    store
}

/// What a refused write must leave unchanged.
fn state(store: &StoreService) -> (u64, usize, usize, String) {
    let stats = store.stats().expect("stats");
    (
        store.current_revision(),
        stats.quad_count,
        stats.inferred_count,
        format!("{:?}", store.consistency()),
    )
}

/// Whether `pattern` (over [`PREFIXES`]) holds in the store, asserted or inferred.
fn holds(store: &StoreService, pattern: &str) -> bool {
    let answer = store
        .execute_query_str(&format!("{PREFIXES}\nASK {{ {pattern} }}"))
        .expect("answered");
    String::from_utf8(answer.payload)
        .expect("text")
        .contains("true")
}

/// Asserts the write was refused for `rule`, and returns what the refusal says.
fn assert_refused_for(result: Result<(), MutationError>, rule: &str, case: &str) -> String {
    match result {
        Err(MutationError::Rejected(reject)) => {
            assert!(
                reject.detail.contains(rule),
                "{case}: refused for {rule}, not: {}",
                reject.detail
            );
            reject.detail
        }
        other => panic!("{case}: refused for {rule}, not {other:?}"),
    }
}

/// An equality that hides an `owl:AllDisjointClasses` marker (invalid OWL 2 DL input, which
/// a store holds all the same): the axiom isn't read while the equality stands, so the
/// rematerialised store reads `ex:x`, in both `ex:A` and `ex:B`, as consistent. Deleting only
/// the equality brings the clash; over a compact stack the split is rebuilt.
#[test]
fn a_split_that_brings_a_clash_is_refused_and_leaves_the_published_state_as_it_was() {
    let store = loaded(
        true,
        "alias-marker",
        &format!(
            "<{EX}alias> <{OWL}sameAs> <{OWL}AllDisjointClasses> .
_:ax <{RDF}type> <{OWL}AllDisjointClasses> .
_:ax <{OWL}members> _:m1 .
_:m1 <{RDF}first> <{EX}A> .
_:m1 <{RDF}rest> _:m2 .
_:m2 <{RDF}first> <{EX}B> .
_:m2 <{RDF}rest> <{RDF}nil> .
<{EX}x> <{RDF}type> <{EX}A> .
<{EX}x> <{RDF}type> <{EX}B> .
"
        ),
    );
    let before = state(&store);
    let result = apply(
        &pipeline(&store),
        "DELETE DATA { ex:alias owl:sameAs owl:AllDisjointClasses . }",
    );
    assert_refused_for(result, "cax-adc", "the alias delete");
    assert_eq!(
        state(&store),
        before,
        "a refused write leaves the published state as it was"
    );
    assert!(
        holds(&store, "ex:alias owl:sameAs owl:AllDisjointClasses"),
        "the equality the refused write deleted is still there"
    );
}

/// Valid OWL 2 DL: a store with `ObjectInverseOf(ex:p)` and `ex:a ex:p ex:b`, so `ex:b` relates
/// to `ex:a` by the inverse. A domain of the inverse puts `ex:b` in `ex:A`, disjoint with its
/// `ex:B`; a disjointness of the inverse with `ex:s` clashes with `ex:b ex:s ex:a`. Under both
/// equality modes.
#[test]
fn a_clash_through_an_inverse_property_expression_is_refused() {
    let cases = [
        (
            "the domain of an inverse",
            "INSERT DATA { _:inv owl:inverseOf ex:p . ex:a ex:p ex:b .
             ex:A owl:disjointWith ex:B . ex:b rdf:type ex:B . }",
            "INSERT { ?inv rdfs:domain ex:A } WHERE { ?inv owl:inverseOf ex:p }",
            "cax-dw",
            "{ ?inv rdfs:domain ex:A } UNION { ex:b rdf:type ex:A }",
        ),
        (
            "an inverse disjoint with a property",
            "INSERT DATA { _:inv owl:inverseOf ex:p . _:inv owl:propertyDisjointWith ex:s .
             ex:a ex:p ex:b . }",
            "INSERT DATA { ex:b ex:s ex:a . }",
            "prp-pdw",
            "ex:b ex:s ex:a",
        ),
    ];
    for (case, base, change, rule, refused_fact) in cases {
        for compact in [false, true] {
            let case = format!("{case} (compact: {compact})");
            let store = store(compact);
            let commits = pipeline(&store);
            apply(&commits, base).unwrap_or_else(|e| panic!("{case}: the base commits: {e}"));
            let before = state(&store);
            assert_refused_for(apply(&commits, change), rule, &case);
            assert_eq!(
                state(&store),
                before,
                "{case}: a refused write leaves the published state as it was"
            );
            assert!(
                !holds(&store, refused_fact),
                "{case}: neither the refused fact nor its consequence is there"
            );
        }
    }
}

/// Valid OWL 2 DL: the domain of `ObjectInverseOf(ex:p)` puts `ex:b` in `ex:A` (through
/// `ex:a ex:p ex:b`), disjoint with `ex:B`. One update deletes `ex:a ex:p ex:b` and types `ex:b`
/// as `ex:B`: the data after it is consistent, as `ex:b` is no longer in `ex:A`, so it commits,
/// under both equality modes, and the inference it removed is gone.
#[test]
fn a_change_through_an_inverse_that_leaves_no_clash_commits() {
    for compact in [false, true] {
        let case = format!("compact: {compact}");
        let store = store(compact);
        let commits = pipeline(&store);
        apply(
            &commits,
            "INSERT DATA { _:inv owl:inverseOf ex:p . ex:a ex:p ex:b . ex:A owl:disjointWith ex:B . }",
        )
        .unwrap_or_else(|e| panic!("{case}: the base commits: {e}"));
        apply(
            &commits,
            "INSERT { ?inv rdfs:domain ex:A } WHERE { ?inv owl:inverseOf ex:p }",
        )
        .unwrap_or_else(|e| panic!("{case}: the domain commits: {e}"));
        assert!(
            holds(&store, "ex:b rdf:type ex:A"),
            "{case}: the domain puts ex:b in ex:A"
        );
        apply(
            &commits,
            "DELETE DATA { ex:a ex:p ex:b . } ; INSERT DATA { ex:b rdf:type ex:B . }",
        )
        .unwrap_or_else(|e| panic!("{case}: a change that leaves no clash commits, not: {e}"));
        assert_eq!(
            store.consistency(),
            ConsistencyStatus::Consistent,
            "{case}: the store after it is consistent"
        );
        assert!(
            holds(&store, "ex:b rdf:type ex:B"),
            "{case}: the inserted type is there"
        );
        assert!(
            !holds(&store, "ex:b rdf:type ex:A"),
            "{case}: the inference the delete removed is gone"
        );
    }
}

/// Valid OWL 2 DL: `DifferentIndividuals(ex:m1 ex:m2 ex:m3)` with `ex:m1 owl:sameAs ex:m2` is
/// inconsistent (`eq-diff2`). As a store's first write under equality by representatives, with
/// aliases that give the members' list 4 * 4 * 5 = 80 walks over every identity (past the
/// 64-walk cap), the write is refused, as it is with fewer aliases.
#[test]
fn a_first_write_past_the_list_cap_is_judged_by_its_rebuild() {
    let store = store(false);
    let commits = pipeline(&store);
    let result = apply(
        &commits,
        "INSERT DATA { _:ad rdf:type owl:AllDifferent . _:ad owl:members (ex:m1 ex:m2 ex:m3) .
         ex:m1 owl:sameAs ex:m2 . ex:m1 owl:sameAs ex:a1 . ex:m1 owl:sameAs ex:a2 .
         ex:m3 owl:sameAs ex:b1 . ex:m3 owl:sameAs ex:b2 . ex:m3 owl:sameAs ex:b3 .
         ex:m3 owl:sameAs ex:b4 . }",
    );
    assert_refused_for(result, "eq-diff2", "the first write");
    assert_eq!(
        store.stats().expect("stats").quad_count,
        0,
        "nothing was published"
    );
    assert!(
        !holds(&store, "ex:m1 owl:sameAs ex:m2"),
        "the refused write's facts are not there"
    );
}

/// A split over a compact stack that breaks no rule commits, consistent; so does one on a store
/// in quarantine, whose clash stood before the write: only what a write brings refuses it.
#[test]
fn a_split_that_brings_no_new_clash_commits() {
    let plain = store(true);
    let commits = pipeline(&plain);
    apply(
        &commits,
        "INSERT DATA { ex:a owl:sameAs ex:b . ex:a rdf:type ex:C . ex:C rdfs:subClassOf ex:D . }",
    )
    .expect("the base commits");
    apply(&commits, "DELETE DATA { ex:a owl:sameAs ex:b . }").expect("an ordinary split commits");
    assert_eq!(plain.consistency(), ConsistencyStatus::Consistent);

    // Under representatives the policy rebuilds a split whose class carries more facts than a
    // rebuild of the whole store would touch (here 30 on `ex:a`).
    let representatives = store(false);
    let commits = pipeline(&representatives);
    let mut base = String::from("INSERT DATA { ex:a owl:sameAs ex:b . ex:c ex:q ex:d .\n");
    for i in 0..30 {
        base.push_str(&format!("ex:a ex:p ex:v{i} .\n"));
    }
    base.push('}');
    apply(&commits, &base).expect("the base commits");
    apply(&commits, "DELETE DATA { ex:a owl:sameAs ex:b . }")
        .expect("a split the policy rebuilds commits");
    assert_eq!(
        commits.last_reasoning_run().map(|run| run.work.plan),
        Some(nrese_reasoner::delta::policy::Plan::Rebuild),
        "the split is rebuilt"
    );
    assert_eq!(representatives.consistency(), ConsistencyStatus::Consistent);

    let quarantined = loaded(
        true,
        "quarantined",
        &format!(
            "<{EX}A> <{OWL}disjointWith> <{EX}B> .
<{EX}y> <{RDF}type> <{EX}A> .
<{EX}y> <{RDF}type> <{EX}B> .
"
        ),
    );
    assert!(
        quarantined.is_quarantined(),
        "the clash stands before the write"
    );
    let commits = pipeline(&quarantined);
    apply(
        &commits,
        "INSERT DATA { ex:a owl:sameAs ex:b . ex:a rdf:type ex:C . }",
    )
    .expect("a write in quarantine that brings no clash commits");
    assert!(
        quarantined.is_quarantined(),
        "still in quarantine before the split"
    );
    apply(&commits, "DELETE DATA { ex:a owl:sameAs ex:b . }")
        .expect("a split in quarantine is not refused for the clash that stood before it");
    assert!(
        quarantined.is_quarantined(),
        "the standing clash keeps the store in quarantine"
    );
}
