//! The process's memory, for limits that keep a store from taking the machine: what the
//! process holds now ([`process_bytes`]), what it may use ([`available_bytes`]: the
//! container's limit, else the machine's memory), and a cheap check for long operations
//! ([`MemoryWatch`]).
//!
//! What "holds" means per platform: on Windows the committed private bytes (what the
//! system must back, and what a job object's limit counts); on Linux the resident set.
//! Queries account their intermediate results exactly ([`crate::budget`]); this is the
//! coarse limit for everything else (materialisation, reasoning, loads), measured rather
//! than accounted.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// The private memory the process holds now (anonymous on Linux, committed private on
/// Windows; mapped files excluded, since the kernel reclaims them); `None` where the
/// platform doesn't say.
pub fn process_bytes() -> Option<u64> {
    imp::process_bytes()
}

/// The most [`process_bytes`] has been since the process started, where the platform
/// keeps it (Windows: the peak committed private bytes; Linux: the peak resident set,
/// mapped files included).
pub fn peak_process_bytes() -> Option<u64> {
    imp::peak_process_bytes()
}

/// The page faults the process has taken since it started (soft and hard: a mapped file's
/// pages count as they are first touched), where the platform keeps them.
pub fn page_faults() -> Option<u64> {
    imp::page_faults()
}

/// The memory resident in the process's working set now, mapped file pages included;
/// `None` where the platform doesn't say.
pub fn resident_bytes() -> Option<u64> {
    imp::resident_bytes()
}

/// The memory this process may use: the smallest of the machine's physical memory and the
/// limits it runs under (on Linux its cgroup's and every ancestor's, so a container, a
/// Kubernetes pod or a systemd scope counts; on Windows its job object's); `None` where the
/// platform doesn't say.
pub fn available_bytes() -> Option<u64> {
    imp::available_bytes()
}

/// Whether the process has gone past a memory limit, for the stop callbacks of long
/// operations. The memory is read at most every [`MemoryWatch::INTERVAL_MICROS`], so a
/// check in a hot loop costs an atomic load and a clock read; once over, it stays over.
#[derive(Debug)]
pub struct MemoryWatch {
    limit: u64,
    started: Instant,
    /// Microseconds after `started` of the last reading.
    last: AtomicU64,
    over: AtomicBool,
}

impl MemoryWatch {
    /// How often the memory is read, at most.
    pub const INTERVAL_MICROS: u64 = 10_000;

    /// A watch for `limit` bytes of the process's memory.
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            started: Instant::now(),
            last: AtomicU64::new(0),
            over: AtomicBool::new(false),
        }
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// Whether the process holds more than the limit (as of the latest reading).
    pub fn exceeded(&self) -> bool {
        if self.over.load(Ordering::Relaxed) {
            return true;
        }
        let now = u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let last = self.last.load(Ordering::Relaxed);
        if now.saturating_sub(last) < Self::INTERVAL_MICROS && last != 0 {
            return false;
        }
        // One reader per interval; the others go on with the old answer.
        if self
            .last
            .compare_exchange(last, now.max(1), Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }
        let over = process_bytes().is_some_and(|held| held > self.limit);
        if over {
            self.over.store(true, Ordering::Relaxed);
        }
        over
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::System::JobObjects::{
        JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        QueryInformationJobObject,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    fn counters() -> Option<PROCESS_MEMORY_COUNTERS_EX> {
        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>()).ok()?,
            ..Default::default()
        };
        // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing, and
        // `counters` is a valid, writable PROCESS_MEMORY_COUNTERS_EX whose size is `cb`;
        // the EX form starts with the plain one, as the API allows.
        let ok = unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                std::ptr::from_mut(&mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
                counters.cb,
            )
        };
        (ok != 0).then_some(counters)
    }

    pub fn process_bytes() -> Option<u64> {
        counters().map(|counters| counters.PrivateUsage as u64)
    }

    pub fn peak_process_bytes() -> Option<u64> {
        counters().map(|counters| counters.PeakPagefileUsage as u64)
    }

    pub fn page_faults() -> Option<u64> {
        counters().map(|counters| u64::from(counters.PageFaultCount))
    }

    pub fn resident_bytes() -> Option<u64> {
        counters().map(|counters| counters.WorkingSetSize as u64)
    }

    pub fn available_bytes() -> Option<u64> {
        let mut status = MEMORYSTATUSEX {
            dwLength: u32::try_from(std::mem::size_of::<MEMORYSTATUSEX>()).ok()?,
            ..Default::default()
        };
        // SAFETY: `status` is a valid, writable MEMORYSTATUSEX with dwLength set, as the
        // API requires.
        let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
        let machine = (ok != 0).then_some(status.ullTotalPhys)?;
        Some(job_limit().map_or(machine, |limit| limit.min(machine)))
    }

    /// The memory limit of the job object the process runs in (a job-wide or a per-process
    /// one, whichever is smaller), if it runs in one that has a limit. Only the innermost job
    /// is visible: a process started by `cargo run` or `cargo test` sits in cargo's own job
    /// (which has no limit), so a cap on an enclosing job is seen only by processes started
    /// directly, as a service or a container starts the server.
    pub(super) fn job_limit() -> Option<u64> {
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        let size =
            u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).ok()?;
        // SAFETY: a null handle names the job of the calling process; `info` is a valid,
        // writable JOBOBJECT_EXTENDED_LIMIT_INFORMATION of `size` bytes, and the returned
        // length isn't wanted. Outside a job the call fails.
        let ok = unsafe {
            QueryInformationJobObject(
                std::ptr::null_mut(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_mut(&mut info).cast(),
                size,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return None;
        }
        let flags = info.BasicLimitInformation.LimitFlags;
        [
            (JOB_OBJECT_LIMIT_JOB_MEMORY, info.JobMemoryLimit),
            (JOB_OBJECT_LIMIT_PROCESS_MEMORY, info.ProcessMemoryLimit),
        ]
        .into_iter()
        .filter(|&(flag, limit)| flags & flag != 0 && limit > 0)
        .map(|(_, limit)| limit as u64)
        .min()
    }
}

