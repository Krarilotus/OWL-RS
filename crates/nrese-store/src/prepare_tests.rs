use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use nrese_reasoner::delta::policy::Plan;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};

use super::*;
use crate::mutation::write_program_hooks::{Stage, set};
use crate::service::hooks;
use crate::{Requester, StoreConfig};

#[path = "prepare_equivalence_tests.rs"]
mod equivalence;

const WAIT: Duration = Duration::from_secs(10);
const PREFIX: &str = "PREFIX ex: <http://example.org/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> PREFIX owl: <http://www.w3.org/2002/07/owl#>";
const SCHEMA: &str = "ex:A rdfs:subClassOf ex:B . ex:x a ex:A .";
const EDIT: &str = "INSERT DATA { ex:y a ex:A }";

fn rl() -> Arc<ReasonerService> {
    Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
        ReasoningMode::Owl2Rl,
    )))
}

fn loaded(store: StoreService, data: &str) -> Arc<StoreService> {
    let store = Arc::new(store);
    let raw = MutationPipeline::new(
        store.clone(),
        Arc::new(ReasonerService::new(ReasonerConfig::default())),
    );
    raw.execute_update_str(&format!("{PREFIX} INSERT DATA {{ {data} }}"))
        .unwrap();
    store
        .rematerialise(rl().config().materialised_program().unwrap())
        .unwrap();
    store
}

fn pipeline() -> MutationPipeline {
    MutationPipeline::new(
        loaded(StoreService::new(StoreConfig::in_memory()).unwrap(), SCHEMA),
        rl(),
    )
}

fn pause(stage: Stage) -> (Receiver<()>, Sender<()>) {
    let (entered, heard) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    set(stage, move || {
        entered.send(()).unwrap();
        let _ = wait.recv_timeout(WAIT);
    });
    (heard, release)
}

fn until(mut ready: impl FnMut() -> bool) {
    let until = Instant::now() + WAIT;
    while !ready() {
        assert!(Instant::now() < until, "preparation did not settle");
        thread::sleep(Duration::from_millis(2));
    }
}

fn ready(writer: &MutationPipeline) {
    until(|| writer.write_program_metrics().0 == 2);
}

fn ask(writer: &MutationPipeline, pattern: &str) -> bool {
    let result = writer
        .store()
        .execute_query_str(&format!("{PREFIX} ASK {{ {pattern} }}"))
        .unwrap();
    serde_json::from_slice::<serde_json::Value>(&result.payload).unwrap()["boolean"]
        .as_bool()
        .unwrap()
}

fn apply(
    writer: &MutationPipeline,
    ticket: &MutationTicket,
) -> Result<MutationCommitReport, MutationError> {
    writer.apply(
        MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIX} {EDIT}"))),
        &Requester::all(),
        ticket,
    )
}

fn answer_at(writer: &MutationPipeline, sparql: &str) {
    writer
        .execute_update_str(&format!("{PREFIX} {sparql}"))
        .unwrap();
}

/// Spec: a primary with rules reports building before its closure is certified.
/// A replica or a repository without rules reports that preparation is disabled.
#[test]
fn only_a_primary_with_rules_reports_building_at_construction() {
    for (mode, replica, expected) in [
        (ReasoningMode::Owl2Rl, false, 1),
        (ReasoningMode::Owl2Rl, true, 0),
        (ReasoningMode::Disabled, false, 0),
    ] {
        let config = StoreConfig {
            replica,
            ..StoreConfig::in_memory()
        };
        let reasoner = ReasonerService::new(ReasonerConfig::for_mode(mode));
        let writer = MutationPipeline::new(
            Arc::new(StoreService::new(config).unwrap()),
            Arc::new(reasoner),
        );
        assert_eq!(writer.write_program_metrics(), (expected, 0, 0));
    }
}

/// Spec: closing the last pipeline releases the directory even while a snapshot
/// is held for background grounding.
#[test]
fn closing_a_pipeline_releases_the_directory_during_grounding() {
    let dir = tempfile::tempdir().unwrap();
    let config = StoreConfig::on_disk(dir.path());
    let store = loaded(StoreService::new(config.clone()).unwrap(), SCHEMA);
    let (entered, release) = pause(Stage::DuringGrounding);
    let pipeline = MutationPipeline::new(store, rl()).prepare_from_start();
    entered.recv_timeout(WAIT).unwrap();
    drop(pipeline);
    let reopened = StoreService::new(config).expect("the closed pipeline released the directory");
    release.send(()).unwrap();
    assert!(reopened.reasoning_state().is_some());
}

