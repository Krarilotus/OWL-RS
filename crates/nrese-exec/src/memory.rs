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

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::Instant;

/// Whether this target has the readers ([`process_bytes`], [`available_bytes`] and the
/// others): Linux and Windows. Elsewhere every reading is [`Unread::Unsupported`], and a
/// memory limit can't be enforced, so its configuration refuses one.
pub const READS_MEMORY: bool = cfg!(any(windows, target_os = "linux"));

/// Why a reading of memory wasn't taken. A limit that needs it can't be enforced: it
/// stops the work ([`MemoryStopCause::Unread`]) or refuses its configuration, never
/// reading as "not over".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unread {
    /// No reader on this target ([`READS_MEMORY`] is false): known at compile time.
    Unsupported,
    /// The target has a reader, and it failed now (`/proc` unreadable or unparsable, the
    /// system call refused).
    Failed,
}

impl std::fmt::Display for Unread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "unread: this platform has no reader",
            Self::Failed => "unread: the reading failed",
        })
    }
}

impl std::error::Error for Unread {}

/// A reading in bytes, or why there is none.
pub type Reading = Result<u64, Unread>;

/// A platform reader's answer as a [`Reading`]: none is a failure where the target reads
/// memory, and unsupported where it doesn't.
fn reading(bytes: Option<u64>) -> Reading {
    match bytes {
        Some(bytes) => Ok(bytes),
        None if READS_MEMORY => Err(Unread::Failed),
        None => Err(Unread::Unsupported),
    }
}

/// The private memory the process holds now (anonymous on Linux, committed private on
/// Windows; mapped files excluded, since the kernel reclaims them).
pub fn process_bytes() -> Reading {
    reading(imp::process_bytes())
}

/// The most [`process_bytes`] has been since the process started, where the platform
/// keeps it (Windows: the peak committed private bytes; Linux: the peak resident set,
/// mapped files included). A [`start_peak_window`] doesn't lower it.
pub fn peak_process_bytes() -> Reading {
    let window = reading(imp::peak_process_bytes())?;
    let before = PEAK_BEFORE_WINDOW.fetch_max(window, Ordering::Relaxed);
    Ok(before.max(window))
}

