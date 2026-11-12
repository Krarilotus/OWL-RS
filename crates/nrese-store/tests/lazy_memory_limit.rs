//! The process's memory limit is found lazily, at an upper level: opening a store reads no
//! system memory, whatever its limit; the memory the process may use is read once per
//! process, at the limit's first need; a limit set by hand is never more than that memory;
//! no limit never reads. The count of readings is the process's
//! ([`nrese_store::memory::system_readings`]), so this file is a test binary of its own, with
//! one test.

use nrese_store::memory;
use nrese_store::{ProcessMemory, StoreConfig, StoreService};

fn open(limit: ProcessMemory) -> StoreService {
    StoreService::new(StoreConfig {
        process_memory_bytes: limit,
        ..StoreConfig::in_memory()
    })
    .expect("the store opens")
}

#[test]
fn the_process_memory_limit_is_read_once_at_its_first_need() {
    if !memory::READS_MEMORY {
        return;
    }
    let before = memory::system_readings();
    let default = open(ProcessMemory::Default);
    let explicit = open(ProcessMemory::Bytes(1 << 30));
    let above = open(ProcessMemory::Bytes(u64::MAX));
    let none = open(ProcessMemory::Bytes(0));
    assert_eq!(
        memory::system_readings(),
        before,
        "opening a store, with the default limit, one set by hand or none, read the system's \
         memory"
    );

    // No limit never reads, and leaves the process no budget.
    assert_eq!(none.process_memory_bytes(), Ok(0));
    assert!(none.runtime().memory().is_none());
    assert_eq!(memory::system_readings(), before, "no limit read");

    // The first need reads once; every later one, any store's, reads nothing.
    let limit = default
        .process_memory_bytes()
        .expect("the limit reads here");
    assert_eq!(
        memory::system_readings(),
        before + 1,
        "the first need reads once"
    );
    let capacity = memory::capacity().expect("read");
    assert_eq!(limit, capacity / 4 * 3, "the default: three quarters");
    assert_eq!(
        explicit.process_memory_bytes(),
        Ok(capacity.min(1 << 30)),
        "a limit set by hand, within the capacity"
    );
    assert_eq!(
        above.process_memory_bytes(),
        Ok(capacity),
        "a limit set by hand above the capacity is the capacity"
    );
    let process = above.runtime().memory().expect("a process budget");
    assert_eq!(
        u64::try_from(process.limit()).ok(),
        Some(capacity),
        "and the process's budget holds to it"
    );
    default
        .execute_query_str("SELECT * WHERE { ?s ?p ?o }")
        .expect("the query runs");
    assert_eq!(
        memory::system_readings(),
        before + 1,
        "read once per process, whatever needs it later"
    );
}
