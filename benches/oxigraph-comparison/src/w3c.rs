//! The W3C test suites as pinned: `scripts/fetch-w3c-tests.sh` checks each W3C repository out
//! at one commit under `.cache` (the script is the one owner of each pin, place and fetched
//! path). The tests and benchmarks that count on the suites, here and in nrese-fuzz, take
//! their files from here: every file of the asked kinds the pinned commit's tree holds under
//! the asked paths, read from that tree's objects. The checkout's index and working files
//! decide nothing: they can drop, repeat or change a file, the pinned tree can't (Sol, PR #61
//! F1). A corpus that is missing, or whose pinned objects can't be read in full, qualifies
//! nothing. The script's other pins, the GeoSPARQL benchmark's checkout and the OWL 2 test
//! cases' file (pinned by its SHA-256), are admitted alike ([`pins`]).

use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The SPARQL suites, as w3c/rdf-tests names their directories under `sparql/`.
pub const SUITES: [&str; 3] = ["sparql10", "sparql11", "sparql12"];

/// One file of a SPARQL suite.
pub struct File {
    pub suite: &'static str,
    /// Its path under `sparql/`, as `sparql11/aggregates/agg01.srx`.
    pub name: String,
    /// Where the checkout holds it (for a base IRI); its bytes are the pinned tree's.
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

/// One file of a pinned checkout: its path in the repository, its bytes as the pinned tree
/// holds them.
pub struct Blob {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// A repository as the fetch script pins it.
pub struct Checkout {
    /// As the script names it: `w3c/rdf-tests`.
    pub repository: String,
    pub commit: String,
    pub root: PathBuf,
    /// The paths the script checks out (the only ones whose objects it fetches).
    pub paths: Vec<String>,
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
                "no pinned corpus at {} (scripts/fetch-w3c-tests.sh)",
                root.display()
            ),
            Self::Broken(reasons) => write!(
                f,
                "a pinned corpus isn't there in full: {}",
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
/// is there but not the pinned one in full (its pin's objects missing or unreadable) fails
/// the run always. An absent one fails it where the W3C data is required; else the run says
/// it doesn't qualify and gets none.
pub fn admitted<T>(corpus: Result<Vec<T>, Unqualified>) -> Option<Vec<T>> {
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

/// The fetch script, its path and its text with each line continued over `\` joined (a
/// Windows checkout's line ends included).
fn script() -> Result<(PathBuf, String), String> {
    let script = workspace().join("scripts/fetch-w3c-tests.sh");
    let text =
        std::fs::read_to_string(&script).map_err(|e| format!("{}: {e}", script.display()))?;
    Ok((script, text.replace("\r\n", "\n").replace("\\\n", " ")))
}

/// Every repository the fetch script pins, with its commit, place and fetched paths.
pub fn checkouts() -> Result<Vec<Checkout>, String> {
    let (script, text) = script()?;
    let cache = workspace().join(".cache");
    let mut out = Vec::new();
    // `fetch <repository> <commit> <place> <path>...`.
    for line in text.lines() {
        let mut words = line.split_whitespace();
        if words.next() != Some("fetch") {
            continue;
        }
        let (Some(repository), Some(commit), Some(place)) =
            (words.next(), words.next(), words.next())
        else {
            return Err(format!(
                "{}: a fetch line without its place: {line}",
                script.display()
            ));
        };
        if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!(
                "{}: {repository} pinned to no commit",
                script.display()
            ));
        }
        // `"$CACHE/n3"`, or `"${NRESE_W3C_TESTS:-$CACHE/rdf-tests}"`: the setting when it is set.
        let place = place.trim_matches('"');
        let place = match place
            .strip_prefix("${")
            .and_then(|rest| rest.strip_suffix('}'))
        {
            Some(inner) => {
                let (name, default) = inner
                    .split_once(":-")
                    .ok_or_else(|| format!("{}: {repository}: place {place}", script.display()))?;
                match std::env::var(name) {
                    Ok(value) if !value.is_empty() => value,
                    _ => default.to_owned(),
                }
            }
            None => place.to_owned(),
        };
        let root = match place.strip_prefix("$CACHE/") {
            Some(rest) => cache.join(rest),
            None => PathBuf::from(place),
        };
        out.push(Checkout {
            repository: repository.to_owned(),
            commit: commit.to_owned(),
            root,
            paths: words.map(str::to_owned).collect(),
        });
    }
    Ok(out)
}

/// The checkout of w3c/rdf-tests, which holds the SPARQL suites.
pub fn rdf_tests() -> Result<Checkout, String> {
    checkouts()?
        .into_iter()
        .find(|checkout| checkout.repository == "w3c/rdf-tests")
        .ok_or_else(|| "scripts/fetch-w3c-tests.sh: w3c/rdf-tests isn't fetched".to_owned())
}

/// A file the fetch script pins by its SHA-256, as `NAME="$CACHE/<path>"` and
/// `NAME_SHA256=<digest>` (the OWL 2 test cases).
pub struct Digested {
    pub name: String,
    pub path: PathBuf,
    pub sha256: String,
}

/// Every file the fetch script pins by its digest.
pub fn digested() -> Result<Vec<Digested>, String> {
    let (script, text) = script()?;
    let cache = workspace().join(".cache");
    let assigned = |line: &str| {
        line.trim()
            .split_once('=')
            .map(|(name, value)| (name.to_owned(), value.trim_matches('"').to_owned()))
    };
    let mut out = Vec::new();
    for (name, digest) in text.lines().filter_map(assigned) {
        let Some(name) = name.strip_suffix("_SHA256") else {
            continue;
        };
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("{}: {name} pinned to no digest", script.display()));
        }
        let place = text
            .lines()
            .filter_map(assigned)
            .find_map(|(n, value)| (n == name).then_some(value))
            .and_then(|value| value.strip_prefix("$CACHE/").map(|rest| cache.join(rest)))
            .ok_or_else(|| format!("{}: {name}'s digest, but no place", script.display()))?;
        out.push(Digested {
            name: name.to_owned(),
            path: place,
            sha256: digest.to_ascii_lowercase(),
        });
    }
    Ok(out)
}

