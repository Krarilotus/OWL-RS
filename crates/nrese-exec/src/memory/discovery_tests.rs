//! What the memory the process may use and could still take read as, from the files the
//! kernel shows (Linux) or the job object's answers (Windows): a limit that may apply but
//! couldn't be read is unknown, never no limit (A-902; the owner's rule: an OS reading that
//! couldn't be determined is never "no limit"). The kernel's file formats: proc(5)
//! (`/proc/self/cgroup`, `/proc/self/mountinfo`, `/proc/meminfo`, `/proc/self/statm`) and
//! the cgroup v1 and v2 interface documents.

use super::*;
use std::collections::HashMap;
use std::io::ErrorKind;

const G: u64 = 1 << 30;

/// 32 GiB in total, 20 GiB available.
const MEMINFO: &str = "MemTotal:       33554432 kB\nMemAvailable:   20971520 kB\n";

/// This process as a group's `cgroup.procs` lists it: its id in the pid namespace it runs in,
/// the one `cgroup.procs` translates to for its reader (cgroup.c, cgroup_procs_show).
fn procs() -> &'static str {
    static PROCS: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| format!("{}\n", std::process::id()));
    PROCS.as_str()
}

/// `/proc/self/stat` where /proc is this pid namespace's: its first field is the same id.
fn stat() -> String {
    format!("{} (nrese-server) S 1\n", std::process::id())
}

/// The namespace's root mount, its own parent as proc_pid_mountinfo(5) shows it.
macro_rules! root {
    () => {
        "23 23 8:1 / / rw - ext4 /dev/root rw\n"
    };
}

/// cgroup v2 where systemd mounts it.
const V2_MOUNT: &str = concat!(
    root!(),
    "30 23 0:26 / /sys/fs/cgroup rw,nosuid,nodev,noexec,relatime shared:4 - \
     cgroup2 cgroup2 rw,nsdelegate,memory_recursiveprot\n"
);

/// cgroup v1's memory hierarchy where systemd mounts it, beside another controller's, on the
/// tmpfs at /sys/fs/cgroup.
const V1_MOUNT: &str = concat!(
    root!(),
    "25 23 0:22 / /sys/fs/cgroup ro - tmpfs tmpfs ro,mode=755\n",
    "34 25 0:30 / /sys/fs/cgroup/cpu,cpuacct rw,nosuid - cgroup cgroup rw,cpu,cpuacct\n",
    "35 25 0:31 / /sys/fs/cgroup/memory rw,nosuid,nodev - cgroup cgroup rw,memory\n"
);

/// A file system: `/proc/meminfo` as [`MEMINFO`] and `/proc/self/stat` as [`STAT`] unless
/// `files` says otherwise, the other `files`, and `denied` (if not empty) refused
/// (permission); any other file is not found.
fn system(
    files: &[(&str, &str)],
    denied: &'static str,
) -> impl Fn(&str) -> std::io::Result<String> {
    let mut map: HashMap<String, String> = HashMap::from([
        ("/proc/meminfo".to_owned(), MEMINFO.to_owned()),
        ("/proc/self/stat".to_owned(), stat()),
    ]);
    for (path, text) in files {
        if text.is_empty() {
            map.remove(*path);
        } else {
            map.insert((*path).to_owned(), (*text).to_owned());
        }
    }
    move |path| {
        if !denied.is_empty() && path == denied {
            return Err(ErrorKind::PermissionDenied.into());
        }
        map.get(path)
            .cloned()
            .ok_or_else(|| ErrorKind::NotFound.into())
    }
}

/// A row: its name, the files the process sees, one it may not read (or ""), and what the
/// memory it may use and could still take read as (`None`: unknown).
type Row<'a> = (
    &'a str,
    Vec<(&'a str, &'a str)>,
    &'static str,
    Option<u64>,
    Option<u64>,
);