/// Spec: closing the last pipeline returns while another thread holds the writer
/// slot and the background preparation is trying to pin a snapshot.
#[test]
fn closing_a_pipeline_does_not_wait_for_a_held_writer() {
    let store = loaded(StoreService::new(StoreConfig::in_memory()).unwrap(), SCHEMA);
    let pipeline = MutationPipeline::new(store.clone(), rl());
    let (held, writer_started) = mpsc::channel();
    let (release, writer_finished) = mpsc::channel();
    let writer = thread::spawn(move || {
        let _tx = store.engine().transaction();
        held.send(()).unwrap();
        writer_finished.recv_timeout(WAIT).unwrap();
    });
    writer_started.recv_timeout(WAIT).unwrap();
    let (attempted, resume) = pause(Stage::WaitingForWriter);
    let pipeline = pipeline.prepare_from_start();
    attempted.recv_timeout(WAIT).unwrap();
    resume.send(()).unwrap();
    let (closed, heard) = mpsc::channel();
    let closer = thread::spawn(move || {
        drop(pipeline);
        closed.send(()).unwrap();
    });
    let result = heard.recv_timeout(Duration::from_secs(1));
    release.send(()).unwrap();
    writer.join().unwrap();
    closer.join().unwrap();
    result.expect("the last pipeline must close with the writer slot still held");
}

/// Spec: a reopened, prepared writer makes the first edit against the current hidden
/// closure without grounding the whole program again.
#[test]
fn restarted_writer_uses_the_prepared_hidden_union_closure() {
    let dir = tempfile::tempdir().unwrap();
    let config = StoreConfig {
        hide_unnamed_classes: true,
        ..StoreConfig::on_disk(dir.path())
    };
    let union = "_:u owl:unionOf (ex:A ex:B) . ex:k ex:consumer _:u .";
    let store = loaded(StoreService::new(config.clone()).unwrap(), union);
    let writer = MutationPipeline::new(store, rl());
    answer_at(
        &writer,
        "DELETE { ex:k ex:consumer ?u } WHERE { ex:k ex:consumer ?u }",
    );
    drop(writer);
    let store = Arc::new(StoreService::new(config).unwrap());
    let reopened = MutationPipeline::new(store, rl()).prepare_from_start();
    ready(&reopened);
    apply(&reopened, &MutationTicket::new()).unwrap();
    assert!(!ask(&reopened, "?u owl:unionOf ?l . ex:y a ?u"));
    assert!(ask(&reopened, "ex:y a ex:A"));
    assert_eq!(reopened.write_program_metrics().2, 0);
    assert_ne!(
        reopened.last_reasoning_run().unwrap().work.plan,
        Plan::Rebuild
    );
}

/// Spec: the first ordinary write after a compact-store restart keeps its prepared
/// program when the repository has no owl:sameAs statements.
#[test]
fn compact_restart_without_same_as_reuses_its_write_program() {
    let dir = tempfile::tempdir().unwrap();
    let config = StoreConfig {
        equality_compact: true,
        ..StoreConfig::on_disk(dir.path())
    };
    drop(loaded(StoreService::new(config.clone()).unwrap(), SCHEMA));
    let store = Arc::new(StoreService::new(config).unwrap());
    let writer = MutationPipeline::new(store, rl()).prepare_from_start();
    ready(&writer);
    until(|| writer.preparation.builds.is_idle());
    assert_eq!(writer.write_program_metrics(), (2, 1, 0));
    assert!(!ask(&writer, "?a owl:sameAs ?b"));
    apply(&writer, &MutationTicket::new()).unwrap();
    assert!(ask(&writer, "ex:y a ex:B"));
    let after_write = writer.write_program_metrics();
    ready(&writer);
    until(|| writer.preparation.builds.is_idle());
    assert_eq!(
        writer.write_program_metrics(),
        (2, 1, 0),
        "after write: {after_write:?}"
    );
}

