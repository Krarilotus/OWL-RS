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
            .filter(|(failing, _)| super::fault::same_dir(failing, dir))
            .map(|&(_, kind)| kind)
    }) {
        return Err(std::io::Error::new(
            kind,
            format!("syncing {} failed (a test's)", dir.display()),
        ));
    }
    #[cfg(feature = "fault-points")]
    if let Some(kind) = crate::fault_points::dir_sync_fails(dir) {
        return Err(std::io::Error::new(
            kind,
            format!("syncing {} failed (a fault point's)", dir.display()),
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

/// Creates the new directory `dir`, claiming it: an `AlreadyExists` error if it exists. Its name
/// is made durable as [`create_dir_all`] does for each level (the directories above it made or
/// found through it, its holder's sync checked before `dir` is made and done after); a failed
/// sync after removes `dir` again (sol-loop round 4 on review r1, S1: a backup's directory).
pub fn create_dir(dir: &Path) -> std::io::Result<()> {
    let holder = match dir.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            create_dir_all(parent)?;
            parent
        }
        Some(_) => Path::new("."),
        None => return Err(std::io::ErrorKind::AlreadyExists.into()),
    };
    sync_dir(holder)?;
    fs::create_dir(dir)?;
    if let Err(error) = sync_dir(holder) {
        let _ = fs::remove_dir(dir);
        return Err(error);
    }
    Ok(())
}

/// A store's root as its open resolved it ([`resolve`]): the directories [`create_dir_all`]
/// syncs for the levels it finds, the directory holding each entry the resolution reads
/// (through links, the link's own entry and its target's chain). A store's open that finds its
/// root defers these syncs to the first durable write that relies on them (`Wal::settle_names`,
/// deferred-sync design note of 8 Nov). Kept so that a directory inside the root is resolved
/// from where the root's resolution ended ([`Self::beyond`]): the root's chain is walked once,
/// and each holder canonicalised once (timing of DUR-4, 9 Nov).
pub(crate) struct Named(Resolution);

impl Named {
    /// Resolves the existing directory `dir`.
    pub(crate) fn resolve(dir: &Path) -> std::io::Result<Self> {
        Ok(Self(resolve(dir)?))
    }

    /// The holders, each with its canonical path, outermost first.
    pub(crate) fn holders(&self) -> impl Iterator<Item = (PathBuf, PathBuf)> + '_ {
        self.0
            .holders
            .iter()
            .cloned()
            .zip(self.0.keys.iter().cloned())
    }

    /// The holders, with their canonical paths, that the resolution of `name`, an existing
    /// directory in this one, reads past this one's: this directory (the holder of `name`'s
    /// entry) and, if `name` is a link, its target's chain (package review r1, R1-L1).
    pub(crate) fn beyond(
        &self,
        name: &std::ffi::OsStr,
    ) -> std::io::Result<Vec<(PathBuf, PathBuf)>> {
        let base = self.0.holders.len();
        let path = self.0.reached.join(name);
        let resolution = walk(
            self.0.clone(),
            [(Step::Name(name.to_owned()), false)].into(),
            &path,
        )?;
        if !resolution.missing.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{} is not a directory", path.display()),
            ));
        }
        let Resolution { holders, keys, .. } = resolution;
        Ok(holders.into_iter().zip(keys).skip(base).collect())
    }
}

/// Where the resolution of a path leads ([`resolve`]).
#[derive(Clone, Default)]
struct Resolution {
    /// The directories holding each entry the resolution read, physical (no link in them),
    /// outermost first, each once.
    holders: Vec<PathBuf>,
    /// Each holder's canonical path, by which they are deduplicated.
    keys: Vec<PathBuf>,
    /// The physical directory the path's existing part leads to (empty: `.`).
    reached: PathBuf,
    /// The path's names past `reached` that aren't there, in order.
    missing: Vec<std::ffi::OsString>,
    /// The links followed so far.
    links: usize,
}

/// Links one resolution follows at most, as Linux's `ELOOP`.
const MAX_LINKS: usize = 40;

/// One step of a resolution.
enum Step {
    /// A path's start: `/`, a Windows prefix and root.
    Start(PathBuf),
    Parent,
    Name(std::ffi::OsString),
}

/// `path`'s steps, each marked with whether it comes from a link's target.
fn steps(path: &Path, from_link: bool) -> Vec<(Step, bool)> {
    use std::path::Component;
    let mut start = PathBuf::new();
    let mut steps = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => start.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => steps.push((Step::Parent, from_link)),
            Component::Normal(name) => steps.push((Step::Name(name.to_owned()), from_link)),
        }
    }
    if !start.as_os_str().is_empty() {
        steps.insert(0, (Step::Start(start), from_link));
    }
    steps
}