/// Checks every row and reports each one that reads otherwise, not only the first.
fn check(rows: Vec<Row<'_>>) {
    let mut wrong = Vec::new();
    for (name, files, denied, available, remaining) in rows {
        let read_available = linux_available(system(&files, denied));
        let read_remaining = linux_remaining(system(&files, denied));
        if (read_available, read_remaining) != (available, remaining) {
            wrong.push(format!(
                "{name}: available {read_available:?}, expected {available:?}; \
                 remaining {read_remaining:?}, expected {remaining:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

/// A path through a mount isn't proof of which cgroup it reaches (SYN-45 review r1 R1-C1a): a
/// launcher keeps the real `limited` subtree bound at /cg-safe, then binds the sibling `loose`
/// subtree over /sys/fs/cgroup/limited. The process stays in /limited/worker (4 GiB); the kernel
/// enforces `limited`'s 1 GiB, visible at /cg-safe, while /sys/fs/cgroup/limited/worker now shows
/// loose/worker (4 GiB, other processes) and /sys/fs/cgroup/limited loose (8 GiB). Each group
/// uses 512 MiB. A value is read only where its path resolves to the group (mount(2) bind
/// semantics, proc_pid_mountinfo(5)) and the own group's `cgroup.procs` lists the process; an
/// ancestor no path reaches is invisible, as one above a namespace is, and the walk stops there
/// (the orchestrator, 8 Nov: option 1 of IB2001-2a56).
#[test]
fn a_value_is_read_only_where_the_path_reaches_the_group() {
    const WITNESS: &str = concat!(
        root!(),
        "30 23 0:26 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n",
        "40 23 0:26 /limited /cg-safe rw - cgroup2 cgroup2 rw\n",
        "50 30 0:26 /loose /sys/fs/cgroup/limited rw - cgroup2 cgroup2 rw\n"
    );
    const WITNESS_REORDERED: &str = concat!(
        "50 30 0:26 /loose /sys/fs/cgroup/limited rw - cgroup2 cgroup2 rw\n",
        "40 23 0:26 /limited /cg-safe rw - cgroup2 cgroup2 rw\n",
        root!(),
        "30 23 0:26 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n"
    );
    // Without /cg-safe: the own group's path reaches loose/worker, and nothing reaches it.
    const NO_OTHER_VIEW: &str = concat!(
        root!(),
        "30 23 0:26 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n",
        "50 30 0:26 /loose /sys/fs/cgroup/limited rw - cgroup2 cgroup2 rw\n"
    );
    // The real worker bound back over loose/worker: the group is reached; `limited` is hidden
    // by the loose bind, which no path through reaches it, so it is invisible and only the
    // group's 4 GiB is read, never loose's 2 GiB.
    const REBOUND: &str = concat!(
        root!(),
        "30 23 0:26 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n",
        "50 30 0:26 /loose /sys/fs/cgroup/limited rw - cgroup2 cgroup2 rw\n",
        "60 50 0:26 /limited/worker /sys/fs/cgroup/limited/worker rw - cgroup2 cgroup2 rw\n"
    );
    // The v1 analogue: the memory hierarchy on the tmpfs at /sys/fs/cgroup, `limited` bound at
    // /cg-safe, loose over /sys/fs/cgroup/memory/limited. The real worker's statistics hold the
    // hierarchy's least limit, 1 GiB; loose/worker's hold 8 GiB.
    const V1_WITNESS: &str = concat!(
        root!(),
        "25 23 0:22 / /sys/fs/cgroup ro - tmpfs tmpfs ro\n",
        "35 25 0:31 / /sys/fs/cgroup/memory rw - cgroup cgroup rw,memory\n",
        "40 23 0:31 /limited /cg-safe rw - cgroup cgroup rw,memory\n",
        "50 35 0:31 /loose /sys/fs/cgroup/memory/limited rw - cgroup cgroup rw,memory\n"
    );
    let (one, four, eight, half) = (
        G.to_string(),
        (4 * G).to_string(),
        (8 * G).to_string(),
        (G / 2).to_string(),
    );
    let (one, four, eight, half) = (one.as_str(), four.as_str(), eight.as_str(), half.as_str());
    let v2 = |mounts: &'static str| -> Vec<(&str, &str)> {
        vec![
            ("/proc/self/mountinfo", mounts),
            ("/proc/self/cgroup", "0::/limited/worker"),
            // Through /sys/fs/cgroup: loose and loose/worker.
            ("/sys/fs/cgroup/limited/cgroup.controllers", "memory"),
            ("/sys/fs/cgroup/limited/memory.max", eight),
            ("/sys/fs/cgroup/limited/memory.current", half),
            ("/sys/fs/cgroup/limited/worker/cgroup.controllers", "memory"),
            ("/sys/fs/cgroup/limited/worker/cgroup.procs", "77\n78\n"),
            ("/sys/fs/cgroup/limited/worker/memory.max", four),
            ("/sys/fs/cgroup/limited/worker/memory.current", half),
            // Through /cg-safe: limited and its worker, the process's group.
            ("/cg-safe/cgroup.controllers", "memory"),
            ("/cg-safe/memory.max", one),
            ("/cg-safe/memory.current", half),
            ("/cg-safe/worker/cgroup.controllers", "memory"),
            ("/cg-safe/worker/cgroup.procs", procs()),
            ("/cg-safe/worker/memory.max", four),
            ("/cg-safe/worker/memory.current", half),
        ]
    };
    let two = (2 * G).to_string();
    let rebound: Vec<(&str, &str)> = vec![
        ("/proc/self/mountinfo", REBOUND),
        ("/proc/self/cgroup", "0::/limited/worker"),
        ("/sys/fs/cgroup/limited/cgroup.controllers", "memory"),
        ("/sys/fs/cgroup/limited/memory.max", two.as_str()),
        ("/sys/fs/cgroup/limited/memory.current", half),
        ("/sys/fs/cgroup/limited/worker/cgroup.controllers", "memory"),
        ("/sys/fs/cgroup/limited/worker/cgroup.procs", procs()),
        ("/sys/fs/cgroup/limited/worker/memory.max", four),
        ("/sys/fs/cgroup/limited/worker/memory.current", half),
    ];
    const NONE: &str = "9223372036854771712";
    let v1: Vec<(&str, &str)> = vec![
        ("/proc/self/mountinfo", V1_WITNESS),
        ("/proc/self/cgroup", "3:memory:/limited/worker"),
        ("/sys/fs/cgroup/memory/memory.limit_in_bytes", NONE),
        ("/sys/fs/cgroup/memory/limited/memory.limit_in_bytes", eight),
        ("/sys/fs/cgroup/memory/limited/memory.usage_in_bytes", half),
        ("/sys/fs/cgroup/memory/limited/worker/cgroup.procs", "77\n"),
        (
            "/sys/fs/cgroup/memory/limited/worker/memory.limit_in_bytes",
            four,
        ),
        (
            "/sys/fs/cgroup/memory/limited/worker/memory.usage_in_bytes",
            half,
        ),
        (
            "/sys/fs/cgroup/memory/limited/worker/memory.stat",
            "hierarchical_memory_limit 8589934592\ntotal_inactive_file 0\n",
        ),
        ("/cg-safe/memory.limit_in_bytes", one),
        ("/cg-safe/memory.usage_in_bytes", half),
        ("/cg-safe/worker/cgroup.procs", procs()),
        ("/cg-safe/worker/memory.limit_in_bytes", four),
        ("/cg-safe/worker/memory.usage_in_bytes", half),
        (
            "/cg-safe/worker/memory.stat",
            "hierarchical_memory_limit 1073741824\ntotal_inactive_file 0\n",
        ),
    ];
    // A later entry replaces an earlier one of the same path.
    let mut lists_others = v2(WITNESS);
    lists_others.push(("/cg-safe/worker/cgroup.procs", "77\n"));
    check(vec![
        ("v2 witness", v2(WITNESS), "", Some(G), Some(G / 2)),
        (
            "v2 witness, mountinfo reordered",
            v2(WITNESS_REORDERED),
            "",
            Some(G),
            Some(G / 2),
        ),
        (
            "own path redirected, no other view",
            v2(NO_OTHER_VIEW),
            "",
            None,
            None,
        ),
        (
            "group rebound under a foreign bind",
            rebound,
            "",
            Some(4 * G),
            Some(4 * G - G / 2),
        ),
        ("v1 witness", v1, "", Some(G), Some(G / 2)),
        (
            "own group's cgroup.procs doesn't list the process",
            lists_others,
            "",
            None,
            None,
        ),
        (
            "own group's cgroup.procs refused",
            v2(WITNESS),
            "/cg-safe/worker/cgroup.procs",
            None,
            None,
        ),
    ]);
}

/// A file is read for a group only where its own path resolves to the group's file (sol-loop
/// r6 S1: mount(2) binds single files too): a launcher binds `/loose/memory.max` (8 GiB) over
/// a group's `memory.max`, or loose/worker's `memory.stat` over the process's. Over the own
/// group's or a reachable ancestor's file it claims that group's value and isn't it: unknown.
/// And the process is named as `cgroup.procs` names it, in the pid namespace it runs in, not
/// as a /proc mounted from another namespace shows it (sol-loop r6 S2: `unshare --pid --fork`
/// keeping the old /proc).
#[test]
fn a_file_is_read_only_where_its_own_path_reaches_the_group() {
    let (one, four, eight, half) = (
        G.to_string(),
        (4 * G).to_string(),
        (8 * G).to_string(),
        (G / 2).to_string(),
    );
    let (one, four, eight, half) = (one.as_str(), four.as_str(), eight.as_str(), half.as_str());
    // The process in /limited; loose's memory.max bound over limited's.
    const OWN_BOUND: &str = concat!(
        root!(),
        "30 23 0:26 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n",
        "70 30 0:26 /loose/memory.max /sys/fs/cgroup/limited/memory.max rw - cgroup2 cgroup2 rw\n"
    );
    let own_bound = vec![
        ("/proc/self/mountinfo", OWN_BOUND),
        ("/proc/self/cgroup", "0::/limited"),
        ("/sys/fs/cgroup/limited/cgroup.controllers", "memory"),
        ("/sys/fs/cgroup/limited/cgroup.procs", procs()),
        ("/sys/fs/cgroup/limited/memory.max", eight),
        ("/sys/fs/cgroup/limited/memory.current", half),
    ];
    // The process in /limited/worker (4 GiB); loose's memory.max bound over limited's (1 GiB).
    const PARENT_BOUND: &str = concat!(
        root!(),
        "30 23 0:26 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n",
        "70 30 0:26 /loose/memory.max /sys/fs/cgroup/limited/memory.max rw - cgroup2 cgroup2 rw\n"
    );
    let parent_bound = vec![
        ("/proc/self/mountinfo", PARENT_BOUND),
        ("/proc/self/cgroup", "0::/limited/worker"),
        ("/sys/fs/cgroup/limited/memory.max", eight),
        ("/sys/fs/cgroup/limited/memory.current", half),
        ("/sys/fs/cgroup/limited/worker/cgroup.controllers", "memory"),
        ("/sys/fs/cgroup/limited/worker/cgroup.procs", procs()),
        ("/sys/fs/cgroup/limited/worker/memory.max", four),
        ("/sys/fs/cgroup/limited/worker/memory.current", half),
    ];
    // v1: the process in /limited/worker, whose memory.stat (hierarchical 1 GiB) loose/worker's
    // (8 GiB) is bound over.
    const STAT_BOUND: &str = concat!(
        root!(),
        "25 23 0:22 / /sys/fs/cgroup ro - tmpfs tmpfs ro\n",
        "35 25 0:31 / /sys/fs/cgroup/memory rw - cgroup cgroup rw,memory\n",
        "70 35 0:31 /loose/worker/memory.stat /sys/fs/cgroup/memory/limited/worker/memory.stat rw - \
         cgroup cgroup rw,memory\n"
    );
    const NONE: &str = "9223372036854771712";
    let stat_bound = vec![
        ("/proc/self/mountinfo", STAT_BOUND),
        ("/proc/self/cgroup", "3:memory:/limited/worker"),
        ("/sys/fs/cgroup/memory/memory.limit_in_bytes", NONE),
        ("/sys/fs/cgroup/memory/limited/memory.limit_in_bytes", NONE),
        ("/sys/fs/cgroup/memory/limited/worker/cgroup.procs", procs()),
        (
            "/sys/fs/cgroup/memory/limited/worker/memory.limit_in_bytes",
            four,
        ),
        (
            "/sys/fs/cgroup/memory/limited/worker/memory.usage_in_bytes",
            half,
        ),
        (
            "/sys/fs/cgroup/memory/limited/worker/memory.stat",
            "hierarchical_memory_limit 8589934592\ntotal_inactive_file 0\n",
        ),
    ];
    // A /proc from the parent pid namespace: its stat names the process otherwise.
    let foreign_proc = format!("{} (nrese-server) S 1\n", std::process::id() + 100_000);
    let pid_namespace = vec![
        ("/proc/self/mountinfo", V2_MOUNT),
        ("/proc/self/cgroup", "0::/app.slice"),
        ("/proc/self/stat", foreign_proc.as_str()),
        ("/sys/fs/cgroup/app.slice/cgroup.controllers", "memory"),
        ("/sys/fs/cgroup/app.slice/cgroup.procs", procs()),
        ("/sys/fs/cgroup/app.slice/memory.max", four),
        ("/sys/fs/cgroup/app.slice/memory.current", one),
    ];
    check(vec![
        (
            "own group's limit file bound from another group",
            own_bound,
            "",
            None,
            None,
        ),
        (
            "an ancestor's limit file bound from another group",
            parent_bound,
            "",
            None,
            None,
        ),
        (
            "v1 own group's statistics bound from another group",
            stat_bound,
            "",
            None,
            None,
        ),
        (
            "/proc from another pid namespace",
            pid_namespace,
            "",
            Some(4 * G),
            Some(3 * G),
        ),
    ]);
}

/// A mount another one covers isn't a view (sol-loop r3 S1; proc_pid_mountinfo(5) lists covered
/// mounts too): a launcher bind-mounts the process's group `worker` (4 GiB, 1 GiB used) over the
/// full v2 tree at /sys/fs/cgroup, or covers /sys/fs with a tmpfs and leaves the group bind-mounted
/// at /cg-local. Only the group is reachable then, at the covering mount, and its limit is read
/// there; its parent, hidden by the cover, is invisible as one above a namespace is (option 1 of
/// IB2001-2a56, the orchestrator 8 Nov). Under a self-parented root (sol-loop r5 S3) nothing is
/// covered and the group's limit is read.
#[test]
fn a_covered_mount_isnt_a_view() {
    const STACKED: &str = concat!(
        root!(),
        "30 23 0:26 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n",
        "40 30 0:26 /limited/worker /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n"
    );
    const COVERED: &str = concat!(
        root!(),
        "30 23 0:26 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n",
        "40 23 0:26 /limited/worker /cg-local rw - cgroup2 cgroup2 rw\n",
        "50 23 0:40 / /sys/fs rw - tmpfs tmpfs rw\n"
    );
    // An older bind mount moved (MS_MOVE) over a newer full tree: mountinfo lists mounts by
    // when they were made, so the covering mount comes first (sol-loop r4 S2).
    const MOVED: &str = concat!(
        "40 30 0:26 /limited/worker /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n",
        root!(),
        "30 23 0:26 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n"
    );
    // A namespace's root is its own parent (proc_pid_mountinfo(5)), as an initramfs lists its
    // rootfs; it covers nothing below it (sol-loop r5 S3).
    const ROOTED: &str = "1 1 0:2 / / rw - rootfs rootfs rw\n\
                          20 1 0:26 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n";
    let (four, used) = ((4 * G).to_string(), G.to_string());
    let (four, used) = (four.as_str(), used.as_str());
    // The group's files where the covering mount shows them.
    let group_at = |at: &'static str| -> [(&'static str, &str); 4] {
        match at {
            "/sys/fs/cgroup" => [
                ("/sys/fs/cgroup/cgroup.controllers", "memory"),
                ("/sys/fs/cgroup/cgroup.procs", procs()),
                ("/sys/fs/cgroup/memory.max", four),
                ("/sys/fs/cgroup/memory.current", used),
            ],
            _ => [
                ("/cg-local/cgroup.controllers", "memory"),
                ("/cg-local/cgroup.procs", procs()),
                ("/cg-local/memory.max", four),
                ("/cg-local/memory.current", used),
            ],
        }
    };
    let with = |mounts: &'static str, at: &'static str| -> Vec<(&str, &str)> {
        let mut files = vec![
            ("/proc/self/mountinfo", mounts),
            ("/proc/self/cgroup", "0::/limited/worker"),
        ];
        files.extend(group_at(at));
        files
    };
    check(vec![
        (
            "group bind-mounted over the full tree",
            with(STACKED, "/sys/fs/cgroup"),
            "",
            Some(4 * G),
            Some(3 * G),
        ),
        (
            "group moved over a newer full tree",
            with(MOVED, "/sys/fs/cgroup"),
            "",
            Some(4 * G),
            Some(3 * G),
        ),
        (
            "full tree covered by a tmpfs above it",
            with(COVERED, "/cg-local"),
            "",
            Some(4 * G),
            Some(3 * G),
        ),
        (
            "full tree under a self-parented root",
            vec![
                ("/proc/self/mountinfo", ROOTED),
                ("/proc/self/cgroup", "0::/limited/worker"),
                ("/sys/fs/cgroup/limited/worker/cgroup.controllers", "memory"),
                ("/sys/fs/cgroup/limited/worker/cgroup.procs", procs()),
                ("/sys/fs/cgroup/limited/worker/memory.max", four),
                ("/sys/fs/cgroup/limited/worker/memory.current", used),
            ],
            "",
            Some(4 * G),
            Some(3 * G),
        ),
    ]);
}

/// A deeper mount of the same hierarchy doesn't hide an ancestor another mount shows (review
/// r1 R1-C1): the full v2 tree at /sys/fs/cgroup, and the process's own group bind-mounted at
/// /cg-local. The kernel enforces the 1 GiB parent `limited` over the 4 GiB `worker`
/// (cgroup-v2.rst: a limit applies to the whole subtree), whichever line mountinfo lists
/// first; the parent's limit refused through the full mount is unknown, not the bind mount's
/// 4 GiB. Each group uses 512 MiB, so 512 MiB of the parent's limit is left.
#[test]
fn a_deeper_mount_doesnt_hide_a_visible_ancestor() {
    const FULL_FIRST: &str = concat!(
        root!(),
        "30 23 0:26 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n",
        "40 23 0:26 /limited/worker /cg-local rw - cgroup2 cgroup2 rw\n"
    );
    const BIND_FIRST: &str = concat!(
        "40 23 0:26 /limited/worker /cg-local rw - cgroup2 cgroup2 rw\n",
        root!(),
        "30 23 0:26 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n"
    );
    let (one, four, half) = (G.to_string(), (4 * G).to_string(), (G / 2).to_string());
    let tree = |mounts: &'static str| -> Vec<(&str, &str)> {
        vec![
            ("/proc/self/mountinfo", mounts),
            ("/proc/self/cgroup", "0::/limited/worker"),
            ("/sys/fs/cgroup/limited/cgroup.controllers", "memory"),
            ("/sys/fs/cgroup/limited/memory.max", one.as_str()),
            ("/sys/fs/cgroup/limited/memory.current", half.as_str()),
            ("/sys/fs/cgroup/limited/worker/cgroup.controllers", "memory"),
            ("/sys/fs/cgroup/limited/worker/cgroup.procs", procs()),
            ("/sys/fs/cgroup/limited/worker/memory.max", four.as_str()),
            (
                "/sys/fs/cgroup/limited/worker/memory.current",
                half.as_str(),
            ),
            ("/cg-local/cgroup.controllers", "memory"),
            ("/cg-local/cgroup.procs", procs()),
            ("/cg-local/memory.max", four.as_str()),
            ("/cg-local/memory.current", half.as_str()),
        ]
    };
    check(vec![
        (
            "full mount listed first",
            tree(FULL_FIRST),
            "",
            Some(G),
            Some(G / 2),
        ),
        (
            "bind mount listed first",
            tree(BIND_FIRST),
            "",
            Some(G),
            Some(G / 2),
        ),
        (
            "parent's limit refused",
            tree(FULL_FIRST),
            "/sys/fs/cgroup/limited/memory.max",
            None,
            None,
        ),
    ]);
}

/// Each row: a process's view of its memory groups, and what the memory it may use and could
/// still take read as (`None`: unknown). Groups with a limit use 1 GiB of it.
#[test]
fn a_limit_that_may_apply_but_wasnt_read_is_unknown_never_none() {
    let four = (4 * G).to_string();
    let eight = (8 * G).to_string();
    let used = G.to_string();
    let four = four.as_str();
    let eight = eight.as_str();
    let used = used.as_str();
    let v2_app = [
        ("/proc/self/mountinfo", V2_MOUNT),
        ("/proc/self/cgroup", "0::/app.slice"),
        (
            "/sys/fs/cgroup/app.slice/cgroup.controllers",
            "cpu io memory pids",
        ),
        ("/sys/fs/cgroup/app.slice/cgroup.procs", procs()),
        ("/sys/fs/cgroup/app.slice/memory.max", four),
        ("/sys/fs/cgroup/app.slice/memory.current", used),
    ];
    let custom = concat!(
        root!(),
        "40 23 0:26 / /custom/cg rw,relatime - cgroup2 cgroup2 rw\n"
    );
    let escaped = concat!(
        root!(),
        "40 23 0:26 / /custom\\040cg rw,relatime - cgroup2 cgroup2 rw\n"
    );
    let unnamespaced = concat!(
        root!(),
        "25 23 0:22 / /sys/fs/cgroup ro - tmpfs tmpfs ro\n",
        "35 25 0:31 /docker/abc /sys/fs/cgroup/memory rw - cgroup cgroup rw,memory\n"
    );
    let neighbour = concat!(
        root!(),
        "25 23 0:22 / /sys/fs/cgroup ro - tmpfs tmpfs ro\n",
        "35 25 0:31 /docker/ab /sys/fs/cgroup/memory rw - cgroup cgroup rw,memory\n"
    );
    let rows: Vec<Row<'_>> = vec![
        // The reference: a v2 group with a limit of 4 GiB, 1 GiB of it used.
        ("v2 group", v2_app.to_vec(), "", Some(4 * G), Some(3 * G)),
        // Membership that can't be read: whatever limit there is wasn't read.
        (
            "membership refused",
            v2_app.to_vec(),
            "/proc/self/cgroup",
            None,
            None,
        ),
        // The machine's available memory unread: what is left of it isn't known either.
        (
            "MemAvailable unread",
            [&v2_app[..], &[("/proc/meminfo", "MemTotal: 33554432 kB\n")]].concat(),
            "",
            Some(4 * G),
            None,
        ),
        (
            "limit refused",
            v2_app.to_vec(),
            "/sys/fs/cgroup/app.slice/memory.max",
            None,
            None,
        ),
        (
            "limit malformed",
            [
                &v2_app[..],
                &[("/sys/fs/cgroup/app.slice/memory.max", "lots")],
            ]
            .concat(),
            "",
            None,
            None,
        ),
        // Where the hierarchies are mounted can't be read: the group's files can't be found.
        (
            "mountinfo refused",
            v2_app.to_vec(),
            "/proc/self/mountinfo",
            None,
            None,
        ),
        // Every v1 memory group has its limit file: one that isn't there was never found.
        (
            "v1 limit missing",
            vec![
                ("/proc/self/mountinfo", V1_MOUNT),
                (
                    "/proc/self/cgroup",
                    "4:cpu,cpuacct:/docker/abc\n3:memory:/docker/abc",
                ),
                ("/sys/fs/cgroup/memory/docker/abc/cgroup.procs", procs()),
            ],
            "",
            None,
            None,
        ),
        // The process's own v2 group isn't there: its path was resolved wrongly.
        (
            "v2 group missing",
            vec![
                ("/proc/self/mountinfo", V2_MOUNT),
                ("/proc/self/cgroup", "0::/gone.scope"),
            ],
            "",
            None,
            None,
        ),
        // cgroup v2 mounted elsewhere than /sys/fs/cgroup.
        (
            "v2 mounted elsewhere",
            vec![
                ("/proc/self/mountinfo", custom),
                ("/proc/self/cgroup", "0::/app.slice"),
                ("/custom/cg/app.slice/cgroup.controllers", "memory"),
                ("/custom/cg/app.slice/cgroup.procs", procs()),
                ("/custom/cg/app.slice/memory.max", four),
                ("/custom/cg/app.slice/memory.current", used),
            ],
            "",
            Some(4 * G),
            Some(3 * G),
        ),
        // proc(5): a space in a mount point is shown as \040.
        (
            "mount point with a space",
            vec![
                ("/proc/self/mountinfo", escaped),
                ("/proc/self/cgroup", "0::/app.slice"),
                ("/custom cg/app.slice/cgroup.controllers", "memory"),
                ("/custom cg/app.slice/cgroup.procs", procs()),
                ("/custom cg/app.slice/memory.max", four),
                ("/custom cg/app.slice/memory.current", used),
            ],
            "",
            Some(4 * G),
            Some(3 * G),
        ),
        // A container without a cgroup namespace: its group is the mount's root, so its files
        // are at the mount point.
        (
            "group at the mount's root",
            vec![
                ("/proc/self/mountinfo", unnamespaced),
                ("/proc/self/cgroup", "3:memory:/docker/abc"),
                ("/sys/fs/cgroup/memory/cgroup.procs", procs()),
                ("/sys/fs/cgroup/memory/memory.limit_in_bytes", four),
                ("/sys/fs/cgroup/memory/memory.usage_in_bytes", used),
            ],
            "",
            Some(4 * G),
            Some(3 * G),
        ),
        // The same container under a parent group capped at 2 GiB, which its namespace hides
        // (`--cgroup-parent`; Sol, P2-A4 r1 S1): v1's statistics show the hierarchy's least
        // limit, and what is left of it is at most that less the group's own use.
        (
            "v1 limit above the namespace",
            vec![
                ("/proc/self/mountinfo", unnamespaced),
                ("/proc/self/cgroup", "3:memory:/docker/abc"),
                ("/sys/fs/cgroup/memory/cgroup.procs", procs()),
                ("/sys/fs/cgroup/memory/memory.limit_in_bytes", four),
                ("/sys/fs/cgroup/memory/memory.usage_in_bytes", used),
                (
                    "/sys/fs/cgroup/memory/memory.stat",
                    "cache 0\nhierarchical_memory_limit 2147483648\ntotal_inactive_file 0\n",
                ),
            ],
            "",
            Some(2 * G),
            Some(G),
        ),
        (
            "v1 statistics refused",
            vec![
                ("/proc/self/mountinfo", unnamespaced),
                ("/proc/self/cgroup", "3:memory:/docker/abc"),
                ("/sys/fs/cgroup/memory/cgroup.procs", procs()),
                ("/sys/fs/cgroup/memory/memory.limit_in_bytes", four),
                ("/sys/fs/cgroup/memory/memory.usage_in_bytes", used),
            ],
            "/sys/fs/cgroup/memory/memory.stat",
            None,
            None,
        ),
        // A mount whose root is another group (`/docker/ab` isn't above `/docker/abc`): the
        // process's group isn't visible, whatever the mount point holds.
        (
            "group outside the mount",
            vec![
                ("/proc/self/mountinfo", neighbour),
                ("/proc/self/cgroup", "3:memory:/docker/abc"),
                ("/sys/fs/cgroup/memory/cgroup.procs", procs()),
                ("/sys/fs/cgroup/memory/memory.limit_in_bytes", four),
                ("/sys/fs/cgroup/memory/memory.usage_in_bytes", used),
            ],
            "",
            None,
            None,
        ),
        // A v2 group without the memory controller has no memory.max: its parent's limit
        // counts.
        (
            "controller off in the own group",
            vec![
                ("/proc/self/mountinfo", V2_MOUNT),
                ("/proc/self/cgroup", "0::/app.slice/run.scope"),
                (
                    "/sys/fs/cgroup/app.slice/run.scope/cgroup.controllers",
                    "cpu io",
                ),
                ("/sys/fs/cgroup/app.slice/run.scope/cgroup.procs", procs()),
                ("/sys/fs/cgroup/app.slice/cgroup.controllers", "memory"),
                ("/sys/fs/cgroup/app.slice/memory.max", eight),
                ("/sys/fs/cgroup/app.slice/memory.current", used),
            ],
            "",
            Some(8 * G),
            Some(7 * G),
        ),
        // No memory hierarchy at all: nothing limits the process but the machine.
        (
            "no memory hierarchy",
            vec![
                ("/proc/self/mountinfo", V2_MOUNT),
                ("/proc/self/cgroup", "1:name=systemd:/user.slice"),
            ],
            "",
            Some(32 * G),
            Some(20 * G),
        ),
    ];
    check(rows);
}

