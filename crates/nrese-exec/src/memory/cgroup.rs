//! The memory limits a Linux process runs under, from its cgroup membership: cgroup v2 (a
//! `0::/path` line) reads `memory.max` in the process's group and every ancestor's, v1 (a
//! line naming the `memory` controller) `memory.limit_in_bytes`; the smallest counts. The
//! groups' files are found through the mounts the process sees its hierarchy through
//! (`/proc/self/mountinfo`), so a hierarchy mounted elsewhere, or a container's own group at
//! its mount's root, is read where it is.
//!
//! A value is read for a group only from a path that resolves to that group, as the kernel
//! resolves it through the mount table (a mount over part of the path redirects it), and the
//! process's own group only where its `cgroup.procs` lists the process (SYN-45 review r1
//! R1-C1a: a path through a mount isn't proof of which group it reaches). An ancestor no path
//! reaches, a mount over its path hiding it, is invisible to the process as one above its
//! namespace is, and the walk stops there (option 1 below).
//!
//! "max", v1's page-aligned maximum and a v2 group without the controller (no `memory.max`)
//! mean no limit; 0 is a limit of nothing. A limit that may apply but can't be read is
//! [`Unknown`], never no limit (A-902; the owner: an OS reading that couldn't be determined is
//! never "no limit"). `read` reads a file's text.
//!
//! Groups above a cgroup namespace's root aren't visible, and their limits still apply. On v1
//! the own group's `hierarchical_memory_limit` (`memory.stat`) includes them; v2 has no such
//! file, so there a limit is known only as far up as the namespace shows, as the JDK, .NET
//! and Go runtimes read it, and a process under a limit above its namespace sets its memory
//! limit by hand (the orchestrator's decision, 3 Nov, P2-A4 IB2001-2a56).

use std::io::{self, ErrorKind};

/// A limit that may apply wasn't read, so what the process may use, or has left, isn't known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Unknown;

/// Above this a limit means "none".
const NONE_ABOVE: u64 = 1 << 62;

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

/// The process's memory group: its hierarchy, and its path in it.
struct Group {
    files: &'static Files,
    path: String,
}

/// The process's memory group; `None` where no hierarchy has the memory controller (nothing
/// limits the process but the machine). The v1 line naming `memory` if there is one (on a
/// hybrid system the controller is there), else the v2 line.
fn group(read: &impl Fn(&str) -> io::Result<String>) -> Result<Option<Group>, Unknown> {
    let own = read("/proc/self/cgroup").map_err(|_| Unknown)?;
    let (mut v1, mut v2) = (None, None);
    for line in own.lines() {
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
    let (files, path) = match (v1, v2) {
        (Some(path), _) => (&V1, path),
        (None, Some(path)) => (&V2, path),
        (None, None) => return Ok(None),
    };
    let path = path.trim();
    let path = match path.trim_end_matches('/') {
        "" => "/",
        trimmed => trimmed,
    };
    Ok(Some(Group {
        files,
        path: path.to_owned(),
    }))
}

/// One mount of `/proc/self/mountinfo` (proc_pid_mountinfo(5)): its id and its parent's, the
/// directory of its filesystem it shows, where it shows it, and whether it is a mount of the
/// process's memory hierarchy.
struct Mount {
    id: u64,
    parent: u64,
    root: String,
    point: String,
    ours: bool,
}

/// The mount table, from mountinfo's text: `None` where a line makes no sense, since a mount
/// left out could be one that redirects a path.
fn mounts(text: &str, files: &Files) -> Option<Vec<Mount>> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            // The fields after the optional ones' `-`: the type, the source, the super options.
            let (before, after) = line.split_once(" - ")?;
            let mut fields = before.split(' ');
            let id = fields.next()?.parse().ok()?;
            let parent = fields.next()?.parse().ok()?;
            let (root, point) = (fields.nth(1)?, fields.next()?);
            let mut after = after.split(' ');
            let (kind, options) = (after.next()?, after.nth(1).unwrap_or(""));
            let ours = if files.v1 {
                kind == "cgroup" && options.split(',').any(|option| option == "memory")
            } else {
                kind == "cgroup2"
            };
            Some(Mount {
                id,
                parent,
                root: unescape(root),
                point: unescape(point),
                ours,
            })
        })
        .collect()
}

