//! Count runner for the engine's committed term reads and a transaction's pending deltas:
//! area "engine internals", suite "term reads and transaction deltas", cases
//! `engine-term-reads-memory` and `engine-term-reads-reopened` (the two shapes of
//! [`store`]). It answers "does B do more work per row than A?" without a timing host.
//!
//! One process counts one op over one store. It builds the store from the seed (not
//! counted), takes one committed snapshot, and runs the op inside its counted window:
//!
//! ```text
//! perf stat -x, -e instructions:u --delay=-1 --control fd:CTL,ACK -- \
//!     term-reads-count --shape memory|reopened --op OP --threads N --seed SEED \
//!     --control fd:CTL,ACK [--side A|B] [--commit SHA]
//! term-reads-count --shape S --op OP --threads N --seed SEED --count allocs
//! ```
//!
//! - **Instructions.** perf hands the workload its control descriptors as open, inherited
//!   files under the numbers it was given; the runner learns them from its own
//!   `--control fd:CTL,ACK` (the same text as perf's), which this pass refuses to run without.
//!   It writes `enable` and `disable` around the op and waits for perf's `ack` after each,
//!   up to a deadline: a missing ack fails the run, so no unconfirmed window is reported.
//!   Inside the window the harness allocates and formats nothing, and a 1-thread op runs on
//!   the main thread with no synchronisation. What is left (returning from the enable's ack,
//!   writing the disable, and at more threads the readers' start and end barriers) is what
//!   the `noop` op counts: the same window around an empty body, so an op's count is read net
//!   of the `noop` at its shape and threads.
//! - **Allocations.** `--count allocs` runs `term-reads-count-allocs`, the binary beside this
//!   one, whose global allocator counts on every thread ([`Counting`]): reset as the op's
//!   window opens and read as it closes, so the load isn't counted. This binary's allocator
//!   is plain mimalloc, so the instruction pass has no counting in it.
//! - **Output.** One JSON line on stdout, the only thing printed on success: `case`,
//!   `shape`, `op`, `threads`, `side`, `commit`, `seed`, `rows`, `instructions_u`, `allocs`,
//!   `alloc_bytes`. `instructions_u` is `null`: perf reports it once the process has ended,
//!   and `merge.sh` (beside this crate's manifest) fills it in from perf's CSV. `allocs` and
//!   `alloc_bytes` are `null` outside the allocation pass; `side` and `commit` are `null`
//!   unless given.
//! - **The kernel's share isn't counted** (`instructions:u`). That matters for one op: in the
//!   reopened shape, `takeover`'s commit appends to the store's WAL (unsynced), and only the
//!   user-space part of that write is in its count.
//!
//! Linux only for the instruction pass (perf); everything else builds anywhere.

use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

mod ops;
mod store;

/// The seed every store and every op's input is drawn from; `--seed` defaults to it.
pub const SEED: u64 = 20_261_108;

/// How long the runner waits for perf's `ack` (`--ack-deadline-ms`). perf polls its control
/// descriptor while the workload runs, so its answer comes at once; the deadline only bounds a
/// perf that has stopped answering.
const ACK_DEADLINE_MS: u64 = 10_000;

const USAGE: &str = "usage: term-reads-count --shape memory|reopened --op OP --threads N \
    [--seed SEED] (--control fd:CTL,ACK [--ack-deadline-ms MS] | --count allocs) \
    [--side NAME] [--commit SHA] [--rows N (decode)]\n\
    ops: decode lookup with_views decode_quad insert_remove takeover pending_snapshot pending_over \
    noop";

/// The two stores ([`store::Store::build`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    /// In memory: terms read from the dictionary's heap.
    Memory,
    /// The same data after a checkpoint and a reopen: terms read from the persisted base.
    Reopened,
}

impl Shape {
    fn name(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Reopened => "reopened",
        }
    }

    fn case(self) -> &'static str {
        match self {
            Self::Memory => "engine-term-reads-memory",
            Self::Reopened => "engine-term-reads-reopened",
        }
    }
}

/// The counted phases ([`ops`]), by their names in the case.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Decode,
    Lookup,
    WithViews,
    DecodeQuad,
    InsertRemove,
    Takeover,
    PendingSnapshot,
    PendingOver,
    /// The window around an empty body: what the harness itself counts.
    Noop,
}

