//! The corpus admission (`w3c.rs`) on a fixture repository: the corpus is what the pinned
//! commit's tree holds, whatever the checkout's index or working files say (Sol, PR #61 F1:
//! a pinned HEAD with three files left in the index admitted three of 505).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use oxigraph_comparison::w3c::{self, Checkout, Unqualified};

fn git(root: &Path, args: &[&str], input: &str) -> String {
    let mut child = Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.org",
            "-C",
        ])
        .arg(root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

const DIRS: [&str; 3] = ["sparql/sparql10", "sparql/sparql11", "sparql/sparql12"];

#[test]
fn the_pinned_tree_is_the_corpus_whatever_the_index_or_the_files_say() {
    let root: PathBuf = std::env::temp_dir().join(format!("w3c-admission-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let pinned = [
        ("sparql/sparql10/a.srx", "<a/>"),
        ("sparql/sparql11/b.srx", "<b/>"),
        ("sparql/sparql12/c.srx", "<c/>"),
    ];
    for (name, text) in pinned.iter().chain([&("sparql/sparql11/d.rq", "ASK {}")]) {
        std::fs::create_dir_all(root.join(name).parent().unwrap()).unwrap();
        std::fs::write(root.join(name), text).unwrap();
    }
    git(&root, &["init", "-q"], "");
    git(&root, &["add", "-A"], "");
    git(&root, &["commit", "-q", "-m", "the pin"], "");
    let checkout = Checkout {
        repository: "fixture".to_owned(),
        commit: git(&root, &["rev-parse", "HEAD"], ""),
        root: root.clone(),
        paths: vec!["sparql".to_owned()],
    };
    let corpus = || match w3c::pinned(&checkout, &DIRS, &["srx"]) {
        Ok(blobs) => blobs
            .into_iter()
            .map(|blob| (blob.name, String::from_utf8(blob.bytes).unwrap()))
            .collect::<Vec<_>>(),
        Err(why) => panic!("{why}"),
    };
    let expected: Vec<(String, String)> = pinned
        .iter()
        .map(|(name, text)| ((*name).to_owned(), (*text).to_owned()))
        .collect();
    let a = git(&root, &["rev-parse", "HEAD:sparql/sparql10/a.srx"], "");
    // Each mutation stays for the next: the corpus is the pinned one after every one.
    let cases: [(&str, &dyn Fn()); 4] = [
        ("two files removed from the index", &|| {
            git(
                &root,
                &[
                    "rm",
                    "-q",
                    "--cached",
                    "sparql/sparql11/b.srx",
                    "sparql/sparql12/c.srx",
                ],
                "",
            );
        }),
        ("a path listed three times (conflict stages)", &|| {
            let stages: String = (1..=3)
                .map(|stage| format!("100644 {a} {stage}\tsparql/sparql10/a.srx\n"))
                .collect();
            let unstage = format!("0 {a} 0\tsparql/sparql10/a.srx\n");
            git(
                &root,
                &["update-index", "--index-info"],
                &(unstage + &stages),
            );
        }),
        ("a file's bytes changed", &|| {
            std::fs::write(root.join("sparql/sparql10/a.srx"), "").unwrap();
        }),
        ("a file gone from the working tree", &|| {
            std::fs::remove_file(root.join("sparql/sparql11/b.srx")).unwrap();
        }),
    ];
    assert_eq!(corpus(), expected, "as fetched");
    for (case, mutate) in cases {
        mutate();
        assert_eq!(
            corpus(),
            expected,
            "SPEC the pinned tree is the corpus: {case}"
        );
    }
    // A pin the store doesn't hold is refused; no checkout at all is absent.
    let elsewhere = Checkout {
        repository: "fixture".to_owned(),
        commit: "0".repeat(40),
        root: root.clone(),
        paths: vec!["sparql".to_owned()],
    };
    assert!(matches!(
        w3c::pinned(&elsewhere, &DIRS, &["srx"]),
        Err(Unqualified::Broken(_))
    ));
    let absent = Checkout {
        root: root.join("none"),
        ..elsewhere
    };
    assert!(matches!(
        w3c::pinned(&absent, &DIRS, &["srx"]),
        Err(Unqualified::Absent(_))
    ));
    std::fs::remove_dir_all(&root).unwrap();
}

/// In a partial clone a pinned object not fetched yet breaks the corpus, and admission starts
/// no fetch: one runs over the network with no deadline (Sol, PR #61 fix check 1).
#[test]
fn an_object_not_fetched_breaks_the_corpus_and_starts_no_fetch() {
    let base: PathBuf = std::env::temp_dir().join(format!("w3c-partial-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let origin = base.join("origin");
    for name in ["sparql10/a.srx", "sparql11/b.srx", "sparql12/c.srx"] {
        let path = origin.join("sparql").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, name).unwrap();
    }
    git(&origin, &["init", "-q"], "");
    git(&origin, &["add", "-A"], "");
    git(&origin, &["commit", "-q", "-m", "the pin"], "");
    git(&origin, &["config", "uploadpack.allowFilter", "true"], "");
    let url = origin.to_string_lossy().replace('\\', "/");
    let url = format!(
        "file://{}{url}",
        if url.starts_with('/') { "" } else { "/" }
    );
    git(
        &base,
        &[
            "clone",
            "-q",
            "--no-checkout",
            "--filter=blob:none",
            url.as_str(),
            "clone",
        ],
        "",
    );
    let clone = base.join("clone");
    // A fetch runs the remote's upload-pack: this one leaves a mark and fails.
    let mark = base.join("fetched");
    let upload = base.join("upload-pack.sh");
    let marked = mark.to_string_lossy().replace('\\', "/");
    std::fs::write(&upload, format!("#!/bin/sh\ntouch '{marked}'\nexit 1\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&upload, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let upload = upload.to_string_lossy().replace('\\', "/");
    git(
        &clone,
        &["config", "remote.origin.uploadpack", upload.as_str()],
        "",
    );
    let checkout = Checkout {
        repository: "fixture".to_owned(),
        commit: git(&origin, &["rev-parse", "HEAD"], ""),
        root: clone,
        paths: vec!["sparql".to_owned()],
    };
    let corpus = w3c::pinned(&checkout, &DIRS, &["srx"]);
    assert!(!mark.exists(), "SPEC admission starts no fetch");
    assert!(
        matches!(corpus, Err(Unqualified::Broken(_))),
        "SPEC a pinned object not fetched breaks the corpus"
    );
    std::fs::remove_dir_all(&base).unwrap();
}

/// A file the fetch script pins by its SHA-256 is admitted by it: with its digest, its bytes;
/// with other bytes, broken; not there, absent.
#[test]
fn a_file_pinned_by_its_digest_is_admitted_by_it() {
    let root: PathBuf = std::env::temp_dir().join(format!("w3c-digest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("all.rdf");
    let pin = |sha256: &str| {
        w3c::Pin::Digest(w3c::Digested {
            name: "OWL_TESTS".to_owned(),
            path: path.clone(),
            sha256: sha256.to_owned(),
        })
    };
    // FIPS 180-4's examples: one block, and two.
    let examples = [
        (
            "abc",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
    ];
    for (text, sha256) in examples {
        std::fs::write(&path, text).unwrap();
        match pin(sha256).files(&["rdf"]) {
            Ok(blobs) => {
                let read: Vec<(&str, &[u8])> = blobs
                    .iter()
                    .map(|blob| (blob.name.as_str(), blob.bytes.as_slice()))
                    .collect();
                assert_eq!(read, [("OWL_TESTS", text.as_bytes())], "SPEC its bytes");
            }
            Err(why) => panic!("SPEC a file with its pinned digest is admitted: {why}"),
        }
        assert!(
            matches!(pin(sha256).files(&["srx"]), Ok(blobs) if blobs.is_empty()),
            "SPEC not a kind asked for"
        );
    }
    let other = "0".repeat(64);
    assert!(
        matches!(
            pin(other.as_str()).files(&["rdf"]),
            Err(Unqualified::Broken(_))
        ),
        "SPEC other bytes than the pinned ones break the corpus"
    );
    std::fs::remove_file(&path).unwrap();
    assert!(
        matches!(
            pin(other.as_str()).files(&["rdf"]),
            Err(Unqualified::Absent(_))
        ),
        "SPEC no file is an absent corpus"
    );
    std::fs::remove_dir_all(&root).unwrap();
}
