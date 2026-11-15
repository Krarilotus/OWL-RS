//! When a DL operation over the store's ontology ends (a classification, a realisation),
//! what the shared workers hold unused is offered back to the system, unforced, on every
//! pool thread: idle workers otherwise keep the operation's freed memory, and the next
//! operation's peak starts above it. A forced release would cost the request time.
//! The allocator's release is registered once per process (`set_memory_release`), so this
//! file is a test binary of its own.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationCommitReport, MutationPipeline, MutationTicket, ReadScope, Requester,
    SparqlUpdateRequest, StoreConfig, StoreService,
};

/// Every release asked of the allocator: whether forced, and on which thread.
static RELEASES: Mutex<Vec<(bool, ThreadId)>> = Mutex::new(Vec::new());

fn record(force: bool) {
    RELEASES
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push((force, thread::current().id()));
}

fn releases() -> Vec<(bool, ThreadId)> {
    std::mem::take(&mut *RELEASES.lock().unwrap_or_else(|p| p.into_inner()))
}

#[test]
fn a_dl_operation_offers_every_pool_threads_unused_memory_back_unforced_when_it_ends() {
    nrese_store::set_memory_release(record);
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
    let pool: HashSet<ThreadId> = rayon::broadcast(|_| thread::current().id())
        .into_iter()
        .collect();

    let operations: [(&str, &dyn Fn()); 2] = [
        ("classification", &|| {
            let report = dl
                .store()
                .classify(&ReadScope::All)
                .expect("classification");
            assert!(report.complete(), "{:?}", report.incomplete);
        }),
        ("realisation", &|| {
            let report = dl.store().realise(&ReadScope::All).expect("realisation");
            assert!(report.complete(), "{:?}", report.incomplete);
        }),
    ];
    for (name, operation) in operations {
        releases();
        operation();
        let after = releases();
        let forced = after.iter().filter(|(force, _)| *force).count();
        assert_eq!(
            forced, 0,
            "{name}: {forced} forced releases, a cost on the request"
        );
        let offered: HashSet<ThreadId> = after.iter().map(|&(_, at)| at).collect();
        let missed = pool.difference(&offered).count();
        assert_eq!(
            missed,
            0,
            "{name}: {missed} of the {} pool threads were not asked to give back what they \
             hold unused ({} releases)",
            pool.len(),
            after.len()
        );
    }
}
