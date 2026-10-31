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
/// mapped files included). A [`start_peak_window`] doesn't lower it.
pub fn peak_process_bytes() -> Option<u64> {
    let window = imp::peak_process_bytes()?;
    let before = PEAK_BEFORE_WINDOW.fetch_max(window, Ordering::Relaxed);
    Some(before.max(window))
}

/// Starts a window for [`window_peak_bytes`], the peak of one phase of a long run. On Linux
/// the kernel's high-water mark is kept for [`peak_process_bytes`] and then reset to the
/// resident set (`/proc/self/clear_refs`), which also resets what `getrusage` and
/// `time -v` report as the maximum resident set; elsewhere no window starts and `false`
/// is returned. Starting a window by writing `clear_refs` directly loses the peak before it.
pub fn start_peak_window() -> bool {
    peak_process_bytes();
    let started = imp::reset_peak();
    WINDOW_UNAVAILABLE.store(!started, Ordering::Relaxed);
    started
}

/// The most the process has held since the last [`start_peak_window`] (since the start
/// without one); `None` where the platform doesn't say, and where a window was asked for
/// but couldn't start (Windows keeps one peak for the process's lifetime: that would be
/// every phase before as well, not this one's).
pub fn window_peak_bytes() -> Option<u64> {
    if WINDOW_UNAVAILABLE.load(Ordering::Relaxed) {
        return None;
    }
    imp::peak_process_bytes()
}

/// Whether the last [`start_peak_window`] couldn't start one.
static WINDOW_UNAVAILABLE: AtomicBool = AtomicBool::new(false);

/// What [`peak_process_bytes`] measures on this platform, by name: `vmhwm` (Linux, the peak
/// resident set, mapped files included), `peak_private_commit` (Windows, the peak committed
/// private bytes, `PeakPagefileUsage`), or `none`.
pub fn peak_kind() -> &'static str {
    if cfg!(windows) {
        "peak_private_commit"
    } else if cfg!(target_os = "linux") {
        "vmhwm"
    } else {
        "none"
    }
}

/// The peak of the process's working set (Windows: `PeakWorkingSetSize`, the most physical
/// memory it held at once, mapped and shared pages included); `None` elsewhere.
pub fn peak_working_set_bytes() -> Option<u64> {
    imp::peak_working_set_bytes()
}

/// The highest mark the kernel had when a peak window started.
static PEAK_BEFORE_WINDOW: AtomicU64 = AtomicU64::new(0);

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

/// The memory the process could still take now, as the system reports it: on Linux the
/// least of the machine's available memory (`MemAvailable`) and what its cgroups leave
/// (each limit less the group's use, inactive file pages not counted); on Windows the
/// least of the machine's available physical memory and what its job object leaves (its
/// job-wide limit less the job's committed memory, its per-process limit less the process's
/// own); `None` where the platform doesn't say. Unlike [`available_bytes`], the total, it
/// changes from one reading to the next, and counts as used what the allocator holds
/// freed, until it returns it. A cap for admitting an operation as it starts, never a
/// reservation: what it reports is taken by whoever allocates first.
pub fn remaining_bytes() -> Option<u64> {
    imp::remaining_bytes()
}

/// The process's memory limit, for long operations whose caller passes no watch of its
/// own: set by whoever owns the process's configuration (server startup, or an explicit
/// embedding host), never by opening a store. Polled through [`process_limit_exceeded`]
/// by the operations that can grow without bound (the reasoner's materialisations and
/// commits). 0: none.
static PROCESS_LIMIT: AtomicU64 = AtomicU64::new(0);
/// Microseconds since [`PROCESS_EPOCH`] of the last reading for the process limit.
static PROCESS_LAST: AtomicU64 = AtomicU64::new(0);
/// Whether that reading was over the limit.
static PROCESS_OVER: AtomicBool = AtomicBool::new(false);
static PROCESS_EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Sets the process's memory limit in bytes (0: none). See [`process_limit_exceeded`].
pub fn set_process_limit(limit: u64) {
    PROCESS_LIMIT.store(limit, Ordering::Relaxed);
    PROCESS_LAST.store(0, Ordering::Relaxed);
    PROCESS_OVER.store(false, Ordering::Relaxed);
}

