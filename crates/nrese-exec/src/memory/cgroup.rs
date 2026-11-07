//! The memory limits a Linux process runs under, from its cgroup membership: cgroup v2 (a
//! `0::/path` line) reads `memory.max` in the process's group and every ancestor's, v1 (a
//! line naming the `memory` controller) `memory.limit_in_bytes`; the smallest counts. The
//! group's files are found through the mount the process sees its hierarchy through
//! (`/proc/self/mountinfo`), as the JDK and the .NET runtime find them, so a hierarchy mounted
//! elsewhere, or a container's own group at its mount's root, is read where it is.
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

/// The process's memory group as this process sees it.
struct Group {
    files: &'static Files,
    /// The group's directory.
    dir: String,
    /// The directory of the mount it is seen through: its ancestors above aren't visible.
    top: String,
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
    let mounts = read("/proc/self/mountinfo").map_err(|_| Unknown)?;
    // The mount of this hierarchy whose root holds the group, the widest such root. Every mount
    // of one hierarchy shows the same tree, so the widest shows every ancestor a deeper one does
    // and more: a bind mount of the group itself must not hide the limits above it (review r1
    // R1-C1). Above the widest, nothing is visible. A mount another one covers isn't a view
    // (sol-loop r3 S1, r4 S2): its files aren't reachable at its mount point.
    let places: Vec<Option<(u64, u64, String)>> = mounts.lines().map(place).collect();
    let mut best: Option<(usize, String, String)> = None;
    for (index, line) in mounts.lines().enumerate() {
        let Some((root, point)) = mount(line, files) else {
            continue;
        };
        if covered(index, &places) {
            continue;
        }
        let Some(rest) = under(path, &root) else {
            continue;
        };
        if best
            .as_ref()
            .is_none_or(|(widest, ..)| root.len() < *widest)
        {
            best = Some((root.len(), point, rest.to_owned()));
        }
    }
    let (_, point, rest) = best.ok_or(Unknown)?;
    let top = point.trim_end_matches('/').to_owned();
    Ok(Some(Group {
        files,
        dir: format!("{top}{rest}"),
        top,
    }))
}

/// A `/proc/self/mountinfo` line's root and mount point, where it mounts `files`'s hierarchy:
/// `cgroup2` for v2, a `cgroup` naming `memory` in its super options for v1 (proc(5): the
/// fields after the optional ones' `-` are the type, the source and the super options).
fn mount(line: &str, files: &Files) -> Option<(String, String)> {
    let (before, after) = line.split_once(" - ")?;
    let mut fields = before.split(' ');
    let (root, point) = (fields.nth(3)?, fields.next()?);
    let mut after = after.split(' ');
    let (kind, options) = (after.next()?, after.nth(1).unwrap_or(""));
    let ours = if files.v1 {
        kind == "cgroup" && options.split(',').any(|option| option == "memory")
    } else {
        kind == "cgroup2"
    };
    ours.then(|| (unescape(root), unescape(point)))
}

/// A mountinfo line's mount id, parent mount id and mount point (proc(5): the first, second
/// and fifth fields).
fn place(line: &str) -> Option<(u64, u64, String)> {
    let mut fields = line.split(' ');
    let id = fields.next()?.parse().ok()?;
    let parent = fields.next()?.parse().ok()?;
    let point = unescape(fields.nth(2)?);
    Some((id, parent, point))
}

/// Whether the mount at `index` of mountinfo is covered, from the mount tree alone (each
/// mount's parent and point), never from the order of the lines, which a moved mount
/// (`MS_MOVE`) doesn't change (sol-loop r4 S2). A mount is covered when
/// - another is mounted on it at its own point (stacked on it), or
/// - another on the same parent sits at a directory above its point (mounted over the path to
///   it: a mount made at that path afterwards would have had the upper one as its parent), or
/// - its parent is covered and it isn't the mount stacked on that parent.
///
/// A line that can't be read covers nothing: then a cgroup mount counts, and a wrong choice
/// reads files that aren't there, which is Unknown, never a limit too high.
fn covered(index: usize, places: &[Option<(u64, u64, String)>]) -> bool {
    let mounts: Vec<&(u64, u64, String)> = places.iter().flatten().collect();
    let Some(this) = &places[index] else {
        return false;
    };
    let mut at = this;
    // Up the parent chain, at most once per mount.
    for _ in 0..=mounts.len() {
        let (id, parent, point) = at;
        let stacked = mounts.iter().any(|(_, on, over)| on == id && over == point);
        let overmounted = mounts.iter().any(|(other, on, over)| {
            other != id && on == parent && under(point, over).is_some_and(|rest| !rest.is_empty())
        });
        if stacked || overmounted {
            return true;
        }
        // Inside its parent: covered with it, unless it is what's stacked on the parent.
        let Some(&up) = mounts
            .iter()
            .find(|(other, ..)| other == parent && other != id)
        else {
            return false;
        };
        if up.2 == *point {
            return false;
        }
        at = up;
    }
    false
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
    mut each: impl FnMut(&Files, &str, u64) -> Result<(), Unknown>,
) -> Result<(), Unknown> {
    let Some(Group {
        files,
        mut dir,
        top,
    }) = group(read)?
    else {
        return Ok(());
    };
    if files.v1 {
        // The kernel's own least limit over the whole hierarchy, groups above a namespace
        // included, which the walk below can't see (since Linux 2.6.29).
        if let Some(limit) = hierarchical(read, &dir)?.filter(|&limit| limit < NONE_ABOVE) {
            each(files, &dir, limit)?;
        }
    } else {
        // Every v2 group has it: without it the group isn't where its path leads.
        read(&format!("{dir}/cgroup.controllers")).map_err(|_| Unknown)?;
    }
    loop {
        match read(&format!("{dir}/{}", files.limit)) {
            Ok(text) => {
                let text = text.trim();
                if text != "max" {
                    let limit: u64 = text.parse().map_err(|_| Unknown)?;
                    if limit < NONE_ABOVE {
                        each(files, &dir, limit)?;
                    }
                }
            }
            // A v2 group without the memory controller, the root among them, has no limit
            // file; every v1 group has one.
            Err(error) if error.kind() == ErrorKind::NotFound && !files.v1 => {}
            Err(_) => return Err(Unknown),
        }
        if dir.len() <= top.len() {
            return Ok(());
        }
        dir.truncate(dir.rfind('/').ok_or(Unknown)?);
    }
}

/// A v1 group's `hierarchical_memory_limit` (`memory.stat`): the least limit of the group and
/// every ancestor. `None` where the kernel doesn't show it; `Unknown` where the statistics
/// can't be read or the value makes no sense.
fn hierarchical(
    read: &impl Fn(&str) -> io::Result<String>,
    dir: &str,
) -> Result<Option<u64>, Unknown> {
    let stat = match read(&format!("{dir}/memory.stat")) {
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
    limited(read, |files, dir, limit| {
        let number = |text: &str| text.trim().parse::<u64>().ok();
        let usage = read(&format!("{dir}/{}", files.usage))
            .ok()
            .and_then(|text| number(&text))
            .ok_or(Unknown)?;
        let inactive = read(&format!("{dir}/memory.stat"))
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
