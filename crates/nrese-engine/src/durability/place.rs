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
    #[cfg(test)]
    super::fault::ON_DIR_SYNC.with_borrow_mut(|seen| {
        if let Some(seen) = seen {
            seen(dir);
        }
    });
    #[cfg(test)]
    if let Some(kind) = super::fault::FAIL_DIR_SYNC_OF.with_borrow(|of| {
        of.as_ref()
            .filter(|(failing, _)| failing.as_path() == dir)
            .map(|&(_, kind)| kind)
    }) {
        return Err(std::io::Error::new(
            kind,
            format!("syncing {} failed (a test's)", dir.display()),
        ));
    }
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

/// Creates the directory `dir` and every missing directory above it, each durably named (review
/// r1, R1-F2): for every level of the path, made now or found, the directory holding it is
/// synced (`.` for a relative name with no parent), as a sync makes the entries in a directory
/// durable, not its own name. A level found can't be taken as durably named: another attempt
/// whose sync failed, or another process still on its way to the sync, may have just made it
/// (sol-loop on review r1, S1).
/// - A sync that fails fails the call, and the levels this call made are removed again (they
///   are empty), so a retry makes and syncs them anew.
/// - A level is made only under a holder whose sync works, checked before it is made, so no
///   open leaves a level in sight under a holder nothing can sync.
/// - One exception: a level found whose holder this process may not open for the sync
///   (`PermissionDenied`) is left as it is. No open made it (see above): it was provisioned,
///   and refusing would close every store beneath it. A process with other rights may have
///   made it a moment ago, before its own sync: not covered.
pub fn create_dir_all(dir: &Path) -> std::io::Result<()> {
    let mut made = Vec::new();
    let named = name_levels(dir, &mut made);
    if named.is_err() {
        // Deepest first; one that isn't empty any more stays.
        for level in made.iter().rev() {
            let _ = fs::remove_dir(level);
        }
    }
    named
}

/// [`create_dir_all`]'s levels, outermost first; those it makes are pushed to `made`.
fn name_levels<'a>(dir: &'a Path, made: &mut Vec<&'a Path>) -> std::io::Result<()> {
    let levels: Vec<&Path> = dir
        .ancestors()
        .filter(|level| !level.as_os_str().is_empty())
        .collect();
    for &level in levels.iter().rev() {
        // The root (or a Windows prefix) is held by nothing.
        let Some(holder) = level.parent() else {
            continue;
        };
        let holder = match holder.as_os_str().is_empty() {
            true => Path::new("."),
            false => holder,
        };
        // Found levels aren't made again (an ancestor may be one this process can't write). A
        // level is made only once its holder's sync is known to work (sol-loop round 2 on
        // review r1, S1): a level made under a holder no sync reaches would be in sight of
        // another process, which couldn't sync it either and would take it for one provisioned
        // beforehand (the exception below). So a found level under such a holder wasn't made by
        // an open, but provisioned.
        let missing = !level.is_dir();
        if missing {
            sync_dir(holder)?;
        }
        let new = missing
            && match fs::create_dir(level) {
                Ok(()) => true,
                // Made meanwhile by someone else.
                Err(error)
                    if error.kind() == std::io::ErrorKind::AlreadyExists && level.is_dir() =>
                {
                    false
                }
                Err(error) => return Err(error),
            };
        if new {
            made.push(level);
        }
        match sync_dir(holder) {
            Ok(()) => {}
            Err(error) if !new && error.kind() == std::io::ErrorKind::PermissionDenied => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
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
/// place and the directory synced. A failure before the rename leaves `to` as it was (and no
/// temporary); one of the directory's sync after it leaves `to` replaced but not known to be
/// durable (review r1, N3).
pub fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut temporary = to.as_os_str().to_owned();
    temporary.push(".placing");
    let temporary = PathBuf::from(temporary);
    // `fs::copy` keeps the source's permissions: a read-only one (an image kept read-only)
    // is made writable for its sync, which needs a writable handle (sol-loop S2).
    let placed = fs::copy(from, &temporary)
        .and_then(|_| writable(&temporary))
        .and_then(|()| sync_file(&temporary))
        .and_then(|()| rename(&temporary, to));
    if let Err(error) = placed {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    sync_dir(to.parent().unwrap_or_else(|| Path::new(".")))
}

/// Lets the owner write the file at `path` (Unix: the owner's write bit; Windows: the
/// read-only attribute cleared), keeping its other permissions.
fn writable(path: &Path) -> std::io::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if permissions.mode() & 0o200 != 0 {
            return Ok(());
        }
        permissions.set_mode(permissions.mode() | 0o200);
    }
    #[cfg(not(unix))]
    {
        if !permissions.readonly() {
            return Ok(());
        }
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "not Unix: only the read-only attribute, no group or other bits"
        )]
        permissions.set_readonly(false);
    }
    fs::set_permissions(path, permissions)
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

    /// A read-only file (an image backup kept read-only) is copied into place and readable
    /// under the name (sol-loop S2). Before the fix the temporary kept the source's read-only
    /// permission, and its sync through a writable handle was refused, so the restore failed.
    #[test]
    fn a_read_only_file_is_placed() {
        let dir = tempfile::tempdir().unwrap();
        let (source, target) = (dir.path().join("image"), dir.path().join("placed"));
        fs::write(&source, b"image").unwrap();
        let mut read_only = fs::metadata(&source).unwrap().permissions();
        read_only.set_readonly(true);
        fs::set_permissions(&source, read_only).unwrap();
        let placed = copy(&source, &target);
        // Writable again, so the directory can be removed (Windows).
        for path in [
            source.clone(),
            target.clone(),
            dir.path().join("placed.placing"),
        ] {
            if let Ok(metadata) = fs::metadata(&path) {
                let mut writable = metadata.permissions();
                #[expect(
                    clippy::permissions_set_readonly_false,
                    reason = "a test's scratch file, made removable"
                )]
                writable.set_readonly(false);
                let _ = fs::set_permissions(&path, writable);
            }
        }
        placed.unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"image");
    }
}
