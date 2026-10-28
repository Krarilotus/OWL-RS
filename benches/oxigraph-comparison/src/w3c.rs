//! The W3C SPARQL suites as pinned: `scripts/fetch-w3c-tests.sh` checks w3c/rdf-tests out
//! at one commit into `.cache/rdf-tests` (`NRESE_W3C_TESTS` overrides the place). The tests
//! and benchmarks that count on the suites take their files from here: every file of the
//! asked kinds the pinned checkout tracks, each read. A corpus that is missing, at another
//! commit or short of a file qualifies nothing.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The suites, as the checkout names their directories under `sparql/`.
pub const SUITES: [&str; 3] = ["sparql10", "sparql11", "sparql12"];

/// One file of a suite.
pub struct File {
    pub suite: &'static str,
    /// Its path under `sparql/`, as `sparql11/aggregates/agg01.srx`.
    pub name: String,
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

/// Why the corpus doesn't qualify a run.
pub enum Unqualified {
    /// Not fetched: no checkout there.
    Absent(PathBuf),
    /// There, but not the pinned suites in full.
    Broken(Vec<String>),
}

impl fmt::Display for Unqualified {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent(root) => write!(
                f,
                "no W3C suites at {} (scripts/fetch-w3c-tests.sh)",
                root.display()
            ),
            Self::Broken(reasons) => write!(
                f,
                "the W3C suites aren't the pinned ones in full: {}",
                reasons.join("; ")
            ),
        }
    }
}

/// Whether the W3C data is required (check.sh and CI set `NRESE_W3C_REQUIRED`): then a run
/// without it fails, else it says it doesn't qualify.
pub fn required() -> bool {
    std::env::var_os("NRESE_W3C_REQUIRED").is_some_and(|value| !value.is_empty())
}

/// The admitted corpus, or the run's verdict, decided here for every caller. A corpus that
/// is there but not the pinned one in full (another commit, a file missing or unreadable)
/// fails the run always. An absent one fails it where the W3C data is required; else the
/// run says it doesn't qualify and gets none.
pub fn admitted(corpus: Result<Vec<File>, Unqualified>) -> Option<Vec<File>> {
    match corpus {
        Ok(files) => Some(files),
        Err(why @ Unqualified::Broken(_)) => panic!("{why}"),
        Err(why @ Unqualified::Absent(_)) => {
            assert!(!required(), "NRESE_W3C_REQUIRED is set, but {why}");
            eprintln!("not qualified: {why}");
            None
        }
    }
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Where the checkout is.
pub fn checkout() -> PathBuf {
    std::env::var_os("NRESE_W3C_TESTS")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace().join(".cache/rdf-tests"))
}

/// The commit the fetch script pins w3c/rdf-tests to: the script is the pin's one owner.
fn pin() -> Result<String, String> {
    let script = workspace().join("scripts/fetch-w3c-tests.sh");
    let text =
        std::fs::read_to_string(&script).map_err(|e| format!("{}: {e}", script.display()))?;
    text.split("fetch w3c/rdf-tests ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .filter(|commit| commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned)
        .ok_or_else(|| format!("{}: no pinned w3c/rdf-tests commit", script.display()))
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git {}: {}", args.join(" "), stderr.trim()));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("git {}: {e}", args.join(" ")))
}

/// Every file with one of `extensions` the pinned SPARQL suites track, read, in the
/// checkout's order; or why there is no such corpus.
pub fn sparql(extensions: &[&str]) -> Result<Vec<File>, Unqualified> {
    let root = checkout();
    if !root.join(".git").exists() {
        return Err(Unqualified::Absent(root));
    }
    let broken = |reason: String| Unqualified::Broken(vec![reason]);
    let pin = pin().map_err(broken)?;
    let head = git(&root, &["rev-parse", "HEAD"]).map_err(broken)?;
    let head = head.trim();
    if head != pin {
        let at = root.display();
        return Err(broken(format!("{at} is at {head}, the pin is {pin}")));
    }
    let dirs = SUITES.map(|suite| format!("sparql/{suite}"));
    let mut args = vec!["ls-files", "-z", "--"];
    args.extend(dirs.iter().map(String::as_str));
    let listed = git(&root, &args).map_err(broken)?;
    let (mut files, mut reasons) = (Vec::new(), Vec::new());
    for name in listed.split('\0').filter(|name| !name.is_empty()) {
        let extension = name.rsplit_once('.').map(|(_, extension)| extension);
        if !extension.is_some_and(|extension| extensions.contains(&extension)) {
            continue;
        }
        let name = name.strip_prefix("sparql/").unwrap_or(name);
        let suite = SUITES
            .into_iter()
            .find(|suite| name.starts_with(&format!("{suite}/")));
        let Some(suite) = suite else {
            reasons.push(format!("{name}: under no suite"));
            continue;
        };
        let path = root.join("sparql").join(name);
        match std::fs::read(&path) {
            Ok(bytes) => files.push(File {
                suite,
                name: name.to_owned(),
                path,
                bytes,
            }),
            Err(e) => reasons.push(format!("{name}: {e}")),
        }
    }
    for suite in SUITES {
        if !files.iter().any(|file| file.suite == suite) {
            let kinds = extensions.join("/.");
            reasons.push(format!("no .{kinds} file in sparql/{suite}"));
        }
    }
    if reasons.is_empty() {
        Ok(files)
    } else {
        Err(Unqualified::Broken(reasons))
    }
}

/// Per suite, how many of `files` it holds: `sparql10 n, sparql11 n, sparql12 n`.
pub fn counts(files: &[File]) -> String {
    SUITES
        .map(|suite| {
            let n = files.iter().filter(|file| file.suite == suite).count();
            format!("{suite} {n}")
        })
        .join(", ")
}
