//! A store's small metadata files beside the engine's (repository settings, namespaces, a
//! removal's tombstone): each written whole or not at all. A crash or a failed write
//! leaves the earlier complete file (or none), never a truncated one, except where the
//! write says otherwise ([`WriteError::renamed`]): the new file is in place, but a crash may
//! still bring the earlier one back.
//!
//! Each write, removal and sync here first names its directory's path durably ([`name`]): a
//! store's open leaves those names to the first durable write that relies on them (DUR-3,
//! deferred-sync design note of 8 Nov), and a metadata file acknowledged under a name a power
//! loss can undo would be lost with it (fix check, DEFER-F1). Every durable write a store makes
//! under its directory outside the engine passes through here, or names its directory itself
//! with the same placement-owner operation (the quarantine's log).

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

/// The temporary file [`write`] stages `file`'s new contents in; what a start ignores.
pub(crate) const STAGING: &str = ".tmp";

/// A failed [`write`].
#[derive(Debug)]
pub(crate) struct WriteError {
    pub error: std::io::Error,
    /// The new contents are in place, but the directory sync failed: whether they survive
    /// a crash is unknown. Never "the file is as it was".
    pub renamed: bool,
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.renamed {
            true => write!(f, "{} (written, but not made durable)", self.error),
            false => self.error.fmt(f),
        }
    }
}

impl From<WriteError> for crate::StoreError {
    fn from(error: WriteError) -> Self {
        crate::StoreError::Io(std::io::Error::new(error.error.kind(), error.to_string()))
    }
}

/// Writes `bytes` to `file`: its directory's path named ([`name`]), then into a temporary file
/// beside it, synced, renamed over `file`, and the directory synced ([`nrese_engine::place`]).
/// A path that can't be named refuses the write before anything is written.
pub(crate) fn write(file: &Path, bytes: &[u8]) -> Result<(), WriteError> {
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    name(dir).map_err(|error| WriteError {
        error,
        renamed: false,
    })?;
    let mut staged = file.as_os_str().to_owned();
    staged.push(STAGING);
    let staged = std::path::PathBuf::from(staged);
    let result = (|| {
        let mut out = File::create(&staged)?;
        out.write_all(bytes)?;
        out.sync_all()?;
        drop(out);
        nrese_engine::place::rename(&staged, file)
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&staged);
        return Err(WriteError {
            error,
            renamed: false,
        });
    }
    sync_dir(dir).map_err(|error| WriteError {
        error,
        renamed: true,
    })
}

/// Removes `file`, the directory synced after: `Ok` only once its absence is durable (an
/// absent file too, whose earlier removal may not have been). An error may come after the file
/// is gone, undurably (the sync): a crash can bring it back.
pub(crate) fn remove(file: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if tests::FAIL_REMOVE.get() {
        return Err(std::io::Error::other("removal failed (a test's)"));
    }
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    name(dir)?;
    match fs::remove_file(file) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    sync_dir(dir)
}

/// Makes what was done in `dir` durable: every entry added, renamed or removed there before
/// survives a crash once this returns `Ok`, `dir`'s own path named first ([`name`]).
pub(crate) fn sync(dir: &Path) -> std::io::Result<()> {
    name(dir)?;
    sync_dir(dir)
}

/// Makes the names leading to `dir` durable: the placement owner's
/// [`nrese_engine::place::name_existing`], which syncs the directory holding each level of the
/// path (one this process may not open is left as it is) and refuses a directory that isn't
/// there. A store's open that finds its directory leaves exactly these to the first durable
/// write (the engine's own writes sync them through it); a metadata write is one, and the
/// catalogue's directory has no engine at all, so this names them itself.
fn name(dir: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if tests::FAIL_NAMING_OF.with_borrow(|failing| failing.as_deref() == Some(dir)) {
        return Err(std::io::Error::other(format!(
            "naming {} failed (a test's)",
            dir.display()
        )));
    }
    nrese_engine::place::name_existing(dir)
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if tests::FAIL_DIR_SYNC.get() {
        return Err(std::io::Error::other("directory sync failed (a test's)"));
    }
    // A real sync on every platform, Windows included (the engine's placement owner).
    nrese_engine::place::sync_dir(dir)
}

#[cfg(test)]
pub(crate) mod tests {
    thread_local! {
        /// Makes this thread's directory syncs fail: a test's fault after a rename.
        pub(crate) static FAIL_DIR_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        /// Makes this thread's removals fail: a test's held or protected file.
        pub(crate) static FAIL_REMOVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        /// Makes naming this directory's path fail, as when a directory holding one of its
        /// levels can't be synced (an I/O error other than `PermissionDenied`).
        pub(crate) static FAIL_NAMING_OF: std::cell::RefCell<Option<std::path::PathBuf>> =
            const { std::cell::RefCell::new(None) };
    }