/// Whether the process's anonymous memory may be backed by transparent huge pages
/// (Linux; elsewhere there are none to switch, and nothing is done). An allocator that
/// asks for them (mimalloc does, for its arenas) then makes a whole 2 MiB page resident
/// for one touched byte, and gives back nothing of it while one of its small pages is in
/// use: under a server's many threads that kept 3 times the memory the system allocator
/// holds for the same work (soak, 7 October 2026). Applies from the next page touched,
/// arenas reserved earlier included. Returns whether the setting was applied.
pub fn set_transparent_huge_pages(allowed: bool) -> bool {
    imp::set_transparent_huge_pages(allowed)
}

/// The process's memory limit, if one is set ([`set_process_limit`]).
pub fn process_limit() -> Option<u64> {
    Some(PROCESS_LIMIT.load(Ordering::Relaxed)).filter(|&limit| limit > 0)
}

/// Whether the process holds more than its memory limit ([`set_process_limit`]), as of a
/// reading at most [`MemoryWatch::INTERVAL_MICROS`] old: an atomic load and a clock read
/// in a hot loop. Unlike a [`MemoryWatch`] it doesn't stay over: an operation the limit
/// stopped frees its memory, and the next one may run.
pub fn process_limit_exceeded() -> bool {
    let Some(limit) = process_limit() else {
        return false;
    };
    let epoch = PROCESS_EPOCH.get_or_init(Instant::now);
    let now = u64::try_from(epoch.elapsed().as_micros())
        .unwrap_or(u64::MAX)
        .max(1);
    let last = PROCESS_LAST.load(Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) < MemoryWatch::INTERVAL_MICROS {
        return PROCESS_OVER.load(Ordering::Relaxed);
    }
    // One reader per interval; the others go on with the last answer.
    if PROCESS_LAST
        .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return PROCESS_OVER.load(Ordering::Relaxed);
    }
    let over = process_bytes().is_some_and(|held| held > limit);
    PROCESS_OVER.store(over, Ordering::Relaxed);
    over
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
    /// Windows backs no allocation with large pages unless asked (and privileged).
    pub fn set_transparent_huge_pages(_allowed: bool) -> bool {
        false
    }

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        IsProcessInJob, JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
        JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOBOBJECTINFOCLASS,
        JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    fn counters() -> Option<PROCESS_MEMORY_COUNTERS_EX> {
        // SAFETY: GetCurrentProcess returns a pseudo-handle, valid and needing no closing.
        unsafe { counters_of(GetCurrentProcess()) }
    }

    /// The memory counters of the process `process` is a handle of.
    ///
    /// # Safety
    /// `process` is a valid process handle with query access.
    unsafe fn counters_of(process: HANDLE) -> Option<PROCESS_MEMORY_COUNTERS_EX> {
        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>()).ok()?,
            ..Default::default()
        };
        // SAFETY: `process` is valid (the caller's contract), and `counters` is a valid,
        // writable PROCESS_MEMORY_COUNTERS_EX whose size is `cb`; the EX form starts with the
        // plain one, as the API allows.
        let ok = unsafe {
            GetProcessMemoryInfo(
                process,
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

    pub fn peak_working_set_bytes() -> Option<u64> {
        counters().map(|counters| counters.PeakWorkingSetSize as u64)
    }

    /// Windows keeps the peak for the process's lifetime.
    pub fn reset_peak() -> bool {
        false
    }

    pub fn page_faults() -> Option<u64> {
        counters().map(|counters| u64::from(counters.PageFaultCount))
    }

    pub fn resident_bytes() -> Option<u64> {
        counters().map(|counters| counters.WorkingSetSize as u64)
    }

    fn status() -> Option<MEMORYSTATUSEX> {
        let mut status = MEMORYSTATUSEX {
            dwLength: u32::try_from(std::mem::size_of::<MEMORYSTATUSEX>()).ok()?,
            ..Default::default()
        };
        // SAFETY: `status` is a valid, writable MEMORYSTATUSEX with dwLength set, as the
        // API requires.
        let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
        (ok != 0).then_some(status)
    }

    pub fn available_bytes() -> Option<u64> {
        let machine = status()?.ullTotalPhys;
        Some(job_limit().map_or(machine, |limit| limit.min(machine)))
    }

    pub fn remaining_bytes() -> Option<u64> {
        super::windows_remaining(status().map(|status| status.ullAvailPhys), job_left())
    }

    /// One class of information about the job object the process runs in; `None` outside a
    /// job. `T` must be the class's structure.
    fn job<T: Default>(class: JOBOBJECTINFOCLASS) -> Option<T> {
        let mut info = T::default();
        let size = u32::try_from(std::mem::size_of::<T>()).ok()?;
        // SAFETY: a null handle names the job of the calling process; `info` is a valid,
        // writable structure of `size` bytes, the one `class` fills (the callers' contract),
        // and the returned length isn't wanted. Outside a job the call fails.
        let ok = unsafe {
            QueryInformationJobObject(
                std::ptr::null_mut(),
                class,
                std::ptr::from_mut(&mut info).cast(),
                size,
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(info)
    }

    /// The job object's memory limits that are set: job-wide and per-process.
    fn job_limits() -> Option<[Option<u64>; 2]> {
        let info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = job(JobObjectExtendedLimitInformation)?;
        let flags = info.BasicLimitInformation.LimitFlags;
        let set = |flag, limit: usize| (flags & flag != 0 && limit > 0).then_some(limit as u64);
        Some([
            set(JOB_OBJECT_LIMIT_JOB_MEMORY, info.JobMemoryLimit),
            set(JOB_OBJECT_LIMIT_PROCESS_MEMORY, info.ProcessMemoryLimit),
        ])
    }

    /// The memory limit of the job object the process runs in (a job-wide or a per-process
    /// one, whichever is smaller), if it runs in one that has a limit. Only the innermost job
    /// is visible: a process started by `cargo run` or `cargo test` sits in cargo's own job
    /// (which has no limit), so a cap on an enclosing job is seen only by processes started
    /// directly, as a service or a container starts the server.
    pub(super) fn job_limit() -> Option<u64> {
        super::least(job_limits()?)
    }

    /// What the job object's limits leave ([`super::job_left_from`]): its job-wide limit less
    /// the memory its processes commit now ([`job_committed`]), its per-process limit less the
    /// process's own committed memory. `Some(None)` outside a job or without a limit; `None`
    /// where a limit is set whose use can't be read.
    pub(super) fn job_left() -> Option<Option<u64>> {
        super::job_left_from(in_job(), job_limits, job_committed, super::process_bytes)
    }

    /// Whether the process runs in a job object; `None` where that can't be asked. Told
    /// apart from a failed query of the job's limits, which outside a job fails too.
    fn in_job() -> Option<bool> {
        let mut result = 0;
        // SAFETY: the current process's pseudo-handle needs no closing; a null job handle asks
        // about any job; `result` is a valid, writable BOOL.
        let ok = unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut result) };
        (ok != 0).then_some(result != 0)
    }

    /// The memory the processes of the job object the process runs in commit now: the sum
    /// of each one's committed private memory, which is what a job's memory limit counts.
    /// From documented calls only: the job's process list, and each process's counters. A
    /// process that ended since the list was read commits nothing; one that can't be asked
    /// leaves the sum unknown.
    fn job_committed() -> Option<u64> {
        /// The ids that fit after the list's two counts, to start with.
        const FIRST: usize = 64;
        /// Windows's answer for a process id that no longer names a process.
        const ERROR_INVALID_PARAMETER: i32 = 87;
        /// The words before the first id: the list's two u32 counts, one word on a 64-bit
        /// host, two on a 32-bit one (Sol, measure-pool r2), as the structure lays them out.
        const HEADER: usize = std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList)
            / std::mem::size_of::<usize>();
        const _: () = assert!(
            std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList)
                == HEADER * std::mem::size_of::<usize>()
                && HEADER > 0,
            "the ids start at a whole word after the counts"
        );
        // The list: its counts in the first HEADER words, then a word per process id.
        let mut words = vec![0usize; HEADER + FIRST];
        let ids = loop {
            let size = u32::try_from(std::mem::size_of_val(words.as_slice())).ok()?;
            // SAFETY: a null handle names the job of the calling process; `words` is a valid,
            // writable buffer of `size` bytes, aligned for and starting with a
            // JOBOBJECT_BASIC_PROCESS_ID_LIST, whose ids follow its counts word by word.
            let ok = unsafe {
                QueryInformationJobObject(
                    std::ptr::null_mut(),
                    JobObjectBasicProcessIdList,
                    words.as_mut_ptr().cast(),
                    size,
                    std::ptr::null_mut(),
                )
            };
            // SAFETY: the buffer starts with the list's header, written or still zeroed, and
            // is aligned for it.
            let header = unsafe {
                words
                    .as_ptr()
                    .cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>()
                    .read()
            };
            let assigned = header.NumberOfAssignedProcesses as usize;
            if ok != 0 && header.NumberOfProcessIdsInList as usize == assigned {
                break assigned;
            }
            // Refused with room for every process: outside a job, or no answer.
            if ok == 0 && HEADER + assigned <= words.len() {
                return None;
            }
            words.resize(HEADER + assigned + FIRST, 0);
        };
        let mut committed = 0u64;
        for &id in &words[HEADER..HEADER + ids] {
            let id = u32::try_from(id).ok()?;
            // SAFETY: opens a handle for query only, closed below.
            let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, id) };
            if process.is_null() {
                if std::io::Error::last_os_error().raw_os_error() == Some(ERROR_INVALID_PARAMETER) {
                    continue;
                }
                return None;
            }
            // SAFETY: `process` is the handle just opened with query access.
            let counters = unsafe { counters_of(process) };
            // SAFETY: closes the handle opened above, once.
            unsafe { CloseHandle(process) };
            committed += counters?.PrivateUsage as u64;
        }
        Some(committed)
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

    pub fn set_transparent_huge_pages(allowed: bool) -> bool {
        use std::ffi::{c_int, c_ulong};
        /// `prctl`'s option for the process's transparent huge pages (linux/prctl.h).
        const PR_SET_THP_DISABLE: c_int = 41;
        unsafe extern "C" {
            fn prctl(option: c_int, ...) -> c_int;
        }
        let disable = c_ulong::from(!allowed);
        // SAFETY: PR_SET_THP_DISABLE takes one integer argument (the rest must be zero)
        // and reads no memory of ours.
        unsafe {
            prctl(
                PR_SET_THP_DISABLE,
                disable,
                0 as c_ulong,
                0 as c_ulong,
                0 as c_ulong,
            ) == 0
        }
    }

    pub fn peak_working_set_bytes() -> Option<u64> {
        None
    }

    pub fn peak_process_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
        let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kib * 1024)
    }

    /// Writing 5 resets the high-water mark to the resident set (Linux 4.0 on).
    pub fn reset_peak() -> bool {
        std::fs::write("/proc/self/clear_refs", "5").is_ok()
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

    pub fn remaining_bytes() -> Option<u64> {
        let read = |path: &str| std::fs::read_to_string(path).ok();
        let meminfo = read("/proc/meminfo");
        let own = read("/proc/self/cgroup");
        super::linux_remaining(meminfo.as_deref(), own.as_deref(), read)
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    pub fn set_transparent_huge_pages(_allowed: bool) -> bool {
        false
    }

    pub fn process_bytes() -> Option<u64> {
        None
    }

    pub fn peak_process_bytes() -> Option<u64> {
        None
    }

    pub fn peak_working_set_bytes() -> Option<u64> {
        None
    }

    pub fn reset_peak() -> bool {
        false
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

    pub fn remaining_bytes() -> Option<u64> {
        None
    }
}

/// The smallest of the readings there are; `None` if there are none.
#[cfg(any(windows, target_os = "linux", test))]
fn least(readings: impl IntoIterator<Item = Option<u64>>) -> Option<u64> {
    readings.into_iter().flatten().min()
}

/// What a set of limits leaves: the least of each limit less what is used of it.
#[cfg(any(windows, test))]
fn left(limits: impl IntoIterator<Item = Option<(u64, u64)>>) -> Option<u64> {
    least(
        limits
            .into_iter()
            .map(|limit| limit.map(|(limit, used)| limit.saturating_sub(used))),
    )
}

/// What a Windows job object's limits leave, from whether the process is in a job, the job's
/// job-wide and per-process limits (those set), and readings of what each counts: the job's
/// committed memory and the process's own. `Some(None)`: no job, or no limit set; `None`: in a
/// job whose limits can't be read, or with a limit whose use can't be read, or where whether
/// it is in a job can't be asked, so what is left isn't known (Sol, measure-pool r2 and r3:
/// not the machine's).
#[cfg(any(windows, test))]
fn job_left_from(
    in_job: Option<bool>,
    limits: impl FnOnce() -> Option<[Option<u64>; 2]>,
    job_committed: impl FnOnce() -> Option<u64>,
    own_committed: impl FnOnce() -> Option<u64>,
) -> Option<Option<u64>> {
    if !in_job? {
        return Some(None);
    }
    let [job_wide, per_process] = limits()?;
    let job_wide = match job_wide {
        Some(limit) => Some((limit, job_committed()?)),
        None => None,
    };
    let per_process = match per_process {
        Some(limit) => Some((limit, own_committed()?)),
        None => None,
    };
    Some(left([job_wide, per_process]))
}

/// The memory a Windows process could still take: the least of the machine's available
/// memory and what its job leaves; `None` where the job's reading isn't known.
#[cfg(any(windows, test))]
fn windows_remaining(machine: Option<u64>, job: Option<Option<u64>>) -> Option<u64> {
    least([machine, job?])
}

/// The memory a Linux process could still take, from `/proc/meminfo`'s text and its cgroup
/// membership (`/proc/self/cgroup`'s): the least of the machine's `MemAvailable` and what
/// its cgroups leave ([`cgroup::left`]); `None` where a cgroup's reading makes no sense, not
/// the machine's alone. `read` reads a file's text.
#[cfg(any(target_os = "linux", test))]
fn linux_remaining(
    meminfo: Option<&str>,
    own: Option<&str>,
    read: impl Fn(&str) -> Option<String>,
) -> Option<u64> {
    let machine = meminfo.and_then(|text| {
        let line = text
            .lines()
            .find(|line| line.starts_with("MemAvailable:"))?;
        let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kib * 1024)
    });
    let groups = match own {
        Some(own) => cgroup::left(own, read)?,
        None => None,
    };
    least([machine, groups])
}

/// The memory limit a Linux process runs under, from its cgroup membership: cgroup v2 (a
/// `0::/path` line) reads `memory.max` in the process's group and every ancestor's, v1 (a
/// line naming the `memory` controller) `memory.limit_in_bytes`; the smallest counts. "max",
/// v1's page-aligned maximum and a group without the file mean no limit; 0 is a limit of
/// nothing. `read` reads a file's text.
#[cfg(any(target_os = "linux", test))]
mod cgroup {
    /// Above this a limit means "none".
    const NONE_ABOVE: u64 = 1 << 62;

    /// Where a cgroup version keeps a group's memory limit, its use, and its statistics, and
    /// the statistic that counts its inactive file pages.
    struct Files {
        root: &'static str,
        limit: &'static str,
        usage: &'static str,
        inactive: &'static str,
    }

    const V2: Files = Files {
        root: "/sys/fs/cgroup",
        limit: "memory.max",
        usage: "memory.current",
        inactive: "inactive_file",
    };

    const V1: Files = Files {
        root: "/sys/fs/cgroup/memory",
        limit: "memory.limit_in_bytes",
        usage: "memory.usage_in_bytes",
        inactive: "total_inactive_file",
    };

    /// `each` of the process's memory group and its ancestors that has a limit, innermost
    /// first: its directory and its limit; `each` returns `None` where it can't read what it
    /// needs. `None`: a limit, or what `each` read, that makes no sense.
    fn limited(
        own: &str,
        read: &impl Fn(&str) -> Option<String>,
        mut each: impl FnMut(&Files, &str, u64) -> Option<()>,
    ) -> Option<()> {
        let Some((files, path)) = own.lines().find_map(|line| {
            let mut fields = line.splitn(3, ':');
            let (_, controllers, path) = (fields.next()?, fields.next()?, fields.next()?);
            if controllers.is_empty() {
                Some((&V2, path))
            } else if controllers.split(',').any(|c| c == "memory") {
                Some((&V1, path))
            } else {
                None
            }
        }) else {
            return Some(());
        };
        let mut dir = format!("{}{}", files.root, path.trim().trim_end_matches('/'));
        loop {
            if let Some(text) = read(&format!("{dir}/{}", files.limit)) {
                let text = text.trim();
                if text != "max" {
                    let limit: u64 = text.parse().ok()?;
                    if limit < NONE_ABOVE {
                        each(files, &dir, limit)?;
                    }
                }
            }
            if dir.len() == files.root.len() {
                return Some(());
            }
            dir.truncate(dir.rfind('/')?);
        }
    }

    pub fn limit(own: &str, read: impl Fn(&str) -> Option<String>) -> Option<u64> {
        let mut smallest = None;
        limited(own, &read, |_, _, limit| {
            smallest = super::least([smallest, Some(limit)]);
            Some(())
        })?;
        smallest
    }

    /// What the process's groups leave of their limits: the least of each limit less the
    /// group's use, its inactive file pages not counted. Those are page cache the kernel
    /// reclaims before it refuses memory, the store's mapped files among them (counted,
    /// a store mapping its checkpoint would leave nothing); its active ones count. `Some(None)`:
    /// no group has a limit; `None`: a group has one but its use can't be read, or a limit
    /// makes no sense, so what is left isn't known.
    pub fn left(own: &str, read: impl Fn(&str) -> Option<String>) -> Option<Option<u64>> {
        let mut least = None;
        limited(own, &read, |files, dir, limit| {
            let number = |text: &str| text.trim().parse::<u64>().ok();
            let usage = number(&read(&format!("{dir}/{}", files.usage))?)?;
            let inactive = read(&format!("{dir}/memory.stat"))
                .and_then(|stat| {
                    stat.lines().find_map(|line| {
                        let (name, value) = line.split_once(' ')?;
                        (name == files.inactive).then(|| number(value)).flatten()
                    })
                })
                .unwrap_or(0);
            let used = usage.saturating_sub(inactive);
            least = super::least([least, Some(limit.saturating_sub(used))]);
            Some(())
        })?;
        Some(least)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The process limit is read as set, holds past it, and doesn't latch: under a limit
    /// no process meets again, it no longer holds. In a process of its own ([`alone`]), as
    /// the limit is the process's; the limit it found is set again.
    ///
    /// [`alone`]: crate::measure::alone
    #[test]
    fn the_process_limit_follows_the_process() {
        if !crate::measure::alone() {
            return;
        }
        process_bytes().expect("the platform reports the process's memory");
        let incoming = process_limit().unwrap_or(0);
        set_process_limit(1);
        assert_eq!(process_limit(), Some(1));
        assert!(process_limit_exceeded(), "one byte is exceeded");
        set_process_limit(u64::MAX);
        assert!(!process_limit_exceeded(), "no process holds that much");
        set_process_limit(0);
        assert_eq!(process_limit(), None);
        assert!(!process_limit_exceeded());
        set_process_limit(incoming);
        assert_eq!(process_limit().unwrap_or(0), incoming);
    }

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

    /// Linux's remaining memory is the least of the machine's available memory and what
    /// each cgroup with a limit leaves of it, inactive file pages not counted as used.
    #[test]
    fn linux_remaining_memory_is_the_least_left() {
        use std::collections::HashMap;
        const G: u64 = 1 << 30;
        let gib = |n: u64| (n * G).to_string();
        let files: HashMap<String, String> = [
            // v2: the slice leaves 8 - (7 - 2) = 3 GiB, the scope 16 - (6 - 1) = 11.
            ("/sys/fs/cgroup/memory.max", "max".to_owned()),
            ("/sys/fs/cgroup/app.slice/memory.max", gib(8)),
            ("/sys/fs/cgroup/app.slice/memory.current", gib(7)),
            (
                "/sys/fs/cgroup/app.slice/memory.stat",
                format!("anon {}\ninactive_file {}\nactive_file 0", gib(4), gib(2)),
            ),
            ("/sys/fs/cgroup/app.slice/run.scope/memory.max", gib(16)),
            ("/sys/fs/cgroup/app.slice/run.scope/memory.current", gib(6)),
            (
                "/sys/fs/cgroup/app.slice/run.scope/memory.stat",
                format!("inactive_file {}", gib(1)),
            ),
            // v1: 4 GiB less 3 used, 1 of them inactive cache: 2 GiB.
            (
                "/sys/fs/cgroup/memory/docker/abc/memory.limit_in_bytes",
                gib(4),
            ),
            (
                "/sys/fs/cgroup/memory/docker/abc/memory.usage_in_bytes",
                gib(3),
            ),
            (
                "/sys/fs/cgroup/memory/docker/abc/memory.stat",
                format!("inactive_file 0\ntotal_inactive_file {}", gib(1)),
            ),
            // A limit of 0 (Sol, measure-pool r1), a limit without its use, and one that
            // makes no sense.
            ("/sys/fs/cgroup/zero.scope/memory.max", "0".to_owned()),
            (
                "/sys/fs/cgroup/zero.scope/memory.current",
                "4096".to_owned(),
            ),
            ("/sys/fs/cgroup/blind.scope/memory.max", gib(4)),
            ("/sys/fs/cgroup/odd.scope/memory.max", "lots".to_owned()),
        ]
        .into_iter()
        .map(|(path, text)| (path.to_owned(), text))
        .collect();
        let read = |path: &str| files.get(path).cloned();
        let meminfo = |available: u64| {
            format!(
                "MemTotal: 33554432 kB\nMemAvailable: {} kB\n",
                available / 1024
            )
        };
        let v2 = Some("0::/app.slice/run.scope");
        assert_eq!(
            linux_remaining(Some(&meminfo(20 * G)), v2, read),
            Some(3 * G)
        );
        // The machine's, where it is less.
        assert_eq!(linux_remaining(Some(&meminfo(G)), v2, read), Some(G));
        let v1 = Some("4:cpu:/docker/abc\n3:memory:/docker/abc");
        assert_eq!(
            linux_remaining(Some(&meminfo(20 * G)), v1, read),
            Some(2 * G)
        );
        // No limit anywhere: the machine's; and no reading at all.
        let free = Some("0::/system.slice/x.service");
        assert_eq!(
            linux_remaining(Some(&meminfo(5 * G)), free, read),
            Some(5 * G)
        );
        assert_eq!(linux_remaining(None, None, read), None);
        // A limit of 0 leaves nothing; one whose use or value can't be read leaves what
        // isn't known, not the machine's.
        let at = |group: &str| format!("0::/{group}");
        let machine = Some(meminfo(G));
        assert_eq!(
            linux_remaining(machine.as_deref(), Some(&at("zero.scope")), read),
            Some(0)
        );
        assert_eq!(cgroup::limit(&at("zero.scope"), read), Some(0));
        for group in ["blind.scope", "odd.scope"] {
            assert_eq!(
                linux_remaining(machine.as_deref(), Some(&at(group)), read),
                None,
                "{group}"
            );
        }
    }

    /// A job whose limit counts something that can't be read leaves what isn't known, and so
    /// does the remaining memory, not the machine's (Sol, measure-pool r2), and so does a job
    /// whose limits can't be read (r3); a job without a limit, or no job, leaves the machine's.
    #[test]
    fn an_unreadable_job_leaves_what_isnt_known() {
        const G: u64 = 1 << 30;
        let read = |bytes: u64| move || Some(bytes);
        let refused = || None;
        assert_eq!(
            job_left_from(
                Some(true),
                || Some([Some(8 * G), None]),
                read(5 * G),
                refused
            ),
            Some(Some(3 * G))
        );
        assert_eq!(
            job_left_from(Some(true), || Some([Some(8 * G), None]), refused, read(G)),
            None
        );
        assert_eq!(
            job_left_from(Some(true), || Some([None, Some(4 * G)]), read(G), refused),
            None
        );
        assert_eq!(
            job_left_from(Some(true), || Some([None, None]), refused, refused),
            Some(None)
        );
        // In a job whose limits can't be read, or where membership can't be asked: unknown,
        // not taken for no job (Sol, measure-pool r3); outside a job: no limit.
        let no_limits = || None;
        assert_eq!(job_left_from(Some(true), no_limits, refused, refused), None);
        assert_eq!(
            job_left_from(None, || Some([None, None]), refused, refused),
            None
        );
        assert_eq!(
            job_left_from(Some(false), no_limits, refused, refused),
            Some(None)
        );
        assert_eq!(windows_remaining(Some(16 * G), None), None);
        assert_eq!(windows_remaining(Some(16 * G), Some(None)), Some(16 * G));
        assert_eq!(
            windows_remaining(Some(16 * G), Some(Some(3 * G))),
            Some(3 * G)
        );
    }

    /// What a job object leaves is the least of its set limits less what is used of each.
    #[test]
    fn a_job_leaves_the_least_of_its_limits_less_their_use() {
        const G: u64 = 1 << 30;
        // Job-wide 8 GiB with 5 committed, per process 4 with 2.
        assert_eq!(
            left([Some((8 * G, 5 * G)), Some((4 * G, 2 * G))]),
            Some(2 * G)
        );
        assert_eq!(left([Some((8 * G, 5 * G)), None]), Some(3 * G));
        // Over a limit leaves nothing; none set, no reading.
        assert_eq!(left([Some((G, 2 * G))]), Some(0));
        assert_eq!(left([None, None]), None);
    }

    /// A job object's memory limit counts as the memory the process may use: the test puts
    /// its own process into a job with a limit below the machine's memory (it becomes the
    /// innermost job) and reads it back. In a process of its own ([`alone`]): a process
    /// stays in its job for life, and the processes it starts are in it too.
    ///
    /// [`alone`]: crate::measure::alone
    #[cfg(windows)]
    #[test]
    fn a_job_objects_memory_limit_counts_as_the_memory_available() {
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_JOB_MEMORY,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        if !crate::measure::alone() {
            return;
        }
        let machine = available_bytes().expect("the platform reports the memory it may use");
        // Below the machine, whole pages, as Windows keeps it; far above the little more than
        // COMMITTED this process holds, alone.
        let limit = (machine / 4 * 3) & !0xFFF;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_JOB_MEMORY;
        info.JobMemoryLimit = usize::try_from(limit).expect("a limit that fits");
        let size = u32::try_from(std::mem::size_of_val(&info)).expect("a small struct");
        // SAFETY: a fresh unnamed job; `info` is a valid JOBOBJECT_EXTENDED_LIMIT_INFORMATION
        // of `size` bytes; the current process's pseudo-handle needs no closing. The job
        // handle stays open for the process's life, so the job lives as long as this child.
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
        // What it leaves is the limit less what the job commits now (this process alone is in
        // it): down by what is committed, and up again when that is freed, at each reading
        // (Sol, measure-pool r1: a reading at a notification is not one now).
        const COMMITTED: u64 = 256 << 20;
        const SLACK: u64 = 64 << 20;
        let left = || imp::job_left().flatten().expect("what the job leaves");
        let follows = |when: &str| {
            let before = left();
            let held = process_bytes().expect("the process's committed memory");
            assert!(
                before + held <= limit + SLACK && before + SLACK >= limit - held,
                "{when}: {before} left, {held} held, of {limit}"
            );
            let block = Vec::<u8>::with_capacity(COMMITTED as usize);
            let during = left();
            drop(std::hint::black_box(block));
            let after = left();
            assert!(
                before - during >= COMMITTED - SLACK && after - during >= COMMITTED - SLACK,
                "{when}: {before}, {during}, {after}"
            );
            assert!(remaining_bytes().is_some_and(|remaining| remaining <= after + SLACK));
        };
        follows("before a violation");
        // The whole limit asked at once: the job refuses it (a violation of its limit).
        let whole = std::alloc::Layout::from_size_align(limit as usize, 4096).expect("a layout");
        // SAFETY: a layout of non-zero size; a block, if one were made, is freed with it.
        unsafe {
            let block = std::alloc::GlobalAlloc::alloc(&std::alloc::System, whole);
            assert!(block.is_null(), "the job allowed its whole limit");
        }
        follows("after a violation");
    }

    /// A phase's peak is its window's or none: on Linux a window starts and has a peak;
    /// elsewhere none starts (Windows keeps one peak for the process's life), and the
    /// lifetime's peak isn't reported as the phase's. In a process of its own ([`alone`]):
    /// a window resets the kernel's mark for the whole process.
    ///
    /// [`alone`]: crate::measure::alone
    #[test]
    fn a_phase_peak_is_its_windows_or_none() {
        if !crate::measure::alone() {
            return;
        }
        let linux = cfg!(target_os = "linux");
        assert_eq!(start_peak_window(), linux, "a window started");
        assert_eq!(
            window_peak_bytes().is_some(),
            linux,
            "the window has a peak"
        );
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

    /// A phase's window starts at what the process holds, and the lifetime peak keeps what
    /// came before it: perf lab reports reset the kernel's mark per phase, and a fast
    /// suite's case peak read after the last phase was that phase's alone. In a process of
    /// its own ([`alone`]): the window resets the kernel's mark for the whole process, and
    /// another test's memory would count in it.
    ///
    /// [`alone`]: crate::measure::alone
    #[cfg(target_os = "linux")]
    #[test]
    fn a_peak_window_keeps_the_lifetime_peak() {
        const HELD: usize = 256 << 20;
        if !crate::measure::alone() {
            return;
        }
        let pages = std::hint::black_box(vec![1u8; HELD]);
        drop(pages);
        let before = peak_process_bytes().unwrap();
        assert!(start_peak_window());
        let window = window_peak_bytes().unwrap();
        assert!(peak_process_bytes().unwrap() >= before);
        assert!(
            window + (HELD as u64) / 2 < before,
            "window {window} B, lifetime {before} B"
        );
    }

    /// Switched off, the kernel shows the process's huge pages off, and on again. In a
    /// process of its own ([`alone`]), as the setting is the process's; the one it found is
    /// set again.
    ///
    /// [`alone`]: crate::measure::alone
    #[cfg(target_os = "linux")]
    #[test]
    fn transparent_huge_pages_switch_for_the_process() {
        if !crate::measure::alone() {
            return;
        }
        // Shown from Linux 5.0 on.
        let state = || {
            let status = std::fs::read_to_string("/proc/self/status").unwrap();
            let line = status.lines().find(|l| l.starts_with("THP_enabled:"));
            let line = line.expect("the kernel shows the process's huge pages");
            line.split_whitespace().nth(1).map(str::to_owned)
        };
        let incoming = state();
        assert!(set_transparent_huge_pages(false));
        assert_eq!(state().as_deref(), Some("0"));
        assert!(set_transparent_huge_pages(true));
        assert_eq!(state().as_deref(), Some("1"));
        assert!(set_transparent_huge_pages(incoming.as_deref() == Some("1")));
        assert_eq!(state(), incoming);
    }
}
