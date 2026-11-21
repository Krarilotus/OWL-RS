//! EARL 1.0 implementation reports for the W3C suites of `w3c/rdf-tests`, written by the
//! runners from one run's outcomes (`#[path]`-included by `w3c_rdf` and nrese-sparql's
//! `w3c_sparql11`).
//!
//! - **One assertion per manifest entry.** A test the runner didn't run is `earl:untested`, by
//!   name, with its reason; it is never left out and never passed. A report whose assertions
//!   don't match its manifests' entries one to one isn't written: the run fails.
//! - **The subject is the build.** Its name is the workspace manifest's
//!   `[workspace.metadata.project] name`; its version, licence and repository are Cargo's; its
//!   developer is the repository's owner. The commit, whether the tree had local changes and
//!   the suite's commit are read with git at run time (cargo rebuilds the runner from the tree
//!   it runs in); the suite's only from a git checkout of w3c/rdf-tests itself, with any change
//!   to it said. The assertor is the runner, not a person.
//! - **Where.** `NRESE_W3C_REPORTS` names the folder (`scripts/w3c-reports.sh` sets it);
//!   without it, `w3c-reports` under the target's temp folder.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Failed,
    Untested,
}

pub struct Assertion {
    /// The test's IRI, without angle brackets.
    pub test: String,
    pub outcome: Outcome,
    /// Why it failed or didn't run, and what it requires (`mf:requires`).
    pub info: Option<String>,
}

pub struct Report {
    /// What the report covers, as its title says: `SPARQL 1.2`, `RDF 1.2 Turtle`.
    pub title: String,
    /// The runner, by its path in the repository.
    pub runner: &'static str,
    /// A directory inside the suite's checkout, whose commit the report names.
    pub suite: PathBuf,
    /// The entries of the manifests the run read, counted apart from the assertions.
    pub entries: usize,
    pub assertions: Vec<Assertion>,
}

/// Writes `report` as `<name>.ttl` in the reports' folder, and returns its path.
pub fn write(name: &str, report: &Report) -> PathBuf {
    let dir = std::env::var_os("NRESE_W3C_REPORTS").map_or_else(
        || Path::new(env!("CARGO_TARGET_TMPDIR")).join("w3c-reports"),
        PathBuf::from,
    );
    write_to(&dir, name, report)
}

/// [`write`] into `dir`. Panics, so failing the run, when the assertions aren't one per entry.
pub fn write_to(dir: &Path, name: &str, report: &Report) -> PathBuf {
    let tests: BTreeSet<&str> = report.assertions.iter().map(|a| a.test.as_str()).collect();
    assert!(
        tests.len() == report.assertions.len() && tests.len() == report.entries,
        "{}: {} assertions on {} distinct tests for {} manifest entries: every entry must be \
         reported once",
        report.title,
        report.assertions.len(),
        tests.len(),
        report.entries
    );
    let text = turtle(
        report,
        &Subject::of_this_build(),
        &suite_commit(&report.suite),
    );
    std::fs::create_dir_all(dir).expect("the reports' folder");
    let path = dir.join(format!("{name}.ttl"));
    std::fs::write(&path, text).expect("the report");
    path
}

/// What the report says of the engine.
struct Subject {
    name: String,
    version: &'static str,
    repository: &'static str,
    licences: Vec<String>,
    /// The commit built, with whether the tree had local changes.
    commit: String,
}

impl Subject {
    fn of_this_build() -> Self {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let clean = git(
            manifest_dir,
            &["status", "--porcelain", "--untracked-files=normal"],
        )
        .map(|status| status.is_empty());
        Self {
            name: project_name(manifest_dir),
            version: env!("CARGO_PKG_VERSION"),
            repository: env!("CARGO_PKG_REPOSITORY"),
            licences: env!("CARGO_PKG_LICENSE")
                .split(" OR ")
                .map(str::to_owned)
                .collect(),
            commit: match clean {
                Some(true) => git_head(manifest_dir),
                Some(false) => format!("{} with local changes", git_head(manifest_dir)),
                None => "unknown".to_owned(),
            },
        }
    }
}

/// `[workspace.metadata.project] name` of the workspace manifest above `dir`.
fn project_name(dir: &Path) -> String {
    let manifest = dir
        .ancestors()
        .map(|d| d.join("Cargo.toml"))
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .find(|text| text.lines().any(|line| line.trim() == "[workspace]"))
        .expect("the workspace manifest");
    let mut section = "";
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line;
        } else if section == "[workspace.metadata.project]"
            && let Some(value) = line.strip_prefix("name").map(str::trim_start)
            && let Some(value) = value.strip_prefix('=')
        {
            return value.trim().trim_matches('"').to_owned();
        }
    }
    panic!("the workspace manifest names no project ([workspace.metadata.project] name)")
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn git_head(dir: &Path) -> String {
    git(dir, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_owned())
}