/// Starts a window for [`window_peak_bytes`], the peak of one phase of a long run. On Linux
/// the kernel's high-water mark is kept for [`peak_process_bytes`] and then reset to the
/// resident set (`/proc/self/clear_refs`), which also resets what `getrusage` and
/// `time -v` report as the maximum resident set; elsewhere no window starts and `false`
/// is returned. Starting a window by writing `clear_refs` directly loses the peak before it.
pub fn start_peak_window() -> bool {
    let _ = peak_process_bytes();
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
/// memory it held at once, mapped and shared pages included); unsupported elsewhere.
pub fn peak_working_set_bytes() -> Reading {
    match imp::peak_working_set_bytes() {
        None if !cfg!(windows) => Err(Unread::Unsupported),
        bytes => reading(bytes),
    }
}

/// The highest mark the kernel had when a peak window started.
static PEAK_BEFORE_WINDOW: AtomicU64 = AtomicU64::new(0);

/// The page faults the process has taken since it started (soft and hard: a mapped file's
/// pages count as they are first touched), where the platform keeps them.
pub fn page_faults() -> Reading {
    reading(imp::page_faults())
}

/// The memory resident in the process's working set now, mapped file pages included.
pub fn resident_bytes() -> Reading {
    reading(imp::resident_bytes())
}

/// The memory this process may use: the smallest of the machine's physical memory and the
/// limits it runs under (on Linux its cgroup's and every ancestor's, so a container, a
/// Kubernetes pod or a systemd scope counts; on Windows its job object's).
pub fn available_bytes() -> Reading {
    SYSTEM_READINGS.fetch_add(1, Ordering::Relaxed);
    reading(imp::available_bytes())
}

/// How many times the process has read the system's memory ([`available_bytes`],
/// [`remaining_bytes`]: on Linux each a discovery of its cgroups), so a check can show that
/// none is taken where none belongs, on an operation's path. Not the process's own counter
/// ([`process_bytes`]).
pub fn system_readings() -> u64 {
    SYSTEM_READINGS.load(Ordering::Relaxed)
}

static SYSTEM_READINGS: AtomicU64 = AtomicU64::new(0);

/// The memory the process could still take now, as the system reports it: on Linux the
/// least of the machine's available memory (`MemAvailable`) and what its cgroups leave
/// (each limit less the group's use, inactive file pages not counted); on Windows the
/// least of the machine's available physical memory and what its job object leaves (its
/// job-wide limit less the job's committed memory, its per-process limit less the process's
/// own). Unlike [`available_bytes`], the total, it changes from one reading to the next,
/// and counts as used what the allocator holds freed, until it returns it. A cap for
/// admitting an operation as it starts, never a reservation: what it reports is taken by
/// whoever allocates first.
pub fn remaining_bytes() -> Reading {
    SYSTEM_READINGS.fetch_add(1, Ordering::Relaxed);
    reading(imp::remaining_bytes())
}

/// [`available_bytes`], read once per process, at its first need: a total, which doesn't
/// change while the process runs (its container's or job's limit, the machine's memory), so
/// every limit that is a share of it or capped by it asks here, and a store's open asks
/// nothing. Concurrent first asks read once, the others waiting for that reading and
/// sharing its answer, a failure too. A failed reading isn't kept: an ask after it reads
/// again (on a host where it keeps failing, each operation that needs the limit reads and is
/// refused).
pub fn capacity() -> Reading {
    CAPACITY.get(available_bytes)
}

/// [`capacity`]'s reading.
static CAPACITY: ReadOnce = ReadOnce::new();

/// A reading taken once and kept, shared by the asks that wait for it: one reads, the
/// others wait for its answer and take it, a failure included; a failure isn't kept, so
/// an ask that comes after it reads again.
struct ReadOnce {
    /// The reading once taken; 0 before.
    known: AtomicU64,
    /// How many readings have failed: an ask that saw fewer before it waited joined a
    /// reading that failed meanwhile.
    failed: AtomicU64,
    /// Held while reading; the latest failure.
    reading: std::sync::Mutex<Option<Unread>>,
}

impl ReadOnce {
    const fn new() -> Self {
        Self {
            known: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            reading: std::sync::Mutex::new(None),
        }
    }

    fn get(&self, read: impl FnOnce() -> Reading) -> Reading {
        let known = self.known.load(Ordering::Acquire);
        if known != 0 {
            return Ok(known);
        }
        let failed = self.failed.load(Ordering::Acquire);
        let mut latest = self
            .reading
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let known = self.known.load(Ordering::Acquire);
        if known != 0 {
            return Ok(known);
        }
        if let Some(unread) = *latest
            && self.failed.load(Ordering::Acquire) != failed
        {
            return Err(unread);
        }
        match read() {
            Ok(bytes) => {
                self.known.store(bytes, Ordering::Release);
                Ok(bytes)
            }
            Err(unread) => {
                *latest = Some(unread);
                self.failed.fetch_add(1, Ordering::Release);
                Err(unread)
            }
        }
    }
}

/// The process's memory limit a setting means, given the memory the process may use as
/// read ([`capacity`]): `explicit` bytes, no more than a capacity that reads (a limit the
/// system would never let the process reach is no limit), and themselves where it doesn't
/// (the operator's size is the configured fallback: nothing unknown is taken as measured);
/// else three quarters of the capacity, unread where it doesn't read.
pub fn limit_within(explicit: Option<u64>, capacity: Reading) -> Reading {
    match explicit {
        Some(bytes) => Ok(capacity.map_or(bytes, |capacity| bytes.min(capacity))),
        None => capacity.map(|capacity| capacity / 4 * 3),
    }
}

/// Why a memory limit stopped the work ([`MemoryWatch::cause`], [`process_limit_stop`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryStopCause {
    /// The process held more than the limit.
    OverLimit,
    /// The process's memory couldn't be read ([`Unread`]): the limit can't be enforced,
    /// so the work stops as it would past it.
    Unread,
}

impl MemoryStopCause {
    /// As kept in an atomic beside a watch's flag: 0 is "not stopped".
    const fn code(cause: Option<Self>) -> u8 {
        match cause {
            None => 0,
            Some(Self::OverLimit) => 1,
            Some(Self::Unread) => 2,
        }
    }

