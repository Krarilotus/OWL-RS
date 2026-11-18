//! Fault points for other crates' tests (feature `fault-points`): each makes one durable step
//! fail, on the calling thread only, so that tests running in parallel don't see each other's
//! faults. Off by default; only nrese-store's dev-dependency on this crate turns it on, and
//! `scripts/check-features.sh` checks that no product build has it. The engine's own tests use
//! their `#[cfg(test)]` seams (`durability::fault`).

use std::cell::RefCell;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

thread_local! {
    /// The directory whose sync fails, canonicalized, and how.
    static FAIL_DIR_SYNC_OF: RefCell<Option<(PathBuf, ErrorKind)>> = const { RefCell::new(None) };
}

/// Makes this thread's syncs of the directory `dir` through [`crate::place`] fail with `kind`;
/// `None` ends it. The directory is compared by its canonical path (it must exist), so a sync
/// that reaches it under another spelling (through a link, a junction, an 8.3 name) fails too.
pub fn fail_dir_sync_of(dir: Option<(&Path, ErrorKind)>) {
    let failing = dir.map(|(dir, kind)| {
        let dir = dir
            .canonicalize()
            .unwrap_or_else(|error| panic!("a fault point's {}: {error}", dir.display()));
        (dir, kind)
    });
    FAIL_DIR_SYNC_OF.set(failing);
}

/// The error a sync of `dir` fails with on this thread, if [`fail_dir_sync_of`] names it.
pub(crate) fn dir_sync_fails(dir: &Path) -> Option<ErrorKind> {
    FAIL_DIR_SYNC_OF.with_borrow(|failing| {
        let (failing, kind) = failing.as_ref()?;
        let dir = dir.canonicalize().ok()?;
        (&dir == failing).then_some(*kind)
    })
}