/// Spec: writes that leave the hidden map unchanged keep its answer and need no rebuild.
#[test]
fn unchanged_hidden_union_does_not_rebuild() {
    let config = StoreConfig {
        hide_unnamed_classes: true,
        ..StoreConfig::in_memory()
    };
    let store = loaded(
        StoreService::new(config).unwrap(),
        "_:u owl:unionOf (ex:A ex:B) . ex:x a ex:A .",
    );
    let writer = MutationPipeline::new(store, rl()).prepare_from_start();
    ready(&writer);
    for suffix in ["v", "w"] {
        answer_at(
            &writer,
            &format!("INSERT DATA {{ ex:k ex:note ex:{suffix} }}"),
        );
        assert!(ask(&writer, &format!("ex:k ex:note ex:{suffix}")));
        assert!(!ask(&writer, "?u owl:unionOf ?l . ex:x a ?u"));
        assert_eq!(writer.write_program_metrics().2, 0);
        assert_ne!(
            writer.last_reasoning_run().unwrap().work.plan,
            Plan::Rebuild
        );
    }
}

/// Spec: an edit waits for its preparation without the writer slot. A cancelled edit
/// publishes nothing even if the preparation ends at the same time.
#[test]
fn joined_write_finishes_or_cancels_without_publishing() {
    for (cancel, finish_first) in [(false, false), (true, false), (true, true)] {
        let (entered, release) = pause(Stage::BeforeInstall);
        let pipeline = pipeline().prepare_from_start();
        entered.recv_timeout(WAIT).unwrap();
        let revision = pipeline.store().current_revision();
        let ticket = MutationTicket::new();
        let next_preflight = Arc::new(AtomicBool::new(false));
        let (answer, result) = mpsc::channel();
        let client = {
            let (pipeline, ticket, next_preflight) =
                (pipeline.clone(), ticket.clone(), next_preflight.clone());
            thread::spawn(move || {
                hooks::AFTER_PREFLIGHT.set(Some(Box::new(move || {
                    hooks::AFTER_PREFLIGHT.set(Some(Box::new(move || {
                        next_preflight.store(true, Ordering::Release)
                    })));
                })));
                answer.send(apply(&pipeline, &ticket)).unwrap();
            })
        };
        until(|| pipeline.preparation.builds.waiting(&()) != 0);
        if cancel {
            assert!(ticket.cancel());
        }
        if !cancel || finish_first {
            release.send(()).unwrap();
        }
        let outcome = result.recv_timeout(WAIT).unwrap();
        if cancel {
            assert!(matches!(outcome, Err(MutationError::Cancelled)));
            assert_eq!(pipeline.store().current_revision(), revision);
            assert!(!ask(&pipeline, "ex:y a ex:A"));
            assert!(!next_preflight.load(Ordering::Acquire));
        } else {
            outcome.unwrap();
            assert!(ask(&pipeline, "ex:y a ex:B"));
            assert_eq!(pipeline.write_program_metrics(), (2, 1, 0));
        }
        drop(release);
        client.join().unwrap();
    }
}

/// Spec: cancellation at the handoff returns while preparation is paused before its
/// claim. A startup build that wins the same race gives the edit a normal answer.
#[test]
fn a_paused_handoff_can_be_cancelled_or_find_the_entry_covered() {
    for cancel in [true, false] {
        let (startup, release_startup) = pause(Stage::BeforeInstall);
        let pipeline = pipeline().prepare_from_start();
        startup.recv_timeout(WAIT).unwrap();
        let ticket = MutationTicket::new();
        let (entered, held) = mpsc::channel();
        let (answer, result) = mpsc::channel();
        let (client_store, client_ticket) = (pipeline.clone(), ticket.clone());
        let client = thread::spawn(move || {
            let (before_claim, release) = pause(Stage::BeforeClaim);
            entered.send((before_claim, release)).unwrap();
            answer.send(apply(&client_store, &client_ticket)).unwrap();
        });
        let (before_claim, release) = held.recv_timeout(WAIT).unwrap();
        before_claim.recv_timeout(WAIT).unwrap();
        if cancel {
            assert!(ticket.cancel());
            assert!(matches!(
                result.recv_timeout(WAIT).unwrap(),
                Err(MutationError::Cancelled)
            ));
            assert!(!ask(&pipeline, "ex:y a ex:A"));
            drop(release);
            drop(release_startup);
        } else {
            release_startup.send(()).unwrap();
            ready(&pipeline);
            release.send(()).unwrap();
            result.recv_timeout(WAIT).unwrap().unwrap();
            assert!(ask(&pipeline, "ex:y a ex:B"));
            assert_eq!(pipeline.write_program_metrics(), (2, 1, 0));
        }
        client.join().unwrap();
    }
}