    const fn of(code: u8) -> Option<Self> {
        match code {
            0 => None,
            1 => Some(Self::OverLimit),
            _ => Some(Self::Unread),
        }
    }
}

/// Whether a reading stops work under `limit`, and why: past the limit, or no reading at
/// all (a limit that can't be read is not "not over").
pub fn stop_cause(reading: Reading, limit: u64) -> Option<MemoryStopCause> {
    match reading {
        Ok(held) => (held > limit).then_some(MemoryStopCause::OverLimit),
        Err(_) => Some(MemoryStopCause::Unread),
    }
}

/// A process-wide limit and its latest reading ([`PROCESS`]), read with `read`.
struct ProcessLimit {
    /// 0: none.
    limit: AtomicU64,
    /// Microseconds since `epoch` of the last reading.
    last: AtomicU64,
    /// That reading's [`MemoryStopCause`], as its code.
    stopped: AtomicU8,
    epoch: OnceLock<Instant>,
    read: fn() -> Reading,
}

impl ProcessLimit {
    const fn new(read: fn() -> Reading) -> Self {
        Self {
            limit: AtomicU64::new(0),
            last: AtomicU64::new(0),
            stopped: AtomicU8::new(0),
            epoch: OnceLock::new(),
            read,
        }
    }

    fn set(&self, limit: u64) {
        self.limit.store(limit, Ordering::Relaxed);
        self.last.store(0, Ordering::Relaxed);
        self.stopped.store(0, Ordering::Relaxed);
    }

    fn limit(&self) -> Option<u64> {
        Some(self.limit.load(Ordering::Relaxed)).filter(|&limit| limit > 0)
    }

    /// Whether the latest reading stops work, and why, from that one reading.
    fn stop(&self) -> Option<MemoryStopCause> {
        let limit = self.limit()?;
        let epoch = self.epoch.get_or_init(Instant::now);
        let now = u64::try_from(epoch.elapsed().as_micros())
            .unwrap_or(u64::MAX)
            .max(1);
        let latest = || MemoryStopCause::of(self.stopped.load(Ordering::Relaxed));
        let last = self.last.load(Ordering::Relaxed);
        if last != 0 && now.saturating_sub(last) < MemoryWatch::INTERVAL_MICROS {
            return latest();
        }
        // One reader per interval; the others go on with the last answer.
        if self
            .last
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return latest();
        }
        let cause = stop_cause((self.read)(), limit);
        self.stopped
            .store(MemoryStopCause::code(cause), Ordering::Relaxed);
        cause
    }
}

/// The process's memory limit, for long operations whose caller passes no watch of its
/// own: set by whoever owns the process's configuration (server startup, or an explicit
/// embedding host), never by opening a store. Polled through [`process_limit_exceeded`]
/// by the operations that can grow without bound (the reasoner's materialisations and
/// commits).
static PROCESS: ProcessLimit = ProcessLimit::new(process_bytes);

/// Sets the process's memory limit in bytes (0: none). See [`process_limit_exceeded`].
/// Its owner refuses a limit where [`READS_MEMORY`] is false, at its configuration.
pub fn set_process_limit(limit: u64) {
    PROCESS.set(limit);
}

/// Whether the process's anonymous memory may be backed by transparent huge pages
/// (Linux; elsewhere there are none to switch, and nothing is done). An allocator that
/// asks for them (mimalloc does, for its arenas) then makes a whole 2 MiB page resident
/// for one touched byte, and gives back nothing of it while one of its small pages is in
/// use: under a server's many threads that kept 3 times the memory the system allocator
/// holds for the same work (soak, 7 October 2026). Applies from the next page touched,
/// arenas reserved earlier included. Returns whether the setting was applied. A store
/// sets it through its mode ([`set_huge_pages`]), which bulk phases follow.
pub fn set_transparent_huge_pages(allowed: bool) -> bool {
    imp::set_transparent_huge_pages(allowed)
}

/// The process's memory limit, if one is set ([`set_process_limit`]).
pub fn process_limit() -> Option<u64> {
    PROCESS.limit()
}

