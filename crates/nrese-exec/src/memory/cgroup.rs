//! The memory limits a Linux process runs under, from its cgroup membership: cgroup v2 (a
//! `0::/path` line) reads `memory.max` in the process's group and every ancestor's, v1 (a
//! line naming the `memory` controller) `memory.limit_in_bytes`; the smallest counts.
//!
//! The kernel resolves every path, and each answer is verified; nothing here interprets a path
//! (SYN-45 review r1 R1-C1a, r2: three rounds of modelling the kernel's walk each left a corner
//! open). `/proc/self/mountinfo` only lists the views of the hierarchy: each mount's id, the
//! group it shows at its point, and where. A view of a group opens its point from the root and
//! then the group's names below the view's group, one name at a time (never `..`, never a path
//! with a `/` in it, no symbolic link followed, needing only search permission on the way), and
//! the handles from the point on must carry the view's mount id, so a mount over the point,
//! over part of the way or over a single file is seen as another mount and refused. Reached so,
//! within one mount of the hierarchy, the directory is that group, an ancestor too. A way the
//! kernel refuses or fails to open leaves the limit unknown. The process's own group must list
//! the process (`getpid`, the pid namespace `cgroup.procs` lists ids in) in its `cgroup.procs`,
//! or in a threaded v2 group its `cgroup.threads`. Each value file is read through its
//! verified directory, on the same mount. Mount ids come from `/proc/self/fdinfo` (Linux 3.15);
//! a kernel that doesn't show them leaves every limit unknown.
//!
//! An ancestor no view reaches, above every mount's root or hidden by a mount, is invisible to
//! the process as one above its cgroup namespace is, and the walk stops there (the
//! orchestrator's decision, 3 and 8 Nov, P2-A4 IB2001-2a56). On v1 the own group's
//! `hierarchical_memory_limit` (`memory.stat`) still includes them; v2 has no such file, so a
//! process under a limit above what it sees sets its memory limit by hand.
//!
//! "max", v1's page-aligned maximum and a v2 group without the controller (no `memory.max`)
//! mean no limit; 0 is a limit of nothing. A limit that may apply but can't be read is
//! [`Unknown`], never no limit (A-902; the owner: an OS reading that couldn't be determined is
//! never "no limit").

use std::io::{self, ErrorKind};
use std::rc::Rc;

/// A limit that may apply wasn't read, so what the process may use, or has left, isn't known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Unknown;

/// Above this a limit means "none".
const NONE_ABOVE: u64 = 1 << 62;

/// The errno `cgroup.procs` answers in a threaded v2 group (EOPNOTSUPP), as the target numbers it:
/// 95 on most Linux ABIs, 122 on MIPS (a hard-coded 95 read such a group as unknown there).
#[cfg(unix)]
const EOPNOTSUPP: i32 = libc::EOPNOTSUPP;
/// Without libc only the tests' declared kernel answers it.
#[cfg(not(unix))]
const EOPNOTSUPP: i32 = 95;

/// What the kernel answers about the files the process reaches: the one place paths are
/// resolved. [`Kernel`] asks the kernel; tests declare its answers.
pub(super) trait Fs {
    /// An open file or directory.
    type Handle;
    /// A whole file's text, by its absolute path (`/proc/self/cgroup`, `/proc/meminfo`).
    fn text(&self, path: &str) -> io::Result<String>;
    /// Opens the process's root directory, where every mount point is (proc_pid_mountinfo(5)).
    fn root(&self) -> io::Result<Self::Handle>;
    /// Opens the entry `name` (one name, not `.` or `..`) of the directory `dir`, not following
    /// a symbolic link.
    fn open_at(&self, dir: &Self::Handle, name: &str) -> io::Result<Self::Handle>;
    /// The id of the mount a handle is on, the one `/proc/self/mountinfo` lists.
    fn mount(&self, handle: &Self::Handle) -> io::Result<u64>;
    /// A handle's whole text.
    fn read(&self, handle: &Self::Handle) -> io::Result<String>;
}

