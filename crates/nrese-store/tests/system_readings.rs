//! An operation is admitted by limits read where the process and the store start, never by
//! reading the system's memory on its path: a commit, and a rematerialisation, that make
//! equality copies read neither what the system has free nor its cgroups
//! ([`nrese_exec::memory::system_readings`]). The count is the process's, so this file is a
//! test binary of its own, with one test.

use std::sync::Arc;

use nrese_exec::memory;
use nrese_reasoner::rulesets::Ruleset;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationPipeline, MutationTicket, ProcessMemory, Requester,
    SparqlUpdateRequest, StoreConfig, StoreService,
};

/// One class of 10 identities (`ex:iN owl:sameAs ex:rep`) and one fact over it in all three
/// places: 1,000 copies, which any limit here admits.
fn data() -> String {
    let mut data = String::from(
        "<http://example.com/rep> <http://example.com/rep> <http://example.com/rep> . ",
    );
    for i in 1..10 {
        data.push_str(&format!(
            "<http://example.com/i{i}> <http://www.w3.org/2002/07/owl#sameAs> <http://example.com/rep> . "
        ));
    }
    format!("INSERT DATA {{ {data} }}")
}

/// Whether a copy of the fact over three other identities holds.
fn copied(store: &StoreService) -> bool {
    let answer = store
        .execute_query_str(
            "ASK { <http://example.com/i1> <http://example.com/i2> <http://example.com/i3> }",
        )
        .expect("the query runs");
    String::from_utf8_lossy(&answer.payload).contains("true")
}

/// With a limit of the store's own (a room to read) and without one (0: the operator's
/// choice, no ceiling put back), by representatives and by replication: a commit's
/// maintenance and a rematerialisation make the copies, admitted, and the system's memory is
/// read only where the store opens.
#[test]
fn equality_copies_are_admitted_without_reading_the_systems_memory() {
    if !memory::READS_MEMORY {
        return;
    }
    assert_eq!(
        memory::process_limit(),
        None,
        "no process limit is set here"
    );
    let held = memory::process_bytes().expect("the process's memory reads here");
    for limit in [held + (1 << 30), 0] {
        for replicate in [false, true] {
            let store = Arc::new(
                StoreService::new(StoreConfig {
                    process_memory_bytes: ProcessMemory::Bytes(limit),
                    equality_by_representatives: !replicate,
                    ..StoreConfig::in_memory()
                })
                .expect("the store opens"),
            );
            let pipeline = MutationPipeline::new(
                Arc::clone(&store),
                Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
                    ReasoningMode::Owl2Rl,
                ))),
            );
            // The store's limit, as resolved for it, read before the count: the operation's
            // path starts after it.
            let _ = store.process_memory_bytes();
            let opened = memory::system_readings();
            pipeline
                .apply(
                    MutationCommand::Update(SparqlUpdateRequest::new(data())),
                    &Requester::all(),
                    &MutationTicket::new(),
                )
                .expect("the commit is admitted");
            assert!(
                copied(&store),
                "limit {limit}, replicate {replicate}: no copies"
            );
            store
                .rematerialise(Ruleset::Owl2Rl)
                .expect("the rematerialisation is admitted");
            assert_eq!(
                memory::system_readings(),
                opened,
                "limit {limit}, replicate {replicate}: the system's memory was read on the \
                 operation's path"
            );
        }
    }
}