/// Spec: retirement refuses a joined edit before the held preparer may install.
#[test]
fn retired_preparer_leaves_a_waiting_edit_unpublished() {
    let (entered, release) = pause(Stage::BeforeInstall);
    let pipeline = pipeline().prepare_from_start();
    entered.recv_timeout(WAIT).unwrap();
    let revision = pipeline.store().current_revision();
    let (answer, result) = mpsc::channel();
    let writer = pipeline.clone();
    let client =
        thread::spawn(move || answer.send(apply(&writer, &MutationTicket::new())).unwrap());
    until(|| pipeline.preparation.builds.waiting(&()) != 0);
    pipeline.retire();
    assert!(matches!(
        result.recv_timeout(WAIT).unwrap(),
        Err(MutationError::Retired)
    ));
    assert_eq!(pipeline.store().current_revision(), revision);
    release.send(()).unwrap();
    until(|| pipeline.preparation.builds.is_idle());
    assert!(pipeline.preparation.entry.lock().is_none());
    client.join().unwrap();
}

/// Spec: a preparer that stops for memory makes one attempt at this version. A later
/// write can request another attempt and publish after memory recovers.
#[test]
fn a_failed_key_waits_for_an_explicit_retry() {
    static STOP: AtomicBool = AtomicBool::new(false);
    fn capacity() -> nrese_exec::memory::Reading {
        if STOP.load(Ordering::Acquire) {
            Err(nrese_exec::memory::Unread::Failed)
        } else {
            Ok(1 << 40)
        }
    }
    let store = StoreService::new(StoreConfig::in_memory())
        .unwrap()
        .with_capacity(capacity);
    let store = loaded(store, SCHEMA);
    STOP.store(true, Ordering::Release);
    let pipeline = MutationPipeline::new(store, rl()).prepare_from_start();
    until(|| pipeline.write_program_metrics().0 == 3 && pipeline.preparation.builds.is_idle());
    let rev = pipeline.store().current_revision();
    for _ in 0..5 {
        thread::sleep(Duration::from_millis(50));
        assert_eq!(pipeline.write_program_metrics(), (3, 1, 0));
    }
    assert_eq!(pipeline.store().current_revision(), rev);
    STOP.store(false, Ordering::Release);
    apply(&pipeline, &MutationTicket::new()).unwrap();
    assert!(ask(&pipeline, "ex:y a ex:B"));
    assert_eq!(pipeline.write_program_metrics(), (2, 2, 0));
}

/// Spec: the blocked preparation of one repository leaves another repository's write
/// free to finish. A write may start its own repository's preparation ahead of startup.
#[test]
fn preparation_is_local_and_a_write_can_beat_startup() {
    let reopen = |dir: &tempfile::TempDir| {
        let config = StoreConfig::on_disk(dir.path());
        drop(loaded(StoreService::new(config.clone()).unwrap(), SCHEMA));
        MutationPipeline::new(Arc::new(StoreService::new(config).unwrap()), rl())
    };
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let a = reopen(&dir_a);
    let b = reopen(&dir_b);
    let (held_a, release_a) = pause(Stage::BeforeInstall);
    let a = a.prepare_from_start();
    held_a.recv_timeout(WAIT).unwrap();
    let (held_b, release_b) = pause(Stage::BeforeClaim);
    let b = b.prepare_from_start();
    held_b.recv_timeout(WAIT).unwrap();
    apply(&b, &MutationTicket::new()).unwrap();
    assert!(ask(&b, "ex:y a ex:B"));
    assert!(!ask(&a, "ex:y a ex:A"));
    assert_eq!(b.write_program_metrics(), (2, 1, 0));
    drop(release_b);
    drop(release_a);
}

