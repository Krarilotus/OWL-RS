//! What the memory the process may use and could still take read as, from the files the
//! kernel shows (Linux) or the job object's answers (Windows): a limit that may apply but
//! couldn't be read is unknown, never no limit (A-902; the owner's rule: an OS reading that
//! couldn't be determined is never "no limit"). The kernel's file formats: proc(5)
//! (`/proc/self/cgroup`, `/proc/self/mountinfo`, `/proc/meminfo`, `/proc/self/statm`) and
//! the cgroup v1 and v2 interface documents.

use super::*;
use std::collections::HashMap;

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
fn system(files: &[(&str, &str)], denied: &str) -> cgroup::declared::Declared {
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
    cgroup::declared::Declared::new(map, denied)
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
        let read_available = linux_available(&system(&files, denied));
        let read_remaining = linux_remaining(&system(&files, denied));
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

/// A group's coordinates in the cgroup namespace aren't a path (SYN-45 fix check r2;
/// cgroup_namespaces(7), path_resolution(7)): a launcher makes the namespace in /limited/scope
/// and then moves the process to the sibling /limited/worker, so /proc/self/cgroup names
/// `/../worker`. Its mounts: the namespace's root (scope, 8 GiB) at /a/cg, the worker (4 GiB,
/// the process) at /a/worker, the real parent /limited (1 GiB) at /cg-safe, and loose's single
/// memory.max (8 GiB) at /a/memory.max. A path through /a/cg/.. leaves /a/cg for /a, so the
/// kernel answers /a/cg/../worker with the worker's files and /a/cg/../memory.max with loose's;
/// the rows keep those answers for a reader of paths. The kernel enforces limited's 1 GiB,
/// which /cg-safe shows. Each group uses 512 MiB. The safe view is listed after and before the
/// namespace's root.
#[test]
fn a_group_above_the_namespace_is_read_where_a_view_shows_it() {
    let (one, four, eight, half) = (
        G.to_string(),
        (4 * G).to_string(),
        (8 * G).to_string(),
        (G / 2).to_string(),
    );
    let (one, four, eight, half) = (one.as_str(), four.as_str(), eight.as_str(), half.as_str());
    const SAFE_AFTER: &str = concat!(
        root!(),
        "30 23 0:26 / /a/cg rw - cgroup2 cgroup2 rw\n",
        "40 23 0:26 /../worker /a/worker rw - cgroup2 cgroup2 rw\n",
        "50 23 0:26 /.. /cg-safe rw - cgroup2 cgroup2 rw\n",
        "60 23 0:26 /../loose/memory.max /a/memory.max rw - cgroup2 cgroup2 rw\n"
    );
    const SAFE_BEFORE: &str = concat!(
        root!(),
        "50 23 0:26 /.. /cg-safe rw - cgroup2 cgroup2 rw\n",
        "30 23 0:26 / /a/cg rw - cgroup2 cgroup2 rw\n",
        "40 23 0:26 /../worker /a/worker rw - cgroup2 cgroup2 rw\n",
        "60 23 0:26 /../loose/memory.max /a/memory.max rw - cgroup2 cgroup2 rw\n"
    );
    let files = |mounts: &'static str| -> Vec<(&str, &str)> {
        vec![
            ("/proc/self/mountinfo", mounts),
            ("/proc/self/cgroup", "0::/../worker"),
            // The namespace's root, scope.
            ("/a/cg/cgroup.controllers", "memory"),
            ("/a/cg/cgroup.procs", "77\n"),
            ("/a/cg/memory.max", eight),
            ("/a/cg/memory.current", half),
            // loose's memory.max, bound over a file.
            ("/a/memory.max", eight),
            // The real parent, limited, and the worker in it.
            ("/cg-safe/cgroup.controllers", "memory"),
            ("/cg-safe/memory.max", one),
            ("/cg-safe/memory.current", half),
            ("/cg-safe/worker/cgroup.controllers", "memory"),
            ("/cg-safe/worker/cgroup.procs", procs()),
            ("/cg-safe/worker/memory.max", four),
            ("/cg-safe/worker/memory.current", half),
            // The worker, the process's group.
            ("/a/worker/cgroup.controllers", "memory"),
            ("/a/worker/cgroup.procs", procs()),
            ("/a/worker/memory.max", four),
            ("/a/worker/memory.current", half),
            // What the kernel answers a path through /a/cg/.. with: /a's entries.
            ("/a/cg/../worker/cgroup.controllers", "memory"),
            ("/a/cg/../worker/cgroup.procs", procs()),
            ("/a/cg/../worker/memory.max", four),
            ("/a/cg/../worker/memory.current", half),
            ("/a/cg/../memory.max", eight),
        ]
    };
    check(vec![
        (
            "own group beside the namespace's root, safe view after it",
            files(SAFE_AFTER),
            "",
            Some(G),
            Some(G / 2),
        ),
        (
            "own group beside the namespace's root, safe view before it",
            files(SAFE_BEFORE),
            "",
            Some(G),
            Some(G / 2),
        ),
    ]);
}