#[cfg(target_os = "linux")]
mod imp {
    /// The size of a page, which /proc/self/statm counts in. 4 KiB on the platforms the
    /// store runs on; reading it would need libc.
    const PAGE: u64 = 4096;

    /// The anonymous resident memory: resident minus shared (file-backed and shared
    /// memory), as Windows's committed private bytes count. The store's mapped checkpoint
    /// and dictionary are file pages the kernel reclaims under pressure; counted, they
    /// could stop a materialisation that fits (LUBM 1000 maps about 5 GB).
    pub fn process_bytes() -> Option<u64> {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let mut fields = statm.split_whitespace().skip(1);
        let resident: u64 = fields.next()?.parse().ok()?;
        let shared: u64 = fields.next()?.parse().ok()?;
        Some(resident.saturating_sub(shared) * PAGE)
    }

    pub fn page_faults() -> Option<u64> {
        // Fields 10 and 12 of /proc/self/stat (minor and major faults), counted after the
        // command name, which may hold spaces but ends with the last ')'.
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
        let minor: u64 = fields.get(7)?.parse().ok()?;
        let major: u64 = fields.get(9)?.parse().ok()?;
        Some(minor + major)
    }

    pub fn resident_bytes() -> Option<u64> {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let resident: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        Some(resident * PAGE)
    }

    pub fn peak_process_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
        let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kib * 1024)
    }

    pub fn available_bytes() -> Option<u64> {
        let read = |path: &str| std::fs::read_to_string(path).ok();
        let machine = read("/proc/meminfo").and_then(|text| {
            let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
            let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
            Some(kib * 1024)
        })?;
        let limit = read("/proc/self/cgroup").and_then(|own| super::cgroup::limit(&own, read));
        Some(limit.map_or(machine, |limit| limit.min(machine)))
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    pub fn process_bytes() -> Option<u64> {
        None
    }

    pub fn peak_process_bytes() -> Option<u64> {
        None
    }

    pub fn page_faults() -> Option<u64> {
        None
    }

    pub fn resident_bytes() -> Option<u64> {
        None
    }

    pub fn available_bytes() -> Option<u64> {
        None
    }
}

/// The memory limit a Linux process runs under, from its cgroup membership: cgroup v2 (a
/// `0::/path` line) reads `memory.max` in the process's group and every ancestor's, v1 (a
/// line naming the `memory` controller) `memory.limit_in_bytes`; the smallest counts. "max"
/// and v1's page-aligned maximum mean no limit. `read` reads a file's text.
#[cfg(any(target_os = "linux", test))]
mod cgroup {
    /// Above this a limit means "none".
    const NONE_ABOVE: u64 = 1 << 62;

