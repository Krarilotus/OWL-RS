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

/// The memory this process may use: the container's limit if there is one (cgroup v2,
/// then v1, on Linux), else the machine's physical memory; `None` where the platform
/// doesn't say.
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

    pub fn available_bytes() -> Option<u64> {
        let mut status = MEMORYSTATUSEX {
            dwLength: u32::try_from(std::mem::size_of::<MEMORYSTATUSEX>()).ok()?,
            ..Default::default()
        };
        // SAFETY: `status` is a valid, writable MEMORYSTATUSEX with dwLength set, as the
        // API requires.
        let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
        (ok != 0).then_some(status.ullTotalPhys)
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
        // cgroup v2, then v1; "max" and absurd values mean "no limit".
        let container = [
            "/sys/fs/cgroup/memory.max",
            "/sys/fs/cgroup/memory/memory.limit_in_bytes",
        ]
        .iter()
        .filter_map(|path| read(path)?.trim().parse::<u64>().ok())
        .find(|&limit| limit > 0 && limit < machine);
        Some(container.unwrap_or(machine))
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

    pub fn available_bytes() -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_process_and_the_machine_report_their_memory() {
        let held = process_bytes().expect("the platform reports the process's memory");
        let total = available_bytes().expect("the platform reports the memory it may use");
        assert!(held > 0 && held < total, "{held} of {total}");
    }

    #[test]
    fn a_watch_trips_past_its_limit_and_stays_tripped() {
        assert!(!MemoryWatch::new(u64::MAX).exceeded());
        let watch = MemoryWatch::new(1);
        assert!(watch.exceeded());
        assert!(watch.exceeded());
    }
}
