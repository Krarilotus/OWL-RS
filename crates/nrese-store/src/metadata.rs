//! A store's small metadata files beside the engine's (repository settings, namespaces, a
//! removal's tombstone): each written whole or not at all. A crash or a failed write
//! leaves the earlier complete file (or none), never a truncated one, except where the
//! write says otherwise ([`WriteError::renamed`]): the new file is in place, but a crash may
//! still bring the earlier one back.

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

/// Writes `bytes` to `file`: into a temporary file beside it, synced, renamed over `file`,
/// and the directory synced (on Unix; Windows has no directory sync, as in the engine).
pub(crate) fn write(file: &Path, bytes: &[u8]) -> Result<(), WriteError> {
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    let mut staged = file.as_os_str().to_owned();
    staged.push(STAGING);
    let staged = std::path::PathBuf::from(staged);
    let result = (|| {
        let mut out = File::create(&staged)?;
        out.write_all(bytes)?;
        out.sync_all()?;
        drop(out);
        fs::rename(&staged, file)
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

/// Removes `file` (absent: nothing to do), the directory synced after.
pub(crate) fn remove(file: &Path) -> std::io::Result<()> {
    match fs::remove_file(file) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    sync_dir(file.parent().unwrap_or_else(|| Path::new(".")))
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if tests::FAIL_DIR_SYNC.get() {
        return Err(std::io::Error::other("directory sync failed (a test's)"));
    }
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    thread_local! {
        /// Makes this thread's directory syncs fail: a test's fault after a rename.
        pub(crate) static FAIL_DIR_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
