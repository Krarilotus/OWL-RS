//! A reader of the parallel `with_views` that returns a wrong value of the right length fails
//! the run (package review r1, finding 2). Built with `--features inject`, whose hook replaces
//! the first IRI one reader views with another IRI of the same length; the run's own check
//! must refuse it. Run it with `cargo test --release --features inject`: it builds a store.

use std::process::Command;

#[test]
fn a_wrong_view_of_the_same_length_on_one_reader_fails_the_run() {
    let run = Command::new(env!("CARGO_BIN_EXE_term-reads-count"))
        .args(["--shape", "memory", "--op", "with_views", "--threads", "16"])
        .args(["--count", "allocs"])
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