    pub fn limit(own: &str, read: impl Fn(&str) -> Option<String>) -> Option<u64> {
        let (root, file, path) = own.lines().find_map(|line| {
            let mut fields = line.splitn(3, ':');
            let (_, controllers, path) = (fields.next()?, fields.next()?, fields.next()?);
            if controllers.is_empty() {
                Some(("/sys/fs/cgroup", "memory.max", path))
            } else if controllers.split(',').any(|c| c == "memory") {
                Some(("/sys/fs/cgroup/memory", "memory.limit_in_bytes", path))
            } else {
                None
            }
        })?;
        let mut dir = path.trim().trim_end_matches('/').to_owned();
        let mut smallest: Option<u64> = None;
        loop {
            if let Some(value) = read(&format!("{root}{dir}/{file}"))
                .and_then(|text| text.trim().parse::<u64>().ok())
                .filter(|&value| value > 0 && value < NONE_ABOVE)
            {
                smallest = Some(smallest.map_or(value, |s| s.min(value)));
            }
            match dir.rfind('/') {
                Some(cut) => dir.truncate(cut),
                None => break,
            }
        }
        smallest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cgroup_limit_is_the_smallest_on_the_path_up() {
        use std::collections::HashMap;
        let files: HashMap<&str, &str> = HashMap::from([
            ("/sys/fs/cgroup/user.slice/memory.max", "max"),
            (
                "/sys/fs/cgroup/user.slice/user-1000.slice/memory.max",
                "34359738368",
            ),
            (
                "/sys/fs/cgroup/user.slice/user-1000.slice/run.scope/memory.max",
                "17179869184",
            ),
            (
                "/sys/fs/cgroup/memory/docker/abc/memory.limit_in_bytes",
                "4294967296",
            ),
            (
                "/sys/fs/cgroup/memory/free/memory.limit_in_bytes",
                "9223372036854771712",
            ),
        ]);
        let read = |path: &str| files.get(path).map(|text| (*text).to_owned());
        // v2: the scope's 16 GiB, under its slice's 32 GiB.
        let own = "0::/user.slice/user-1000.slice/run.scope";
        assert_eq!(cgroup::limit(own, read), Some(16 << 30));
        // v1: the memory controller's line, not the others'.
        let own = "4:cpu,cpuacct:/docker/abc\n3:memory:/docker/abc";
        assert_eq!(cgroup::limit(own, read), Some(4 << 30));
        // No limit anywhere, or v1's "none" value.
        assert_eq!(cgroup::limit("0::/system.slice/x.service", read), None);
        assert_eq!(cgroup::limit("3:memory:/free", read), None);
    }

    /// A job object's memory limit counts as the memory the process may use: the test puts
    /// its own process into a job with a limit below the machine's memory (it becomes the
    /// innermost job) and reads it back.
    #[cfg(windows)]
    #[test]
    fn a_job_objects_memory_limit_counts_as_the_memory_available() {
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_JOB_MEMORY,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let machine = available_bytes().expect("the platform reports the memory it may use");
        // Above what the tests use, below the machine; whole pages, as Windows keeps it.
        let limit = (machine / 4 * 3).max(4 << 30) & !0xFFF;
        if limit >= machine {
            return;
        }
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_JOB_MEMORY;
        info.JobMemoryLimit = usize::try_from(limit).expect("a limit that fits");
        let size = u32::try_from(std::mem::size_of_val(&info)).expect("a small struct");
        // SAFETY: a fresh unnamed job; `info` is a valid JOBOBJECT_EXTENDED_LIMIT_INFORMATION
        // of `size` bytes; the current process's pseudo-handle needs no closing. The job
        // handle stays open for the process's life, so the job lives as long as the test
        // binary.
        let assigned = unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            !job.is_null()
                && SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&info).cast(),
                    size,
                ) != 0
                && AssignProcessToJobObject(job, GetCurrentProcess()) != 0
        };
        assert!(assigned, "{}", std::io::Error::last_os_error());
        assert_eq!(imp::job_limit(), Some(limit));
        assert_eq!(available_bytes(), Some(limit));
    }

    #[test]
    fn the_process_and_the_machine_report_their_memory() {
        let held = process_bytes().expect("the platform reports the process's memory");
        let total = available_bytes().expect("the platform reports the memory it may use");
        assert!(held > 0 && held < total, "{held} of {total}");
        let resident = resident_bytes().expect("the platform reports the working set");
        assert!(resident > 0 && resident < total, "{resident} of {total}");
        // Touching fresh pages faults them in.
        let before = page_faults().expect("the platform counts page faults");
        let pages = vec![1u8; 64 << 20];
        let touched: u64 = pages.iter().step_by(4096).map(|&b| u64::from(b)).sum();
        assert!(page_faults().unwrap() > before, "{touched} pages touched");
    }

    #[test]
    fn a_watch_trips_past_its_limit_and_stays_tripped() {
        assert!(!MemoryWatch::new(u64::MAX).exceeded());
        let watch = MemoryWatch::new(1);
        assert!(watch.exceeded());
        assert!(watch.exceeded());
    }
}
