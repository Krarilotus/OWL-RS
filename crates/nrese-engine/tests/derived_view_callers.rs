//! Guard of dictionary 3.0b: `Snapshot::with_inferred_derived_within` keeps its source's
//! publication, so its views may enter shared caches. It's sound only for statements the
//! source's own DL bounds derive from it alone, so only the store's DL bounds may call it
//! (the master, 8 Nov, IB2250-153c). Visibility can't say so across crates; this test does.
//!
//! - Input: every `.rs` file under the workspace's `crates/*/src`, test files excepted
//!   (`*_tests.rs`, `tests.rs`, anything under a `tests` directory).
//! - Expected: the method's name followed by `(` appears only in the store's DL bounds and in
//!   the engine's snapshot module, which defines it.

use std::path::{Path, PathBuf};

const METHOD: &str = "with_inferred_derived_within(";

/// The files allowed to name it, sorted: its definition and its one caller.
const ALLOWED: [&str; 2] = [
    "nrese-engine/src/engine/snapshot.rs",
    "nrese-store/src/dl/bounds.rs",
];

fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "tests" {
                sources(&path, found);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" && !name.ends_with("_tests.rs") {
            found.push(path);
        }
    }
}

#[test]
fn only_the_dl_bounds_derive_published_views() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(crates).unwrap() {
        let src = entry.unwrap().path().join("src");
        if src.is_dir() {
            sources(&src, &mut files);
        }
    }
    assert!(
        files.len() > 100,
        "the workspace's sources were found: {}",
        files.len()
    );
    let mut callers: Vec<String> = files
        .iter()
        .filter(|path| std::fs::read_to_string(path).unwrap().contains(METHOD))
        .map(|path| {
            path.strip_prefix(crates)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    callers.sort();
    assert_eq!(
        callers,
        ALLOWED.map(str::to_owned).to_vec(),
        "callers of {METHOD}"
    );
}