/// Spec: a same-revision replacement while a write waits for the slot makes that
/// write join the replacement. A successor with another reading drops its program.
#[test]
fn a_waiting_write_joins_the_replacement_and_drops_its_reading() {
    let config = StoreConfig {
        equality_compact: true,
        ..StoreConfig::in_memory()
    };
    let store = loaded(
        StoreService::new(config).unwrap(),
        "ex:x owl:sameAs ex:y . ex:y ex:p ex:z .",
    );
    let writer = MutationPipeline::new(store.clone(), rl()).prepare_from_start();
    ready(&writer);
    let tx = store.engine().transaction();
    let original = ground_key(tx.base());
    assert!(original.1.is_some());
    let certificate = tx.base().certificate().cloned();
    let (before_slot, waiting_for_slot) = mpsc::channel();
    let (joined, waiting_to_write) = mpsc::channel();
    let (resume, wait) = mpsc::channel();
    let client_writer = writer.clone();
    let client = thread::spawn(move || {
        hooks::AFTER_PREFLIGHT.set(Some(Box::new(move || {
            hooks::AFTER_PREFLIGHT.set(Some(Box::new(move || {
                joined.send(()).unwrap();
                wait.recv_timeout(WAIT).unwrap();
            })));
            before_slot.send(()).unwrap();
        })));
        answer_at(&client_writer, "INSERT DATA { ex:z ex:p ex:x }");
    });
    waiting_for_slot.recv_timeout(WAIT).unwrap();
    let (install, release) = pause(Stage::BeforeInstall);
    tx.install(None, certificate);
    until(|| !writer.preparation.builds.is_idle());
    drop(tx);
    install.recv_timeout(WAIT).unwrap();
    until(|| writer.preparation.builds.waiting(&()) != 0);
    release.send(()).unwrap();
    waiting_to_write.recv_timeout(WAIT).unwrap();
    let alternate = cached(&writer.preparation.entry.lock(), (original.0, None))
        .cloned()
        .expect("the joined program belongs to the alternate reading");
    resume.send(()).unwrap();
    client.join().unwrap();
    let successor = ground_key(&store.engine().snapshot());
    assert!(successor.0 > original.0);
    assert_eq!(successor.1, original.1);
    assert!(
        writer
            .preparation
            .entry
            .lock()
            .as_ref()
            .is_none_or(|(_, ground)| !Arc::ptr_eq(ground, &alternate))
    );
    assert_eq!(writer.write_program_metrics().2, 0);
    store
        .rematerialise(rl().config().materialised_program().unwrap())
        .unwrap();
    assert!(ask(&writer, "ex:z ex:p ex:y"));
}

/// Spec: a newer certified revision published while a write still tags its old
/// result is ready before the next edit arrives.
#[test]
fn publication_during_the_write_seam_wakes_the_new_head() {
    for control in [false, true] {
        let pipeline = pipeline().prepare_from_start();
        ready(&pipeline);
        let (entered, seam) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let writer = pipeline.clone();
        let commit = thread::spawn(move || {
            hooks::AFTER_WRITE_PUBLICATION.set(Some(Box::new(move || {
                entered.send(()).unwrap();
                wait.recv_timeout(WAIT).unwrap();
            })));
            apply(&writer, &MutationTicket::new())
        });
        seam.recv_timeout(WAIT).unwrap();
        pipeline.store().clear_inferred().unwrap();
        pipeline
            .store()
            .rematerialise(rl().config().materialised_program().unwrap())
            .unwrap();
        release.send(()).unwrap();
        commit.join().unwrap().unwrap();
        let writer = if control {
            let store = Arc::clone(pipeline.store());
            pipeline.retire();
            MutationPipeline::new(store, rl()).prepare_from_start()
        } else {
            pipeline
        };
        ready(&writer);
        answer_at(&writer, "INSERT DATA { ex:z a ex:A }");
        assert!(ask(&writer, "ex:z a ex:B"));
        assert_eq!(writer.write_program_metrics().2, 0);
    }
}

