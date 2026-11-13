//! A store's durable names resolved at their first use (D16-548): an open that finds its root
//! and changes nothing reads none of them; the first durable write that relies on them, or the
//! log served to a follower, resolves and syncs them before anything is acknowledged or leaves
//! the process (DUR-4, deferred-sync design note of 8 Nov).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::fault;
use crate::{DurabilityConfig, Engine, EngineConfig, SyncPolicy};

fn config() -> EngineConfig {
    EngineConfig {
        background_maintenance: false,
        durability: DurabilityConfig {
            sync: SyncPolicy::EveryCommit,
            ..DurabilityConfig::default()
        },
        ..EngineConfig::default()
    }
}

/// The quad `n` of these tests.
fn quad(n: u64) -> nrese_rdf::Quad {
    let node =
        |name: String| nrese_rdf::NamedNode::new_unchecked(format!("http://example.com/{name}"));
    nrese_rdf::Quad::new(
        node(format!("s{n}")),
        node("p".to_owned()),
        node(format!("o{n}")),
        nrese_rdf::GraphName::DefaultGraph,
    )
}

/// Commits `quad(n)`.
fn commit(engine: &Engine, n: u64) -> crate::EngineResult<crate::CommitSummary> {
    let mut tx = engine.transaction();
    tx.insert(quad(n).as_ref());
    tx.commit()
}

/// Whether `engine`'s latest snapshot holds `quad(n)`.
fn holds(engine: &Engine, n: u64) -> bool {
    let snapshot = engine.snapshot();
    snapshot
        .lookup_quad(quad(n).as_ref())
        .is_some_and(|encoded| snapshot.contains(&encoded))
}

fn canonical(dir: &Path) -> PathBuf {
    dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())
}

/// What `step` returns, the name resolutions it began, and the directories it synced
/// (canonical).
fn observed<T>(step: impl FnOnce() -> T) -> (T, usize, BTreeSet<PathBuf>) {
    use std::rc::Rc;
    let synced = Rc::new(std::cell::RefCell::new(BTreeSet::new()));
    let seen = Rc::clone(&synced);
    fault::ON_DIR_SYNC.set(Some(Box::new(move |dir: &Path| {
        seen.borrow_mut().insert(canonical(dir));
    })));
    let before = fault::RESOLUTIONS.get();
    let out = step();
    let resolutions = fault::RESOLUTIONS.get() - before;
    fault::ON_DIR_SYNC.set(None);
    (out, resolutions, synced.take())
}

/// A store at `a/b/store` in a new directory, made with `quad(1)` committed and closed.
fn made_store() -> (tempfile::TempDir, PathBuf) {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("a/b/store");
    let engine = Engine::open(&root, config()).unwrap();
    commit(&engine, 1).unwrap();
    drop(engine);
    (base, root)
}

/// Spec: a warm open of a store whose root it finds, and which it doesn't change, resolves no
/// durable name: not the root's path, not its WAL directory, not its files. The first commit
/// resolves them and syncs the holder of the store's own entry (`a/b`) before it is
/// acknowledged.
#[test]
fn a_warm_open_that_changes_nothing_resolves_no_name() {
    let (base, root) = made_store();
    let (engine, at_open, synced_at_open) = observed(|| Engine::open(&root, config()).unwrap());
    assert_eq!(
        at_open, 0,
        "the open resolved names (synced {synced_at_open:?})"
    );
    assert!(
        synced_at_open.is_empty(),
        "the open synced {synced_at_open:?}"
    );
    let (committed, at_commit, synced) = observed(|| commit(&engine, 2));
    committed.unwrap();
    assert!(at_commit > 0, "the first commit resolved no name");
    let holder = canonical(&base.path().join("a/b"));
    assert!(
        synced.contains(&holder),
        "{} not synced: {synced:?}",
        holder.display()
    );
}

/// A first commit whose resolution of the store's names fails is refused before anything is
/// written: the log isn't poisoned, the next commit resolves the names and is acknowledged, and
/// a reopen holds it and not the refused one.
#[test]
fn a_failed_resolution_refuses_the_write_and_the_next_resolves() {
    let (_base, root) = made_store();
    let engine = Engine::open(&root, config()).unwrap();
    fault::FAIL_RESOLUTION.set(true);
    let refused = commit(&engine, 2);
    fault::FAIL_RESOLUTION.set(false);
    assert!(
        refused.is_err(),
        "acknowledged with its names unresolved: {refused:?}"
    );
    commit(&engine, 3).expect("the commit after a refused resolution");
    drop(engine);
    let engine = Engine::open(&root, config()).unwrap();
    assert!(
        holds(&engine, 1) && holds(&engine, 3),
        "an acknowledged commit lost"
    );
    assert!(!holds(&engine, 2), "the refused commit is there");
}

/// The log served to a follower before any write on this open resolves and syncs the store's
/// names first: nothing leaves the process before they are durable.
#[test]
fn the_log_served_first_names_the_store_before_it_leaves() {
    let (base, root) = made_store();
    let engine = Engine::open(&root, config()).unwrap();
    let (batch, resolutions, synced) = observed(|| engine.log_since(0, 1 << 20));
    let batch = batch.unwrap();
    assert!(batch.last >= 1, "no record served: {batch:?}");
    assert!(resolutions > 0, "served with no name resolved");
    let holder = canonical(&base.path().join("a/b"));
    assert!(
        synced.contains(&holder),
        "{} not synced: {synced:?}",
        holder.display()
    );
}