/// A group is read wherever the kernel shows it to the process (fix check r2, F1 and F2): in a
/// chroot whose root isn't a mount, mountinfo has no `/` line (the kernel leaves out mounts
/// outside the root; fs/proc_namespace.c), and the cgroup mount's parent isn't listed; the
/// process's group /app (2 GiB) is read there. In a threaded v2 group (cgroup-v2.rst, "Threads")
/// `cgroup.procs` refuses with EOPNOTSUPP and `cgroup.threads` lists the process's leader,
/// whose thread id is the process's; the memory controller is a domain one, so the threaded
/// group has no memory.max and its domain's limit (2 GiB) is the one that applies. Each uses
/// 512 MiB.
#[test]
fn a_group_is_read_wherever_the_kernel_shows_it() {
    let (two, half) = ((2 * G).to_string(), (G / 2).to_string());
    let (two, half) = (two.as_str(), half.as_str());
    const CHROOT: &str = concat!(
        "31 22 0:5 / /proc rw - proc proc rw\n",
        "30 22 0:26 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n"
    );
    let chroot = vec![
        ("/proc/self/mountinfo", CHROOT),
        ("/proc/self/cgroup", "0::/app"),
        ("/sys/fs/cgroup/cgroup.controllers", "memory"),
        ("/sys/fs/cgroup/app/cgroup.controllers", "memory"),
        ("/sys/fs/cgroup/app/cgroup.procs", procs()),
        ("/sys/fs/cgroup/app/memory.max", two),
        ("/sys/fs/cgroup/app/memory.current", half),
    ];
    let threads = format!("{}\n{}\n", std::process::id(), std::process::id() + 1);
    let threaded = vec![
        ("/proc/self/mountinfo", V2_MOUNT),
        ("/proc/self/cgroup", "0::/domain/thread"),
        ("/sys/fs/cgroup/cgroup.controllers", "memory"),
        ("/sys/fs/cgroup/domain/cgroup.controllers", "memory"),
        ("/sys/fs/cgroup/domain/cgroup.procs", procs()),
        ("/sys/fs/cgroup/domain/memory.max", two),
        ("/sys/fs/cgroup/domain/memory.current", half),
        // A threaded group's controllers are threaded ones.
        ("/sys/fs/cgroup/domain/thread/cgroup.controllers", "pids"),
        ("/sys/fs/cgroup/domain/thread/cgroup.type", "threaded"),
        ("/sys/fs/cgroup/domain/thread/cgroup.procs", "EOPNOTSUPP"),
        (
            "/sys/fs/cgroup/domain/thread/cgroup.threads",
            threads.as_str(),
        ),
    ];
    check(vec![
        (
            "chroot whose root isn't a mount",
            chroot,
            "",
            Some(2 * G),
            Some(2 * G - G / 2),
        ),
        (
            "threaded v2 group",
            threaded,
            "",
            Some(2 * G),
            Some(2 * G - G / 2),
        ),
    ]);
}