/// Where a cgroup version keeps a group's memory limit, its use, and its statistics, and the
/// statistic that counts its inactive file pages.
struct Files {
    v1: bool,
    limit: &'static str,
    usage: &'static str,
    inactive: &'static str,
}

const V2: Files = Files {
    v1: false,
    limit: "memory.max",
    usage: "memory.current",
    inactive: "inactive_file",
};

const V1: Files = Files {
    v1: true,
    limit: "memory.limit_in_bytes",
    usage: "memory.usage_in_bytes",
    inactive: "total_inactive_file",
};

/// A group's path in its hierarchy as the process's cgroup namespace shows it: its names from
/// the namespace's root, `..` for a step above it (`/../worker`).
type Path = Vec<String>;

/// A path's names; empty for the namespace's root.
fn names(path: &str) -> Path {
    path.split('/')
        .filter(|name| !name.is_empty() && *name != ".")
        .map(str::to_owned)
        .collect()
}

/// The group above `group`: one name fewer, or one step more above the namespace's root.
fn parent(group: &Path) -> Path {
    let mut up = group.clone();
    match group.last().map(String::as_str) {
        Some(name) if name != ".." => {
            up.pop();
        }
        _ => up.push("..".to_owned()),
    }
    up
}

/// The process's memory group: its hierarchy, and its path in it. `None` where no hierarchy has
/// the memory controller (nothing limits the process but the machine). The v1 line naming
/// `memory` if there is one (on a hybrid system the controller is there), else the v2 line.
fn group(text: &str) -> Option<(&'static Files, Path)> {
    let (mut v1, mut v2) = (None, None);
    for line in text.lines() {
        let mut fields = line.splitn(3, ':');
        let (Some(id), Some(controllers), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if controllers.split(',').any(|c| c == "memory") {
            v1 = Some(path);
        } else if id == "0" && controllers.is_empty() {
            v2 = Some(path);
        }
    }
    match (v1, v2) {
        (Some(path), _) => Some((&V1, names(path.trim()))),
        (None, Some(path)) => Some((&V2, names(path.trim()))),
        (None, None) => None,
    }
}

/// A mount of the process's memory hierarchy (proc_pid_mountinfo(5)): its id, the group it
/// shows at its point, and its point.
struct View {
    id: u64,
    root: Path,
    point: String,
}

/// The mounts of `files`'s hierarchy, from mountinfo's text: `cgroup2` for v2, a `cgroup`
/// naming `memory` in its super options for v1 (the fields after the optional ones' `-` are the
/// type, the source and the super options). `None` where a line makes no sense.
fn views(text: &str, files: &Files) -> Option<Vec<View>> {
    let mut views = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let (before, after) = line.split_once(" - ")?;
        let mut fields = before.split(' ');
        let id: u64 = fields.next()?.parse().ok()?;
        let (root, point) = (fields.nth(2)?, fields.next()?);
        let mut after = after.split(' ');
        let (kind, options) = (after.next()?, after.nth(1).unwrap_or(""));
        let ours = if files.v1 {
            kind == "cgroup" && options.split(',').any(|option| option == "memory")
        } else {
            kind == "cgroup2"
        };
        if ours {
            views.push(View {
                id,
                root: names(&unescape(root)),
                point: unescape(point),
            });
        }
    }
    Some(views)
}