/// Whether the process holds more than its memory limit ([`set_process_limit`]), or its
/// memory can't be read, and which, as of a reading at most
/// [`MemoryWatch::INTERVAL_MICROS`] old: an atomic load and a clock read in a hot loop;
/// `None` without a limit or under it. Unlike a [`MemoryWatch`] it doesn't stay stopped:
/// an operation the limit stopped frees its memory, and the next one may run. An operation
/// that must not go on after one stop latches it itself, with the cause this returned
/// (another operation's poll may read the limit again meanwhile).
pub fn process_limit_stop() -> Option<MemoryStopCause> {
    PROCESS.stop()
}

/// [`process_limit_stop`] as a flag.
pub fn process_limit_exceeded() -> bool {
    process_limit_stop().is_some()
}

/// Whether the process has gone past a memory limit, for the stop callbacks of long
/// operations. The memory is read at most every [`MemoryWatch::INTERVAL_MICROS`], so a
/// check in a hot loop costs an atomic load and a clock read; once stopped, it stays
/// stopped, and [`Self::cause`] says why.
#[derive(Debug)]
pub struct MemoryWatch {
    limit: u64,
    started: Instant,
    /// Microseconds after `started` of the last reading.
    last: AtomicU64,
    /// Why it stopped, as a [`MemoryStopCause`]'s code (0: it didn't); set once.
    stopped: AtomicU8,
    read: fn() -> Reading,
}

impl MemoryWatch {
    /// How often the memory is read, at most.
    pub const INTERVAL_MICROS: u64 = 10_000;

    /// A watch for `limit` bytes of the process's memory ([`process_bytes`]).
    pub fn new(limit: u64) -> Self {
        Self::with_reader(limit, process_bytes)
    }