/// An ancestor reached on a view's own mount is read, whatever is bound over the group's entry
/// in it (sol-loop r7 S1): a launcher binds the process's group /limited/worker (4 GiB) over
/// itself, and /limited (1 GiB) is reached through the full tree. And a view whose way the
/// kernel refuses may show a limit that applies (r7 S2): the full tree at /x/cg behind a
/// refused /x, the group bound at /cg-local, is unknown, never the group's 4 GiB alone. Each
/// group uses 512 MiB.
#[test]
fn a_reached_ancestor_is_read_and_a_refused_view_is_unknown() {
    let (one, four, half) = (G.to_string(), (4 * G).to_string(), (G / 2).to_string());
    let (one, four, half) = (one.as_str(), four.as_str(), half.as_str());
    const SELF_BOUND: &str = concat!(
        root!(),
        "30 23 0:26 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n",
        "40 30 0:26 /limited/worker /sys/fs/cgroup/limited/worker rw - cgroup2 cgroup2 rw\n"
    );
    let self_bound = vec![
        ("/proc/self/mountinfo", SELF_BOUND),
        ("/proc/self/cgroup", "0::/limited/worker"),
        ("/sys/fs/cgroup/limited/cgroup.controllers", "memory"),
        ("/sys/fs/cgroup/limited/memory.max", one),
        ("/sys/fs/cgroup/limited/memory.current", half),
        ("/sys/fs/cgroup/limited/worker/cgroup.controllers", "memory"),
        ("/sys/fs/cgroup/limited/worker/cgroup.procs", procs()),
        ("/sys/fs/cgroup/limited/worker/memory.max", four),
        ("/sys/fs/cgroup/limited/worker/memory.current", half),
    ];
    const BEHIND_REFUSED: &str = concat!(
        root!(),
        "30 23 0:26 / /x/cg rw - cgroup2 cgroup2 rw\n",
        "40 23 0:26 /limited/worker /cg-local rw - cgroup2 cgroup2 rw\n"
    );
    let behind_refused = vec![
        ("/proc/self/mountinfo", BEHIND_REFUSED),
        ("/proc/self/cgroup", "0::/limited/worker"),
        ("/x/cg/limited/cgroup.controllers", "memory"),
        ("/x/cg/limited/memory.max", one),
        ("/x/cg/limited/memory.current", half),
        ("/x/cg/limited/worker/cgroup.controllers", "memory"),
        ("/x/cg/limited/worker/cgroup.procs", procs()),
        ("/x/cg/limited/worker/memory.max", four),
        ("/x/cg/limited/worker/memory.current", half),
        ("/cg-local/cgroup.controllers", "memory"),
        ("/cg-local/cgroup.procs", procs()),
        ("/cg-local/memory.max", four),
        ("/cg-local/memory.current", half),
    ];
    check(vec![
        (
            "own group bound over itself",
            self_bound,
            "",
            Some(G),
            Some(G / 2),
        ),
        (
            "the full tree behind a way the kernel refuses",
            behind_refused,
            "/x",
            None,
            None,
        ),
    ]);
}

/// The kernel lets the reader pass a directory it may search but not list (mode 0111; sol-loop
/// r7 S2): a file below it is opened and read. As root, whom no mode refuses, the directory
/// doesn't refuse a listing either, and the test says so.
#[cfg(target_os = "linux")]
#[test]
fn a_directory_that_may_be_searched_but_not_listed_is_passed() {
    use cgroup::Fs;
    use std::os::unix::fs::PermissionsExt;
    let base = std::env::temp_dir().join(format!("nrese-exec-search-{}", std::process::id()));
    let inner = base.join("x").join("y");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join("memory.max"), "1073741824\n").unwrap();
    let file = std::fs::canonicalize(inner.join("memory.max")).unwrap();
    let x = base.join("x");
    std::fs::set_permissions(&x, std::fs::Permissions::from_mode(0o111)).unwrap();
    let kernel = cgroup::Kernel;
    let read = (|| {
        let mut handle = kernel.root()?;
        for name in file.iter().skip(1) {
            handle = kernel.open_at(&handle, &name.to_string_lossy())?;
        }
        kernel.read(&handle)
    })();
    let listed = std::fs::read_dir(&x).is_ok();
    std::fs::set_permissions(&x, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_dir_all(&base).unwrap();
    if listed {
        eprintln!("run as root: the mode refuses no listing, so this only shows the read");
    }
    assert_eq!(read.ok().as_deref(), Some("1073741824\n"));
}