/// A mountinfo field as the path it names: proc(5) writes space, tab, newline and backslash
/// as three octal digits after a backslash (`\040`).
fn unescape(field: &str) -> String {
    let mut out = Vec::with_capacity(field.len());
    let mut rest = field.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        let code = (byte == b'\\')
            .then(|| tail.get(..3))
            .flatten()
            .and_then(|digits| u8::from_str_radix(std::str::from_utf8(digits).ok()?, 8).ok());
        match code {
            Some(code) => {
                out.push(code);
                rest = &tail[3..];
            }
            None => {
                out.push(byte);
                rest = tail;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A group's directory as the process reached it through one view: the handle, and its mount.
struct Place<H> {
    dir: Rc<H>,
    mount: u64,
}

/// One view's way down to the process's group, walked once: the directories reached on the
/// view's own mount, `dirs[i]` the group `i` names below the view's group (`dirs[0]` the view's
/// point). It ends where the way left the mount or a name wasn't there. Each ancestor the view
/// reaches is a directory on this way, so the ancestors are read from it, not walked again
/// (each walk from the root made the reading's opens grow with the square of the depth).
struct Trail<H> {
    view: usize,
    dirs: Vec<Rc<H>>,
}

/// What resolution needs: the kernel, the hierarchy's files and its views.
struct Walk<'a, F: Fs> {
    fs: &'a F,
    files: &'static Files,
    views: Vec<View>,
}

impl<F: Fs> Walk<'_, F> {
    /// The mount a handle is on. `Unknown` where the kernel doesn't say (before Linux 3.15).
    fn mount(&self, handle: &F::Handle) -> Result<u64, Unknown> {
        self.fs.mount(handle).map_err(|_| Unknown)
    }

    /// A mount point's directory, opened from the root one name at a time, no symbolic link
    /// followed: the point mountinfo shows is the mount's own path, which has none.
    fn point(&self, point: &str) -> io::Result<F::Handle> {
        let mut dir = self.fs.root()?;
        for name in names(point) {
            dir = self.fs.open_at(&dir, &name)?;
        }
        Ok(dir)
    }

    /// Every directory of `group` the views reach: through each view whose group holds it
    /// without a step above (its names below the view's), its point opened and then each name,
    /// every handle on the view's own mount. A view the kernel answers otherwise doesn't reach
    /// it (another mount over the point or the way, or a name not there). A way the kernel
    /// refuses or fails to open is a view that may show the group and wasn't read: `Unknown`
    /// (sol-loop r7 S2).
    fn places(&self, group: &Path) -> Result<Vec<Place<F::Handle>>, Unknown> {
        let mut found = Vec::new();
        for view in &self.views {
            let Some(below) = group.strip_prefix(view.root.as_slice()) else {
                continue;
            };
            if below.iter().any(|name| name == "..") {
                continue;
            }
            let mut dir = match self.point(&view.point) {
                Ok(dir) => dir,
                Err(error) if absent(&error) => continue,
                Err(_) => return Err(Unknown),
            };
            let mut reached = self.mount(&dir)? == view.id;
            for name in below {
                if !reached {
                    break;
                }
                match self.fs.open_at(&dir, name) {
                    Ok(next) => {
                        reached = self.mount(&next)? == view.id;
                        dir = next;
                    }
                    Err(error) if absent(&error) => reached = false,
                    Err(_) => return Err(Unknown),
                }
            }
            if reached {
                found.push(Place {
                    dir: Rc::new(dir),
                    mount: view.id,
                });
            }
        }
        Ok(found)
    }

    /// Each view's way to `group` ([`Trail`]), in the views' order: the opens and checks of
    /// [`Self::places`] for `group`, every directory on the way kept. A way the kernel refuses
    /// or fails to open is `Unknown`, as there. The kept directories stay open until the reading
    /// ends, one descriptor for each name on each view's way: a process
    /// short of descriptors gets `EMFILE`, and its limit is unknown, not absent.
    fn trails(&self, group: &Path) -> Result<Vec<Trail<F::Handle>>, Unknown> {
        let mut trails = Vec::new();
        for (at, view) in self.views.iter().enumerate() {
            let Some(below) = group.strip_prefix(view.root.as_slice()) else {
                continue;
            };
            if below.iter().any(|name| name == "..") {
                continue;
            }
            let point = match self.point(&view.point) {
                Ok(dir) => dir,
                Err(error) if absent(&error) => continue,
                Err(_) => return Err(Unknown),
            };
            let mut dirs = Vec::new();
            if self.mount(&point)? == view.id {
                dirs.push(Rc::new(point));
            }
            for name in below {
                let Some(dir) = dirs.last() else {
                    break;
                };
                match self.fs.open_at(dir, name) {
                    Ok(next) if self.mount(&next)? == view.id => dirs.push(Rc::new(next)),
                    Ok(_) => break,
                    Err(error) if absent(&error) => break,
                    Err(_) => return Err(Unknown),
                }
            }
            trails.push(Trail { view: at, dirs });
        }
        Ok(trails)
    }

    /// The first view's directory of `up`, a group on the ways `trails` walked (`up` a prefix
    /// of their group): what [`Self::places`] would find first for it, without walking again.
    fn on_trails(&self, trails: &[Trail<F::Handle>], up: &Path) -> Option<Place<F::Handle>> {
        trails.iter().find_map(|trail| {
            let view = &self.views[trail.view];
            let below = up.strip_prefix(view.root.as_slice())?;
            if below.iter().any(|name| name == "..") {
                return None;
            }
            let dir = trail.dirs.get(below.len())?;
            Some(Place {
                dir: Rc::clone(dir),
                mount: view.id,
            })
        })
    }

    /// The group's file `name` in `place`, opened on the place's own mount: a file bound over
    /// it (mount(2) binds single files too) is another group's: `Unknown`. A file that isn't
    /// there is the kernel's error, as it is.
    fn file(&self, place: &Place<F::Handle>, name: &str) -> Result<io::Result<String>, Unknown> {
        let file = match self.fs.open_at(&place.dir, name) {
            Ok(file) => file,
            Err(error) => return Ok(Err(error)),
        };
        if self.mount(&file)? != place.mount {
            return Err(Unknown);
        }
        Ok(self.fs.read(&file))
    }

    /// Whether `place` is the process's own group: its `cgroup.procs` lists the process's id
    /// (`getpid`: the pid namespace the process runs in, the one `cgroup.procs` lists its
    /// reader's ids in; cgroup.c, cgroup_procs_show), or, in a threaded v2 group, whose
    /// `cgroup.procs` the kernel refuses, its `cgroup.threads` lists the thread group's leader,
    /// whose thread id is that id. And in v2 it has `cgroup.controllers`, which only v2 groups
    /// have. A process is in one group of a hierarchy.
    fn own(&self, place: &Place<F::Handle>) -> Result<bool, Unknown> {
        let pid = u64::from(std::process::id());
        let lists = |text: String| text.lines().any(|line| line.trim().parse() == Ok(pid));
        let procs = match self.file(place, "cgroup.procs") {
            Ok(Err(error)) if error.raw_os_error() == Some(EOPNOTSUPP) => {
                self.file(place, "cgroup.threads")
            }
            other => other,
        };
        let listed = match procs {
            Ok(Ok(text)) => lists(text),
            Ok(Err(_)) | Err(Unknown) => false,
        };
        Ok(
            listed
                && (self.files.v1 || matches!(self.file(place, "cgroup.controllers"), Ok(Ok(_)))),
        )
    }
}

/// An open error that says the name isn't there (or a part of the way isn't a directory): a
/// view that doesn't reach the group. Any other is the kernel refusing or failing.
fn absent(error: &io::Error) -> bool {
    matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
}

/// `each` of the process's memory group and its visible ancestors that has a limit, innermost
/// first, with the place its files are read in. `Unknown`: a limit that may apply wasn't read,
/// or `each` couldn't read what it needs.
fn limited<F: Fs>(
    fs: &F,
    mut each: impl FnMut(&Walk<'_, F>, &Place<F::Handle>, u64) -> Result<(), Unknown>,
) -> Result<(), Unknown> {
    let text = fs.text("/proc/self/cgroup").map_err(|_| Unknown)?;
    let Some((files, mut group)) = group(&text) else {
        return Ok(());
    };
    let mounts = fs.text("/proc/self/mountinfo").map_err(|_| Unknown)?;
    let walk = Walk {
        fs,
        files,
        views: views(&mounts, files).ok_or(Unknown)?,
    };
    // The ways down to the group, walked once: the group and its ancestors are read from them.
    let trails = walk.trails(&group)?;
    let own = group.clone();
    let mut place = None;
    for trail in &trails {
        let below = own.len() - walk.views[trail.view].root.len();
        let Some(dir) = trail.dirs.get(below) else {
            continue;
        };
        let candidate = Place {
            dir: Rc::clone(dir),
            mount: walk.views[trail.view].id,
        };
        if walk.own(&candidate)? {
            place = Some(candidate);
            break;
        }
    }
    let mut place = place.ok_or(Unknown)?;
    if files.v1 {
        // The kernel's own least limit over the whole hierarchy, groups above a namespace
        // included, which the walk below can't see (since Linux 2.6.29).
        if let Some(limit) = hierarchical(&walk, &place)?.filter(|&limit| limit < NONE_ABOVE) {
            each(&walk, &place, limit)?;
        }
    }
    // Every ancestor of a hierarchy is finitely far up; a bound all the same.
    for _ in 0..4096 {
        match walk.file(&place, files.limit)? {
            Ok(text) => {
                let text = text.trim();
                if text != "max" {
                    let limit: u64 = text.parse().map_err(|_| Unknown)?;
                    if limit < NONE_ABOVE {
                        each(&walk, &place, limit)?;
                    }
                }
            }
            // A v2 group without the memory controller, the root among them, has no limit
            // file; every v1 group has one.
            Err(error) if error.kind() == ErrorKind::NotFound && !files.v1 => {}
            Err(_) => return Err(Unknown),
        }
        // The parent, reached on a view's own mount by its names below the view's group, is
        // that group: within one mount of the hierarchy a name is the group's child. Whatever
        // is bound over the child's entry there (the child itself, sol-loop r7 S1, or another
        // group) doesn't change which group the parent is.
        let up = parent(&group);
        // None reaches it (above every view's group, or hidden by a mount): not visible to
        // the process, as above a namespace, so nothing above it is read (option 1). A group
        // on the ways down is read from them; one past their top (a step above the namespace's
        // root, `..`) is walked to as before.
        let found = if own.starts_with(&up) {
            walk.on_trails(&trails, &up)
        } else {
            walk.places(&up)?.into_iter().next()
        };
        let Some(found) = found else {
            return Ok(());
        };
        place = found;
        group = up;
    }
    Err(Unknown)
}

/// A v1 group's `hierarchical_memory_limit` (`memory.stat`): the least limit of the group and
/// every ancestor. `None` where the kernel doesn't show it; `Unknown` where the statistics
/// can't be read or the value makes no sense.
fn hierarchical<F: Fs>(
    walk: &Walk<'_, F>,
    place: &Place<F::Handle>,
) -> Result<Option<u64>, Unknown> {
    let stat = match walk.file(place, "memory.stat")? {
        Ok(stat) => stat,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Unknown),
    };
    let Some(value) = stat.lines().find_map(|line| {
        let (name, value) = line.split_once(' ')?;
        (name == "hierarchical_memory_limit").then_some(value)
    }) else {
        return Ok(None);
    };
    value.trim().parse().map(Some).map_err(|_| Unknown)
}

/// The smallest limit of the process's groups; `None`: no group has one.
pub(super) fn limit(fs: &impl Fs) -> Result<Option<u64>, Unknown> {
    let mut smallest = None;
    limited(fs, |_, _, limit| {
        smallest = super::least([smallest, Some(limit)]);
        Ok(())
    })?;
    Ok(smallest)
}

/// What the process's groups leave of their limits: the least of each limit less the group's
/// use, its inactive file pages not counted. Those are page cache the kernel reclaims before
/// it refuses memory, the store's mapped files among them (counted, a store mapping its
/// checkpoint would leave nothing); its active ones count. Without the statistics none is
/// taken off, which leaves less, never more. `None`: no group has a limit.
pub(super) fn left(fs: &impl Fs) -> Result<Option<u64>, Unknown> {
    let mut least = None;
    limited(fs, |walk, place, limit| {
        let number = |text: &str| text.trim().parse::<u64>().ok();
        let usage = walk
            .file(place, walk.files.usage)?
            .ok()
            .and_then(|text| number(&text))
            .ok_or(Unknown)?;
        let inactive = walk
            .file(place, "memory.stat")?
            .ok()
            .and_then(|stat| {
                stat.lines().find_map(|line| {
                    let (name, value) = line.split_once(' ')?;
                    (name == walk.files.inactive)
                        .then(|| number(value))
                        .flatten()
                })
            })
            .unwrap_or(0);
        let used = usage.saturating_sub(inactive);
        least = super::least([least, Some(limit.saturating_sub(used))]);
        Ok(())
    })?;
    Ok(least)
}

/// The kernel's own answers: every handle a path-only descriptor (`O_PATH`, which needs search
/// permission on the way and none on the entry, so a directory the process may pass but not
/// list is passed, sol-loop r7 S2), opened one name at a time by `openat` on the directory's
/// descriptor, no symbolic link followed (the name alone is resolved, in the directory the
/// descriptor holds, which no rename or mount elsewhere changes); mount ids from `statx`'s
/// `STATX_MNT_ID` (Linux 5.8), else `/proc/self/fdinfo/N` ([`Kernel::mount_in_fdinfo`]); a file
/// read by opening its descriptor's own link, the same file on the same mount, no path walked
/// again.
#[cfg(target_os = "linux")]
pub(super) struct Kernel;

#[cfg(target_os = "linux")]
impl Kernel {
    fn path_only(path: &str) -> io::Result<std::fs::File> {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW)
            .open(path)
    }

    /// The mount id `statx` gives for a descriptor, where the kernel fills it in (`STATX_MNT_ID`
    /// in the answer's mask: Linux 5.8; the same ids `/proc/self/mountinfo` lists). `None`
    /// before 5.8 (the bit left out of the mask) or where the call fails: `fdinfo` is asked.
    #[cfg(target_env = "gnu")]
    pub(super) fn mount_in_statx(handle: &std::fs::File) -> Option<u64> {
        use std::os::fd::AsRawFd;
        let mut answer = std::mem::MaybeUninit::<libc::statx>::zeroed();
        // SAFETY: an empty path with `AT_EMPTY_PATH` asks about the open descriptor itself,
        // whatever it is (an `O_PATH` one included); `answer` is a `statx` the call writes,
        // zeroed so every field is initialised whatever the kernel fills in.
        let done = unsafe {
            libc::statx(
                handle.as_raw_fd(),
                c"".as_ptr(),
                libc::AT_EMPTY_PATH,
                libc::STATX_MNT_ID,
                answer.as_mut_ptr(),
            )
        };
        if done != 0 {
            return None;
        }
        // SAFETY: zeroed, then written by the kernel: every field holds a value.
        let answer = unsafe { answer.assume_init() };
        mount_filled(answer.stx_mask, answer.stx_mnt_id)
    }

    /// The mount id `/proc/self/fdinfo/N` shows (Linux 3.15): the reading before `statx` had
    /// one, and where it doesn't fill it in.
    pub(super) fn mount_in_fdinfo(handle: &std::fs::File) -> io::Result<u64> {
        use std::os::fd::AsRawFd;
        let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", handle.as_raw_fd()))?;
        info.lines()
            .find_map(|line| line.strip_prefix("mnt_id:"))
            .and_then(|id| id.trim().parse().ok())
            .ok_or_else(|| ErrorKind::Unsupported.into())
    }

    /// The mount id: `statx`'s where it gave one, else `fdinfo`'s.
    pub(super) fn mount_from(handle: &std::fs::File, statx: Option<u64>) -> io::Result<u64> {
        match statx {
            Some(id) => Ok(id),
            None => Self::mount_in_fdinfo(handle),
        }
    }
}

