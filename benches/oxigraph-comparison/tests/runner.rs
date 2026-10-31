//! The syntax benchmark's receipt: a run proves it timed what it selected (Sol, PR #61 F3: a
//! filter matching no comparison timed nothing and exited successfully).

use std::process::Command;

use oxigraph_comparison::w3c;

#[test]
fn a_filter_selecting_no_comparison_fails_the_run() {
    // Without the suites the binary refuses before any selection: nothing to show here.
    if w3c::admitted(w3c::sparql(&["rq", "ru"])).is_none() {
        return;
    }
    let out = Command::new(env!("CARGO_BIN_EXE_syntax"))
        .arg("__no_comparison_is_named_so__")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "SPEC a run that times nothing fails\n{stderr}"
    );
    assert!(
        stderr.contains("0 of 0 selected comparisons timed"),
        "{stderr}"
    );
}
