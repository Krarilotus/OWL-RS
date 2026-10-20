#!/usr/bin/env python3
"""The perf lab's timing loop without a thread start in the timed window, for every side of a
release gate alike (benches/PROTOCOL.md §5, "Noise").

Every revision's perf lab (main f9b515e8, v1.5 0f465633, v2) spawned a canceller thread per run
just before its timer started; the thread's start (its stack and TLS faulted into the shared
address space, its scheduling) ran inside the timed call: 1-5 µs on microsecond queries against
0.3-0.45 µs of difference between the engines in a clean loop (the peak-memory agent's profile,
19 Oct 2026). This replaces it, textually, with one process-wide watchdog, started once and
confirmed running before any timing, armed before the timer and disarmed after it: no thread
starts inside the window, and the watchdog's own 10 ms polls take no lock the timed call takes.
The engine is untouched; only the example's measuring loop changes.

    perf_lab_watchdog.py FILE      applied (exit 0, "applied"), already there ("present"), or
                                   the loop isn't the known one (exit 1, "no match": the side's
                                   loop is measured as it is and the gate refuses to compare it)

release-gate.sh build applies it to each revision's perf lab source before building it.
"""
from __future__ import annotations

import sys
from pathlib import Path

MARKER = "mod perf_lab_watchdog"

SPAWN = """    let token = CancellationToken::new();
    let canceller = {
        let token = token.clone();
        let (done, wait) = std::sync::mpsc::channel::<()>();
        (
            std::thread::spawn(move || {
                if wait.recv_timeout(timeout).is_err() {
                    token.cancel();
                }
            }),
            done,
        )
    };
"""
ARM = """    let token = CancellationToken::new();
    // The process's watchdog cancels at the timeout: armed before the timed window, disarmed
    // after it, so that no thread starts inside it (benches/fast/patches/perf_lab_watchdog.py).
    let armed = perf_lab_watchdog::arm(&token, timeout);
"""
JOIN = """    let _ = canceller.1.send(());
    let _ = canceller.0.join();
"""
DISARM = """    drop(armed);
"""
MODULE = """
/// The timeout of every timed run, by one thread for the whole process (release gate harness:
/// benches/fast/patches/perf_lab_watchdog.py): started once, before any timing, and running
/// before it returns; it polls every 10 ms and takes the slot's lock only then, which the timed
/// call never takes.
mod perf_lab_watchdog {
    use super::CancellationToken;
    use std::sync::{Mutex, OnceLock, mpsc};
    use std::time::{Duration, Instant};

    type Slot = Mutex<Option<(Instant, CancellationToken)>>;
    const POLL: Duration = Duration::from_millis(10);

    fn slot() -> &'static Slot {
        static SLOT: OnceLock<Slot> = OnceLock::new();
        static STARTED: OnceLock<()> = OnceLock::new();
        let slot: &'static Slot = SLOT.get_or_init(|| Mutex::new(None));
        STARTED.get_or_init(|| {
            let (ready, started) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = ready.send(());
                loop {
                    std::thread::sleep(POLL);
                    let mut armed = slot.lock().unwrap_or_else(|e| e.into_inner());
                    if armed
                        .as_ref()
                        .is_some_and(|(deadline, _)| Instant::now() >= *deadline)
                        && let Some((_, token)) = armed.take()
                    {
                        token.cancel();
                    }
                }
            });
            let _ = started.recv();
        });
        slot
    }

    /// Disarms the watchdog when dropped.
    pub struct Armed;

    impl Drop for Armed {
        fn drop(&mut self) {
            *slot().lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }

    /// Cancels `token` once `timeout` has passed, unless the guard is dropped first.
    pub fn arm(token: &CancellationToken, timeout: Duration) -> Armed {
        *slot().lock().unwrap_or_else(|e| e.into_inner()) =
            Some((Instant::now() + timeout, token.clone()));
        Armed
    }
}
"""


def patch(text: str) -> tuple[str, str]:
    """The source with the watchdog, and what happened: applied, present, or no match."""
    if MARKER in text:
        return text, "present"
    if text.count(SPAWN) != 1 or text.count(JOIN) != 1:
        return text, "no match"
    return text.replace(SPAWN, ARM).replace(JOIN, DISARM).rstrip("\n") + "\n" + MODULE, "applied"


def main() -> int:
    path = Path(sys.argv[1])
    text, outcome = patch(path.read_text(encoding="utf-8"))
    if outcome == "applied":
        path.write_text(text, encoding="utf-8")
    print(outcome)
    return 1 if outcome == "no match" else 0


if __name__ == "__main__":
    sys.exit(main())