/// The mount `path` resolves through and the rest of the path below its point, as the kernel
/// walks it: from the namespace's root mount (at `/`, its parent itself or not listed), at each
/// mount into a mount stacked on it at its own point, else into the child whose point is the
/// shortest prefix of what is left (a child at a shorter point hides the deeper ones: a mount
/// made below it afterwards would have had it as parent). `None` where the table doesn't say:
/// no root, or two mounts at one place.
fn resolve<'m>(mounts: &'m [Mount], path: &str) -> Option<(&'m Mount, String)> {
    let listed = |id: u64| mounts.iter().any(|mount| mount.id == id);
    let mut roots = mounts
        .iter()
        .filter(|m| m.point == "/" && (m.parent == m.id || !listed(m.parent)));
    let (mut at, None) = (roots.next()?, roots.next()) else {
        return None;
    };
    for _ in 0..=mounts.len() {
        let children = || mounts.iter().filter(|m| m.parent == at.id && m.id != at.id);
        let mut stacked = children().filter(|m| m.point == at.point);
        if let Some(on) = stacked.next() {
            if stacked.next().is_some() {
                return None;
            }
            at = on;
            continue;
        }
        let mut below: Vec<&Mount> = children()
            .filter(|m| under(path, &m.point).is_some())
            .collect();
        below.sort_by_key(|m| m.point.trim_end_matches('/').len());
        match below.as_slice() {
            [] => return Some((at, under(path, &at.point)?.to_owned())),
            [first, second, ..] if first.point == second.point => return None,
            [first, ..] => at = *first,
        }
    }
    None
}

/// The group of the process's hierarchy that `path` resolves to, if it resolves into it.
fn group_at(mounts: &[Mount], path: &str) -> Option<String> {
    let (mount, rest) = resolve(mounts, path)?;
    mount.ours.then(|| join(&mount.root, &rest))
}

/// `base` with `rest` (empty, or from a `/`) after it.
fn join(base: &str, rest: &str) -> String {
    match (base.trim_end_matches('/'), rest) {
        ("", "") => "/".to_owned(),
        (base, rest) => format!("{base}{rest}"),
    }
}

/// The paths that resolve to `group`: through each mount of the hierarchy whose root holds it,
/// those the mount table doesn't redirect elsewhere.
fn views(mounts: &[Mount], group: &str) -> Vec<String> {
    let mut found = Vec::new();
    for mount in mounts.iter().filter(|m| m.ours) {
        let Some(rest) = under(group, &mount.root) else {
            continue;
        };
        let path = join(&mount.point, rest);
        if group_at(mounts, &path).as_deref() == Some(group) && !found.contains(&path) {
            found.push(path);
        }
    }
    found
}

/// The group above `group`; `None` above the root.
fn parent_of(group: &str) -> Option<&str> {
    match group.rfind('/')? {
        _ if group == "/" => None,
        0 => Some("/"),
        at => Some(&group[..at]),
    }
}

/// A group's directory as the process reaches it, and the group: what its files are read
/// through.
struct Place<'m> {
    mounts: &'m [Mount],
    dir: String,
    group: String,
}

impl Place<'_> {
    /// The path of the group's file `name`, where that path resolves to the group's file: a
    /// single file bound over it (mount(2) binds files too) is another group's (sol-loop r6
    /// S1). `Unknown` otherwise: the path claims the group's file and isn't it.
    fn file(&self, name: &str) -> Result<String, Unknown> {
        let name = format!("/{name}");
        let path = join(&self.dir, &name);
        let expected = join(&self.group, &name);
        match group_at(self.mounts, &path) {
            Some(found) if found == expected => Ok(path),
            _ => Err(Unknown),
        }
    }
}

/// Of `paths` to the process's group, one that is the process's own: its `cgroup.procs` lists
/// the process (a process is in one group of a hierarchy), and, in v2, it has
/// `cgroup.controllers`, which only v2 groups have; both read where they are the group's own.
/// The process's id is `getpid`'s, in the pid namespace it runs in: `cgroup.procs` lists ids
/// in its reader's namespace (cgroup.c, cgroup_procs_show), which a /proc mounted from another
/// namespace doesn't show (sol-loop r6 S2). A path that can't be read so isn't one.
fn own<'m>(
    read: &impl Fn(&str) -> io::Result<String>,
    files: &Files,
    mounts: &'m [Mount],
    group: &str,
) -> Option<Place<'m>> {
    let pid = u64::from(std::process::id());
    let reads =
        |place: &Place<'_>, name: &str| place.file(name).ok().and_then(|path| read(&path).ok());
    views(mounts, group)
        .into_iter()
        .map(|dir| Place {
            mounts,
            dir,
            group: group.to_owned(),
        })
        .find(|place| {
            let lists = reads(place, "cgroup.procs").is_some_and(|procs| {
                procs
                    .lines()
                    .any(|line| line.trim().parse::<u64>() == Ok(pid))
            });
            lists && (files.v1 || reads(place, "cgroup.controllers").is_some())
        })
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

