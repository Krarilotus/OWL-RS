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

/// cgroup v2 where systemd mounts it.
const V2_MOUNT: &str = "30 23 0:26 / /sys/fs/cgroup rw,nosuid,nodev,noexec,relatime shared:4 - \
                        cgroup2 cgroup2 rw,nsdelegate,memory_recursiveprot\n";

/// cgroup v1's memory hierarchy where systemd mounts it, beside another controller's.
const V1_MOUNT: &str = "34 25 0:30 / /sys/fs/cgroup/cpu,cpuacct rw,nosuid - cgroup cgroup \
                        rw,cpu,cpuacct\n35 25 0:31 / /sys/fs/cgroup/memory rw,nosuid,nodev - \
                        cgroup cgroup rw,memory\n";

/// A file system: `/proc/meminfo` as [`MEMINFO`] unless `files` says otherwise, the other
/// `files`, and `denied` (if not empty) refused (permission); any other file is not found.
fn system(
    files: &[(&str, &str)],
    denied: &'static str,
) -> impl Fn(&str) -> std::io::Result<String> {
    let mut map: HashMap<String, String> =
        HashMap::from([("/proc/meminfo".to_owned(), MEMINFO.to_owned())]);
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
        ("/sys/fs/cgroup/app.slice/memory.max", four),
        ("/sys/fs/cgroup/app.slice/memory.current", used),
    ];
    let custom = "40 23 0:26 / /custom/cg rw,relatime - cgroup2 cgroup2 rw\n";
    let escaped = "40 23 0:26 / /custom\\040cg rw,relatime - cgroup2 cgroup2 rw\n";
    let unnamespaced =
        "35 25 0:31 /docker/abc /sys/fs/cgroup/memory rw - cgroup cgroup rw,memory\n";
    let neighbour = "35 25 0:31 /docker/ab /sys/fs/cgroup/memory rw - cgroup cgroup rw,memory\n";
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
    for (name, files, denied, available, remaining) in rows {
        assert_eq!(
            linux_available(system(&files, denied)),
            available,
            "{name}: available"
        );
        assert_eq!(
            linux_remaining(system(&files, denied)),
            remaining,
            "{name}: remaining"
        );
    }
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