/// The commit of the suite checkout `dir` is in, claimed only for a git checkout of
/// w3c/rdf-tests itself (not a copy inside this or another repository, which git would answer
/// for) without changes to it.
fn suite_commit(dir: &Path) -> String {
    let top = |d: &Path| git(d, &["rev-parse", "--show-toplevel"]);
    let suite = git(dir, &["remote", "get-url", "origin"])
        .is_some_and(|url| url.trim_end_matches(".git").ends_with("/w3c/rdf-tests"));
    let own = top(dir)
        .filter(|t| suite && Some(t) != top(Path::new(env!("CARGO_MANIFEST_DIR"))).as_ref());
    match own.and_then(|_| git(dir, &["status", "--porcelain", "--untracked-files=normal"])) {
        Some(status) if status.is_empty() => git_head(dir),
        Some(_) => format!("{} with local changes", git_head(dir)),
        None => "unknown (not a git checkout of w3c/rdf-tests)".to_owned(),
    }
}

fn turtle(report: &Report, subject: &Subject, suite_commit: &str) -> String {
    let count = |outcome| {
        report
            .assertions
            .iter()
            .filter(|a| a.outcome == outcome)
            .count()
    };
    let repository = subject.repository;
    let mut out = String::from(
        "@prefix earl: <http://www.w3.org/ns/earl#> .\n\
         @prefix doap: <http://usefulinc.com/ns/doap#> .\n\
         @prefix foaf: <http://xmlns.com/foaf/0.1/> .\n\
         @prefix dc: <http://purl.org/dc/terms/> .\n\
         @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\n",
    );
    let description = format!(
        "Written by {} from one run of commit {} against w3c/rdf-tests {suite_commit}: {} \
         passed, {} failed, {} untested.",
        report.runner,
        subject.commit,
        count(Outcome::Passed),
        count(Outcome::Failed),
        count(Outcome::Untested)
    );
    let _ = write!(
        out,
        "<> foaf:primaryTopic <{repository}> ;\n  dc:title {} ;\n  dc:description {} ;\n  \
         dc:issued \"{}\"^^xsd:dateTime .\n\n",
        literal(&format!("{}: W3C {} tests", subject.name, report.title)),
        literal(&description),
        now_utc()
    );
    let _ = write!(
        out,
        "<{repository}> a doap:Project, earl:TestSubject, earl:Software ;\n  doap:name {} ;\n  \
         doap:homepage <{repository}> ;\n  doap:release [ doap:revision {} ] ",
        literal(&subject.name),
        literal(subject.version)
    );
    for licence in &subject.licences {
        let _ = write!(
            out,
            ";\n  doap:license <https://spdx.org/licenses/{licence}>"
        );
    }
    // The developer is the repository's owner: https://github.com/<owner>/<repository>.
    let owner = repository.split_once("://").and_then(|(scheme, rest)| {
        let mut parts = rest.split('/');
        let host = parts.next()?;
        let owner = parts.next().filter(|owner| !owner.is_empty())?;
        Some((format!("{scheme}://{host}/{owner}"), owner))
    });
    match owner {
        Some((account, owner)) => {
            let _ = write!(
                out,
                ";\n  doap:developer <{account}> .\n\n<{account}> a foaf:Agent ;\n  foaf:nick {} \
                 .\n\n",
                literal(owner)
            );
        }
        None => out.push_str(".\n\n"),
    }
    let _ = write!(
        out,
        "<#runner> a earl:Assertor, earl:Software ;\n  dc:title {} .\n\n",
        literal(report.runner)
    );
    let mut assertions: Vec<&Assertion> = report.assertions.iter().collect();
    assertions.sort_by(|a, b| a.test.cmp(&b.test));
    for assertion in assertions {
        let outcome = match assertion.outcome {
            Outcome::Passed => "passed",
            Outcome::Failed => "failed",
            Outcome::Untested => "untested",
        };
        let info = assertion
            .info
            .as_deref()
            .map(|info| format!(" ;\n    earl:info {}", literal(info)))
            .unwrap_or_default();
        let _ = write!(
            out,
            "[] a earl:Assertion ;\n  earl:assertedBy <#runner> ;\n  earl:subject <{repository}> ;\n  \
             earl:test <{}> ;\n  earl:mode earl:automatic ;\n  earl:result [ a earl:TestResult ;\n    \
             earl:outcome earl:{outcome}{info} ] .\n\n",
            assertion.test
        );
    }
    out
}

/// A Turtle string literal.
fn literal(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The time now, as an `xsd:dateTime` in UTC (to the second).
fn now_utc() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock after 1970")
        .as_secs();
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    // The civil date of a day count (Howard Hinnant's `civil_from_days`).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}
