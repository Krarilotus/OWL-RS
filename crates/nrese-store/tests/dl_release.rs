//! When a DL operation over the store's ontology ends (a classification, a realisation),
//! what the shared workers hold unused is offered back to the system, unforced: idle
//! workers otherwise keep the operation's freed memory, and the next operation's peak
//! starts above it. A forced release would cost the request time. The calling thread
//! releases before the operation returns; each pool thread when it is next free, so a
//! worker busy with other work doesn't hold the operation up.
//!
//! The allocator's release is registered once per process (`set_memory_release`), and the
//! global pool is given a fixed width of two before anything uses it, so this file is a test
//! binary of its own and its tests run one at a time.

use std::collections::HashSet;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationCommitReport, MutationPipeline, MutationTicket, ReadScope, Requester,
    SparqlUpdateRequest, StoreConfig, StoreService,
};

/// The global pool's width in this binary.
const WORKERS: usize = 2;

/// How long a pool thread's release may take to arrive once the thread is free.
const ARRIVAL: Duration = Duration::from_secs(10);

/// Every release asked of the allocator: whether forced, and on which thread.
static RELEASES: Mutex<Vec<(bool, ThreadId)>> = Mutex::new(Vec::new());
static RECORDED: Condvar = Condvar::new();

fn record(force: bool) {
    RELEASES
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push((force, thread::current().id()));
    RECORDED.notify_all();
}

/// The releases recorded so far, cleared.
fn releases() -> Vec<(bool, ThreadId)> {
    std::mem::take(&mut *RELEASES.lock().unwrap_or_else(|p| p.into_inner()))
}

/// The releases recorded so far, cleared, once every thread of `threads` has an unforced one
/// among them or `ARRIVAL` has passed.
fn releases_once_offered_on(threads: &HashSet<ThreadId>) -> Vec<(bool, ThreadId)> {
    let all = RELEASES.lock().unwrap_or_else(|p| p.into_inner());
    let (mut all, _) = RECORDED
        .wait_timeout_while(all, ARRIVAL, |all| {
            let offered: HashSet<ThreadId> = all
                .iter()
                .filter(|(force, _)| !force)
                .map(|&(_, at)| at)
                .collect();
            !threads.is_subset(&offered)
        })
        .unwrap_or_else(|p| p.into_inner());
    std::mem::take(&mut *all)
}

/// The binary's one setup (the pool's width, the recording release), and the lock that runs
/// its tests one at a time.
fn serial() -> MutexGuard<'static, ()> {
    static SETUP: OnceLock<Mutex<()>> = OnceLock::new();
    SETUP
        .get_or_init(|| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(WORKERS)
                .build_global()
                .expect("the global pool is set before anything uses it");
            nrese_store::set_memory_release(record);
            Mutex::new(())
        })
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// The global pool's threads.
fn pool() -> HashSet<ThreadId> {
    let pool: HashSet<ThreadId> = rayon::broadcast(|_| thread::current().id())
        .into_iter()
        .collect();
    assert_eq!(pool.len(), WORKERS);
    pool
}

/// A store in the `owl2-dl` mode holding a small ontology with an assertion.
fn ontology() -> MutationPipeline {
    let store = StoreService::new(StoreConfig::in_memory()).expect("store");
    let dl = MutationPipeline::new(
        Arc::new(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    );
    let report = dl
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(
                "PREFIX : <http://example.com/> \
                 PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
                 INSERT DATA { :A rdfs:subClassOf :B . :B rdfs:subClassOf :C . :x a :A . }",
            )),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .expect("the ontology commits");
    assert!(
        matches!(report, MutationCommitReport::Applied { .. }),
        "{report:?}"
    );
    dl
}

fn classify(dl: &MutationPipeline) {
    let report = dl
        .store()
        .classify(&ReadScope::All)
        .expect("classification");
    assert!(report.complete(), "{:?}", report.incomplete);
}

fn realise(dl: &MutationPipeline) {
    let report = dl.store().realise(&ReadScope::All).expect("realisation");
    assert!(report.complete(), "{:?}", report.incomplete);
}

#[test]
fn a_dl_operation_offers_the_unused_memory_of_its_caller_and_every_pool_thread_back_unforced() {
    let _serial = serial();
    let dl = ontology();
    let pool = pool();
    let operations: [(&str, fn(&MutationPipeline)); 2] =
        [("classification", classify), ("realisation", realise)];
    for (name, operation) in operations {
        releases();
        operation(&dl);
        let caller = thread::current().id();
        let at_return = RELEASES.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert!(
            at_return.contains(&(false, caller)),
            "{name}: the calling thread was not asked to give back what it holds unused before \
             the operation returned ({at_return:?})"
        );
        let after = releases_once_offered_on(&pool);
        let forced = after.iter().filter(|(force, _)| *force).count();
        assert_eq!(
            forced, 0,
            "{name}: {forced} forced releases, a cost on the request"
        );
        let offered: HashSet<ThreadId> = after.iter().map(|&(_, at)| at).collect();
        let missed = pool.difference(&offered).count();
        assert_eq!(
            missed, 0,
            "{name}: {missed} of the {WORKERS} pool threads were not asked to give back what \
             they hold unused within {ARRIVAL:?} ({after:?})"
        );
    }
}

#[test]
fn a_dl_operation_returns_while_a_pool_worker_is_busy_with_other_work() {
    let _serial = serial();
    let dl = ontology();
    let pool = pool();
    // One worker blocks until it is freed, or by itself after `HELD`: the other work a
    // finished operation must not wait for. The operation's own work has the other worker.
    const HELD: Duration = Duration::from_secs(6);
    let (free, freed) = mpsc::channel::<()>();
    let (holding, held) = mpsc::channel::<()>();
    rayon::spawn(move || {
        let _ = holding.send(());
        let _ = freed.recv_timeout(HELD);
    });
    held.recv_timeout(HELD)
        .expect("a pool worker took the blocking job");
    releases();
    let started = Instant::now();
    classify(&dl);
    let took = started.elapsed();
    let _ = free.send(());
    assert!(
        took < HELD / 2,
        "the classification returned after {took:?}, with a pool worker busy elsewhere for \
         up to {HELD:?}: it waited for that worker"
    );
    // Once free, the busy worker gives back what it holds too.
    let after = releases_once_offered_on(&pool);
    let offered: HashSet<ThreadId> = after
        .iter()
        .filter(|(force, _)| !force)
        .map(|&(_, at)| at)
        .collect();
    assert!(
        pool.is_subset(&offered),
        "the pool threads did not all release once free ({after:?})"
    );
    // Nothing queued on the workers is left for the next test.
    rayon::broadcast(|_| ());
    releases();
}
