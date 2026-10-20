//! A store's small metadata files beside the engine's (repository settings, namespaces, a
//! removal's tombstone): each written whole or not at all. A crash or a failed write
//! leaves the earlier complete file (or none), never a truncated one.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

/// The temporary file [`write`] stages `file`'s new contents in; what a start ignores.
pub(crate) const STAGING: &str = ".tmp";

/// Writes `bytes` to `file`: into a temporary file beside it, synced, renamed over `file`,
/// and the directory synced (on Unix; Windows has no directory sync, as in the engine).
pub(crate) fn write(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
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
    if result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    result?;
    sync_dir(dir)
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(test)]
mod tests {
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
