//! D14 (LM-V15-4): an embedded store's memory limit (`process_memory_bytes`, its own watch,
//! not the process's global limit) admits the reasoner's equality copies before they are
//! made, in each of the store's operations that reason: a rematerialisation, a commit's
//! maintenance and the equality report. Measured with a counting allocator, so this file is
//! a test binary of its own.

use std::sync::{Arc, Mutex};

use nrese_exec::heap;
use nrese_reasoner::rulesets::Ruleset;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationError, MutationPipeline, MutationTicket, ProcessMemory, Requester,
    SparqlUpdateRequest, StoreConfig, StoreError, StoreService,
};

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

/// The tests share the allocator's counts.
static SERIAL: Mutex<()> = Mutex::new(());

/// What the store's limit leaves above what the process holds when it opens.
const ROOM: u64 = 32 << 20;

/// One class of 300 identities (`ex:iN owl:sameAs ex:rep`) and one fact over it in all three
/// places: 300^3 = 27 M copies, about 650 MB, where the store's limit leaves [`ROOM`].
fn data() -> String {
    let mut data = String::from(
        "<http://example.com/rep> <http://example.com/rep> <http://example.com/rep> . ",
    );
    for i in 1..300 {
        data.push_str(&format!(
            "<http://example.com/i{i}> <http://www.w3.org/2002/07/owl#sameAs> <http://example.com/rep> . "
        ));
    }
    format!("INSERT DATA {{ {data} }}")
}

/// A store whose own limit is [`ROOM`] above what the process holds now, with no global
/// limit set: its equality by representatives, or by replication.
fn store(replicate: bool) -> Arc<StoreService> {
    assert_eq!(
        nrese_exec::memory::process_limit(),
        None,
        "the store's own limit is the only one"
    );
    let held = nrese_exec::memory::process_bytes().expect("the process's memory reads here");
    Arc::new(
        StoreService::new(StoreConfig {
            process_memory_bytes: ProcessMemory::Bytes(held + ROOM),
            equality_by_representatives: !replicate,
            ..StoreConfig::in_memory()
        })
        .expect("the store opens"),
    )
}

/// The heap's peak above what it held before, while `work` runs.
fn peak<T>(work: impl FnOnce() -> T) -> (T, usize) {
    let before = heap::live();
    heap::start("work");
    let done = work();
    let phases = heap::finish();
    let peak = phases.iter().map(|phase| phase.peak).max().expect("phases");
    (done, peak.saturating_sub(before))
}

/// A rematerialisation whose copies don't fit the store's limit is refused for it, with that
/// limit, and nothing is applied; the heap stays under twice the room.
#[test]
fn a_rematerialisation_is_refused_by_the_stores_own_limit_before_the_copies() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    if !nrese_exec::memory::READS_MEMORY {
        return;
    }
    for replicate in [false, true] {
        let store = store(replicate);
        let writer = MutationPipeline::new(
            Arc::clone(&store),
            Arc::new(ReasonerService::new(ReasonerConfig::default())),
        );
        writer.execute_update_str(&data()).expect("the data loads");
        let limit = store.process_memory_bytes().expect("the limit reads here");
        let (result, peak) = peak(|| store.rematerialise(Ruleset::Owl2Rl));
        let error = result.expect_err("the copies were made");
        assert!(
            matches!(error, StoreError::ProcessMemoryLimit { limit: l } if l == limit),
            "replicate {replicate}: {error}, not the store's limit {limit}"
        );
        assert!(store.reasoning_state().is_none(), "nothing was applied");
        assert!(
            peak < 2 * ROOM as usize,
            "replicate {replicate}: {peak} bytes at the heap's peak"
        );
    }
}

/// A commit whose maintenance would make the copies past the store's limit is refused for
/// it, with that limit; the heap stays under twice the room.
#[test]
fn a_commits_maintenance_is_refused_by_the_stores_own_limit_before_the_copies() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    if !nrese_exec::memory::READS_MEMORY {
        return;
    }
    for replicate in [false, true] {
        let store = store(replicate);
        let pipeline = MutationPipeline::new(
            Arc::clone(&store),
            Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
                ReasoningMode::Owl2Rl,
            ))),
        );
        let limit = store.process_memory_bytes().expect("the limit reads here");
        let (result, peak) = peak(|| {
            pipeline.apply(
                MutationCommand::Update(SparqlUpdateRequest::new(data())),
                &Requester::all(),
                &MutationTicket::new(),
            )
        });
        let error = result.expect_err("the copies were made");
        assert!(
            matches!(
                &error,
                MutationError::Store { source: StoreError::ProcessMemoryLimit { limit: l }, .. }
                    if *l == limit
            ),
            "replicate {replicate}: {error}, not the store's limit {limit}"
        );
        assert!(
            peak < 2 * ROOM as usize,
            "replicate {replicate}: {peak} bytes at the heap's peak"
        );
    }
}

/// The equality report, whose replicated closure would make the copies past the store's
/// limit, says it stopped for it; the heap stays under twice the room.
#[test]
fn the_equality_report_is_refused_by_the_stores_own_limit_before_the_copies() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    if !nrese_exec::memory::READS_MEMORY {
        return;
    }
    let store = store(false);
    let writer = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::default())),
    );
    writer.execute_update_str(&data()).expect("the data loads");
    let (report, peak) = peak(|| store.equality_report(Ruleset::Owl2Rl));
    assert_eq!(report, "equality: stopped by the process memory limit");
    assert!(peak < 2 * ROOM as usize, "{peak} bytes at the heap's peak");
}