/// The walk's opens grow with the group's depth, not its square: the group's ancestors are
/// directories on the way to it. A group 32 names below the root of a v2 hierarchy at
/// /sys/fs/cgroup needs at most 3 + 32 opens to reach, 2 for its `cgroup.procs` and
/// `cgroup.controllers`, and one `memory.max` for each of its 33 groups (the root's isn't there);
/// the least limit, 2 GiB ten names down, is read. A walk from the root for each ancestor opened
/// about 660. Counted are names opened, not the mount readings beside them.
#[test]
fn the_walk_opens_each_name_on_the_way_once() {
    let depth = 32;
    let mut paths = vec![String::from("/sys/fs/cgroup")];
    for level in 0..depth {
        paths.push(format!("{}/g{level}", paths[level]));
    }
    let leaf = &paths[depth];
    let mut files: Vec<(String, String)> = vec![
        ("/proc/self/mountinfo".into(), V2_MOUNT.into()),
        (
            "/proc/self/cgroup".into(),
            format!("0::{}", &leaf["/sys/fs/cgroup".len()..]),
        ),
        (format!("{leaf}/cgroup.procs"), procs().into()),
        (format!("{leaf}/cgroup.controllers"), "memory".into()),
    ];
    for (level, path) in paths.iter().enumerate().skip(1) {
        let limit = match level {
            10 => (2 * G).to_string(),
            l if l == depth => (4 * G).to_string(),
            _ => "max".into(),
        };
        files.push((format!("{path}/memory.max"), limit));
    }
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let kernel = system(&files, "");
    assert_eq!(cgroup::limit(&kernel), Ok(Some(2 * G)));
    let budget = 3 + depth + 2 + (depth + 1);
    let opens = kernel.opens.get();
    assert!(opens <= budget, "{opens} opens, more than {budget}");
}

/// Both of the kernel's mount readings name the mount a handle is on, a directory's and a
/// file's, across a mount (the root's file system, then /proc), the id `/proc/self/mountinfo`
/// lists for it: `statx`'s `STATX_MNT_ID`, which a kernel from 5.8 fills in, and
/// `/proc/self/fdinfo/N`, which the reading takes where `statx` gives none. A handle past a
/// mount carries that mount's id, which is how a mount over a group's point, over part of the
/// way or over a single file is told apart.
#[cfg(all(target_os = "linux", not(miri)))]
#[test]
fn both_mount_readings_name_the_mount_a_handle_is_on() {
    use cgroup::Fs;
    let kernel = cgroup::Kernel;
    let root = kernel.root().expect("/");
    let proc = kernel.open_at(&root, "proc").expect("/proc");
    let meminfo = kernel.open_at(&proc, "meminfo").expect("/proc/meminfo");
    // mountinfo's id for a mount point: the last line names the mount on top.
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
    let listed = |point: &str| -> Option<u64> {
        let line = mountinfo
            .lines()
            .rfind(|line| line.split(' ').nth(4) == Some(point));
        line.and_then(|line| line.split(' ').next()?.parse().ok())
    };
    let (root_mount, proc_mount) = (listed("/").expect("/"), listed("/proc").expect("/proc"));
    assert_ne!(root_mount, proc_mount, "/proc is a mount of its own");
    // A host may bind a file of its own over /proc/meminfo; then that is its mount.
    let meminfo_mount = listed("/proc/meminfo").unwrap_or(proc_mount);
    #[cfg(target_env = "gnu")]
    let statx_fills = {
        let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap();
        let mut parts = release.split(|c: char| !c.is_ascii_digit());
        let mut next = || parts.next().and_then(|part| part.parse::<u32>().ok());
        (next().unwrap(), next().unwrap()) >= (5, 8)
    };
    for (name, handle, mount) in [
        ("/", &root, root_mount),
        ("/proc", &proc, proc_mount),
        ("/proc/meminfo", &meminfo, meminfo_mount),
    ] {
        assert_eq!(kernel.mount(handle).ok(), Some(mount), "{name}");
        let fdinfo = cgroup::Kernel::mount_in_fdinfo(handle).ok();
        assert_eq!(fdinfo, Some(mount), "{name}: fdinfo");
        // The reading where `statx` gives none: fdinfo's.
        let without = cgroup::Kernel::mount_from(handle, None).ok();
        assert_eq!(without, Some(mount), "{name}: without statx");
        #[cfg(target_env = "gnu")]
        {
            let statx = cgroup::Kernel::mount_in_statx(handle);
            assert_eq!(statx, statx_fills.then_some(mount), "{name}: statx");
        }
    }
    #[cfg(target_env = "gnu")]
    {
        // The id is taken only where the answer's mask has the bit, whatever else it has.
        let basic = libc::STATX_BASIC_STATS;
        assert_eq!(cgroup::mount_filled(0, 7), None);
        assert_eq!(cgroup::mount_filled(basic, 7), None);
        assert_eq!(cgroup::mount_filled(libc::STATX_MNT_ID, 7), Some(7));
        assert_eq!(cgroup::mount_filled(basic | libc::STATX_MNT_ID, 7), Some(7));
    }
}

