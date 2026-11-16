//! What the one-shot commands write on standard output, run as an operator or a script
//! runs them: with the default logging (no `RUST_LOG`). Their standard output is their
//! result; the log lines (INFO: opening the store) go to standard error.

use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The longest one command on an empty store may take.
const LIMIT: Duration = Duration::from_secs(120);

/// This build's binary on an on-disk store in `data`, without reasoning, with the default
/// logging, run in `dir` (no `.env` of the checkout).
fn nrese(dir: &Path, data: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nrese-server"));
    command
        .current_dir(dir)
        .env_remove("RUST_LOG")
        // Plain log lines in the failure messages.
        .env("NO_COLOR", "1")
        .env("NRESE_STORE_MODE", "on-disk")
        .env("NRESE_DATA_DIR", data)
        .env("NRESE_REASONING_MODE", "disabled");
    command
}

struct Output {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

/// Runs `command` to its end, its standard output and standard error kept apart; killed
/// and an error if it runs longer than [`LIMIT`].
fn run(command: &mut Command) -> Result<Output, Box<dyn std::error::Error>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let (sender, received) = mpsc::channel();
    let streams: [Box<dyn Read + Send>; 2] = [
        Box::new(child.stdout.take().expect("piped")),
        Box::new(child.stderr.take().expect("piped")),
    ];
    for (index, mut stream) in streams.into_iter().enumerate() {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stream.read_to_end(&mut bytes);
            let _ = sender.send((index, String::from_utf8_lossy(&bytes).into_owned()));
        });
    }
    let until = Instant::now() + LIMIT;
    let mut texts = [String::new(), String::new()];
    for _ in 0..texts.len() {
        match received.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok((index, text)) => texts[index] = text,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("still running after {LIMIT:?}").into());
            }
        }
    }
    let status = child.wait()?;
    let [stdout, stderr] = texts;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// `query --format json`: standard output is one SPARQL 1.1 JSON results document (`ASK {}`
/// is true), nothing before or after it; the INFO lines are on standard error.
#[test]
fn query_json_output_is_the_results_document_alone() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let output =
        run(nrese(dir.path(), &dir.path().join("data"))
            .args(["query", "--format", "json", "ASK {}"]))?;
    assert!(
        output.status.success(),
        "{}:\n{}\n{}",
        output.status,
        output.stdout,
        output.stderr
    );
    let document: serde_json::Value = serde_json::from_str(&output.stdout).map_err(|error| {
        format!(
            "standard output is not one JSON document ({error}):\n{}",
            output.stdout
        )
    })?;
    assert_eq!(
        document["boolean"],
        serde_json::Value::Bool(true),
        "{document}"
    );
    // Still logged, elsewhere.
    assert!(output.stderr.contains("INFO"), "{}", output.stderr);
    Ok(())
}

/// `print-query` against the configured store: standard output starts with its
/// completeness comment, then the query (its documented output).
#[test]
fn print_query_output_starts_with_its_completeness_comment()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let output = run(nrese(dir.path(), &dir.path().join("data"))
        .args(["print-query", "SELECT ?s WHERE { ?s a <urn:C> }"]))?;
    assert!(
        output.status.success(),
        "{}:\n{}\n{}",
        output.status,
        output.stdout,
        output.stderr
    );
    assert!(
        output.stdout.starts_with("# NRESE completeness: "),
        "{}",
        output.stdout
    );
    Ok(())
}

/// An annotation property's domain, disjoint with its subject's class: the OWL 2 RL rules
/// clash (prp-dom on the annotation), while OWL 2 DL gives the axiom no meaning there.
const ANNOTATION_CLASH: &str = "<http://example.com/note> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#AnnotationProperty> .
<http://example.com/note> <http://www.w3.org/2000/01/rdf-schema#domain> <http://example.com/C> .
<http://example.com/C> <http://www.w3.org/2002/07/owl#disjointWith> <http://example.com/D> .
<http://example.com/x> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://example.com/D> .
<http://example.com/x> <http://example.com/note> \"v\" .
";

/// Under `owl2-dl`, `load` of [`ANNOTATION_CLASH`] into a fresh on-disk store fails, and
/// says what is known: the rules' clash, which this release doesn't decide, not an
/// inconsistency.
#[test]
fn load_of_a_clash_owl2_dl_reads_differently_fails_as_undecided()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("annotation-clash.nt");
    std::fs::write(&file, ANNOTATION_CLASH)?;
    let output = run(nrese(dir.path(), &dir.path().join("data"))
        .env("NRESE_REASONING_MODE", "owl2-dl")
        .arg("load")
        .arg(&file))?;
    assert!(
        !output.status.success()
            && output
                .stderr
                .contains("annotation-property axioms or a punned name")
            && !output.stderr.contains("inconsistent under"),
        "{}:\n{}\n{}",
        output.status,
        output.stdout,
        output.stderr
    );
    Ok(())
}
