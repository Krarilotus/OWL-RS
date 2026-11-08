//! Faults the runner must catch, injected through the test-only feature `inject`, each named
//! by the run's `TRC_INJECT`. Run them with `cargo test --release --features inject`: each
//! builds a store.

use std::process::{Command, Output};

/// The allocation pass (no perf needed) of `op` at 16 threads, with `fault` injected.
fn run(op: &str, fault: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_term-reads-count"))
        .args(["--shape", "memory", "--op", op, "--threads", "16"])
        .args(["--count", "allocs"])
        .env("TRC_INJECT", fault)
        .output()
        .expect("the runner starts")
}

/// A reader of the parallel `with_views` that returns a wrong value of the right length fails
/// the run (package review r1, finding 2): the first IRI one reader views is replaced by
/// another IRI of the same length.
#[test]
fn a_wrong_view_of_the_same_length_on_one_reader_fails_the_run() {
    let run = run("with_views", "wrong-view");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        !run.status.success(),
        "the wrong view was accepted: {}",
        String::from_utf8_lossy(&run.stdout)
    );
    assert!(
        stderr.contains("viewed right"),
        "the run failed, but not on the wrong view: {stderr}"
    );
}

/// A reader that starts late still does its setup before the counted phase (fix check r2,
/// R2-F1): the first of 16 readers waits a second before its warm-up allocation, and `noop`'s
/// phase, which allocates nothing itself, must count no allocation.
#[test]
fn a_late_readers_warm_up_stays_out_of_the_counted_phase() {
    let run = run("noop", "late-reader");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success(),
        "the run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        stdout.contains("\"allocs\":0,\"alloc_bytes\":0}"),
        "the late reader's warm-up was counted in the phase: {stdout}"
    );
}
