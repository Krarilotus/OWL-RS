use super::{UNREAD, capacity, hooks, pipeline};
use crate::{
    BulkLoadRequest, ConsistencyStatus, Entailment, GraphTarget, InvalidStatements,
    MutationCommand, MutationPipeline, MutationTicket, Requester, SparqlQueryRequest,
    SparqlUpdateRequest, StoreConfig, StoreError, StoreService,
};
use nrese_engine::{EncodedTriple, QuadPattern, ReadModel};
use nrese_rdf::{
    NamedNode, Triple,
    vocab::{owl, rdf},
};
use nrese_reasoner::ReasoningMode;
use nrese_reasoner::rulesets::Ruleset;
use std::path::Path;
use std::sync::{Arc, Mutex};

const EX: &str = "PREFIX ex: <http://example.org/>";
const CLASH: &str = "ex:A owl:disjointWith ex:B . ex:x a ex:A, ex:B .";
const INSERT: &str = "INSERT DATA { ex:y a ex:A }";
const REPAIR: &str = "DELETE DATA { ex:x a ex:B }";
const READ: &str = "SELECT ?x WHERE { ?x a ex:A }";

fn load(store: &StoreService, data: &str) -> crate::StoreResult<()> {
    load_with_mode(store, data, false)
}

fn load_with_mode(store: &StoreService, data: &str, replace: bool) -> crate::StoreResult<()> {
    let dir = tempfile::tempdir().expect("setup: input directory");
    let file = dir.path().join("data.ttl");
    std::fs::write(
        &file,
        format!(
            "@prefix ex: <http://example.org/> .\n\
             @prefix owl: <http://www.w3.org/2002/07/owl#> .\n\
             @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\
             @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n{data}"
        ),
    )
    .expect("setup: Turtle file");
    store
        .bulk_load(&BulkLoadRequest {
            files: vec![file],
            replace,
            graph: GraphTarget::DefaultGraph,
            invalid: Some(InvalidStatements::Strict),
        })
        .map(|_| ())
}

fn update(pipeline: &MutationPipeline, update: &str) {
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!("{EX} {update}"))),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .expect("setup: update committed");
}

pub(in crate::service) fn quarantined(
    config: StoreConfig,
) -> (Arc<StoreService>, MutationPipeline) {
    let store = Arc::new(
        StoreService::new(config)
            .expect("setup: store")
            .with_capacity(capacity),
    );
    load(&store, CLASH).expect("setup: disjoint classes");
    let pipeline = pipeline(&store, ReasoningMode::Owl2Rl);
    store
        .rematerialise(Ruleset::Owl2Rl)
        .expect("setup: rebuild");
    assert_eq!(
        store.consistency(),
        ConsistencyStatus::Inconsistent { violations: 1 },
        "setup: the clash was assessed"
    );
    assert!(
        store.is_quarantined(),
        "setup: rebuild established the hold"
    );
    (store, pipeline)
}

fn open_over_old_verdict(mut config: StoreConfig, path: &Path, data: &str) -> StoreService {
    let compact = config.equality_compact;
    config.ql_rewriting = crate::QlRewritingMode::On;
    let store = StoreService::new(config.clone()).expect("setup: store");
    load(&store, data).expect("setup: data as left by an older writer");
    drop(store);
    let equality = if compact {
        "equality compact\n"
    } else {
        "lists nil-class\n"
    };
    std::fs::write(
        path.join(crate::service::REASONING_MARKER),
        format!("ruleset owl2-rl\nfingerprint 0000000000000001\nviolations 0\n{equality}"),
    )
    .expect("setup: legacy marker");
    let store = StoreService::new(config).expect("setup: bare open");
    assert!(
        !store.reasoning_is_current(Ruleset::Owl2Rl),
        "setup: older fingerprint"
    );
    store
}

fn conclusion() -> [Triple; 1] {
    [Triple::new(
        NamedNode::new_unchecked("http://example.org/z"),
        rdf::TYPE.into_owned(),
        NamedNode::new_unchecked("http://example.org/H"),
    )]
}