/// A corpus the fetch script pins: a repository at a commit, or a file by its digest.
pub enum Pin {
    Git(Checkout),
    Digest(Digested),
}

impl Pin {
    /// As the script names it: the repository, or the file's variable.
    pub fn name(&self) -> &str {
        match self {
            Self::Git(checkout) => &checkout.repository,
            Self::Digest(file) => &file.name,
        }
    }

    /// Every file with one of `extensions` the pin holds as pinned (a checkout's under the
    /// paths the script fetches); or why there is no such corpus.
    pub fn files(&self, extensions: &[&str]) -> Result<Vec<Blob>, Unqualified> {
        match self {
            Self::Git(checkout) => {
                let dirs: Vec<&str> = checkout.paths.iter().map(String::as_str).collect();
                pinned(checkout, &dirs, extensions)
            }
            Self::Digest(file) => digest_pinned(file, extensions),
        }
    }
}

/// Every corpus the fetch script pins.
pub fn pins() -> Result<Vec<Pin>, String> {
    let mut pins: Vec<Pin> = checkouts()?.into_iter().map(Pin::Git).collect();
    pins.extend(digested()?.into_iter().map(Pin::Digest));
    Ok(pins)
}

/// The digest-pinned `file`, read and checked against its digest, if its kind is one of
/// `extensions`; or why there is no such corpus.
fn digest_pinned(file: &Digested, extensions: &[&str]) -> Result<Vec<Blob>, Unqualified> {
    let at = file.path.display();
    let bytes = match std::fs::read(&file.path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Unqualified::Absent(file.path.clone()));
        }
        Err(e) => return Err(Unqualified::Broken(vec![format!("{at}: {e}")])),
    };
    let digest = sha256(&bytes);
    if digest != file.sha256 {
        let pin = &file.sha256;
        return Err(Unqualified::Broken(vec![format!(
            "{at}: SHA-256 {digest}, pinned {pin}"
        )]));
    }
    let extension = file.path.extension().and_then(|e| e.to_str());
    if !extension.is_some_and(|extension| extensions.contains(&extension)) {
        return Ok(Vec::new());
    }
    Ok(vec![Blob {
        name: file.name.clone(),
        bytes,
    }])
}

