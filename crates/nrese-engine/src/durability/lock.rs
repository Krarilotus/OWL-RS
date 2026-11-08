//! The store's lock: one engine per data directory, across processes and within one (open row
//! IB1312-8ba4, master's decision IB1440-0730 (A) with (i), 7 Nov 2026).
//!
//! - **Within the process,** a registry of the open stores' directories refuses a second open
//!   before the lock file is touched.
//! - **Across processes,** on Unix a POSIX record lock (`fcntl F_SETLK`) on `<dir>/LOCK`. It
//!   belongs to the process: a child forked while a thread starts it never holds it. The `flock`
//!   this replaced belongs to the open file description, which a forked child shares until it
//!   execs, so a store closed and reopened while any thread was starting a child process could
//!   not be reopened (`Locked`) until that child ran. Closing any descriptor of a file drops the
//!   process's record locks on it; the registry makes this the only descriptor of it.
//! - **Earlier binaries** lock with `flock`, which record locks don't see. Opening takes that
//!   `flock` too, without waiting, and releases it at once: held, an earlier binary has the store
//!   open, and the open is refused. Nothing holds it afterwards, so children can't inherit it
//!   (but in that moment). An earlier binary opening the store *after* this one isn't refused by
//!   this; once this one has written a checkpoint (format 12) it refuses the store itself. Run no
//!   earlier binary on a data directory a newer one uses (docs/ops/server-setup.md).
//! - **Windows:** the handle-based lock (`LockFileEx`); std's handles aren't inherited by a
//!   child.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::{EngineError, EngineResult};

/// The directories of the stores this process has open.
static OPEN: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// The lock of an open store, released when dropped.
pub(crate) struct StoreLock {
    /// Held open for the lock (Unix: the record lock lives as long as it does); closed, when
    /// dropped, before [`OPEN`] lets another open of the store in.
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
        if !lock_process(&file)? {
            return Err(locked());
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
        // Closed first, under the registry: closing a descriptor of the file drops all this
        // process's record locks on it, so it must not close after another open of the store
        // has taken its own (sol-loop S1).
        drop(self.file.take());
        open.remove(&self.dir);
        drop(open);
        #[cfg(test)]
        tests::after_release();
    }
}

/// Takes the store's lock on `file` for this process, without waiting: `false` if another
/// process has the store. Unix: no earlier binary holds its `flock` (taken and released at
/// once), then the process's exclusive record lock.
#[cfg(unix)]
fn lock_process(file: &File) -> std::io::Result<bool> {
    // An earlier binary's lock: held, it has the store open.
    if file.try_lock().is_err() {
        return Ok(false);
    }
    let held = record_lock(file);
    file.unlock()?;
    held
}

/// The process's exclusive record lock on `file` (`fcntl F_SETLK`, the whole file), without
/// waiting: `false` if another process holds one.
#[cfg(unix)]
fn record_lock(file: &File) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: `flock` is a plain C struct, valid zeroed; the fields set are its documented
    // ones.
    let mut range: libc::flock = unsafe { std::mem::zeroed() };
    range.l_type = libc::F_WRLCK as _;
    range.l_whence = libc::SEEK_SET as _;
    range.l_start = 0;
    range.l_len = 0; // to the end, however long the file grows
    // SAFETY: the descriptor is open for writing (as `F_WRLCK` needs) for the whole call, and
    // `range` is a valid `flock` that outlives it.
    let done = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &range) };
    match done {
        0 => Ok(true),
        _ => {
            let error = std::io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EAGAIN) | Some(libc::EACCES) => Ok(false),
                _ => Err(error),
            }
        }
    }
}

/// Windows: the handle's own lock (`LockFileEx`, as earlier binaries take it), held as long
/// as the handle is; a child doesn't inherit the handle.
#[cfg(not(unix))]
fn lock_process(file: &File) -> std::io::Result<bool> {
    Ok(file.try_lock().is_ok())
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

    /// Whether another process is refused the store's lock on `lock`: a child that tries the
    /// record lock (`fcntl F_SETLK`) on it between its fork and its exec.
    #[cfg(unix)]
    fn held_against_another_process(lock: &Path) -> bool {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::process::CommandExt as _;
        let path = std::ffi::CString::new(lock.as_os_str().as_bytes()).unwrap();
        let mut command = std::process::Command::new("true");
        // SAFETY: the hook runs in the forked child before exec and makes only system calls
        // (open, fcntl, _exit) on memory made before the fork: no allocation and no lock.
        unsafe {
            command.pre_exec(move || {
                let fd = libc::open(path.as_ptr(), libc::O_RDWR);
                if fd < 0 {
                    libc::_exit(2);
                }
                let mut range: libc::flock = std::mem::zeroed();
                range.l_type = libc::F_WRLCK as _;
                range.l_whence = libc::SEEK_SET as _;
                let taken = libc::fcntl(fd, libc::F_SETLK, &range);
                libc::_exit(if taken == 0 { 0 } else { 1 });
            });
        }
        let code = command.status().unwrap().code();
        assert!(matches!(code, Some(0 | 1)), "the probe failed: {code:?}");
        code == Some(1)
    }

    /// Windows: a second handle's lock conflicts with the store's, as another process's does.
    #[cfg(not(unix))]
    fn held_against_another_process(lock: &Path) -> bool {
        let other = OpenOptions::new().write(true).open(lock).unwrap();
        other.try_lock().is_err()
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
        let lock = dir.path().join("LOCK");
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
            held_against_another_process(&lock),
            "another process could lock the store opened again"
        );
        drop(second);
        assert!(
            !held_against_another_process(&lock),
            "released, another process is still refused"
        );
    }
}