/// A held directory stays the directory it was opened as: renamed, and another directory put
/// under its old name, a name opened through it is its own, not the newcomer's.
#[cfg(all(target_os = "linux", not(miri)))]
#[test]
fn a_held_directory_is_the_one_opened_after_a_rename() {
    use cgroup::Fs;
    let base = std::env::temp_dir().join(format!("nrese-exec-held-{}", std::process::id()));
    std::fs::create_dir_all(base.join("group")).unwrap();
    std::fs::write(base.join("group").join("memory.max"), "1073741824\n").unwrap();
    let canonical = std::fs::canonicalize(&base).unwrap();
    let kernel = cgroup::Kernel;
    let read = (|| {
        let mut dir = kernel.root()?;
        for name in canonical.iter().skip(1) {
            dir = kernel.open_at(&dir, &name.to_string_lossy())?;
        }
        let held = kernel.open_at(&dir, "group")?;
        std::fs::rename(base.join("group"), base.join("moved"))?;
        std::fs::create_dir(base.join("group"))?;
        std::fs::write(base.join("group").join("memory.max"), "max\n")?;
        kernel.read(&kernel.open_at(&held, "memory.max")?)
    })();
    std::fs::remove_dir_all(&base).unwrap();
    assert_eq!(read.ok().as_deref(), Some("1073741824\n"));
}

/// A symbolic link on the way isn't followed: opened, a name is the link itself, and a name
/// below it isn't there (`ENOTDIR`); the directory the link names is reached by its own name.
#[cfg(all(target_os = "linux", not(miri)))]
#[test]
fn a_link_on_the_way_is_not_followed() {
    use cgroup::Fs;
    let base = std::env::temp_dir().join(format!("nrese-exec-link-{}", std::process::id()));
    std::fs::create_dir_all(base.join("real")).unwrap();
    std::fs::write(base.join("real").join("memory.max"), "1073741824\n").unwrap();
    std::os::unix::fs::symlink("real", base.join("link")).unwrap();
    let canonical = std::fs::canonicalize(&base).unwrap();
    let kernel = cgroup::Kernel;
    let opened = (|| {
        let mut dir = kernel.root()?;
        for name in canonical.iter().skip(1) {
            dir = kernel.open_at(&dir, &name.to_string_lossy())?;
        }
        let through_link = kernel
            .open_at(&kernel.open_at(&dir, "link")?, "memory.max")
            .map(|_| ());
        let real = kernel.read(&kernel.open_at(&kernel.open_at(&dir, "real")?, "memory.max")?)?;
        std::io::Result::Ok((through_link, real))
    })();
    std::fs::remove_dir_all(&base).unwrap();
    let (through_link, real) = opened.expect("the way to the test's directory");
    assert_eq!(
        through_link.map_err(|error| error.kind()),
        Err(std::io::ErrorKind::NotADirectory)
    );
    assert_eq!(real, "1073741824\n");
}