/// SHA-256 (FIPS 180-4) of `data`, in lower-case hex: the digest the fetch script pins a file by.
fn sha256(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&(data.len() as u64).wrapping_mul(8).to_be_bytes());
    for block in message.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for (k, wi) in K.iter().zip(w) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(*k)
                .wrapping_add(wi);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            (hh, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    h.map(|x| format!("{x:08x}")).concat()
}

/// Git in `root`, its repository's alone and reading objects as stored: none of the caller's
/// repository settings (a commit hook's GIT_DIR or GIT_INDEX_FILE) reaches it, and no
/// replace ref stands another object in.
fn git(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    for local in [
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_DIR",
        "GIT_GRAFT_FILE",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_PREFIX",
        "GIT_REPLACE_REF_BASE",
        "GIT_SHALLOW_FILE",
        "GIT_WORK_TREE",
    ] {
        command.env_remove(local);
    }
    command
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .arg("-C")
        .arg(root)
        .args(args);
    command
}

/// Every file with one of `extensions` that `checkout`'s pinned commit holds under `dirs`
/// (paths the script fetches), read from its objects, in the tree's order; or why there is
/// no such corpus.
pub fn pinned(
    checkout: &Checkout,
    dirs: &[&str],
    extensions: &[&str],
) -> Result<Vec<Blob>, Unqualified> {
    let root = &checkout.root;
    if !root.join(".git").exists() {
        return Err(Unqualified::Absent(root.clone()));
    }
    let broken = |reason: String| Unqualified::Broken(vec![reason]);
    let at = format!("{} at {}", checkout.repository, checkout.commit);
    // Only objects already here are read: in a partial clone, ls-tree and cat-file fetch a
    // missing one over the network, with no deadline (Sol, PR #61 fix check 1). rev-list with
    // --missing=print fetches nothing (Git 2.16 on) and marks each object under the paths
    // that isn't here; one such is a pin not fetched in full, and fetching is the script's.
    let tree = format!("{}^{{tree}}", checkout.commit);
    let mut args = vec![
        "rev-list",
        "--objects",
        "--missing=print",
        tree.as_str(),
        "--",
    ];
    args.extend(dirs);
    let here = git(root, &args)
        .output()
        .map_err(|e| broken(format!("git: {e}")))?;
    if !here.status.success() {
        let stderr = String::from_utf8_lossy(&here.stderr);
        return Err(broken(format!("{at}: {}", stderr.trim())));
    }
    let here = String::from_utf8_lossy(&here.stdout);
    let missing: Vec<&str> = here.lines().filter_map(|l| l.strip_prefix('?')).collect();
    if let Some(first) = missing.first() {
        let n = missing.len();
        return Err(broken(format!(
            "{at}: {n} objects under {} not fetched, as {first} (scripts/fetch-w3c-tests.sh)",
            dirs.join(", ")
        )));
    }
    let mut args = vec![
        "ls-tree",
        "-r",
        "-z",
        "--full-tree",
        checkout.commit.as_str(),
        "--",
    ];
    args.extend(dirs);
    let listed = git(root, &args)
        .output()
        .map_err(|e| broken(format!("git: {e}")))?;
    if !listed.status.success() {
        let stderr = String::from_utf8_lossy(&listed.stderr);
        return Err(broken(format!("{at}: {}", stderr.trim())));
    }
    let (mut wanted, mut reasons) = (Vec::new(), Vec::new());
    for record in listed.stdout.split(|&b| b == 0).filter(|r| !r.is_empty()) {
        let record = String::from_utf8_lossy(record);
        let Some((meta, name)) = record.split_once('\t') else {
            reasons.push(format!("{at}: {record}"));
            continue;
        };
        let file = name.rsplit_once('/').map_or(name, |(_, file)| file);
        let extension = file.rsplit_once('.').map(|(_, extension)| extension);
        if !extension.is_some_and(|extension| extensions.contains(&extension)) {
            continue;
        }
        match meta.split(' ').collect::<Vec<_>>()[..] {
            ["100644" | "100755", "blob", id] => wanted.push((name.to_owned(), id.to_owned())),
            _ => reasons.push(format!("{name}: {meta}, not a file")),
        }
    }
    if !reasons.is_empty() {
        return Err(Unqualified::Broken(reasons));
    }
    let mut blobs = read(root, &wanted).map_err(|why| broken(format!("{at}: {why}")))?;
    // The tree lists each path once; the objects come in the order asked.
    for (blob, (name, _)) in blobs.iter_mut().zip(&wanted) {
        blob.name = name.clone();
    }
    Ok(blobs)
}

