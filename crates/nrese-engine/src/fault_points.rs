//! Fault points for tests. Cross-crate tests use the `fault-points` feature, which
//! `scripts/check-features.sh` keeps out of product builds.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Duration;

use parking_lot::Mutex;

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
#[cfg(feature = "fault-points")]
pub(crate) fn dir_sync_fails(dir: &Path) -> Option<ErrorKind> {
    FAIL_DIR_SYNC_OF.with_borrow(|failing| {
        let (failing, kind) = failing.as_ref()?;
        let dir = dir.canonicalize().ok()?;
        (&dir == failing).then_some(*kind)
    })
}

type MaintenanceSite = (PathBuf, &'static str);
static MAINTENANCE_PANICS: Mutex<BTreeMap<MaintenanceSite, SyncSender<()>>> =
    Mutex::new(BTreeMap::new());

/// A one-shot panic in a named background maintenance operation of one durable store.
pub struct MaintenancePanic {
    site: MaintenanceSite,
    reached: Receiver<()>,
}

impl MaintenancePanic {
    pub fn arm(dir: &Path, step: &'static str) -> Self {
        let site = (
            dir.canonicalize().expect("maintenance test directory"),
            step,
        );
        let (reached, wait) = mpsc::sync_channel(1);
        assert!(
            MAINTENANCE_PANICS
                .lock()
                .insert(site.clone(), reached)
                .is_none(),
            "fault already armed"
        );
        Self {
            site,
            reached: wait,
        }
    }

    pub fn wait(&self, deadline: Duration) {
        self.reached
            .recv_timeout(deadline)
            .expect("maintenance did not reach the fault point");
    }
}

impl Drop for MaintenancePanic {
    fn drop(&mut self) {
        MAINTENANCE_PANICS.lock().remove(&self.site);
    }
}

/// A held close's channels: it says it is held, waits for its release, and says how it ended.
type CloseSite = (SyncSender<()>, Receiver<()>, SyncSender<bool>);
static CLOSE_HOLDS: Mutex<BTreeMap<PathBuf, CloseSite>> = Mutex::new(BTreeMap::new());

/// How long a held close waits for its release before it goes on by itself.
const CLOSE_HOLD_LIMIT: Duration = Duration::from_secs(60);

/// A one-shot hold in the close of one durable store, before it lets its directory go: the
/// thread that closes it waits there until the hold is released or dropped (at most
/// [`CLOSE_HOLD_LIMIT`]), so a test can act while the store is closing.
pub struct CloseHold {
    dir: PathBuf,
    reached: Receiver<()>,
    release: Option<SyncSender<()>>,
    ended: Receiver<bool>,
}

impl CloseHold {
    pub fn arm(dir: &Path) -> Self {
        let dir = dir.canonicalize().expect("held store's directory");
        let (reached, wait) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(1);
        let (end, ended) = mpsc::sync_channel(1);
        assert!(
            CLOSE_HOLDS
                .lock()
                .insert(dir.clone(), (reached, released, end))
                .is_none(),
            "hold already armed"
        );
        Self {
            dir,
            reached: wait,
            release: Some(release),
            ended,
        }
    }

    /// Waits until the store's close is held.
    pub fn wait(&self, deadline: Duration) {
        self.reached
            .recv_timeout(deadline)
            .expect("the store's close did not reach the hold");
    }

    /// Lets the held close go on and waits, up to `deadline`, until it has: whether it was
    /// held until now. `false` if it went on by itself at [`CLOSE_HOLD_LIMIT`] first (what a
    /// test did while it believed the close held then proves nothing), or didn't go on in time.
    pub fn release(mut self, deadline: Duration) -> bool {
        self.release.take();
        self.ended.recv_timeout(deadline) == Ok(true)
    }
}

impl Drop for CloseHold {
    /// Lets a held close go on (its release channel closes).
    fn drop(&mut self) {
        CLOSE_HOLDS.lock().remove(&self.dir);
    }
}

/// Where a store in `dir` (canonical) begins to let its directory go.
pub(crate) fn hold_close(dir: &Path) {
    let held = CLOSE_HOLDS.lock().remove(dir);
    if let Some((reached, released, end)) = held {
        let _ = reached.try_send(());
        let by_release =
            released.recv_timeout(CLOSE_HOLD_LIMIT) != Err(mpsc::RecvTimeoutError::Timeout);
        let _ = end.try_send(by_release);
    }
}

pub(crate) fn panic_in_maintenance(dir: &Path, step: &'static str) {
    let Ok(dir) = dir.canonicalize() else { return };
    let reached = MAINTENANCE_PANICS.lock().remove(&(dir, step));
    if let Some(reached) = reached {
        let _ = reached.try_send(());
        panic!("simulated failure in {step}");
    }
}

// Counted fault points let a test stop a sequence of durable steps on its own thread.
type DirSyncFailure = (PathBuf, usize, Box<dyn FnOnce()>);
thread_local! {
    /// The directory whose next sync fails once this many more of its syncs have passed.
    static FAIL_DIR_SYNC_AFTER: RefCell<Option<DirSyncFailure>> = const { RefCell::new(None) };
    /// How many steps of a recovery's point-in-time cut pass before the next one fails.
    static STOP_IN_CUT_AFTER: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

/// Makes one sync of the directory `dir` through [`crate::place`] on this thread fail, the
/// first after `passes` of them have passed; `None` ends it. `dir` need not exist yet: both
/// sides are compared by their canonical paths at each sync.
pub fn fail_dir_sync_after(dir: Option<(&Path, usize)>) {
    FAIL_DIR_SYNC_AFTER.set(dir.map(|(dir, passes)| {
        (
            dir.to_path_buf(),
            passes,
            Box::new(|| {}) as Box<dyn FnOnce()>,
        )
    }));
}

/// Like [`fail_dir_sync_after`], running `inspect` just before the sync fails.
pub fn inspect_failed_dir_sync(dir: &Path, passes: usize, inspect: impl FnOnce() + 'static) {
    FAIL_DIR_SYNC_AFTER.set(Some((dir.to_path_buf(), passes, Box::new(inspect))));
}

/// Whether this sync of `dir` is the one [`fail_dir_sync_after`] makes fail.
#[cfg(feature = "fault-points")]
pub(crate) fn counted_dir_sync_fails(dir: &Path) -> bool {
    let inspect = FAIL_DIR_SYNC_AFTER.with_borrow_mut(|armed| {
        let (failing, passes, _) = armed.as_mut()?;
        if !matches!(
            (dir.canonicalize(), failing.canonicalize()),
            (Ok(a), Ok(b)) if a == b
        ) {
            return None;
        }
        if let Some(left) = passes.checked_sub(1) {
            *passes = left;
            return None;
        }
        armed.take().map(|(_, _, inspect)| inspect)
    });
    if let Some(inspect) = inspect {
        inspect();
        return true;
    }
    false
}

/// Makes a recovery's point-in-time cut (`DurabilityConfig::recover_until`) on this thread
/// stop with an error once `passes` of its steps have passed: step 0 is before anything is
/// cut, each later one before the removal of a segment after the cut. `None` ends it.
pub fn stop_in_cut_after(passes: Option<usize>) {
    STOP_IN_CUT_AFTER.set(passes);
}

/// One step of a recovery's point-in-time cut ([`stop_in_cut_after`]).
#[cfg(feature = "fault-points")]
pub(crate) fn cut_step() -> std::io::Result<()> {
    match STOP_IN_CUT_AFTER.get() {
        None => Ok(()),
        Some(0) => {
            STOP_IN_CUT_AFTER.set(None);
            Err(std::io::Error::other(
                "stopped in the recovery's cut (a fault point's)",
            ))
        }
        Some(left) => {
            STOP_IN_CUT_AFTER.set(Some(left - 1));
            Ok(())
        }
    }
}