// A bare open over an old rules marker answers Unknown until its rebuild settles consistency.
#[test]
fn a_bare_open_cannot_report_the_old_rules_verdict() {
    let aliases: String = (1..=7)
        .map(|i| format!("ex:C{i} owl:sameAs ex:D{i} . "))
        .collect();
    let intersection = format!(
        "{aliases} ex:I owl:intersectionOf (ex:C1 ex:C2 ex:C3 ex:C4 ex:C5 ex:C6 ex:C7) . \
         ex:x a ex:C1, ex:C2, ex:C3, ex:C4, ex:C5, ex:C6, ex:C7 ."
    );
    let alternatives = (1..=64)
        .map(|i| format!("ex:R{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let chain = format!(
        "ex:alt rdfs:subPropertyOf rdf:first . ex:k2 ex:alt {alternatives} . \
         ex:p owl:propertyChainAxiom ex:k1 . ex:k1 rdf:first ex:q ; rdf:rest ex:k2 . \
         ex:k2 rdf:first ex:r ; rdf:rest rdf:nil . ex:a ex:q ex:b ."
    );
    let clash = format!("{intersection} ex:I owl:disjointWith ex:E . ex:x a ex:E .");
    let mut r1 = String::from(
        "ex:alt rdfs:subPropertyOf rdf:first . ex:ax a owl:AllDisjointClasses ; \
         owl:members ex:p1 . ex:x a ex:P1, ex:P2 . ",
    );
    for i in 1..=101 {
        use std::fmt::Write;
        let rest = if i == 101 {
            "rdf:nil".to_owned()
        } else {
            format!("ex:p{}", i + 1)
        };
        write!(r1, "ex:p{i} rdf:first ex:P{i} ; rdf:rest {rest} . ").unwrap();
        if [95, 96, 97, 98, 99, 100, 101].contains(&i) {
            write!(r1, "ex:p{i} ex:alt ex:pZ{i} . ").unwrap();
        }
    }
    let mut cells = super::Cells::default();
    for (equality, representatives, compact) in [
        ("replication", false, false),
        ("representatives", true, false),
        ("compact", true, true),
    ] {
        for (shape, data, inconsistent) in [
            (
                "long disjoint list",
                if compact { "" } else { &r1 },
                !compact,
            ),
            ("stale chain", chain.as_str(), false),
            (
                "missing intersection inference",
                intersection.as_str(),
                false,
            ),
            ("intersection clash", clash.as_str(), true),
        ] {
            let dir = tempfile::tempdir().expect("setup: store directory");
            let config = StoreConfig {
                equality_by_representatives: representatives,
                equality_compact: compact,
                ..StoreConfig::on_disk(dir.path())
            };
            let store = open_over_old_verdict(config, dir.path(), data);
            let name = format!("{shape}, {equality}");
            let before = store.consistency();
            cells.check(&name, before.as_str(), |word| word == "unknown", "unknown");
            store.rematerialise(Ruleset::Owl2Rl).expect("rebuilt");
            let want = if inconsistent {
                "inconsistent"
            } else {
                "consistent"
            };
            let after = store.consistency();
            cells.check(
                &format!("{name}, rebuilt"),
                after.as_str(),
                |word| word == want,
                want,
            );
        }
    }
    cells.finish();
}

// A DL query detects the asserted disjointness clash even when a stale marker has zero violations.
#[test]
fn a_dl_read_cannot_prove_consistency_from_an_old_rules_marker() {
    let dir = tempfile::tempdir().expect("setup: store directory");
    let store = open_over_old_verdict(StoreConfig::on_disk(dir.path()), dir.path(), CLASH);
    store.use_dl(true, false);
    let answer = store
        .execute_query(&SparqlQueryRequest::all(format!("{EX} {READ}")))
        .expect("setup: DL query answered");
    let status = answer
        .completeness
        .expect("setup: DL query reported its status");
    assert!(
        status.reasons.iter().any(|reason| {
            reason.text == "the data is inconsistent under OWL 2 DL, so it entails every answer"
        }),
        "expected an inconsistent DL verdict, got {}",
        status.header()
    );
    assert_eq!(store.consistency(), ConsistencyStatus::Unknown);
}

// A held revision awaiting revalidation refuses to answer entailment by the rules.
#[test]
fn entailment_refuses_a_commit_awaiting_revalidation() {
    let (store, pipeline) = quarantined(StoreConfig::in_memory());
    let seen = Arc::new(Mutex::new(None));
    let (reader, result) = (Arc::clone(&store), Arc::clone(&seen));
    hooks::BEFORE_REVALIDATION.set(Some(Box::new(move || {
        *result.lock().unwrap() = Some(reader.entails(Ruleset::Owl2Rl, &conclusion()));
    })));
    update(&pipeline, INSERT);
    let answer = seen
        .lock()
        .unwrap()
        .take()
        .expect("setup: reached revalidation");
    assert!(
        matches!(&answer, Err(StoreError::Configuration(_))),
        "got {answer:?}"
    );
    assert!(
        answer
            .unwrap_err()
            .to_string()
            .contains("rematerialise first")
    );
}

// An assessed hold can establish an inconsistent premise on a bare open.
#[test]
fn entailment_reports_an_inconsistent_premise_on_a_bare_held_open() {
    let dir = tempfile::tempdir().expect("setup: store directory");
    let config = StoreConfig::on_disk(dir.path());
    drop(quarantined(config.clone()));
    let store = StoreService::new(config).expect("setup: reopened bare");
    assert!(store.is_quarantined(), "setup: reopened over a hold");
    assert_eq!(
        store
            .entails(Ruleset::Owl2Rl, &conclusion())
            .expect("entailment answer"),
        Entailment::InconsistentPremise,
    );
}

fn fail_revalidation(pipeline: &MutationPipeline) {
    hooks::BEFORE_REVALIDATION.set(Some(Box::new(|| UNREAD.set((true, false)))));
    update(pipeline, INSERT);
    let (_, tried) = UNREAD.replace((false, false));
    assert!(
        tried,
        "setup: revalidation attempted to read unavailable memory"
    );
}

// A committed version with failed revalidation has no assessment, and the next preflight rebuilds.
#[test]
fn failed_revalidation_leaves_the_next_commit_to_rebuild_in_preflight() {
    let (store, pipeline) = quarantined(StoreConfig::in_memory());
    fail_revalidation(&pipeline);
    assert!(store.reasoning_state().is_none());
    assert!(!store.reasoning_is_current(Ruleset::Owl2Rl));
    let current = Arc::new(Mutex::new(None));
    let (reader, result) = (Arc::clone(&store), Arc::clone(&current));
    hooks::AFTER_PREFLIGHT.set(Some(Box::new(move || {
        *result.lock().unwrap() = Some(reader.consistency());
    })));
    update(&pipeline, REPAIR);
    let at_preflight = current
        .lock()
        .unwrap()
        .take()
        .expect("setup: reached preflight");
    assert_eq!(
        at_preflight,
        ConsistencyStatus::Inconsistent { violations: 1 }
    );
    assert_eq!(store.consistency(), ConsistencyStatus::Consistent);
}

// A load preserves the hold of a quarantined commit both in the revalidation window and later.
#[test]
fn a_bulk_load_preserves_a_commit_hold_before_and_after_failed_revalidation() {
    let (store, pipeline) = quarantined(StoreConfig::in_memory());
    let seen = Arc::new(Mutex::new(None));
    let (reader, result) = (Arc::clone(&store), Arc::clone(&seen));
    hooks::BEFORE_REVALIDATION.set(Some(Box::new(move || {
        load(&reader, "ex:b ex:p ex:c .").expect("setup: load before revalidation");
        *result.lock().unwrap() = Some(reader.is_quarantined());
    })));
    update(&pipeline, INSERT);
    assert!(
        seen.lock()
            .unwrap()
            .take()
            .expect("setup: reached revalidation")
    );

    let (store, pipeline) = quarantined(StoreConfig::in_memory());
    fail_revalidation(&pipeline);
    load(&store, "ex:b ex:p ex:c .").expect("setup: load after failed revalidation");
    assert!(store.is_quarantined());
}

// Invalidation clears the rules' verdict while the store stays in quarantine until repaired.
#[test]
fn invalidation_preserves_quarantine_until_repair() {
    let dir = tempfile::tempdir().expect("setup: store directory");
    let (store, pipeline) = quarantined(StoreConfig::on_disk(dir.path()));
    store.invalidate_reasoning().expect("setup: invalidated");
    assert!(store.is_quarantined(), "readiness after invalidation");
    assert_eq!(store.consistency(), ConsistencyStatus::Unknown);
    update(&pipeline, REPAIR);
    assert!(!store.is_quarantined());
    assert_eq!(store.consistency(), ConsistencyStatus::Consistent);
}

// A refutation accepts an identical recertification and refuses a changed reading at the same revision.
#[test]
fn refutation_checks_the_reading_after_a_same_revision_rebuild() {
    let store = StoreService::new(StoreConfig {
        equality_by_representatives: true,
        equality_compact: true,
        ..StoreConfig::in_memory()
    })
    .expect("setup: compact store");
    load(
        &store,
        "ex:A owl:disjointWith ex:B . ex:u a ex:A . ex:v a ex:B . \
         ex:alias owl:sameAs ex:item . ex:item a ex:Seed . ex:Seed rdfs:subClassOf ex:C .",
    )
    .expect("setup: premise loaded");
    store
        .rematerialise(Ruleset::Owl2Rl)
        .expect("setup: premise rebuilt");
    let conclusion = [Triple::new(
        NamedNode::new_unchecked("http://example.org/u"),
        owl::DIFFERENT_FROM.into_owned(),
        NamedNode::new_unchecked("http://example.org/v"),
    )];
    assert_eq!(
        store
            .entails(Ruleset::Owl2Rl, &conclusion)
            .expect("setup: refutation"),
        Entailment::Entailed
    );

    let before = store.engine().snapshot();
    let other = store.clone();
    hooks::BETWEEN_ENTAILMENT_HALVES.set(Some(Box::new(move || {
        other.set_ql_rewriting(crate::QlRewritingMode::On);
    })));
    let allowed = store.entails(Ruleset::Owl2Rl, &conclusion);
    let after = store.engine().snapshot();
    assert!(
        !before.same_version(&after),
        "setup: a new record was published"
    );
    assert_eq!(
        before.identity(),
        after.identity(),
        "setup: same stack and reading"
    );
    assert_eq!(
        allowed.expect("entailment after recertification"),
        Entailment::Entailed
    );

    let other = store.clone();
    hooks::BETWEEN_ENTAILMENT_HALVES.set(Some(Box::new(move || {
        let rebuild = other.engine().rematerialisation();
        let rows = rebuild
            .base()
            .stored()
            .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
            .map(EncodedTriple::from)
            .collect();
        // Keeping the stored triples and switching off equality expansion exercises
        // the rebuild's same-revision install.
        rebuild
            .finish_held(rows, None, None)
            .expect("setup: rebuilt reading");
    })));
    let answer = store.entails(Ruleset::Owl2Rl, &conclusion);
    let rebuilt = store.engine().snapshot();
    assert!(
        after.identity().equality.is_some(),
        "setup: compact reading"
    );
    assert_eq!(rebuilt.revision(), after.revision(), "setup: same revision");
    assert_eq!(rebuilt.identity().equality, None, "setup: changed reading");
    assert!(
        matches!(&answer, Err(StoreError::Undecided(why))
            if why == "the data changed during the entailment check: ask again"),
        "refutation after the reading changed: {answer:?}"
    );
}

// A failed load leaves the held version, and a successful load carries its hold into the new data.
#[test]
fn bulk_publication_preserves_quarantine_until_repair() {
    check_bulk_publication(false, 0);
}

// A mapped replacement with spilling disabled carries the hold through the streamed publication.
#[test]
fn streamed_bulk_publication_preserves_quarantine_until_repair() {
    check_bulk_publication(true, 0);
}

// A mapped replacement exceeding the spill threshold carries the hold through spilled publication.
#[test]
fn spilled_bulk_publication_preserves_quarantine_until_repair() {
    check_bulk_publication(true, 2048);
}

fn check_bulk_publication(replace: bool, bulk_load_memory_bytes: u64) {
    let dir = tempfile::tempdir().expect("setup: store directory");
    let (store, pipeline) = quarantined(StoreConfig {
        bulk_load_memory_bytes,
        map_checkpoints: true,
        ..StoreConfig::on_disk(dir.path())
    });
    let mut data = String::from("ex:tom a ex:A .");
    if replace {
        data.push_str(CLASH);
    }
    let spills = bulk_load_memory_bytes > 0;
    if spills {
        use std::fmt::Write;
        // The engine's 2 KiB test budget starts a spill at 21 quads.
        for i in 0..32 {
            write!(data, " ex:s{i} ex:p ex:o .").unwrap();
        }
    }
    let read = |store: &StoreService| {
        store
            .execute_query(&SparqlQueryRequest::all(format!("{EX} {READ}")))
            .expect("query answered")
    };
    let old = read(&store);
    let before = store.current_revision();
    nrese_engine::fault_points::fail_dir_sync_of(Some((dir.path(), std::io::ErrorKind::Other)));
    let failed = load_with_mode(&store, &data, replace);
    nrese_engine::fault_points::fail_dir_sync_of(None);
    assert!(failed.is_err(), "setup: load publication failed");
    assert!(store.is_quarantined(), "readiness after failed load");
    assert_eq!(
        store.consistency(),
        ConsistencyStatus::Inconsistent { violations: 1 }
    );
    assert_eq!(store.current_revision(), before);
    assert_eq!(read(&store).payload, old.payload);

    let seen = Arc::new(Mutex::new(None));
    let (reader, result) = (Arc::clone(&store), Arc::clone(&seen));
    let spill = dir.path().join("bulk-spill");
    hooks::AFTER_INVALIDATION.set(Some(Box::new(move || {
        assert_eq!(spill.is_dir(), spills, "setup: selected bulk path");
        *result.lock().unwrap() = Some((reader.is_quarantined(), read(&reader)));
    })));
    load_with_mode(&store, &data, replace).expect("setup: successful load");
    let (held, during) = seen
        .lock()
        .unwrap()
        .take()
        .expect("setup: reached publication");
    let after = read(&store);
    assert_eq!(
        (held, store.is_quarantined()),
        (true, true),
        "readiness before and after load"
    );
    assert_eq!(during.payload, old.payload);
    assert_eq!(during.completeness, old.completeness);
    assert_eq!(store.consistency(), ConsistencyStatus::Unknown);
    assert!(String::from_utf8_lossy(&after.payload).contains("http://example.org/tom"));
    update(&pipeline, REPAIR);
    assert!(!store.is_quarantined());
    assert_eq!(store.consistency(), ConsistencyStatus::Consistent);
}