const OPS: [(Op, &str); 9] = [
    (Op::Decode, "decode"),
    (Op::Lookup, "lookup"),
    (Op::WithViews, "with_views"),
    (Op::DecodeQuad, "decode_quad"),
    (Op::InsertRemove, "insert_remove"),
    (Op::Takeover, "takeover"),
    (Op::PendingSnapshot, "pending_snapshot"),
    (Op::PendingOver, "pending_over"),
    (Op::Noop, "noop"),
];

impl Op {
    fn name(self) -> &'static str {
        OPS.iter()
            .find(|(op, _)| *op == self)
            .map_or("", |(_, name)| name)
    }

    /// The read ops (and `noop`), which run on any number of threads; a transaction is
    /// single-writer.
    fn reads(self) -> bool {
        matches!(
            self,
            Self::Decode | Self::Lookup | Self::WithViews | Self::DecodeQuad | Self::Noop
        )
    }
}

struct Args {
    shape: Shape,
    op: Op,
    threads: usize,
    seed: u64,
    count_allocs: bool,
    control: Option<(i32, i32)>,
    ack_deadline_ms: u64,
    /// `decode`'s ids, if not the case's 1,000,000: to check that a count net of `noop`
    /// scales with its rows.
    rows: Option<usize>,
    side: Option<String>,
    commit: Option<String>,
}

impl Args {
    fn parse(raw: &[String]) -> Result<Self, String> {
        let (mut shape, mut op, mut threads) = (None, None, None);
        let mut args = Self {
            shape: Shape::Memory,
            op: Op::Decode,
            threads: 1,
            seed: SEED,
            count_allocs: false,
            control: None,
            ack_deadline_ms: ACK_DEADLINE_MS,
            rows: None,
            side: None,
            commit: None,
        };
        let mut raw = raw.iter();
        while let Some(flag) = raw.next() {
            let mut value = || raw.next().ok_or_else(|| format!("{flag} needs a value"));
            match flag.as_str() {
                "--shape" => {
                    shape = Some(match value()?.as_str() {
                        "memory" => Shape::Memory,
                        "reopened" => Shape::Reopened,
                        other => return Err(format!("unknown shape {other}")),
                    })
                }
                "--op" => {
                    let wanted = value()?;
                    op = Some(
                        OPS.iter()
                            .find(|(_, known)| *known == wanted.as_str())
                            .map(|(op, _)| *op)
                            .ok_or_else(|| format!("unknown op {wanted}"))?,
                    );
                }
                "--threads" => threads = Some(number(value()?)? as usize),
                "--seed" => args.seed = number(value()?)?,
                "--count" => match value()?.as_str() {
                    "allocs" => args.count_allocs = true,
                    other => return Err(format!("unknown count {other}")),
                },
                "--control" => args.control = Some(control_fds(value()?)?),
                "--ack-deadline-ms" => args.ack_deadline_ms = number(value()?)?,
                "--rows" => args.rows = Some(number(value()?)? as usize),
                "--side" => args.side = Some(name(value()?)?),
                "--commit" => args.commit = Some(name(value()?)?),
                other => return Err(format!("unknown argument {other}")),
            }
        }
        args.shape = shape.ok_or("--shape is required")?;
        args.op = op.ok_or("--op is required")?;
        args.threads = threads.ok_or("--threads is required")?;
        if args.threads == 0 {
            return Err("--threads is at least 1".into());
        }
        if !args.op.reads() && args.threads != 1 {
            return Err(format!("{} runs on 1 thread", args.op.name()));
        }
        if args
            .rows
            .is_some_and(|rows| args.op != Op::Decode || rows == 0)
        {
            return Err("--rows is decode's, at least 1".into());
        }
        match (args.count_allocs, args.control.is_some()) {
            (true, true) => Err("--count allocs is a pass of its own, without --control".into()),
            (false, false) => Err("the instruction pass runs under perf, with --control".into()),
            _ => Ok(args),
        }
    }
}

fn number(text: &str) -> Result<u64, String> {
    text.parse().map_err(|_| format!("not a number: {text}"))
}

/// perf's `fd:CTL,ACK`.
fn control_fds(text: &str) -> Result<(i32, i32), String> {
    let fds = text.strip_prefix("fd:").and_then(|fds| fds.split_once(','));
    let parse = |fd: &str| fd.parse::<i32>().ok().filter(|fd| *fd > 2);
    match fds.map(|(ctl, ack)| (parse(ctl), parse(ack))) {
        Some((Some(ctl), Some(ack))) => Ok((ctl, ack)),
        _ => Err(format!("--control takes fd:CTL,ACK, got {text}")),
    }
}

