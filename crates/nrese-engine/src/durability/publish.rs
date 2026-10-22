//! Publishing a written file under its final name, durably: the one place that knows each
//! platform's primitive. Every name a store's recovery reads (a checkpoint, a WAL segment,
//! a sidecar, a restored image or segment) is placed through [`publish`]:
//!
//! - the file's data is flushed first (`sync_all`: `fsync`, `FlushFileBuffers` on Windows);
//! - Unix: `rename`, then an `fsync` of the directory, which makes the new entry durable;
//! - Windows: `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH`, which
//!   returns only once the move is on the disk (its documentation). A directory can't be
//!   synced there, and a plain rename (`std::fs::rename`) leaves the new name in the cache.
//!
//! The guarantee is the platform's own; no power-cut test has been run on either.

use std::fs::OpenOptions;
use std::io;
use std::path::Path;

/// How [`publish`] failed.
pub(crate) enum Failed {
    /// The file is not under its new name.
    NotMoved(io::Error),
    /// It may be, and may survive a crash or not.
    Unknown(io::Error),
}

impl Failed {
    pub(crate) fn into_io(self) -> io::Error {
        match self {
            Self::NotMoved(error) | Self::Unknown(error) => error,
        }
    }
}

/// Flushes `from` and moves it to `to` (replacing it), durably once this returns `Ok`.
pub(crate) fn publish(from: &Path, to: &Path) -> Result<(), Failed> {
    #[cfg(test)]
    record::push(record::Op::Publish(to.to_path_buf()));
    OpenOptions::new()
        .write(true)
        .open(from)
        .and_then(|file| file.sync_all())
        .map_err(Failed::NotMoved)?;
    if let Err(error) = rename(from, to) {
        return Err(after_failed_rename(error, to.try_exists()));
    }
    sync_parent(to).map_err(Failed::Unknown)
}

/// Flushes `path`'s data to the disk (through a writable handle: Windows flushes no other).
pub(crate) fn sync_file(path: &Path) -> io::Result<()> {
    #[cfg(test)]
    record::push(record::Op::Sync(path.to_path_buf()));
    OpenOptions::new().write(true).open(path)?.sync_all()
}

/// The order of this thread's syncs and publications, for tests: what reaches the disk
/// before what.
#[cfg(test)]
pub(crate) mod record {
    use std::cell::RefCell;
    use std::path::PathBuf;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum Op {
        Sync(PathBuf),
        Publish(PathBuf),
    }

    thread_local! {
        static OPS: RefCell<Vec<Op>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn push(op: Op) {
        OPS.with_borrow_mut(|ops| ops.push(op));
    }

    /// This thread's operations since the last call.
    pub(crate) fn take() -> Vec<Op> {
        OPS.take()
    }
}

/// What a failed rename left, by whether its target is there (`target`): only a target
/// known to be absent means the file wasn't moved; a target there, or one whose state can't
/// be read, may be the file.
fn after_failed_rename(error: io::Error, target: io::Result<bool>) -> Failed {
    match target {
        Ok(false) => Failed::NotMoved(error),
        Ok(true) | Err(_) => Failed::Unknown(error),
    }
}

/// [`publish`] where either failure is just an error.
pub(crate) fn publish_or_fail(from: &Path, to: &Path) -> io::Result<()> {
    publish(from, to).map_err(Failed::into_io)
}

#[cfg(not(windows))]
fn rename(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(not(windows))]
fn sync_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(dir) => std::fs::File::open(dir)?.sync_all(),
        None => Ok(()),
    }
}

#[cfg(windows)]
fn rename(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let wide = |path: &Path| -> Vec<u16> { path.as_os_str().encode_wide().chain([0]).collect() };
    let (from, to) = (wide(from), wide(to));
    // SAFETY: both are NUL-terminated UTF-16 paths that outlive the call; MoveFileExW only
    // reads them.
    let moved = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    match moved != 0 {
        true => Ok(()),
        false => Err(io::Error::last_os_error()),
    }
}

/// Nothing to do: the move was written through.
#[cfg(windows)]
fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file is flushed and moved over what was at its new name; a missing one isn't moved.
    #[test]
    fn publish_moves_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("a.tmp"), dir.path().join("a"));
        std::fs::write(&to, b"old").unwrap();
        std::fs::write(&from, b"new").unwrap();
        assert!(publish(&from, &to).is_ok());
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
        assert!(!from.exists());
        assert!(matches!(publish(&from, &to), Err(Failed::NotMoved(_))));
    }

    /// A failed rename is "not moved" only when its target is known to be absent; a target
    /// there, or one that can't be looked at, leaves the outcome unknown (crash r1 F8).
    #[test]
    fn a_failed_rename_is_not_moved_only_with_its_target_absent() {
        let failed = || io::Error::other("rename failed");
        assert!(matches!(
            after_failed_rename(failed(), Ok(false)),
            Failed::NotMoved(_)
        ));
        assert!(matches!(
            after_failed_rename(failed(), Ok(true)),
            Failed::Unknown(_)
        ));
        let unreadable = Err(io::Error::from(io::ErrorKind::PermissionDenied));
        assert!(matches!(
            after_failed_rename(failed(), unreadable),
            Failed::Unknown(_)
        ));
    }

    /// Every name recovery reads is placed through [`publish`]: no other rename in the
    /// durability code, nor where a restore places an image (a plain rename isn't durable on
    /// Windows, Sol r1 R2).
    #[test]
    fn durable_names_are_placed_only_through_publish() {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut files: Vec<_> = std::fs::read_dir(crates.join("nrese-engine/src/durability"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.file_name().is_some_and(|name| name != "publish.rs"))
            .collect();
        files.push(crates.join("nrese-store/src/image_backup.rs"));
        for file in files {
            let source = std::fs::read_to_string(&file).unwrap();
            assert!(
                !source.contains("rename("),
                "{} renames other than through publish",
                file.display()
            );
        }
    }
}
