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

pub(crate) fn panic_in_maintenance(dir: &Path, step: &'static str) {
    let Ok(dir) = dir.canonicalize() else { return };
    let reached = MAINTENANCE_PANICS.lock().remove(&(dir, step));
    if let Some(reached) = reached {
        let _ = reached.try_send(());
        panic!("simulated failure in {step}");
    }
}