/// Spec: restoration of the reading at the same revision prepares that exact key,
/// whether startup observed the invalidation or had already prepared the entry.
#[test]
fn recording_the_same_revision_wakes_its_restored_reading() {
    for delayed in [true, false] {
        let config = StoreConfig {
            equality_compact: true,
            ..StoreConfig::in_memory()
        };
        let store = loaded(
            StoreService::new(config).unwrap(),
            "ex:x owl:sameAs ex:y . ex:y ex:p ex:z .",
        );
        let wait = delayed.then(|| pause(Stage::BeforeClaim));
        let writer = MutationPipeline::new(store.clone(), rl()).prepare_from_start();
        if let Some((entered, _)) = &wait {
            entered.recv_timeout(WAIT).unwrap();
        } else {
            ready(&writer);
        }
        let original = ground_key(&store.engine().snapshot());
        assert!(original.1.is_some(), "compact base");
        store.invalidate_reasoning().unwrap();
        store.invalidate_reasoning().unwrap();
        if let Some((_, release)) = wait {
            release.send(()).unwrap();
        }
        store
            .rematerialise(rl().config().materialised_program().unwrap())
            .unwrap();
        assert_eq!(ground_key(&store.engine().snapshot()), original);
        ready(&writer);
        answer_at(&writer, "INSERT DATA { ex:z ex:p ex:x }");
        assert!(ask(&writer, "ex:z ex:p ex:y"));
        assert_eq!(writer.write_program_metrics().2, 0);
    }
}

/// Spec: preparation for a superseded revision leaves the replacement eligible and
/// the next edit uses only that replacement's ground program.
#[test]
fn a_superseded_preparation_moves_to_the_newer_head() {
    let (entered, release) = pause(Stage::BeforeInstall);
    let writer = pipeline().prepare_from_start();
    entered.recv_timeout(WAIT).unwrap();
    let before = writer.store().current_revision();
    writer.store().clear_inferred().unwrap();
    writer
        .store()
        .rematerialise(rl().config().materialised_program().unwrap())
        .unwrap();
    assert!(writer.store().current_revision() > before);
    release.send(()).unwrap();
    ready(&writer);
    apply(&writer, &MutationTicket::new()).unwrap();
    assert!(ask(&writer, "ex:y a ex:B"));
    assert_eq!(writer.write_program_metrics().2, 0);
    assert_ne!(writer.preparation.builds.ends().later, 0);
}

/// Spec: retiring a pipeline returns while a commit is still paused in preflight.
#[test]
fn retirement_does_not_wait_for_preflight() {
    let pipeline = pipeline().prepare_from_start();
    ready(&pipeline);
    let (entered, seen) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let writer = pipeline.clone();
    let commit = thread::spawn(move || {
        hooks::AFTER_PREFLIGHT.set(Some(Box::new(move || {
            entered.send(()).unwrap();
            wait.recv_timeout(WAIT).unwrap();
        })));
        apply(&writer, &MutationTicket::new())
    });
    seen.recv_timeout(WAIT).unwrap();
    let (done, retired) = mpsc::channel();
    let retiring = pipeline.clone();
    let retire = thread::spawn(move || {
        retiring.retire();
        done.send(()).unwrap();
    });
    retired.recv_timeout(WAIT).unwrap();
    release.send(()).unwrap();
    assert!(matches!(
        commit.join().unwrap(),
        Err(MutationError::Retired)
    ));
    assert!(!ask(&pipeline, "ex:y a ex:A"));
    retire.join().unwrap();
}

/// Spec: a stopped old preparation cannot install into the catalogue's successor.
#[test]
fn a_cutover_has_its_own_prepared_entry() {
    use crate::catalog::{Catalog, DEFAULT_REPOSITORY, RepositorySettings};
    let store = loaded(StoreService::new(StoreConfig::in_memory()).unwrap(), SCHEMA);
    let (entered, release) = pause(Stage::BeforeInstall);
    let catalogue = Catalog::open((*store).clone(), (*rl()).clone()).unwrap();
    let repository = catalogue.get(DEFAULT_REPOSITORY).unwrap();
    let old = repository.pipeline.read().clone();
    entered.recv_timeout(WAIT).unwrap();
    catalogue
        .change(
            DEFAULT_REPOSITORY,
            RepositorySettings {
                reasoning: Some("rdfs".to_owned()),
                ..RepositorySettings::default()
            },
        )
        .unwrap();
    let current = repository.pipeline.read().clone();
    ready(&current);
    release.send(()).unwrap();
    until(|| old.preparation.builds.is_idle());
    apply(&current, &MutationTicket::new()).unwrap();
    assert!(ask(&current, "ex:y a ex:B"));
    assert_eq!(current.write_program_metrics(), (2, 1, 0));
    assert!(old.is_retired());
    assert!(old.preparation.entry.lock().is_none());
}