/// A kernel that shows no mount ids (`/proc/self/fdinfo` has `mnt_id` from Linux 3.15) can't
/// say which mount a file was reached through, so a limit that may apply is unknown; the same
/// files with mount ids read as the group's 4 GiB.
#[test]
fn without_mount_ids_a_limit_is_unknown() {
    let four = (4 * G).to_string();
    let files = vec![
        ("/proc/self/mountinfo", V2_MOUNT),
        ("/proc/self/cgroup", "0::/app.slice"),
        ("/sys/fs/cgroup/app.slice/cgroup.controllers", "memory"),
        ("/sys/fs/cgroup/app.slice/cgroup.procs", procs()),
        ("/sys/fs/cgroup/app.slice/memory.max", four.as_str()),
        ("/sys/fs/cgroup/app.slice/memory.current", "0"),
    ];
    let shown = system(&files, "");
    let mut hidden = system(&files, "");
    hidden.no_mount_ids = true;
    assert_eq!(
        (linux_available(&shown), linux_available(&hidden)),
        (Some(4 * G), None)
    );
    assert_eq!(
        (linux_remaining(&shown), linux_remaining(&hidden)),
        (Some(4 * G), None)
    );
}

/// The kernel's own answers where the test runs, a fleet host's job among them: the process's
/// memory group is found through a view, its `cgroup.procs` (or `cgroup.threads`) lists the
/// process, and its limits are read. Where a plain view of the whole v2 hierarchy is mounted at
/// /sys/fs/cgroup and the group is below the namespace's root, the limit read is the least
/// `memory.max` on the group's path, read there directly. A host whose kernel shows the process
/// no memory hierarchy fails by that name rather than passing.
#[cfg(target_os = "linux")]
#[test]
fn the_kernel_shows_the_process_its_own_group() {
    let membership = std::fs::read_to_string("/proc/self/cgroup").expect("/proc/self/cgroup");
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").expect("/proc/self/mountinfo");
    let v1 = membership.lines().find(|line| {
        line.split(':')
            .nth(1)
            .is_some_and(|c| c.split(',').any(|c| c == "memory"))
    });
    let v2 = membership.lines().find_map(|line| line.strip_prefix("0::"));
    assert!(
        v1.is_some() || v2.is_some(),
        "no cgroup memory hierarchy on this host: /proc/self/cgroup reads {membership:?}"
    );
    let limit = cgroup::limit(&cgroup::Kernel);
    let left = cgroup::left(&cgroup::Kernel);
    eprintln!("membership {membership:?}; limit {limit:?}; left {left:?}");
    assert!(
        limit.is_ok() && left.is_ok(),
        "own group not found, listed or read: limit {limit:?}, left {left:?}; \
         /proc/self/cgroup {membership:?}; mountinfo {mounts:?}"
    );
    // The independent reading: a plain whole-hierarchy view, no step above the root.
    let plain = mounts.lines().any(|line| {
        let fields: Vec<&str> = line.split(' ').collect();
        fields.get(3) == Some(&"/")
            && fields.get(4) == Some(&"/sys/fs/cgroup")
            && line.contains(" - cgroup2 ")
    });
    match v2.map(str::trim) {
        Some(path) if v1.is_none() && plain && !path.split('/').any(|name| name == "..") => {
            let mut at = String::from("/sys/fs/cgroup");
            let mut least: Option<u64> = None;
            for name in std::iter::once("").chain(path.split('/').filter(|n| !n.is_empty())) {
                if !name.is_empty() {
                    at = format!("{at}/{name}");
                }
                let value = std::fs::read_to_string(format!("{at}/memory.max"))
                    .ok()
                    .and_then(|text| text.trim().parse::<u64>().ok());
                least = super::least([least, value]);
            }
            eprintln!("read directly at /sys/fs/cgroup{path}: {least:?}");
            assert_eq!(limit, Ok(least));
        }
        _ => eprintln!("no plain v2 view of the group: the direct reading isn't made"),
    }
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