/// Windows: whether the process is in a job, and the job's limits, decide the memory it may
/// use; either unasked is unknown, and so is the machine's memory unread.
#[test]
fn a_job_that_cant_be_asked_is_unknown_never_no_job() {
    let limits = |set: [Option<u64>; 2]| move || Some(set);
    let refused = || None;
    let machine = Some(16 * G);
    assert_eq!(
        windows_available(machine, Some(false), refused),
        Some(16 * G)
    );
    assert_eq!(
        windows_available(machine, Some(true), limits([None, None])),
        Some(16 * G)
    );
    assert_eq!(
        windows_available(machine, Some(true), limits([Some(8 * G), Some(4 * G)])),
        Some(4 * G)
    );
    assert_eq!(windows_available(machine, None, limits([None, None])), None);
    assert_eq!(windows_available(machine, Some(true), refused), None);
    assert_eq!(windows_available(None, Some(false), refused), None);
    // What the job leaves, with the machine's available memory unread.
    assert_eq!(windows_remaining(None, Some(Some(3 * G))), None);
}

/// proc(5): statm counts pages, of the system's page size (4, 16 or 64 KiB on aarch64
/// Linux): resident 50 pages, 10 of them shared.
#[test]
fn statm_counts_pages_of_the_given_size() {
    for page in [4096, 16384, 65536] {
        assert_eq!(
            statm_bytes("100 50 10 7 0 30 0\n", page),
            Some(Statm {
                resident: 50 * page,
                anonymous: 40 * page,
            }),
            "{page} B pages"
        );
    }
}

