//! Test-only fault points: each makes one durable step fail, on this thread only, so that
//! tests running in parallel don't see each other's faults.

use std::cell::{Cell, RefCell};
use std::thread::LocalKey;

/// A reader of a snapshot ([`IMAGE_READER`]).
pub(crate) type Reader = Box<dyn FnMut(&crate::Snapshot)>;

thread_local! {
    /// [`Wal::rotate`](super::wal::Wal::rotate) fails before it changes anything.
    pub(crate) static FAIL_ROTATE: Cell<bool> = const { Cell::new(false) };
    /// A checkpoint's directory sync fails after its rename.
    pub(crate) static FAIL_DIR_SYNC: Cell<bool> = const { Cell::new(false) };
    /// A new WAL segment's directory sync fails after its rename.
    pub(crate) static FAIL_SEGMENT_PLACED: Cell<bool> = const { Cell::new(false) };
    /// Mapping a checkpoint just written fails.
    pub(crate) static FAIL_MAP: Cell<bool> = const { Cell::new(false) };
    /// Syncing `wal-cut/` after a copy is placed (or found) fails.
    pub(crate) static FAIL_CUT_DIR_SYNC: Cell<bool> = const { Cell::new(false) };
    /// The wall clock, microseconds since 1970, as commits on this thread read it.
    pub(crate) static CLOCK: Cell<Option<u64>> = const { Cell::new(None) };
    /// What `CLOCK` reads once a checkpoint's parts are written, before its commit time:
    /// the time a long checkpoint write took.
    pub(crate) static CLOCK_AT_TRAILER: Cell<Option<u64>> = const { Cell::new(None) };
    /// What `CLOCK` reads once a transaction's record is about to be written to the log,
    /// its preparation (runs, a layout conversion, a rotation) done: the time that took.
    pub(crate) static CLOCK_AT_APPEND: Cell<Option<u64>> = const { Cell::new(None) };
    /// The files this thread synced through `durability::place`, in order.
    pub(crate) static SYNCED: std::cell::RefCell<Vec<std::path::PathBuf>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// A file sync through `durability::place` fails.
    pub(crate) static FAIL_FILE_SYNC: Cell<bool> = const { Cell::new(false) };
    /// The sync of this directory through `durability::place` fails, with this kind of error.
    pub(crate) static FAIL_DIR_SYNC_OF: std::cell::RefCell<
        Option<(std::path::PathBuf, std::io::ErrorKind)>,
    > = const { std::cell::RefCell::new(None) };
    /// Called with each directory `durability::place` syncs, before the sync: what a test
    /// sees at that moment.
    pub(crate) static ON_DIR_SYNC: std::cell::RefCell<Option<DirSyncHook>> =
        const { std::cell::RefCell::new(None) };
    /// Reads a durable bulk load's checkpoint image before the checkpoint is written, as a
    /// reader of it would (the own-state slots' 3.0c item 5).
    pub(crate) static IMAGE_READER: RefCell<Option<Reader>> = const { RefCell::new(None) };
}

/// What [`ON_DIR_SYNC`] calls.
pub(crate) type DirSyncHook = Box<dyn FnMut(&std::path::Path)>;

/// An error if `point` is set on this thread.
pub(crate) fn check(point: &'static LocalKey<Cell<bool>>, what: &str) -> std::io::Result<()> {
    match point.get() {
        true => Err(std::io::Error::other(format!("{what} failed (a test's)"))),
        false => Ok(()),
    }
}
