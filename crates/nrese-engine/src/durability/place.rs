//! Durable placement: the one owner of "a name recovery reads is on disk" (DUR-3: F2, F3, F5).
//!
//! A file recovery reads (a checkpoint, a WAL segment, a `wal-cut` copy, an archived segment,
//! a restored image, store metadata) is written under a temporary name and synced, then
//! [`rename`]d into place, and its directory [`sync_dir`]ed. Once both return `Ok`, a power
//! loss finds the file under its name with all its content, on every platform:
//! - Unix: `rename`, then `fsync` of the directory.
//! - Windows: `MoveFileExW` with `MOVEFILE_WRITE_THROUGH` (it returns once the move is on
//!   disk), then `FlushFileBuffers` on the directory, opened for writing with
//!   `FILE_FLAG_BACKUP_SEMANTICS`. Before, the directory sync was a no-op there and `rename`
//!   wasn't written through, so an acknowledged bulk load could be gone after a power loss.
//!
//! An error is always returned, never taken as done: a filesystem that can't flush a
//! directory (some network shares) fails the placement rather than passing it unsynced.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

/// Moves the synced file `from` to `to`, replacing a file there, the move itself on disk when
/// this returns (Windows: written through). The directory still needs [`sync_dir`] for the
/// move to survive a power loss on Unix.
pub fn rename(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    return windows::move_file(from, to);
    #[cfg(not(windows))]
    fs::rename(from, to)
}

/// Makes what was created, renamed or removed in `dir` durable.
pub fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    return fs::File::open(dir)?.sync_all();
    #[cfg(windows)]
    return windows::flush_dir(dir);
    #[cfg(not(any(unix, windows)))]
    {
        let _ = dir;
        Err(std::io::Error::other(
            "no durable directory sync on this platform",
        ))
    }
}

/// Makes the content of the file at `path` durable, through a writable handle (Windows'
/// `FlushFileBuffers` needs one).
pub fn sync_file(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    {
        super::fault::check(&super::fault::FAIL_FILE_SYNC, "file sync")?;
        super::fault::SYNCED.with_borrow_mut(|synced| synced.push(path.to_path_buf()));
    }
    OpenOptions::new().write(true).open(path)?.sync_all()
}

/// Copies `from` to `to` durably: to a temporary name beside `to`, synced, [`rename`]d into
/// place and the directory synced. A failure leaves `to` as it was (and no temporary).
pub fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut temporary = to.as_os_str().to_owned();
    temporary.push(".placing");
    let temporary = PathBuf::from(temporary);
    let placed = fs::copy(from, &temporary)
        .and_then(|_| sync_file(&temporary))
        .and_then(|()| rename(&temporary, to));
    if let Err(error) = placed {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    sync_dir(to.parent().unwrap_or_else(|| Path::new(".")))
}

#[cfg(windows)]
mod windows {
    use std::fs::OpenOptions;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::Path;

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    pub(super) fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
        let (from, to) = (wide(from), wide(to));
        // SAFETY: both are NUL-terminated UTF-16 strings that live across the call, as
        // `MoveFileExW` requires; it keeps no pointer to them.
        let moved = unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        match moved {
            0 => Err(std::io::Error::last_os_error()),
            _ => Ok(()),
        }
    }

    pub(super) fn flush_dir(dir: &Path) -> std::io::Result<()> {
        // A directory opens as a file only with backup semantics; flushing it needs write
        // access.
        OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(dir)?
            .sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A placement replaces what was there, and the result is readable under the name; a
    /// copy whose sync fails leaves the old file under the name and no temporary.
    #[test]
    fn a_placement_replaces_or_leaves_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let (source, target) = (dir.path().join("new"), dir.path().join("placed"));
        fs::write(&source, b"new").unwrap();
        fs::write(&target, b"old").unwrap();

        super::super::fault::FAIL_FILE_SYNC.set(true);
        let failed = copy(&source, &target);
        super::super::fault::FAIL_FILE_SYNC.set(false);
        assert!(failed.is_err());
        assert_eq!(fs::read(&target).unwrap(), b"old");
        let left: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left.len(), 2, "no temporary left: {left:?}");

        copy(&source, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        let moved = dir.path().join("moved");
        rename(&source, &moved).unwrap();
        rename(&moved, &target).unwrap();
        sync_dir(dir.path()).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert!(!moved.exists() && !source.exists());
    }
}