/// Resolves `path`'s names as the kernel does when the path is used (DUR-4, fix check
/// DEFER-F1-SYM): component by component from its start (`.` for a relative path), each entry
/// read in the physical directory reached so far, which is that entry's holder. A link (a
/// symbolic link; on Windows a junction too) is followed: its own entry's holder counted, then
/// its target walked the same way (an absolute target from its start, a relative one from the
/// link's directory), so a link's name and its target's whole chain are both in the holders.
/// `..` steps to the physical parent on Unix; Windows takes `..` lexically first ([`as_walked`]).
/// Refused: a link whose target isn't there (dangling), more than [`MAX_LINKS`] links (a loop),
/// an entry that isn't a directory, a `..` past a missing name. Holders are deduplicated by
/// their canonical path (a junction's target is spelled `\\?\…`).
fn resolve(path: &Path) -> std::io::Result<Resolution> {
    walk(
        Resolution::default(),
        steps(&as_walked(path), false).into(),
        path,
    )
}

/// [`resolve`]'s walk: `queue`'s steps from where `resolution` stands (`path` names the walk in
/// errors).
fn walk(
    resolution: Resolution,
    mut queue: std::collections::VecDeque<(Step, bool)>,
    path: &Path,
) -> std::io::Result<Resolution> {
    use std::io::{Error, ErrorKind};
    let Resolution {
        mut holders,
        mut keys,
        reached: mut current,
        mut missing,
        mut links,
    } = resolution;
    while let Some((step, from_link)) = queue.pop_front() {
        if !missing.is_empty() {
            match step {
                Step::Name(name) => {
                    missing.push(name);
                    continue;
                }
                _ => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!(
                            "{}: leads back past a name that isn't there",
                            path.display()
                        ),
                    ));
                }
            }
        }
        match step {
            Step::Start(start) => current = start,
            Step::Parent => step_up(&mut current),
            Step::Name(name) => {
                let holder = match current.as_os_str().is_empty() {
                    true => PathBuf::from("."),
                    false => current.clone(),
                };
                let entry = current.join(&name);
                match fs::symlink_metadata(&entry) {
                    Ok(meta) if meta.file_type().is_symlink() => {
                        links += 1;
                        if links > MAX_LINKS {
                            return Err(Error::other(format!(
                                "{}: more than {MAX_LINKS} links",
                                path.display()
                            )));
                        }
                        let target = fs::read_link(&entry)?;
                        // Windows resolves a relative target against the link's directory, `..`
                        // taken lexically; Unix walks it from there physically. The joined
                        // target holds the link's directory already: a relative one (from a
                        // relative root) is walked from the start again (sol-loop round 2, S2).
                        #[cfg(windows)]
                        let target = as_walked(&current.join(&target));
                        #[cfg(windows)]
                        if target.is_relative() {
                            current = PathBuf::new();
                        }
                        hold(&mut holders, &mut keys, holder);
                        for step in steps(&target, true).into_iter().rev() {
                            queue.push_front(step);
                        }
                    }
                    Ok(meta) if meta.is_dir() => {
                        hold(&mut holders, &mut keys, holder);
                        current = entry;
                    }
                    Ok(_) => {
                        return Err(Error::new(
                            ErrorKind::NotADirectory,
                            format!("{} is not a directory", entry.display()),
                        ));
                    }
                    Err(error) if error.kind() == ErrorKind::NotFound && from_link => {
                        return Err(Error::new(
                            ErrorKind::NotFound,
                            format!("{}: a link's target isn't there", entry.display()),
                        ));
                    }
                    Err(error) if error.kind() == ErrorKind::NotFound => missing.push(name),
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(Resolution {
        holders,
        keys,
        reached: current,
        missing,
        links,
    })
}

/// The path the file system walks for `path`. Unix: `path` as it is (its `..` taken physically
/// as the walk reaches it). Windows: a path that isn't verbatim (`\\?\…`) has its `.` and `..`
/// taken lexically first, as Windows normalises a path before the file system sees it, so
/// `alias\..\x` is `x` whatever `alias` points to (sol-loop round 1, S1); a relative path keeps
/// its leading `..`, and a root's `..` is the root.
pub(super) fn as_walked(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let verbatim = matches!(
            path.components().next(),
            Some(Component::Prefix(prefix)) if matches!(
                prefix.kind(),
                Prefix::Verbatim(_) | Prefix::VerbatimDisk(_) | Prefix::VerbatimUNC(..)
            )
        );
        if !verbatim {
            let mut walked = PathBuf::new();
            for component in path.components() {
                match component {
                    Component::CurDir => {}
                    Component::ParentDir => step_up(&mut walked),
                    other => walked.push(other.as_os_str()),
                }
            }
            return walked;
        }
    }
    path.to_path_buf()
}

/// `path`'s parent, in place: its last name dropped; a root (`/`, `C:\`, `\\server\share\`) its
/// own parent; and where neither is there to drop (an empty or relative start, a drive-relative
/// `C:`, a `..` already) a `..` kept, which the file system resolves from the working directory
/// (sol-loop round 2, S3: a drive-relative prefix doesn't absorb a `..` as a root does).
fn step_up(path: &mut PathBuf) {
    use std::path::Component;
    match path.components().next_back() {
        Some(Component::Normal(_)) => {
            path.pop();
        }
        Some(Component::RootDir) => {}
        _ => path.push(".."),
    }
}

/// Adds `holder` to `holders` unless it's there already under some spelling.
fn hold(holders: &mut Vec<PathBuf>, seen: &mut Vec<PathBuf>, holder: PathBuf) {
    let key = holder.canonicalize().unwrap_or_else(|_| holder.clone());
    if !seen.contains(&key) {
        seen.push(key);
        holders.push(holder);
    }
}

/// Makes the names leading to the existing directory `dir` durable, as [`create_dir_all`] does
/// for the levels it finds, and makes none: the holder of each entry its resolution reads
/// synced ([`resolve`]: through links, the link's own entry and its target's chain), outermost
/// first (a holder this process may not open for the sync, `PermissionDenied`, is left as it
/// is). `NotFound` if `dir` isn't there (or a link on its way is dangling): a write into a
/// directory gone meanwhile must not make it again. For writes under a store's directory that
/// don't go through the engine (the store's metadata, DUR-3 fix check DEFER-F1): its open
/// leaves these names to the first durable write.
pub fn name_existing(dir: &Path) -> std::io::Result<()> {
    let resolution = resolve(dir)?;
    if !resolution.missing.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{} is not a directory", dir.display()),
        ));
    }
    for holder in resolution.holders {
        match sync_dir(&holder) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Names `dir`, a directory in the store's root, and returns whether it made it, so that a
/// reopen of a store at an absolute path syncs each directory once (timing of 8 Nov; a
/// relative or `.` root still syncs `.` twice: fix check ONCE-N1):
/// - missing, it is made as [`create_dir_all`] makes a level: the root's sync checked before,
///   done after, `dir` removed again if that fails;
/// - found (or made meanwhile by someone else), the root isn't synced here: its sync after
///   replay, at the open or before the first durable write (deferred-sync design note), names
///   every directory found in it at once.
pub(crate) fn create_store_dir(dir: &Path) -> std::io::Result<bool> {
    if dir.is_dir() {
        return Ok(false);
    }
    let root = match dir.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    sync_dir(root)?;
    match fs::create_dir(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => {
            return Ok(false);
        }
        Err(error) => return Err(error),
    }
    if let Err(error) = sync_dir(root) {
        let _ = fs::remove_dir(dir);
        return Err(error);
    }
    Ok(true)
}

/// [`create_dir_all`]'s levels, outermost first; those it makes are pushed to `made`. The
/// levels found are those [`resolve`] reads (through links, the link's own entry and its
/// target's chain), their holders synced; the missing ones are made in the physical directory
/// the resolution reached, never through a dangling link.
fn name_levels(dir: &Path, made: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let Resolution {
        holders,
        reached,
        missing,
        ..
    } = resolve(dir)?;
    // Found levels aren't made again (an ancestor may be one this process can't write); a
    // holder of one this process may not open for the sync was provisioned (the exception).
    for holder in &holders {
        match sync_dir(holder) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
            Err(error) => return Err(error),
        }
    }
    let mut at = reached;
    for name in missing {
        let holder = match at.as_os_str().is_empty() {
            true => PathBuf::from("."),
            false => at.clone(),
        };
        let level = at.join(&name);
        // A level is made only once its holder's sync is known to work (sol-loop round 2 on
        // review r1, S1): a level made under a holder no sync reaches would be in sight of
        // another process, which couldn't sync it either and would take it for one provisioned
        // beforehand (the exception above).
        sync_dir(&holder)?;
        let new = match fs::create_dir(&level) {
            Ok(()) => true,
            // Made meanwhile by someone else.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && level.is_dir() => {
                false
            }
            Err(error) => return Err(error),
        };
        if new {
            made.push(level.clone());
        }
        match sync_dir(&holder) {
            Ok(()) => {}
            Err(error) if !new && error.kind() == std::io::ErrorKind::PermissionDenied => {}
            Err(error) => return Err(error),
        }
        at = level;
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