    /// A store's metadata is acknowledged only once the names leading to its directory are
    /// durable (DUR-3 fix check, DEFER-F1). A reopen of a store that changes nothing leaves
    /// those names to the first durable write; a metadata write is one. Here naming the store's
    /// directory fails (`FAIL_NAMING_OF`), as when a directory above it can't be synced.
    /// Expected, judged apart:
    /// - the reopen and its reads work (guard: a reopen changes nothing on disk);
    /// - a namespace change is refused, not published, and leaves the store available;
    /// - the reasoning marker of a rematerialisation is refused (`InferencesNotRecorded`: the
    ///   inferences are published in memory, unrecorded);
    /// - reopened, neither the binding nor the reasoning is there;
    /// - once naming works, the same change and rematerialisation are acknowledged (a retry
    ///   after a refusal), and the binding and the reasoning are there after a reopen;
    /// - guard: a store whose names a commit already made durable takes a change.
    ///
    /// Before the fix a metadata write synced only the store's own directory: both were
    /// acknowledged while the names above it were not known durable.
    #[test]
    fn metadata_waits_for_its_store_to_be_durably_named() {
        use crate::{BulkLoadRequest, GraphTarget, StoreConfig, StoreError, StoreService};
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("store");
        let data = base.path().join("data.nt");
        std::fs::write(
            &data,
            "<http://example.com/a> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> \
             <http://example.com/C> .\n<http://example.com/C> \
             <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://example.com/D> .\n",
        )
        .unwrap();
        let load = || BulkLoadRequest {
            files: vec![data.clone()],
            replace: false,
            graph: GraphTarget::DefaultGraph,
            invalid: None,
        };
        let program = nrese_reasoner::ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::Rdfs)
            .materialised_program()
            .expect("RDFS reasons");
        let new = "https://example.test/new/";
        {
            let store = StoreService::new(StoreConfig::on_disk(&dir)).unwrap();
            store.bulk_load(&load()).unwrap();
        }
        let mut wrong = Vec::new();
        FAIL_NAMING_OF.set(Some(dir.clone()));
        let store = StoreService::new(StoreConfig::on_disk(&dir));
        let store = match store {
            Ok(store) => store,
            Err(error) => panic!("guard: the reopen failed: {error}"),
        };
        if store.namespaces().get("rdf").is_none() {
            wrong.push("guard: the reopened store can't read its namespaces".to_owned());
        }
        if store.namespaces().set("ex", new).is_ok() {
            wrong.push("a namespace change acknowledged before its store was named".to_owned());
        }
        if store.namespaces().get("ex").is_some() {
            wrong.push("a refused namespace change published".to_owned());
        }
        if let Some(why) = store.unavailable() {
            wrong.push(format!(
                "a refused namespace change made the store unavailable: {why}"
            ));
        }
        match store.rematerialise(&program) {
            Err(StoreError::InferencesNotRecorded(_)) => {}
            other => wrong.push(format!(
                "a reasoning marker placed before its store was named: {:?}",
                other.map(drop)
            )),
        }
        FAIL_NAMING_OF.set(None);
        // Reopened (the refusals' durable outcome, modelled): neither is there.
        drop(store);
        let store = StoreService::new(StoreConfig::on_disk(&dir)).unwrap();
        if store.namespaces().get("ex").is_some() {
            wrong.push("a refused namespace change is there after a reopen".to_owned());
        }
        if store.reasoning_state().is_some() {
            wrong.push("a refused reasoning marker claims its reasoning after a reopen".to_owned());
        }
        if let Err(error) = store.namespaces().set("ex", new) {
            wrong.push(format!("the retried namespace change: {error}"));
        }
        if let Err(error) = store.rematerialise(&program) {
            wrong.push(format!("the retried rematerialisation: {error}"));
        }
        drop(store);
        let store = StoreService::new(StoreConfig::on_disk(&dir)).unwrap();
        if store.namespaces().get("ex").as_deref() != Some(new) {
            wrong.push("the acknowledged namespace change is gone after a reopen".to_owned());
        }
        if store.reasoning_state().is_none() {
            wrong.push("the recorded reasoning is gone after a reopen".to_owned());
        }
        // Guard: a store whose names a durable write (a load) already made durable.
        store.bulk_load(&load()).unwrap();
        if let Err(error) = store.namespaces().set("settled", new) {
            wrong.push(format!("guard: a change on a settled store: {error}"));
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn a_failed_write_leaves_the_earlier_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        super::write(&file, b"{\"old\": 1}").unwrap();
        // The staging name taken by a directory: the write fails before the rename.
        std::fs::create_dir(dir.path().join("settings.json.tmp")).unwrap();
        assert!(super::write(&file, b"{\"new\": 2}").is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"{\"old\": 1}");
    }
}