/// The objects `wanted` names, read in one `git cat-file --batch`, each checked to be the
/// blob asked for.
fn read(root: &Path, wanted: &[(String, String)]) -> Result<Vec<Blob>, String> {
    // Its diagnostics go to the caller's: a pipe nobody reads would stop it once full (Sol,
    // PR #61 r1 sol-loop S2: GIT_TRACE_PACK_ACCESS writes 140,000 characters on the suites).
    let mut child = git(root, &["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("git cat-file: {e}"))?;
    let mut stdin = child.stdin.take().expect("piped");
    let ids: String = wanted.iter().map(|(_, id)| format!("{id}\n")).collect();
    // Written beside the reading: the objects' bytes would fill the pipe first.
    let writer = std::thread::spawn(move || stdin.write_all(ids.as_bytes()));
    let mut stdout = BufReader::new(child.stdout.take().expect("piped"));
    let mut blobs = Vec::with_capacity(wanted.len());
    let mut failure = None;
    for (name, id) in wanted {
        let mut header = String::new();
        if let Err(e) = stdout.read_line(&mut header) {
            failure = Some(format!("{name}: {e}"));
            break;
        }
        let size = match header.trim_end().split(' ').collect::<Vec<_>>()[..] {
            [got, "blob", size] if got == id => size.parse::<usize>().ok(),
            _ => None,
        };
        let Some(size) = size else {
            failure = Some(format!("{name}: {id}: {}", header.trim_end()));
            break;
        };
        let mut bytes = vec![0; size + 1];
        if let Err(e) = stdout.read_exact(&mut bytes) {
            failure = Some(format!("{name}: {e}"));
            break;
        }
        bytes.pop(); // the newline after each object
        blobs.push(Blob {
            name: String::new(),
            bytes,
        });
    }
    drop(stdout);
    let written = writer.join().expect("the id writer");
    let status = child.wait().map_err(|e| format!("git cat-file: {e}"))?;
    if let Some(failure) = failure {
        return Err(failure);
    }
    written.map_err(|e| format!("git cat-file: {e}"))?;
    if !status.success() {
        return Err(format!("git cat-file: {status}"));
    }
    Ok(blobs)
}

/// Every file with one of `extensions` the pinned SPARQL suites hold, read from the pinned
/// tree, in its order; or why there is no such corpus. Each suite has at least one.
pub fn sparql(extensions: &[&str]) -> Result<Vec<File>, Unqualified> {
    let checkout = rdf_tests().map_err(|why| Unqualified::Broken(vec![why]))?;
    let dirs = SUITES.map(|suite| format!("sparql/{suite}"));
    let dirs: Vec<&str> = dirs.iter().map(String::as_str).collect();
    let blobs = pinned(&checkout, &dirs, extensions)?;
    // Each file's place from the checkout's, never from the file, which may be gone (Sol, PR
    // #61 r1 sol-loop S1): the pinned bytes are the file.
    let root = checkout.root.canonicalize().map_err(|e| {
        let at = checkout.root.display();
        Unqualified::Broken(vec![format!("{at}: {e}")])
    })?;
    let mut files = Vec::new();
    for blob in blobs {
        let name = blob
            .name
            .strip_prefix("sparql/")
            .unwrap_or(&blob.name)
            .to_owned();
        let suite = SUITES
            .into_iter()
            .find(|suite| name.starts_with(&format!("{suite}/")))
            .expect("a path under one of the suites asked for");
        files.push(File {
            suite,
            path: root.join(&blob.name),
            name,
            bytes: blob.bytes,
        });
    }
    let reasons: Vec<String> = SUITES
        .into_iter()
        .filter(|suite| !files.iter().any(|file| file.suite == *suite))
        .map(|suite| format!("no .{} file in sparql/{suite}", extensions.join("/.")))
        .collect();
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
