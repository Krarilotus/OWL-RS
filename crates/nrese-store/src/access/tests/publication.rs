//! A change of the access state and a write's publication (docs/architecture/contracts.md,
//! lock order): a change that comes while a write publishes under its rights publishes after
//! that write, having waited for its publication step alone.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nrese_rdf::{GraphName, NamedNode, Quad};

use super::*;

/// Half the largest window a commit merges itself (`CompactionPolicy::inline_entry_limit`,
/// 32 Ki entries): the second of two runs this size merges both inside its commit.
const HALF: usize = 16 * 1024;

/// `HALF` statements, numbered from `from`.
fn statements(from: usize) -> Vec<Quad> {
    let node = |name: String| NamedNode::new_unchecked(name);
    (from..from + HALF)
        .map(|i| {
            let (s, o) = (node(format!("urn:x:s{i}")), node(format!("urn:x:o{i}")));
            Quad::new(s, node("urn:x:p".to_owned()), o, GraphName::DefaultGraph)
        })
        .collect()
}

/// The change's publication waits for the write's (it reads the write's `published` mark
/// only after it), and the time it waited is the publication step of a commit that merges
/// the largest inline window: printed for the contracts page.
#[test]
fn a_change_of_rights_publishes_after_a_write_publishing_under_them() {
    let system = StoreService::new(StoreConfig::in_memory()).unwrap();
    let access = Arc::new(AccessControl::open(system, "urn:nrese:").unwrap());
    let reason = "test";
    access
        .apply(&admin(), Change::Import(policy()), reason)
        .unwrap();
    let dataset = StoreService::new(StoreConfig::in_memory()).unwrap();
    let mut first = dataset.engine().transaction();
    for quad in statements(0) {
        first.insert(quad.as_ref());
    }
    first.commit().unwrap();
    let mut tx = dataset.engine().transaction();
    for quad in statements(HALF) {
        tx.insert(quad.as_ref());
    }
    let view = access.view_checked(&roles(&["analyst"]), "nrese");
    let rights = view.rights.expect("a checked view carries its rights");
    let held = rights.hold().expect("rights nothing changed hold");
    let published = Arc::new(AtomicBool::new(false));
    let change = std::thread::spawn({
        let (access, published) = (Arc::clone(&access), Arc::clone(&published));
        move || {
            let started = Instant::now();
            let removed = Change::RemoveRole("editor".to_owned());
            access.apply(&admin(), removed, reason).unwrap();
            (published.load(Ordering::Acquire), started.elapsed())
        }
    });
    // Time enough for the change to reach its publication many times over: its own commit,
    // in the system store, takes milliseconds.
    let lead = Duration::from_millis(500);
    std::thread::sleep(lead);
    let committing = Instant::now();
    tx.commit().unwrap();
    published.store(true, Ordering::Release);
    drop(held);
    let step = committing.elapsed();
    let (after, applied) = change.join().unwrap();
    assert!(after, "the change published while a write held its rights");
    eprintln!(
        "publication step merging {} entries inline: {step:?}; the change took {applied:?}, \
         {:?} of it after the step began",
        2 * HALF,
        applied.saturating_sub(lead)
    );
}