/// A side or commit name, printed into the JSON line as it is: no character needs escaping.
fn name(text: &str) -> Result<String, String> {
    let plain = |c: char| c.is_ascii_alphanumeric() || "._-".contains(c);
    match !text.is_empty() && text.chars().all(plain) {
        true => Ok(text.to_owned()),
        false => Err(format!("names are [A-Za-z0-9._-]: {text}")),
    }
}

/// The runner's entry point: `counting` where the global allocator is [`Counting`].
pub fn main(counting: bool) -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = match Args::parse(&raw) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("term-reads-count: {error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if args.count_allocs != counting {
        return match counting {
            false => allocation_pass(&raw),
            true => {
                eprintln!("term-reads-count-allocs is the --count allocs pass\n{USAGE}");
                ExitCode::from(2)
            }
        };
    }
    match run(&args) {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("term-reads-count: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `--count allocs`: the same arguments to the counting binary beside this one.
fn allocation_pass(raw: &[String]) -> ExitCode {
    let binary = std::env::current_exe().map(|exe| {
        exe.with_file_name(format!(
            "term-reads-count-allocs{}",
            std::env::consts::EXE_SUFFIX
        ))
    });
    match binary.and_then(|binary| std::process::Command::new(binary).args(raw).status()) {
        Ok(status) => ExitCode::from(status.code().map_or(1, |code| code as u8)),
        Err(error) => {
            eprintln!("term-reads-count: starting term-reads-count-allocs failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<String, String> {
    let mut window = Window {
        control: args
            .control
            .map(|fds| Control::open(fds, args.ack_deadline_ms))
            .transpose()?,
        counting: args.count_allocs,
        counted: None,
    };
    let store = store::Store::build(args.shape, args.seed)?;
    let rows = ops::run(
        args.op,
        args.threads,
        args.rows,
        &store,
        args.seed,
        &mut window,
    )?;
    let json =
        |value: &Option<String>| value.as_ref().map_or("null".into(), |v| format!("\"{v}\""));
    let (allocs, bytes) = match window.counted {
        Some((allocs, bytes)) => (allocs.to_string(), bytes.to_string()),
        None => ("null".into(), "null".into()),
    };
    Ok(format!(
        "{{\"case\":\"{}\",\"shape\":\"{}\",\"op\":\"{}\",\"threads\":{},\"side\":{},\
         \"commit\":{},\"seed\":{},\"rows\":{rows},\"instructions_u\":null,\"allocs\":{allocs},\
         \"alloc_bytes\":{bytes}}}",
        args.shape.case(),
        args.shape.name(),
        args.op.name(),
        args.threads,
        json(&args.side),
        json(&args.commit),
        args.seed,
    ))
}

/// The counted window around one op: perf's events and the allocation counts.
pub(crate) struct Window {
    control: Option<Control>,
    counting: bool,
    /// Allocations and their bytes inside the window, once it has closed (allocation pass).
    counted: Option<(u64, u64)>,
}

impl Window {
    /// Runs `op` inside the window. `op` runs even if perf refuses to open it, so that
    /// threads waiting inside it are let go; the refusal is the result then. Between perf's
    /// enable and its disable the harness only returns from the enable's wait, checks the
    /// allocation flag and writes the disable: nothing allocates or formats (the `noop` op
    /// counts what is left).
    pub(crate) fn count<R>(&mut self, op: impl FnOnce() -> R) -> Result<R, String> {
        // The load's and the checkpoint's pool threads go to sleep first: perf counts every
        // thread of the process.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let opened = match &mut self.control {
            Some(control) => control.send(b"enable\n"),
            None => Ok(()),
        };
        if self.counting {
            ALLOCATIONS.store(0, Ordering::SeqCst);
            ALLOCATED_BYTES.store(0, Ordering::SeqCst);
        }
        let result = op();
        if self.counting {
            self.counted = Some((
                ALLOCATIONS.load(Ordering::SeqCst),
                ALLOCATED_BYTES.load(Ordering::SeqCst),
            ));
        }
        let closed = match (&mut self.control, &opened) {
            (Some(control), Ok(())) => control.send(b"disable\n"),
            _ => Ok(()),
        };
        opened
            .map_err(control_failed("enable"))
            .and(closed.map_err(control_failed("disable")))
            .map(|()| result)
    }
}

fn control_failed(command: &'static str) -> impl Fn(&'static str) -> String {
    move |error| format!("perf control ({command}): {error}")
}

/// perf's control and acknowledgement descriptors (`--control fd:CTL,ACK`).
#[cfg(target_os = "linux")]
struct Control {
    ctl: std::fs::File,
    ack: std::fs::File,
    /// `poll`'s timeout for one answer, in milliseconds.
    deadline_ms: i32,
}

#[cfg(target_os = "linux")]
impl Control {
    fn open((ctl, ack): (i32, i32), deadline_ms: u64) -> Result<Self, String> {
        use std::os::fd::FromRawFd;
        for fd in [ctl, ack] {
            if std::fs::metadata(format!("/proc/self/fd/{fd}")).is_err() {
                return Err(format!(
                    "--control: descriptor {fd} isn't open (run under perf)"
                ));
            }
        }
        // SAFETY: both descriptors are open (checked above), inherited from perf's caller for
        // this process to write commands to and read acknowledgements from; nothing else in
        // the process uses them, so these files own them.
        let (ctl, ack) = unsafe {
            (
                std::fs::File::from_raw_fd(ctl),
                std::fs::File::from_raw_fd(ack),
            )
        };
        Ok(Self {
            ctl,
            ack,
            deadline_ms: i32::try_from(deadline_ms).unwrap_or(i32::MAX),
        })
    }

    /// Writes `command` (a line) and waits for perf's `ack\n` (perf writes a NUL after it),
    /// failing past the deadline: a window perf didn't confirm is never reported. Runs in the
    /// window, so it allocates and formats nothing: the answer goes into a buffer on the stack.
    fn send(&mut self, command: &'static [u8]) -> Result<(), &'static str> {
        use std::io::{Read, Write};
        use std::os::fd::AsRawFd;
        self.ctl
            .write_all(command)
            .map_err(|_| "writing the command failed")?;
        let (mut answer, mut length) = ([0u8; 8], 0);
        while !answer[..length].contains(&b'\n') {
            let mut ready = libc::pollfd {
                fd: self.ack.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one initialised pollfd, for a descriptor this process owns, and its count.
            match unsafe { libc::poll(&mut ready, 1, self.deadline_ms) } {
                0 => return Err("no ack within --ack-deadline-ms"),
                1.. => {}
                _ => return Err("waiting for the ack failed"),
            }
            let mut read = [0u8; 8];
            let got = match self.ack.read(&mut read) {
                Ok(0) | Err(_) => return Err("the ack descriptor closed"),
                Ok(got) => got,
            };
            for &byte in read[..got].iter().filter(|&&byte| byte != 0) {
                *answer.get_mut(length).ok_or("answered more than ack")? = byte;
                length += 1;
            }
        }
        match &answer[..length] == b"ack\n" {
            true => Ok(()),
            false => Err("answered something other than ack"),
        }
    }
}

#[cfg(not(target_os = "linux"))]
struct Control;

#[cfg(not(target_os = "linux"))]
impl Control {
    fn open(_: (i32, i32), _: u64) -> Result<Self, String> {
        Err("--control needs Linux (perf)".into())
    }

    fn send(&mut self, _: &'static [u8]) -> Result<(), &'static str> {
        Ok(())
    }
}

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

/// The allocation pass's global allocator: `A`, counting each allocation, zeroed allocation
/// and reallocation (as one allocation of its new size) and the bytes it asks for. Frees
/// aren't counted.
pub struct Counting<A>(pub A);

// SAFETY: every call is passed on to `A` unchanged, with the caller's arguments; counting
// touches only two atomics and never allocates.
unsafe impl<A: std::alloc::GlobalAlloc> std::alloc::GlobalAlloc for Counting<A> {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        counted(layout.size());
        // SAFETY: the caller's contract for `alloc`, passed on.
        unsafe { self.0.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        counted(layout.size());
        // SAFETY: the caller's contract for `alloc_zeroed`, passed on.
        unsafe { self.0.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        counted(new_size);
        // SAFETY: the caller's contract for `realloc`, passed on.
        unsafe { self.0.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        // SAFETY: the caller's contract for `dealloc`, passed on.
        unsafe { self.0.dealloc(ptr, layout) }
    }
}

fn counted(bytes: usize) {
    ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    ALLOCATED_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
}