/// A `statx` answer's mount id, if its mask says the kernel filled it in.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(super) fn mount_filled(mask: u32, id: u64) -> Option<u64> {
    (mask & libc::STATX_MNT_ID != 0).then_some(id)
}

#[cfg(target_os = "linux")]
impl Fs for Kernel {
    type Handle = std::fs::File;

    fn text(&self, path: &str) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn root(&self) -> io::Result<std::fs::File> {
        Self::path_only("/")
    }

    fn open_at(&self, dir: &std::fs::File, name: &str) -> io::Result<std::fs::File> {
        use std::os::fd::{AsRawFd, FromRawFd};
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(ErrorKind::InvalidInput.into());
        }
        let name = std::ffi::CString::new(name).map_err(|_| ErrorKind::InvalidInput)?;
        // SAFETY: `dir` is an open descriptor and `name` a NUL-terminated single name; the
        // call makes a new descriptor or none.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is the new descriptor the call made, owned by nothing else.
        Ok(unsafe { std::fs::File::from_raw_fd(fd) })
    }

    fn mount(&self, handle: &std::fs::File) -> io::Result<u64> {
        #[cfg(target_env = "gnu")]
        let statx = Self::mount_in_statx(handle);
        #[cfg(not(target_env = "gnu"))]
        let statx = None;
        Self::mount_from(handle, statx)
    }

    fn read(&self, handle: &std::fs::File) -> io::Result<String> {
        use std::os::fd::AsRawFd;
        std::fs::read_to_string(format!("/proc/self/fd/{}", handle.as_raw_fd()))
    }
}