    /// A watch for `limit` bytes as `read` reports them: [`Self::new`]'s reader, or
    /// another (a test's that fails).
    pub fn with_reader(limit: u64, read: fn() -> Reading) -> Self {
        Self {
            limit,
            started: Instant::now(),
            last: AtomicU64::new(0),
            stopped: AtomicU8::new(0),
            read,
        }
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// What the process may still take under the limit now: the limit less what it holds
    /// (none past it), read now, not at the interval of [`Self::exceeded`]. For admitting
    /// an allocation whose size is known before it is made, which a poll would only see
    /// after. A failed reading leaves no room to admit by ([`Unread`]).
    pub fn room(&self) -> Reading {
        (self.read)().map(|held| self.limit.saturating_sub(held))
    }

    /// Why it stopped, once [`Self::exceeded`] said so; `None` before.
    pub fn cause(&self) -> Option<MemoryStopCause> {
        MemoryStopCause::of(self.stopped.load(Ordering::Relaxed))
    }

    /// Whether the process holds more than the limit, or its memory couldn't be read (as
    /// of the latest reading).
    pub fn exceeded(&self) -> bool {
        if self.stopped.load(Ordering::Relaxed) != 0 {
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
        let Some(cause) = stop_cause((self.read)(), self.limit) else {
            return false;
        };
        // The first cause stays: a later reading doesn't rename a stop already taken.
        let _ = self.stopped.compare_exchange(
            0,
            MemoryStopCause::code(Some(cause)),
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
        true
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
        super::windows_available(
            status().map(|status| status.ullTotalPhys),
            in_job(),
            job_limits,
        )
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
    ///
    /// Decided (the orchestrator, 7 Nov 17:10, P2-A4 review r1 R1-C2, IB1709-5712): a limit on
    /// an enclosing job is a documented limitation, as a cgroup v2 limit above the namespace is
    /// (IB2001-2a56). No supported call shows it: `QueryInformationJobObject` answers for the
    /// immediate job, and `GlobalMemoryStatusEx`'s commit fields ignore job limits (fleet
    /// `p2a4x-jobprobe-79195bb6`: an outer 1 GiB cap enforced, both fields unchanged). Under a
    /// capped parent job the server's memory limit is set by hand (`process_memory_bytes`).
    #[cfg(test)]
    pub(super) fn job_limit() -> Option<u64> {
        super::least(job_limits()?)
    }

    /// What the job object's limits leave ([`super::job_left_from`]): its job-wide limit less
    /// the memory its processes commit now ([`job_committed`]), its per-process limit less the
    /// process's own committed memory. `Some(None)` outside a job or without a limit; `None`
    /// where a limit is set whose use can't be read.
    pub(super) fn job_left() -> Option<Option<u64>> {
        super::job_left_from(in_job(), job_limits, job_committed, process_bytes)
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
    /// The size of a page, which /proc/self/statm counts in: the system's (4, 16 or 64 KiB
    /// on aarch64), read once ([`crate::region::page_size`]).
    fn page() -> u64 {
        crate::region::page_size() as u64
    }

    /// The anonymous resident memory: resident minus shared (file-backed and shared
    /// memory), as Windows's committed private bytes count. The store's mapped checkpoint
    /// and dictionary are file pages the kernel reclaims under pressure; counted, they
    /// could stop a materialisation that fits (LUBM 1000 maps about 5 GB).
    pub fn process_bytes() -> Option<u64> {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        super::statm_bytes(&statm, page()).map(|bytes| bytes.anonymous)
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
        super::statm_bytes(&statm, page()).map(|bytes| bytes.resident)
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
        super::linux_available(&super::cgroup::Kernel)
    }

    pub fn remaining_bytes() -> Option<u64> {
        super::linux_remaining(&super::cgroup::Kernel)
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
/// memory and what its job leaves; `None` where either reading isn't known.
#[cfg(any(windows, test))]
fn windows_remaining(machine: Option<u64>, job: Option<Option<u64>>) -> Option<u64> {
    least([Some(machine?), job?])
}

/// The memory a Windows process may use, from the machine's physical memory, whether the
/// process is in a job object, and the job's limits (those set): the least of the machine's
/// and the job's. `None` where membership can't be asked, or the job's limits can't be read
/// (outside a job that query fails too: told apart by membership), not the machine's. The
/// job is the immediate one; an enclosing job's limit isn't observable (`imp::job_limit`).
#[cfg(any(windows, test))]
fn windows_available(
    machine: Option<u64>,
    in_job: Option<bool>,
    limits: impl FnOnce() -> Option<[Option<u64>; 2]>,
) -> Option<u64> {
    let job = if in_job? { least(limits()?) } else { None };
    least([Some(machine?), job])
}

/// A `/proc/self/statm` reading in bytes: the resident set, and its anonymous part (resident
/// less shared: file-backed and shared memory).
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Statm {
    resident: u64,
    anonymous: u64,
}

/// `/proc/self/statm`'s text in bytes. Its fields count pages, of `page` bytes: the system's
/// page size, which on aarch64 Linux may be 4, 16 or 64 KiB.
#[cfg(any(target_os = "linux", test))]
fn statm_bytes(text: &str, page: u64) -> Option<Statm> {
    let mut fields = text.split_whitespace().skip(1);
    let resident: u64 = fields.next()?.parse().ok()?;
    let shared: u64 = fields.next()?.parse().ok()?;
    Some(Statm {
        resident: resident.checked_mul(page)?,
        anonymous: resident.saturating_sub(shared).checked_mul(page)?,
    })
}

/// A `/proc/meminfo` field (`MemTotal:`, `MemAvailable:`) in bytes.
#[cfg(any(target_os = "linux", test))]
fn meminfo_bytes(text: &str, field: &str) -> Option<u64> {
    let line = text.lines().find(|line| line.starts_with(field))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kib.checked_mul(1024)
}

/// The memory a Linux process may use: the least of the machine's `MemTotal` and its cgroups'
/// limits ([`cgroup::limit`]); `None` where either isn't known, not the machine's alone. `fs`
/// answers as the kernel does ([`cgroup::Kernel`]).
#[cfg(any(target_os = "linux", test))]
fn linux_available(fs: &impl cgroup::Fs) -> Option<u64> {
    let machine = meminfo_bytes(&fs.text("/proc/meminfo").ok()?, "MemTotal:")?;
    least([Some(machine), cgroup::limit(fs).ok()?])
}

/// The memory a Linux process could still take: the least of the machine's `MemAvailable`
/// and what its cgroups leave ([`cgroup::left`]); `None` where either isn't known, not the
/// other alone.
#[cfg(any(target_os = "linux", test))]
fn linux_remaining(fs: &impl cgroup::Fs) -> Option<u64> {
    let machine = meminfo_bytes(&fs.text("/proc/meminfo").ok()?, "MemAvailable:")?;
    least([Some(machine), cgroup::left(fs).ok()?])
}

#[cfg(any(target_os = "linux", test))]
mod cgroup;

mod huge_pages;
pub use huge_pages::{HugePages, HugePagesPhase, set_huge_pages, transparent_huge_pages};

#[cfg(test)]
mod discovery_tests;

#[cfg(test)]
mod tests {
    /// The memory the process may use, read once: asks that wait for a reading share its
    /// answer, a failure too (one reading for all of them); an ask after a failure reads
    /// again, and a reading taken is kept.
    #[test]
    fn concurrent_first_asks_share_one_reading_a_failed_one_too() {
        use std::sync::atomic::AtomicUsize;
        static ONCE: super::ReadOnce = super::ReadOnce::new();
        static READS: AtomicUsize = AtomicUsize::new(0);
        static ASKING: AtomicUsize = AtomicUsize::new(0);
        fn failing() -> super::Reading {
            READS.fetch_add(1, Ordering::Relaxed);
            // The reading lasts until every asker has asked, and a while beyond, for each to
            // be waiting for it.
            while ASKING.load(Ordering::Relaxed) < 8 {
                std::thread::yield_now();
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            Err(super::Unread::Failed)
        }
        let answers: Vec<_> = std::thread::scope(|scope| {
            let asks: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        ASKING.fetch_add(1, Ordering::Relaxed);
                        ONCE.get(failing)
                    })
                })
                .collect();
            asks.into_iter().map(|ask| ask.join().unwrap()).collect()
        });
        assert_eq!(READS.load(Ordering::Relaxed), 1, "one reading for all");
        assert!(
            answers
                .iter()
                .all(|answer| *answer == Err(super::Unread::Failed))
        );
        assert_eq!(
            ONCE.get(|| Ok(4096)),
            Ok(4096),
            "read again after a failure"
        );
        assert_eq!(ONCE.get(|| panic!("kept: not read")), Ok(4096));
    }

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
        assert_eq!(process_limit_stop(), Some(MemoryStopCause::OverLimit));
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
        const NONE: &str = "9223372036854771712";
        let files = fixture(&[
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
                "/sys/fs/cgroup/user.slice/user-1000.slice/run.scope/cgroup.controllers",
                "memory",
            ),
            (
                "/sys/fs/cgroup/system.slice/x.service/cgroup.controllers",
                "memory",
            ),
            (
                "/sys/fs/cgroup/user.slice/user-1000.slice/run.scope/cgroup.procs",
                procs(),
            ),
            (
                "/sys/fs/cgroup/system.slice/x.service/cgroup.procs",
                procs(),
            ),
            ("/sys/fs/cgroup/memory/docker/abc/cgroup.procs", procs()),
            ("/sys/fs/cgroup/memory/free/cgroup.procs", procs()),
            ("/sys/fs/cgroup/memory/memory.limit_in_bytes", NONE),
            ("/sys/fs/cgroup/memory/docker/memory.limit_in_bytes", NONE),
            (
                "/sys/fs/cgroup/memory/docker/abc/memory.limit_in_bytes",
                "4294967296",
            ),
            ("/sys/fs/cgroup/memory/free/memory.limit_in_bytes", NONE),
        ]);
        let limit = |own: &str| cgroup::limit(&system(&files, None, Some(own)));
        // v2: the scope's 16 GiB, under its slice's 32 GiB.
        assert_eq!(
            limit("0::/user.slice/user-1000.slice/run.scope"),
            Ok(Some(16 << 30))
        );
        // v1: the memory controller's line, not the others'.
        assert_eq!(
            limit("4:cpu,cpuacct:/docker/abc\n3:memory:/docker/abc"),
            Ok(Some(4 << 30))
        );
        // No limit anywhere, or v1's "none" value.
        assert_eq!(limit("0::/system.slice/x.service"), Ok(None));
        assert_eq!(limit("3:memory:/free"), Ok(None));
    }

    /// Linux's remaining memory is the least of the machine's available memory and what
    /// each cgroup with a limit leaves of it, inactive file pages not counted as used.
    #[test]
    fn linux_remaining_memory_is_the_least_left() {
        use std::collections::HashMap;
        const G: u64 = 1 << 30;
        let gib = |n: u64| (n * G).to_string();
        let files: HashMap<String, String> = [
            ("/proc/self/mountinfo", MOUNTS.to_owned()),
            // v2: the slice leaves 8 - (7 - 2) = 3 GiB, the scope 16 - (6 - 1) = 11.
            ("/sys/fs/cgroup/memory.max", "max".to_owned()),
            (
                "/sys/fs/cgroup/app.slice/run.scope/cgroup.controllers",
                "memory".to_owned(),
            ),
            (
                "/sys/fs/cgroup/system.slice/x.service/cgroup.controllers",
                "memory".to_owned(),
            ),
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
            // v1: 4 GiB less 3 used, 1 of them inactive cache: 2 GiB; no limit above it.
            (
                "/sys/fs/cgroup/memory/memory.limit_in_bytes",
                "9223372036854771712".to_owned(),
            ),
            (
                "/sys/fs/cgroup/memory/docker/memory.limit_in_bytes",
                "9223372036854771712".to_owned(),
            ),
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
        .chain(["zero", "blind", "odd"].map(|scope| {
            let path = format!("/sys/fs/cgroup/{scope}.scope/cgroup.controllers");
            (path, "memory".to_owned())
        }))
        .chain(
            [
                "app.slice/run.scope",
                "system.slice/x.service",
                "memory/docker/abc",
                "zero.scope",
                "blind.scope",
                "odd.scope",
            ]
            .map(|group| {
                let path = format!("/sys/fs/cgroup/{group}/cgroup.procs");
                (path, procs().to_owned())
            }),
        )
        .collect();
        let meminfo = |available: u64| {
            format!(
                "MemTotal: 33554432 kB\nMemAvailable: {} kB\n",
                available / 1024
            )
        };
        let remaining = |meminfo: Option<String>, own: Option<&str>| {
            linux_remaining(&system(&files, meminfo.as_deref(), own))
        };
        let v2 = Some("0::/app.slice/run.scope");
        assert_eq!(remaining(Some(meminfo(20 * G)), v2), Some(3 * G));
        // The machine's, where it is less.
        assert_eq!(remaining(Some(meminfo(G)), v2), Some(G));
        let v1 = Some("4:cpu:/docker/abc\n3:memory:/docker/abc");
        assert_eq!(remaining(Some(meminfo(20 * G)), v1), Some(2 * G));
        // No limit anywhere: the machine's; and no reading at all.
        let free = Some("0::/system.slice/x.service");
        assert_eq!(remaining(Some(meminfo(5 * G)), free), Some(5 * G));
        assert_eq!(remaining(None, None), None);
        // A limit of 0 leaves nothing; one whose use or value can't be read leaves what
        // isn't known, not the machine's.
        let at = |group: &str| format!("0::/{group}");
        assert_eq!(
            remaining(Some(meminfo(G)), Some(&at("zero.scope"))),
            Some(0)
        );
        assert_eq!(
            cgroup::limit(&system(&files, None, Some(&at("zero.scope")))),
            Ok(Some(0))
        );
        for group in ["blind.scope", "odd.scope"] {
            assert_eq!(
                remaining(Some(meminfo(G)), Some(&at(group))),
                None,
                "{group}"
            );
        }
    }

    /// The namespace's root, cgroup v2 at /sys/fs/cgroup and v1's memory hierarchy at
    /// /sys/fs/cgroup/memory (mounted on the v2 tree here, to keep both under one prefix).
    const MOUNTS: &str = "23 23 8:1 / / rw - ext4 /dev/root rw\n\
                          30 23 0:26 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n\
                          35 30 0:31 / /sys/fs/cgroup/memory rw - cgroup cgroup rw,memory\n";

    /// This process as its group's `cgroup.procs` lists it (its id in its own pid namespace),
    /// and `/proc/self/stat` where /proc is that namespace's.
    fn procs() -> &'static str {
        static PROCS: std::sync::LazyLock<String> =
            std::sync::LazyLock::new(|| format!("{}\n", std::process::id()));
        PROCS.as_str()
    }

    fn stat() -> String {
        format!("{} (nrese-server) S 1\n", std::process::id())
    }

    /// `files` and [`MOUNTS`] as `/proc/self/mountinfo`, for [`system`].
    fn fixture(files: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        [("/proc/self/mountinfo", MOUNTS)]
            .iter()
            .chain(files)
            .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
            .collect()
    }

    /// The kernel's answers for `files`, with `/proc/meminfo` and `/proc/self/cgroup` as given:
    /// a file it doesn't have is not found.
    fn system(
        files: &std::collections::HashMap<String, String>,
        meminfo: Option<&str>,
        own: Option<&str>,
    ) -> cgroup::declared::Declared {
        let mut files = files.clone();
        files.insert("/proc/self/stat".to_owned(), stat());
        for (path, text) in [("/proc/meminfo", meminfo), ("/proc/self/cgroup", own)] {
            match text {
                Some(text) => files.insert(path.to_owned(), text.to_owned()),
                None => files.remove(path),
            };
        }
        cgroup::declared::Declared::new(files, "")
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
        assert_eq!(available_bytes(), Ok(limit));
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
            assert!(remaining_bytes().is_ok_and(|remaining| remaining <= after + SLACK));
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
        assert_eq!(watch.cause(), Some(MemoryStopCause::OverLimit));
    }

    fn failed() -> Reading {
        Err(Unread::Failed)
    }

    fn unsupported() -> Reading {
        Err(Unread::Unsupported)
    }

    /// DC-07: a reading at the limit isn't over it, one byte past is; a missing reading of
    /// either kind stops as well, named as unread, never as "not over".
    #[test]
    fn a_missing_reading_stops_and_says_so() {
        use MemoryStopCause::{OverLimit, Unread as NotRead};
        let rows = [
            (Ok(100), 100, None),
            (Ok(101), 100, Some(OverLimit)),
            (Err(Unread::Failed), u64::MAX, Some(NotRead)),
            (Err(Unread::Unsupported), u64::MAX, Some(NotRead)),
        ];
        for (reading, limit, cause) in rows {
            assert_eq!(
                stop_cause(reading, limit),
                cause,
                "{reading:?} under {limit}"
            );
        }
    }

    /// DC-07: a watch whose reader fails stops the work, under any limit, stays stopped,
    /// and names the cause as unread.
    #[test]
    fn a_watch_that_cant_read_stops_as_unread() {
        for read in [failed as fn() -> Reading, unsupported] {
            let watch = MemoryWatch::with_reader(u64::MAX, read);
            assert_eq!(watch.cause(), None, "nothing read yet");
            assert!(watch.exceeded(), "an unread limit stops the work");
            assert!(watch.exceeded(), "and stays stopped");
            assert_eq!(watch.cause(), Some(MemoryStopCause::Unread));
        }
    }

    /// D14 (LM-V15-4): a watch's room is its limit less what its reader says the process
    /// holds, none past the limit, and no reading where the reader fails: an allocation
    /// of a known size is admitted by it, never against a room it couldn't read.
    #[test]
    fn a_watchs_room_is_its_limit_less_what_the_process_holds() {
        fn holds_100() -> Reading {
            Ok(100)
        }
        assert_eq!(MemoryWatch::with_reader(250, holds_100).room(), Ok(150));
        assert_eq!(MemoryWatch::with_reader(100, holds_100).room(), Ok(0));
        assert_eq!(MemoryWatch::with_reader(40, holds_100).room(), Ok(0));
        assert_eq!(
            MemoryWatch::with_reader(u64::MAX, failed).room(),
            Err(Unread::Failed)
        );
    }

    /// DC-07: the process limit stops the work when its reader fails, names the cause as
    /// unread, and without a limit (0) nothing is read and nothing stops.
    #[test]
    fn the_process_limit_stops_when_it_cant_read() {
        let limit = ProcessLimit::new(failed);
        limit.set(u64::MAX);
        assert_eq!(
            limit.stop(),
            Some(MemoryStopCause::Unread),
            "an unread process limit stops the work"
        );
        limit.set(0);
        assert_eq!(limit.stop(), None, "no limit: nothing to enforce");
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
