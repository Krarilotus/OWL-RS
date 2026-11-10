//! Faults the runner must catch, injected through the test-only feature `inject`, each named
//! by the run's `TRC_INJECT`. Run them with `cargo test --release --features inject`: each
//! builds a store.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Output, Stdio};

/// The allocation pass (no perf needed) of `op` at 16 threads, with `fault` injected.
fn runner(op: &str, fault: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_term-reads-count"));
    command
        .args(["--shape", "memory", "--op", op, "--threads", "16"])
        .args(["--count", "allocs"])
        .env("TRC_INJECT", fault);
    command
}

/// A reader of the parallel `with_views` that returns a wrong value of the right length fails
/// the run (package review r1, finding 2): the first IRI one reader views is replaced by
/// another IRI of the same length.
#[test]
fn a_wrong_view_of_the_same_length_on_one_reader_fails_the_run() {
    let run: Output = runner("with_views", "wrong-view")
        .output()
        .expect("the runner starts");
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

/// A reader that is late with its setup holds the window shut, and its setup isn't counted
/// (fix check r2, R2-F1; r3: by the test's own schedule, no timing). The first of 16 readers
/// is held before its warm-up until this test releases it on stdin. While it is held, the
/// controller must stop short of the window: it says it waits for its readers, and must not
/// say the window opens. Released, the reader must find the window not yet open, and noop's
/// phase, which allocates nothing itself, must count no allocation.
#[test]
fn a_late_readers_warm_up_stays_out_of_the_counted_phase() {
    let mut child = runner("noop", "late-reader")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the runner starts");
    let mut stderr = BufReader::new(child.stderr.take().expect("its stderr"));
    let (mut held, mut waits, mut seen) = (false, false, Vec::new());
    while !(held && waits) {
        let mut line = String::new();
        let read = stderr.read_line(&mut line).expect("its stderr reads");
        seen.push(line.trim_end().to_owned());
        match line.trim_end() {
            "inject: late reader held" => held = true,
            "inject: the controller waits for its readers" => waits = true,
            "inject: the window opens" => {
                let _ = child.kill();
                panic!("the window began to open while a reader was still held: {seen:?}");
            }
            _ if read == 0 => panic!("the run ended before the reader was released: {seen:?}"),
            _ => {}
        }
    }
    let mut stdin = child.stdin.take().expect("its stdin");
    stdin.write_all(b"go\n").expect("the reader is released");
    drop(stdin);
    let mut rest = String::new();
    for line in stderr.lines() {
        rest.push_str(&line.expect("its stderr reads"));
        rest.push('\n');
    }
    let run = child.wait_with_output().expect("the run ends");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "the run failed: {rest}");
    assert!(
        rest.contains("inject: late reader released, the window not yet open"),
        "the window opened before the late reader was released: {rest}"
    );
    assert!(
        stdout.contains("\"allocs\":0,\"alloc_bytes\":0}"),
        "the late reader's warm-up was counted in the phase: {stdout}"
    );
}