/// The kernel's answers declared for a test: the files a process sees, by absolute path, and
/// its mount table. Each path is resolved as the kernel does for a path with no `..` (the walk
/// tests of earlier rounds verified for those): from the root mount, into a mount stacked at a
/// mount's point, else into the child at the shortest point on the way. A handle's mount is the
/// mount it was reached through. A path the rows deny is one the kernel refuses to open, a
/// directory on the way to it included.
#[cfg(test)]
pub(super) mod declared {
    use super::*;
    use std::collections::HashMap;

    /// A mount of the declared table: any file system.
    struct Mount {
        id: u64,
        parent: u64,
        point: String,
    }

    /// A handle: the absolute path it was opened at.
    #[derive(Debug, Clone)]
    pub struct At(String);

    /// The declared kernel.
    pub struct Declared {
        files: HashMap<String, String>,
        denied: String,
        mounts: Vec<Mount>,
        /// The kernel shows no mount ids (before Linux 3.15).
        pub no_mount_ids: bool,
        /// How many names were opened (`open_at`), refused ones included.
        pub opens: std::cell::Cell<usize>,
    }

    impl Declared {
        /// `files` by absolute path (`/proc/self/mountinfo` among them), `denied` (if not
        /// empty) refused. A file whose text is `EOPNOTSUPP` answers that error. A path with a
        /// `.` or `..` in it is a spelling a reader of paths asked the kernel for (the rows keep
        /// what the kernel answered it, for the reader before SYN-45 r2); no directory holds
        /// such an entry, so it isn't a file here.
        pub fn new(mut files: HashMap<String, String>, denied: &str) -> Self {
            files.retain(|path, _| !path.split('/').any(|name| name == "." || name == ".."));
            let mounts = files
                .get("/proc/self/mountinfo")
                .map(|text| {
                    text.lines()
                        .filter_map(|line| {
                            let (before, _) = line.split_once(" - ")?;
                            let mut fields = before.split(' ');
                            let id = fields.next()?.parse().ok()?;
                            let parent = fields.next()?.parse().ok()?;
                            let point = fields.nth(2)?;
                            Some(Mount {
                                id,
                                parent,
                                point: unescape(point),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Self {
                files,
                denied: denied.to_owned(),
                mounts,
                no_mount_ids: false,
                opens: std::cell::Cell::new(0),
            }
        }

        /// The mount `path` resolves through, and the rest below its point.
        fn resolve(&self, path: &str) -> Option<(&Mount, String)> {
            let listed = |id: u64| self.mounts.iter().any(|m| m.id == id);
            let mut tops: Vec<&Mount> = self
                .mounts
                .iter()
                .filter(|m| m.parent == m.id || !listed(m.parent))
                .filter(|m| below(path, &m.point).is_some())
                .collect();
            tops.sort_by_key(|m| m.point.trim_end_matches('/').len());
            let mut at = *tops.first()?;
            for _ in 0..=self.mounts.len() {
                let children = || {
                    self.mounts
                        .iter()
                        .filter(|m| m.parent == at.id && m.id != at.id)
                };
                if let Some(on) = children().find(|m| m.point == at.point) {
                    at = on;
                    continue;
                }
                let mut next: Vec<&Mount> = children()
                    .filter(|m| below(path, &m.point).is_some())
                    .collect();
                next.sort_by_key(|m| m.point.trim_end_matches('/').len());
                match next.first() {
                    Some(first) => at = first,
                    None => return Some((at, below(path, &at.point)?.to_owned())),
                }
            }
            None
        }

        fn check(&self, path: &str) -> io::Result<()> {
            if !self.denied.is_empty() && path == self.denied {
                return Err(ErrorKind::PermissionDenied.into());
            }
            Ok(())
        }
    }

    /// `path` below `point`: the rest, "" or from a `/`.
    fn below<'a>(path: &'a str, point: &str) -> Option<&'a str> {
        let rest = path.strip_prefix(point.trim_end_matches('/'))?;
        (rest.is_empty() || rest.starts_with('/')).then_some(rest)
    }

    /// Whether `path` is a directory: a declared file is below it, or a mount is at it.
    fn is_dir(files: &HashMap<String, String>, mounts: &[Mount], path: &str) -> bool {
        mounts.iter().any(|m| m.point == path)
            || files
                .keys()
                .any(|file| below(file, path).is_some_and(|rest| !rest.is_empty()))
    }

    impl Fs for Declared {
        type Handle = At;

        fn text(&self, path: &str) -> io::Result<String> {
            self.check(path)?;
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| ErrorKind::NotFound.into())
        }

        fn root(&self) -> io::Result<At> {
            Ok(At("/".to_owned()))
        }

        fn open_at(&self, dir: &At, name: &str) -> io::Result<At> {
            assert!(
                !name.is_empty() && name != "." && name != ".." && !name.contains('/'),
                "one name: {name:?}"
            );
            self.opens.set(self.opens.get() + 1);
            let path = format!("{}/{name}", dir.0.trim_end_matches('/'));
            self.check(&path)?;
            if self.files.contains_key(&path) || is_dir(&self.files, &self.mounts, &path) {
                Ok(At(path))
            } else {
                Err(ErrorKind::NotFound.into())
            }
        }

        fn mount(&self, handle: &At) -> io::Result<u64> {
            if self.no_mount_ids {
                return Err(ErrorKind::Unsupported.into());
            }
            self.resolve(&handle.0)
                .map(|(mount, _)| mount.id)
                .ok_or_else(|| ErrorKind::NotFound.into())
        }

        fn read(&self, handle: &At) -> io::Result<String> {
            let text = self.text(&handle.0)?;
            if text == "EOPNOTSUPP" {
                return Err(io::Error::from_raw_os_error(EOPNOTSUPP));
            }
            Ok(text)
        }
    }
}

/// The number the reader takes for a threaded group's refusal is the one the C library names
/// "Operation not supported" (glibc's strerror; cgroup-v2.rst: `cgroup.procs` in a threaded group
/// fails with EOPNOTSUPP), whatever ABI the test is built for.
#[cfg(all(test, target_os = "linux", target_env = "gnu"))]
#[test]
fn a_threaded_groups_refusal_is_the_targets_eopnotsupp() {
    assert_eq!(
        io::Error::from_raw_os_error(EOPNOTSUPP).to_string(),
        format!("Operation not supported (os error {EOPNOTSUPP})")
    );
}
