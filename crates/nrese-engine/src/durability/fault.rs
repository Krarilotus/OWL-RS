//! Test-only fault points: each makes one durable step fail, on this thread only, so that
//! tests running in parallel don't see each other's faults.

use std::cell::Cell;
use std::thread::LocalKey;

thread_local! {
    /// [`Wal::rotate`](super::wal::Wal::rotate) fails before it changes anything.
    pub(crate) static FAIL_ROTATE: Cell<bool> = const { Cell::new(false) };
    /// A checkpoint's directory sync fails after its rename.
    pub(crate) static FAIL_DIR_SYNC: Cell<bool> = const { Cell::new(false) };
    /// A new WAL segment's directory sync fails after its rename.
    pub(crate) static FAIL_SEGMENT_PLACED: Cell<bool> = const { Cell::new(false) };
    /// Mapping a checkpoint just written fails.
    pub(crate) static FAIL_MAP: Cell<bool> = const { Cell::new(false) };
}

/// An error if `point` is set on this thread.
pub(crate) fn check(point: &'static LocalKey<Cell<bool>>, what: &str) -> std::io::Result<()> {
    match point.get() {
        true => Err(std::io::Error::other(format!("{what} failed (a test's)"))),
        false => Ok(()),
    }
}