/// The process's memory as statm reads it agrees with `/proc/self/status`, which counts in
/// kB whatever the page size: off by a factor of the page size's ratio to 4 KiB where the
/// page size is assumed. The readings are moments apart, so within a factor of two, and in a
/// process of its own ([`alone`]), where no other test allocates between them (Sol, P2-A4 r1
/// S2).
///
/// [`alone`]: crate::measure::alone
#[cfg(target_os = "linux")]
#[test]
fn statm_agrees_with_status_in_kilobytes() {
    if !crate::measure::alone() {
        return;
    }
    let status = std::fs::read_to_string("/proc/self/status").expect("/proc/self/status");
    let kib = |field: &str| -> u64 {
        let line = status.lines().find(|line| line.starts_with(field));
        let line = line.unwrap_or_else(|| panic!("{field} in /proc/self/status"));
        line.split_whitespace()
            .nth(1)
            .unwrap()
            .parse::<u64>()
            .unwrap()
            * 1024
    };
    let (rss, anon) = (kib("VmRSS:"), kib("RssAnon:"));
    let resident = resident_bytes().expect("the resident set");
    let anonymous = process_bytes().expect("the anonymous resident memory");
    for (statm, status, what) in [(resident, rss, "resident"), (anonymous, anon, "anonymous")] {
        assert!(
            statm <= 2 * status && status <= 2 * statm,
            "{what}: statm {statm} B, status {status} B"
        );
    }
}
