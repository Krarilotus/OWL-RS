//! The store's lock: one engine per data directory, across processes and within one (open row
//! IB1312-8ba4, master's decision IB1440-0730 (A) with (i), 7 Nov 2026).
//!
//! - **Within the process,** a registry of the open stores' directories refuses a second open
//!   before the lock file is touched.
//! - **Across processes,** an exclusive lock of the open file description on `<dir>/LOCK`
//!   (`File::try_lock`: Unix `flock`, Windows `LockFileEx`), as earlier binaries take it, so
//!   they and this exclude each other both ways.
//!   - Another descriptor of the file, opened and closed by anyone in this process (a reader, an
//!     image placement hashing it), is another description and releases nothing. A POSIX
//!     record lock (`fcntl`), which any close drops, was tried and gave that up (review r1,
//!     R1-F1).
//!   - A child forked while the store is open shares the description until it execs or ends.
//!     The lock is therefore released explicitly when the store is dropped (`unlock`, which
//!     releases it for the description, the child's share included), never by the last close:
//!     a child in its fork-to-exec window can't keep a closed store locked (open row
//!     IB1312-8ba4).
//!   - After a crash (no drop), a child that shares the description holds the lock until it
//!     execs or ends: the store is refused, never shared. This crate's children all exec
//!     (`std::process::Command`).
//!   - On NFS, Linux emulates `flock` with record locks, so another close releases the lock
//!     there: don't put a data directory on NFS.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::{EngineError, EngineResult};

/// The directories of the stores this process has open.
static OPEN: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// The lock of an open store, released when dropped.
pub(crate) struct StoreLock {
    /// Holds the lock; unlocked and closed, when dropped, before [`OPEN`] lets another open of
    /// the store in.
    file: Option<File>,
    /// Its entry in [`OPEN`].
    dir: PathBuf,
}

impl StoreLock {
    /// Locks the store in `root` (created), or [`EngineError::Locked`] if another engine, in
    /// this process or another, has it open.
    pub(crate) fn acquire(root: &Path) -> EngineResult<Self> {
        let dir = root.canonicalize()?;
        let locked = || EngineError::Locked(root.to_path_buf());
        let mut open = OPEN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if open.contains(&dir) {
            return Err(locked());
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("LOCK"))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(locked()),
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
        open.insert(dir.clone());
        Ok(Self {
            file: Some(file),
            dir,
        })
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let mut open = OPEN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Released explicitly, for the description (a forked child may share it), and closed,
        // both under the registry, before another open of the store can try (sol-loop S1).
        if let Some(file) = self.file.take()
            && let Err(error) = file.unlock()
        {
            tracing::warn!(dir = %self.dir.display(), %error, "unlocking a store failed; its lock goes with its last descriptor");
        }
        open.remove(&self.dir);
        drop(open);
        #[cfg(test)]
        tests::after_release();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    thread_local! {
        /// What a test does once a lock's drop has let the registry go.
        static AFTER_RELEASE: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    }

    pub(super) fn after_release() {
        if let Some(then) = AFTER_RELEASE.with_borrow_mut(Option::take) {
            then();
        }
    }

    /// The variable that makes [`a_second_process_locks`] act: the store it locks.
    const SECOND_LOCK: &str = "NRESE_TEST_SECOND_LOCK";

    /// Run in a second process of this test binary ([`held_against_another_process`]): takes
    /// the store's lock in [`SECOND_LOCK`] and exits 3 if it is refused `Locked` (asserted
    /// here, from its own acquire), 0 if it takes it, 4 for any other failure. In a test run
    /// (no variable) it does nothing.
    #[test]
    fn a_second_process_locks() {
        let Ok(dir) = std::env::var(SECOND_LOCK) else {
            return;
        };
        let code = match StoreLock::acquire(Path::new(&dir)) {
            Err(EngineError::Locked(_)) => 3,
            Ok(_) => 0,
            Err(error) => {
                eprintln!("the second lock: {error}");
                4
            }
        };
        std::process::exit(code);
    }

    /// Whether another process, this test binary run again ([`a_second_process_locks`]), is
    /// refused the lock of the store in `dir`.
    fn held_against_another_process(dir: &Path) -> bool {
        let code = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "durability::lock::tests::a_second_process_locks",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(SECOND_LOCK, dir)
            .status()
            .unwrap()
            .code();
        assert!(
            matches!(code, Some(0 | 3)),
            "the second process failed: {code:?}"
        );
        code == Some(3)
    }

    /// A store opened again in this process as it closes (one thread's engine dropped while
    /// another opens its directory) stays locked against other processes (sol-loop S1). The
    /// second open runs at the moment the first's drop has let the registry go. Before the
    /// fix the first's descriptor closed only after that, and closing any descriptor of the
    /// file drops the process's record locks, the second's too: another process could then
    /// lock the store and write it beside this one (Windows: the second open was refused, the
    /// first handle still locking). Guard: released, another process locks it.
    #[test]
    fn a_store_reopened_as_it_closes_stays_locked() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path();
        let first = StoreLock::acquire(dir.path()).unwrap();
        let reopened = Rc::new(RefCell::new(None));
        let (slot, root) = (Rc::clone(&reopened), dir.path().to_path_buf());
        AFTER_RELEASE.set(Some(Box::new(move || {
            *slot.borrow_mut() = Some(StoreLock::acquire(&root));
        })));
        drop(first);
        let second = reopened.borrow_mut().take().expect("the hook ran");
        let second = second.expect("opened again as the first closed");
        assert!(
            held_against_another_process(lock),
            "another process could lock the store opened again"
        );
        drop(second);
        assert!(
            !held_against_another_process(lock),
            "released, another process is still refused"
        );
    }
}