/// `path` below `root`: the rest of it, "" for `root` itself, else from a `/`; `None` where it
/// isn't below (`/docker/ab` isn't above `/docker/abc`).
fn under<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    let rest = path.strip_prefix(root.trim_end_matches('/'))?;
    (rest.is_empty() || rest.starts_with('/')).then(|| rest.trim_end_matches('/'))
}

/// `each` of the process's memory group and its visible ancestors that has a limit, innermost
/// first: its directory and its limit. `Unknown`: a limit that may apply wasn't read, or
/// `each` couldn't read what it needs.
fn limited(
    read: &impl Fn(&str) -> io::Result<String>,
    mut each: impl FnMut(&Files, &Place<'_>, u64) -> Result<(), Unknown>,
) -> Result<(), Unknown> {
    let Some(Group { files, path }) = group(read)? else {
        return Ok(());
    };
    let text = read("/proc/self/mountinfo").map_err(|_| Unknown)?;
    let mounts = mounts(&text, files).ok_or(Unknown)?;
    let mut place = own(read, files, &mounts, &path).ok_or(Unknown)?;
    if files.v1 {
        // The kernel's own least limit over the whole hierarchy, groups above a namespace
        // included, which the walk below can't see (since Linux 2.6.29).
        if let Some(limit) = hierarchical(read, &place)?.filter(|&limit| limit < NONE_ABOVE) {
            each(files, &place, limit)?;
        }
    }
    loop {
        match read(&place.file(files.limit)?) {
            Ok(text) => {
                let text = text.trim();
                if text != "max" {
                    let limit: u64 = text.parse().map_err(|_| Unknown)?;
                    if limit < NONE_ABOVE {
                        each(files, &place, limit)?;
                    }
                }
            }
            // A v2 group without the memory controller, the root among them, has no limit
            // file; every v1 group has one.
            Err(error) if error.kind() == ErrorKind::NotFound && !files.v1 => {}
            Err(_) => return Err(Unknown),
        }
        let Some(up) = parent_of(&place.group) else {
            return Ok(());
        };
        let up = up.to_owned();
        // No path reaches it (above every mount's root, or hidden by a mount over its path): not
        // visible to the process, as above a namespace, so nothing above it is read (the
        // orchestrator, 8 Nov 02:28: option 1 of IB2001-2a56 covers it).
        let Some(dir) = views(&mounts, &up).into_iter().next() else {
            return Ok(());
        };
        place = Place {
            mounts: &mounts,
            dir,
            group: up,
        };
    }
}

/// A v1 group's `hierarchical_memory_limit` (`memory.stat`): the least limit of the group and
/// every ancestor. `None` where the kernel doesn't show it; `Unknown` where the statistics
/// can't be read or the value makes no sense.
fn hierarchical(
    read: &impl Fn(&str) -> io::Result<String>,
    place: &Place<'_>,
) -> Result<Option<u64>, Unknown> {
    let stat = match read(&place.file("memory.stat")?) {
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
pub(super) fn limit(read: &impl Fn(&str) -> io::Result<String>) -> Result<Option<u64>, Unknown> {
    let mut smallest = None;
    limited(read, |_, _, limit| {
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
pub(super) fn left(read: &impl Fn(&str) -> io::Result<String>) -> Result<Option<u64>, Unknown> {
    let mut least = None;
    limited(read, |files, place, limit| {
        let number = |text: &str| text.trim().parse::<u64>().ok();
        let usage = read(&place.file(files.usage)?)
            .ok()
            .and_then(|text| number(&text))
            .ok_or(Unknown)?;
        let inactive = read(&place.file("memory.stat")?)
            .ok()
            .and_then(|stat| {
                stat.lines().find_map(|line| {
                    let (name, value) = line.split_once(' ')?;
                    (name == files.inactive).then(|| number(value)).flatten()
                })
            })
            .unwrap_or(0);
        let used = usage.saturating_sub(inactive);
        least = super::least([least, Some(limit.saturating_sub(used))]);
        Ok(())
    })?;
    Ok(least)
}
