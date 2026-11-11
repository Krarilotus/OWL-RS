//! `budgets.huge_pages = "bulk"` (`HugePages::Bulk`): transparent huge pages are off while
//! the store serves, on while a bulk load or a materialisation runs, and off again once
//! either ends, also when it ends in an error. Read from the kernel (`THP_enabled`), in a
//! process of its own: the switch is the process's.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nrese_exec::memory::HugePages;
use nrese_reasoner::rulesets::Ruleset;

use crate::service::hooks;
use crate::{BulkLoadRequest, GraphTarget, LoadProgress, StoreConfig, StoreError, StoreService};

/// Whether the kernel lets huge pages back this process (`THP_enabled`, Linux 5.0 on).
fn enabled() -> bool {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let line = status.lines().find(|l| l.starts_with("THP_enabled:"));
    let line = line.expect("the kernel shows the process's huge pages");
    line.split_whitespace().nth(1) == Some("1")
}

/// What `enabled` read while a bulk load ran: at the load's hook, once it holds the writer
/// slot and before it reads its files. `then` runs there too.
fn during_the_load(then: impl FnOnce() + 'static) -> Arc<AtomicBool> {
    let seen = Arc::new(AtomicBool::new(false));
    let at_hook = Arc::clone(&seen);
    hooks::AFTER_INVALIDATION.set(Some(Box::new(move || {
        at_hook.store(enabled(), Ordering::Relaxed);
        then();
    })));
    seen
}

#[test]
fn bulk_has_huge_pages_only_while_a_load_or_a_materialisation_runs() {
    if !nrese_exec::measure::alone() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("data.ttl");
    std::fs::write(
        &input,
        "@prefix : <http://example.com/> .\n\
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
         :A rdfs:subClassOf :B . :x a :A .\n",
    )
    .unwrap();
    let request = BulkLoadRequest {
        files: vec![input],
        replace: false,
        graph: GraphTarget::DefaultGraph,
        invalid: None,
    };
    let store = StoreService::new(StoreConfig {
        huge_pages: HugePages::Bulk,
        ..StoreConfig::in_memory()
    })
    .unwrap();
    assert!(!enabled(), "off while serving");

    let during = during_the_load(|| {});
    store.bulk_load(&request).unwrap();
    assert!(during.load(Ordering::Relaxed), "on while a load runs");
    assert!(!enabled(), "off after a load");

    let progress = Arc::new(LoadProgress::default());
    let cancel = Arc::clone(&progress);
    let during = during_the_load(move || cancel.cancel());
    let failed = store.bulk_load_with(&request, &progress).unwrap_err();
    assert!(matches!(failed, StoreError::LoadCancelled), "{failed}");
    assert!(during.load(Ordering::Relaxed), "on while a load runs");
    assert!(!enabled(), "off after a load that failed");

    let during = AtomicBool::new(false);
    let failed = store
        .rematerialise_until(Ruleset::Rdfs, &|| {
            during.store(enabled(), Ordering::Relaxed);
            true
        })
        .unwrap_err();
    assert!(
        matches!(failed, StoreError::MaterialisationCancelled),
        "{failed}"
    );
    assert!(
        during.load(Ordering::Relaxed),
        "on while a materialisation runs"
    );
    assert!(!enabled(), "off after a materialisation that was stopped");

    store.rematerialise(Ruleset::Rdfs).unwrap();
    assert!(!enabled(), "off after a materialisation");
}
