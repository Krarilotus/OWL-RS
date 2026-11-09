//! Names reached through links (DUR-4; fix check DEFER-F1-SYM): every directory entry the
//! resolution of a store's path reads is made durable, the link's own entry and its target's
//! chain alike, not only the path's lexical holders.
//!
//! Linux runs every shape with real symbolic links. Windows runs the shapes on junctions (no
//! privilege needed) and again on directory symlinks where the host may make them; where it may
//! not, the symlink rows print "not run" and run nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::fault;
use super::place;
use crate::{DurabilityConfig, Engine, EngineConfig, SyncPolicy};

fn config() -> EngineConfig {
    EngineConfig {
        background_maintenance: false,
        durability: DurabilityConfig {
            sync: SyncPolicy::EveryCommit,
            ..DurabilityConfig::default()
        },
        ..EngineConfig::default()
    }
}

/// How a row links one directory to another.
#[derive(Clone, Copy, Debug)]
enum Link {
    Symlink,
    #[cfg(windows)]
    Junction,
}

/// The kinds this platform runs: Linux symbolic links; Windows junctions and symlinks.
fn kinds() -> Vec<Link> {
    #[cfg(windows)]
    return vec![Link::Junction, Link::Symlink];
    #[cfg(not(windows))]
    vec![Link::Symlink]
}

/// Makes `link` a link of `kind` to the directory `target` (absolute); `false` if this host may
/// not make it (a Windows symlink without the privilege: the row isn't run).
fn link(kind: Link, target: &Path, link: &Path) -> bool {
    match kind {
        Link::Symlink => {
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, link).unwrap();
            // A Windows link keeps its target as spelled: backslashes only.
            #[cfg(windows)]
            match std::os::windows::fs::symlink_dir(
                target.to_string_lossy().replace('/', "\\"),
                link.to_string_lossy().replace('/', "\\"),
            ) {
                Ok(()) => {}
                // ERROR_PRIVILEGE_NOT_HELD
                Err(error) if error.raw_os_error() == Some(1314) => return false,
                Err(error) => panic!(
                    "symlink {} -> {}: {error}",
                    link.display(),
                    target.display()
                ),
            }
            true
        }
        #[cfg(windows)]
        Link::Junction => {
            // `cmd` reads a `/` in an argument as an option's start: backslashes only.
            let native = |path: &Path| path.to_string_lossy().replace('/', "\\");
            let made = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(native(link))
                .arg(native(target))
                .output()
                .unwrap();
            assert!(
                made.status.success(),
                "mklink /J {} {}: {}",
                link.display(),
                target.display(),
                String::from_utf8_lossy(&made.stderr)
            );
            true
        }
    }
}

/// A directory's canonical path, or the path as given if it can't be resolved.
fn canonical(dir: &Path) -> PathBuf {
    dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())
}

/// What `step` returns, and the directories it synced (canonical), each with how often.
fn syncs_of<T>(step: impl FnOnce() -> T) -> (T, BTreeMap<PathBuf, usize>) {
    use std::rc::Rc;
    let synced = Rc::new(std::cell::RefCell::new(BTreeMap::<PathBuf, usize>::new()));
    let seen = Rc::clone(&synced);
    fault::ON_DIR_SYNC.set(Some(Box::new(move |dir: &Path| {
        *seen.borrow_mut().entry(canonical(dir)).or_default() += 1;
    })));
    let out = step();
    fault::ON_DIR_SYNC.set(None);
    (out, synced.take())
}

/// What's wrong with `synced` against `needed` (canonical): a needed directory not synced, or,
/// where only found levels are named (`once`), one synced more than once (a level made has its
/// holder synced before and after).
fn judge(
    row: &str,
    synced: &BTreeMap<PathBuf, usize>,
    needed: &[&Path],
    once: bool,
) -> Vec<String> {
    let mut wrong = Vec::new();
    for dir in needed {
        if !synced.contains_key(&canonical(dir)) {
            wrong.push(format!(
                "{row}: {} not synced (synced: {synced:?})",
                dir.display()
            ));
        }
    }
    for (dir, times) in synced.iter().filter(|_| once) {
        if *times > 1 {
            wrong.push(format!("{row}: {} synced {times} times", dir.display()));
        }
    }
    wrong
}

/// Naming an existing directory reached through a link syncs every entry its resolution reads
/// (`place::name_existing`, which store metadata uses): the link's holder, and each directory
/// of the target's chain, each once; a holder of the target's chain whose sync fails refuses
/// it. Rows, judged apart, for each link kind: the store's own level a link
/// (`stable/store` → `data/new/store`); an intermediate link (`a` → `real/a`, then
/// `a/b/store`); a target-holder fault (`data/new` failing). Before the fix only the path's
/// lexical holders (`stable`, `a`, `a/b`) were synced: the target's (`data`, `data/new`,
/// `real`, `real/a`) never were, and the fault was never reached.
#[test]
fn naming_a_directory_reaches_through_links() {
    let mut wrong = Vec::new();
    for kind in kinds() {
        let base = tempfile::tempdir().unwrap();
        let base = base.path();
        std::fs::create_dir_all(base.join("data/new/store")).unwrap();
        std::fs::create_dir(base.join("stable")).unwrap();
        if !link(
            kind,
            &base.join("data/new/store"),
            &base.join("stable/store"),
        ) {
            eprintln!("{kind:?} rows: not run (no privilege to make one on this host)");
            continue;
        }
        let row = format!("{kind:?}, the store's level a link");
        let (named, synced) = syncs_of(|| place::name_existing(&base.join("stable/store")));
        if let Err(error) = named {
            wrong.push(format!("{row}: {error}"));
        }
        wrong.extend(judge(
            &row,
            &synced,
            &[
                base,
                &base.join("stable"),
                &base.join("data"),
                &base.join("data/new"),
            ],
            true,
        ));

        std::fs::create_dir_all(base.join("real/a/b/store")).unwrap();
        link(kind, &base.join("real/a"), &base.join("a"));
        let row = format!("{kind:?}, an intermediate link");
        let (named, synced) = syncs_of(|| place::name_existing(&base.join("a/b/store")));
        if let Err(error) = named {
            wrong.push(format!("{row}: {error}"));
        }
        wrong.extend(judge(
            &row,
            &synced,
            &[
                base,
                &base.join("real"),
                &base.join("real/a"),
                &base.join("real/a/b"),
            ],
            true,
        ));

        let row = format!("{kind:?}, a target holder failing");
        fault::FAIL_DIR_SYNC_OF.set(Some((base.join("data/new"), std::io::ErrorKind::Other)));
        let named = place::name_existing(&base.join("stable/store"));
        fault::FAIL_DIR_SYNC_OF.set(None);
        if named.is_ok() {
            wrong.push(format!("{row}: named though a target holder's sync failed"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Making a directory through a link makes its missing levels in the link's target, each made
/// level's physical holder synced before and after, and syncs the target's chain (the link's
/// holder and the target's ancestors) as found levels. Row, for each link kind: `stable2` →
/// `t/data2`, then `create_dir_all(stable2/new/deeper)`: `t/data2/new/deeper` made, and `t`
/// (holding `data2`) synced. Before the fix the target's ancestor `t` never was. Guard: a link
/// whose target isn't there (dangling) is refused and nothing is made where it points.
#[test]
fn making_a_directory_reaches_through_links() {
    let mut wrong = Vec::new();
    for kind in kinds() {
        let base = tempfile::tempdir().unwrap();
        let base = base.path();
        std::fs::create_dir_all(base.join("t/data2")).unwrap();
        if !link(kind, &base.join("t/data2"), &base.join("stable2")) {
            eprintln!("{kind:?} rows: not run (no privilege to make one on this host)");
            continue;
        }
        let row = format!("{kind:?}, missing levels under a link");
        let (made, synced) = syncs_of(|| place::create_dir_all(&base.join("stable2/new/deeper")));
        if let Err(error) = made {
            wrong.push(format!("{row}: {error}"));
        }
        if !base.join("t/data2/new/deeper").is_dir() {
            wrong.push(format!("{row}: not made in the target"));
        }
        wrong.extend(judge(
            &row,
            &synced,
            &[
                base,
                &base.join("t"),
                &base.join("t/data2"),
                &base.join("t/data2/new"),
            ],
            false,
        ));
    }
    // Guard (right before the fix too): a dangling link refuses both, and nothing is made.
    #[cfg(unix)]
    {
        let base = tempfile::tempdir().unwrap();
        let base = base.path();
        std::os::unix::fs::symlink(base.join("nowhere/store"), base.join("d")).unwrap();
        if place::name_existing(&base.join("d")).is_ok() {
            wrong.push("a dangling link named".to_owned());
        }
        if place::create_dir_all(&base.join("d/x")).is_ok() {
            wrong.push("made through a dangling link".to_owned());
        }
        if base.join("nowhere").exists() {
            wrong.push("a dangling link's target made".to_owned());
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// `..` after a link on Windows (sol-loop round 1, S1): Windows takes a path's `..` lexically
/// before the file system sees it (a verbatim `\\?\` path aside), so `b\alias\..\tenant\store`
/// is `b\tenant\store` whatever `alias` points to. Row, for each link kind: `b\alias` →
/// `t\deep`, both `b\tenant\store` and `t\tenant\store` there; naming the path syncs `b` and
/// `b\tenant` (the holders of the store's actual names), and a failing sync of `b\tenant`
/// refuses it. Before the fix the resolution followed `alias` first and named `t\tenant`'s
/// chain instead: `b\tenant` was never synced.
#[cfg(windows)]
#[test]
fn dot_dot_after_a_link_is_taken_as_windows_takes_it() {
    let mut wrong = Vec::new();
    for kind in kinds() {
        let base = tempfile::tempdir().unwrap();
        let base = base.path();
        std::fs::create_dir_all(base.join("b\\tenant\\store")).unwrap();
        std::fs::create_dir_all(base.join("t\\deep")).unwrap();
        std::fs::create_dir_all(base.join("t\\tenant\\store")).unwrap();
        if !link(kind, &base.join("t\\deep"), &base.join("b\\alias")) {
            eprintln!("{kind:?} rows: not run (no privilege to make one on this host)");
            continue;
        }
        let path = base.join("b\\alias\\..\\tenant\\store");
        let row = format!("{kind:?}, `..` after a link");
        let (named, synced) = syncs_of(|| place::name_existing(&path));
        if let Err(error) = named {
            wrong.push(format!("{row}: {error}"));
        }
        wrong.extend(judge(
            &row,
            &synced,
            &[&base.join("b"), &base.join("b\\tenant")],
            true,
        ));
        fault::FAIL_DIR_SYNC_OF.set(Some((base.join("b\\tenant"), std::io::ErrorKind::Other)));
        let named = place::name_existing(&path);
        fault::FAIL_DIR_SYNC_OF.set(None);
        if named.is_ok() {
            wrong.push(format!(
                "{row}: named though the actual holder's sync failed"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Windows' lexical normalisation, as `place::as_walked` applies it (sol-loop round 2, S3):
/// only a root absorbs a `..`. A drive-relative path (`C:x`, no root) keeps a `..` that goes
/// above its start, as a relative path does. A verbatim path is left as it is. Rows of (path,
/// what Windows walks), from Windows' path normalisation rules. Before the fix a drive-relative
/// prefix absorbed `..` like a root: `C:alias\..\..\tenant\store` became `C:tenant\store`.
#[cfg(windows)]
#[test]
fn windows_paths_are_walked_as_windows_normalises_them() {
    let rows = [
        (r"C:alias\..\..\tenant\store", r"C:..\tenant\store"),
        (r"C:\a\..\..\b", r"C:\b"),
        (r"\\server\share\..\x", r"\\server\share\x"),
        (r"a\..\..\b", r"..\b"),
        (r"..\a", r"..\a"),
        (r"a\.\b", r"a\b"),
        (r"\\?\C:\a\..\b", r"\\?\C:\a\..\b"),
    ];
    let wrong: Vec<String> = rows
        .iter()
        .filter_map(|(path, walked)| {
            let got = place::as_walked(Path::new(path));
            (got != Path::new(walked))
                .then(|| format!("{path}: walked {}, not {walked}", got.display()))
        })
        .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A drive-relative path whose `..` goes above its start, and a relative link target from a
/// relative root (sol-loop round 2, S2 and S3), on Windows. Both are relative to this test's
/// working directory (never changed: a directory made inside it).
/// - S3: `D:..\<cwd's name>\<made>\a\store` (D the working directory's drive) names the chain
///   through the working directory's parent, and succeeds; before the fix the prefix absorbed
///   the `..` and the path named didn't exist.
/// - S2 (a directory symlink: needs the privilege): `<made>\a\alias` → `..\data`, both
///   `<made>\data\store` and `<made>\a\data\store` there; naming `<made>\a\alias\store` syncs
///   `<made>\data` (the actual store's holder), and its failing sync refuses. Before the fix the
///   joined target was walked again from `<made>\a`, naming `<made>\a\data`'s chain.
#[cfg(windows)]
#[test]
fn relative_windows_paths_name_what_windows_reaches() {
    let mut wrong = Vec::new();
    let cwd = std::env::current_dir().unwrap();
    let made = tempfile::tempdir_in(&cwd).unwrap();
    let relative = made.path().strip_prefix(&cwd).unwrap().to_path_buf();
    std::fs::create_dir_all(made.path().join("a\\store")).unwrap();

    // S3: a drive-relative path with a leading `..`.
    let drive = match cwd.components().next() {
        Some(std::path::Component::Prefix(prefix)) => prefix.as_os_str().to_owned(),
        other => panic!("the working directory has no prefix: {other:?}"),
    };
    let mut path = PathBuf::from(&drive);
    path.push("..");
    path.push(cwd.file_name().unwrap());
    path.push(&relative);
    path.push("a\\store");
    let (named, synced) = syncs_of(|| place::name_existing(&path));
    if let Err(error) = named {
        wrong.push(format!("{}: {error}", path.display()));
    }
    wrong.extend(judge(
        "a drive-relative `..`",
        &synced,
        &[
            cwd.parent().unwrap(),
            &cwd,
            made.path(),
            &made.path().join("a"),
        ],
        true,
    ));

    // S2: a relative target from a relative root.
    std::fs::create_dir_all(made.path().join("data\\store")).unwrap();
    std::fs::create_dir_all(made.path().join("a\\data\\store")).unwrap();
    match std::os::windows::fs::symlink_dir("..\\data", made.path().join("a\\alias")) {
        Ok(()) => {
            let path = relative.join("a\\alias\\store");
            let (named, synced) = syncs_of(|| place::name_existing(&path));
            if let Err(error) = named {
                wrong.push(format!("{}: {error}", path.display()));
            }
            wrong.extend(judge(
                "a relative target from a relative root",
                &synced,
                &[&made.path().join("a"), &made.path().join("data")],
                true,
            ));
            fault::FAIL_DIR_SYNC_OF
                .set(Some((made.path().join("data"), std::io::ErrorKind::Other)));
            let named = place::name_existing(&path);
            fault::FAIL_DIR_SYNC_OF.set(None);
            if named.is_ok() {
                wrong
                    .push("a relative target: named though the actual holder's sync failed".into());
            }
        }
        Err(error) if error.raw_os_error() == Some(1314) => {
            eprintln!(
                "a relative target row: not run (no privilege to make a symlink on this host)"
            );
        }
        Err(error) => panic!("symlink: {error}"),
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Links in links, relative links and `..` after a link (Linux): the resolution follows them as
/// the kernel does. Rows, judged apart: a chain `l1` → `l2` → `t/inner` of relative links,
/// `l1/store` synced through `t` and `t/inner`; `..` after a link (`x` → `p/q`, then
/// `x/../s`): the physical parent `p` holds `s`, which is found; a loop (`la` ↔ `lb`): refused.
/// Before the fix the chain's target holders weren't synced; the `..` and loop rows are guards.
#[cfg(unix)]
#[test]
fn links_in_links_and_dot_dot_resolve_as_the_kernel_does() {
    use std::os::unix::fs::symlink;
    let mut wrong = Vec::new();
    let base = tempfile::tempdir().unwrap();
    let base = base.path();
    std::fs::create_dir_all(base.join("t/inner/store")).unwrap();
    symlink("l2", base.join("l1")).unwrap();
    symlink("t/inner", base.join("l2")).unwrap();
    let (named, synced) = syncs_of(|| place::name_existing(&base.join("l1/store")));
    if let Err(error) = named {
        wrong.push(format!("a chain: {error}"));
    }
    wrong.extend(judge(
        "a chain",
        &synced,
        &[base, &base.join("t"), &base.join("t/inner")],
        true,
    ));

    std::fs::create_dir_all(base.join("p/q")).unwrap();
    std::fs::create_dir(base.join("p/s")).unwrap();
    symlink(base.join("p/q"), base.join("x")).unwrap();
    let (named, synced) = syncs_of(|| place::name_existing(&base.join("x/../s")));
    if let Err(error) = named {
        wrong.push(format!("`..` after a link: {error}"));
    }
    wrong.extend(judge(
        "`..` after a link",
        &synced,
        &[base, &base.join("p")],
        true,
    ));

    symlink("lb", base.join("la")).unwrap();
    symlink("la", base.join("lb")).unwrap();
    if place::name_existing(&base.join("la/x")).is_ok() {
        wrong.push("a loop of links named".to_owned());
    }
    if place::create_dir_all(&base.join("la/x")).is_ok() {
        wrong.push("made through a loop of links".to_owned());
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A store opened through a link leaves its target's names to the first durable write too,
/// and that write syncs them (the deferred list): a store made at `data/new/store`, reopened as
/// `stable/store` → it, then a commit. The first commit syncs `data` and `data/new` (the
/// target's holders) and `stable` (the link's holder). Before the fix only the lexical holders
/// were pending.
#[test]
fn a_store_opened_through_a_link_syncs_its_targets_names() {
    let mut wrong = Vec::new();
    for kind in kinds() {
        let base = tempfile::tempdir().unwrap();
        let base = base.path();
        let target = base.join("data/new/store");
        drop(Engine::open(&target, config()).unwrap());
        std::fs::create_dir(base.join("stable")).unwrap();
        if !link(kind, &target, &base.join("stable/store")) {
            eprintln!("{kind:?} rows: not run (no privilege to make one on this host)");
            continue;
        }
        let engine = Engine::open(base.join("stable/store"), config()).unwrap();
        let (committed, synced) = syncs_of(|| {
            let mut tx = engine.transaction();
            tx.insert(
                nrese_rdf::Quad::new(
                    nrese_rdf::NamedNode::new_unchecked("http://example.com/s"),
                    nrese_rdf::NamedNode::new_unchecked("http://example.com/p"),
                    nrese_rdf::NamedNode::new_unchecked("http://example.com/o"),
                    nrese_rdf::GraphName::DefaultGraph,
                )
                .as_ref(),
            );
            tx.commit()
        });
        let row = format!("{kind:?}, a store opened through a link");
        if let Err(error) = committed {
            wrong.push(format!("{row}: {error}"));
        }
        let needed: BTreeSet<_> = [
            base.join("stable"),
            base.join("data"),
            base.join("data/new"),
        ]
        .into_iter()
        .collect();
        let needed: Vec<&Path> = needed.iter().map(PathBuf::as_path).collect();
        wrong.extend(judge(&row, &synced, &needed, true));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The quad `n` of these rows.
fn quad(n: u64) -> nrese_rdf::Quad {
    let node =
        |name: String| nrese_rdf::NamedNode::new_unchecked(format!("http://example.com/{name}"));
    nrese_rdf::Quad::new(
        node(format!("s{n}")),
        node("p".to_owned()),
        node(format!("o{n}")),
        nrese_rdf::GraphName::DefaultGraph,
    )
}

/// Commits `quad(n)`.
fn commit(engine: &Engine, n: u64) -> crate::EngineResult<crate::CommitSummary> {
    let mut tx = engine.transaction();
    tx.insert(quad(n).as_ref());
    tx.commit()
}

/// Whether `engine`'s latest snapshot holds `quad(n)`.
fn holds(engine: &Engine, n: u64) -> bool {
    let snapshot = engine.snapshot();
    snapshot
        .lookup_quad(quad(n).as_ref())
        .is_some_and(|encoded| snapshot.contains(&encoded))
}

/// A store whose own WAL directory (or WAL archive) is a link to a directory elsewhere has the
/// names its target's resolution reads made durable before a commit is acknowledged (review
/// r1, R1-L1): the store's internal directories are opened through such a link, so their
/// target's chain holds every record. A store made and closed at `store`, its `wal` (or, with
/// the WAL archive, `wal-archive`) then moved to `data/new-parent/` and linked back. With the
/// sync of `data/new-parent` (a target ancestor) failing with a non-permission error, the
/// reopen or its first commit is refused, not acknowledged. The fault cleared, the store
/// reopened (the log may be poisoned by the refusal: a reopen settles it), a commit is
/// acknowledged and is there after another reopen, with the first. Whether the refused commit
/// is there isn't judged (its outcome is unknown). Rows, judged apart: the WAL, the archive;
/// for each link kind. Before the fix only the store's own path and the link's leaf were synced:
/// the commit was acknowledged with `data/new-parent` never synced.
#[test]
fn a_store_whose_wal_is_a_link_names_its_targets_chain() {
    let mut wrong = Vec::new();
    for kind in kinds() {
        for (dir, archive) in [("wal", false), ("wal-archive", true)] {
            let configured = || EngineConfig {
                durability: DurabilityConfig {
                    wal_archive: archive,
                    ..config().durability
                },
                ..config()
            };
            let base = tempfile::tempdir().unwrap();
            let base = base.path();
            let store = base.join("store");
            {
                let engine = Engine::open(&store, configured()).unwrap();
                commit(&engine, 1).unwrap();
            }
            let target = base.join("data").join("new-parent").join(dir);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::rename(store.join(dir), &target).unwrap();
            if !link(kind, &target, &store.join(dir)) {
                eprintln!("{kind:?} rows: not run (no privilege to make one on this host)");
                break;
            }
            let row = format!("{kind:?}, the store's {dir} a link");
            fault::FAIL_DIR_SYNC_OF.set(Some((
                base.join("data").join("new-parent"),
                std::io::ErrorKind::Other,
            )));
            let refused = match Engine::open(&store, configured()) {
                Err(_) => true,
                Ok(engine) => commit(&engine, 2).is_err(),
            };
            fault::FAIL_DIR_SYNC_OF.set(None);
            if !refused {
                wrong.push(format!(
                    "{row}: acknowledged though a target ancestor's sync failed"
                ));
            }
            match Engine::open(&store, configured()) {
                Ok(engine) => {
                    if let Err(error) = commit(&engine, 3) {
                        wrong.push(format!("{row}: the commit once the fault cleared: {error}"));
                    }
                }
                Err(error) => {
                    wrong.push(format!("{row}: the reopen once the fault cleared: {error}"))
                }
            }
            match Engine::open(&store, configured()) {
                Ok(engine) => {
                    for n in [1, 3] {
                        if !holds(&engine, n) {
                            wrong.push(format!(
                                "{row}: acknowledged commit {n} gone after a reopen"
                            ));
                        }
                    }
                }
                Err(error) => wrong.push(format!("{row}: the last reopen: {error}")),
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Makes `link` a symbolic link to the file `target` (as spelled: absolute or relative to the
/// link's directory); `false` if this host may not make one (Windows without the privilege).
fn link_file(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).unwrap();
        true
    }
    #[cfg(windows)]
    match std::os::windows::fs::symlink_file(
        target.to_string_lossy().replace('/', "\\"),
        link.to_string_lossy().replace('/', "\\"),
    ) {
        Ok(()) => true,
        // ERROR_PRIVILEGE_NOT_HELD
        Err(error) if error.raw_os_error() == Some(1314) => false,
        Err(error) => panic!(
            "symlink {} -> {}: {error}",
            link.display(),
            target.display()
        ),
    }
}

/// Naming an existing file reached through links (`place::name_existing_file`, DUR-5) syncs
/// every directory entry its target's resolution reads, beyond the file's own directory, and
/// refuses what can't be a durable file. Rows, each in its own directory, judged apart:
/// - a chain of file links (`a/f` → `b/g` → `c/h`): `b`, `c` and their holder synced;
/// - a relative target (`d/f` → `../c/h`): `c` and its holder synced;
/// - a target holder's sync failing (`c`, in the chain): refused;
/// - a dangling link (`e/f` → `missing/h`), a loop (`l/x` ↔ `l/y`), a link to a directory
///   (`m/f` → `c`): refused;
/// - guard, a regular file (`c/h`): named, nothing refused.
///
/// Before the fix nothing beyond the file's own directory was synced and nothing was refused.
#[test]
fn naming_a_file_reaches_through_links() {
    let mut wrong = Vec::new();
    let made = |base: &Path| {
        for dir in ["a", "b", "c", "d", "e", "l", "m"] {
            std::fs::create_dir(base.join(dir)).unwrap();
        }
        std::fs::write(base.join("c/h"), b"held").unwrap();
    };
    // (row, the link to make (link, target), the file named, a holder failing, needed, refused)
    type FileRow = (
        &'static str,
        &'static [(&'static str, &'static str, bool)],
        &'static str,
        Option<&'static str>,
        &'static [&'static str],
        bool,
    );
    let rows: [FileRow; 7] = [
        (
            "a chain of file links",
            &[("b/g", "c/h", true), ("a/f", "b/g", true)],
            "a/f",
            None,
            &["", "b", "c"],
            false,
        ),
        (
            "a relative target",
            &[("d/f", "../c/h", false)],
            "d/f",
            None,
            &["", "c"],
            false,
        ),
        (
            "a target holder failing",
            &[("b/g", "c/h", true), ("a/f", "b/g", true)],
            "a/f",
            Some("c"),
            &[],
            true,
        ),
        (
            "a dangling link",
            &[("e/f", "missing/h", true)],
            "e/f",
            None,
            &[],
            true,
        ),
        (
            "a loop",
            &[("l/x", "l/y", true), ("l/y", "l/x", true)],
            "l/x",
            None,
            &[],
            true,
        ),
        (
            "a link to a directory",
            &[("m/f", "c", true)],
            "m/f",
            None,
            &[],
            true,
        ),
        ("guard, a regular file", &[], "c/h", None, &[], false),
    ];
    'rows: for (row, links, named, failing, needed, refused) in rows {
        let base = tempfile::tempdir().unwrap();
        let base = base.path();
        made(base);
        for (at, target, absolute) in links {
            let target = match absolute {
                true => base.join(target),
                false => PathBuf::from(target),
            };
            if !link_file(&target, &base.join(at)) {
                eprintln!("{row}: not run (no privilege to make a file symlink on this host)");
                continue 'rows;
            }
        }
        fault::FAIL_DIR_SYNC_OF.set(failing.map(|dir| (base.join(dir), std::io::ErrorKind::Other)));
        let (outcome, synced) = syncs_of(|| place::name_existing_file(&base.join(named)));
        fault::FAIL_DIR_SYNC_OF.set(None);
        match (outcome, refused) {
            (Ok(()), true) => wrong.push(format!("{row}: named, not refused")),
            (Err(error), false) => wrong.push(format!("{row}: {error}")),
            _ => {}
        }
        let needed: Vec<PathBuf> = needed.iter().map(|dir| base.join(dir)).collect();
        let needed: Vec<&Path> = needed.iter().map(PathBuf::as_path).collect();
        wrong.extend(judge(row, &synced, &needed, true));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A file link with a relative target in a directory reached through a link (sol-loop r1 on
/// DUR-5, S1): its target is taken from the directory the link is in, as reached, not as
/// spelled. `p` links to `real/q`, and `real/q/f` links to `../data/new/h`: naming `p/f`
/// syncs `real/data/new` and its holders, where the file is (`data/new/h`, which the spelling
/// `p/../data/new/h` reads lexically on Windows, is a decoy there). Rows for each link kind.
/// Before the fix Windows named the decoy's chain; Unix walked `..` physically and was right.
#[test]
fn a_relative_file_link_in_a_linked_directory_is_taken_from_where_it_is() {
    let mut wrong = Vec::new();
    for kind in kinds() {
        let base = tempfile::tempdir().unwrap();
        let base = base.path();
        for dir in ["real/q", "real/data/new", "data/new"] {
            std::fs::create_dir_all(base.join(dir)).unwrap();
        }
        std::fs::write(base.join("real/data/new/h"), b"held").unwrap();
        std::fs::write(base.join("data/new/h"), b"decoy").unwrap();
        if !link(kind, &base.join("real/q"), &base.join("p"))
            || !link_file(Path::new("../data/new/h"), &base.join("real/q/f"))
        {
            eprintln!("{kind:?} row: not run (no privilege to make one on this host)");
            continue;
        }
        let row = format!("{kind:?}, a relative file link in a linked directory");
        let (named, synced) = syncs_of(|| place::name_existing_file(&base.join("p/f")));
        if let Err(error) = named {
            wrong.push(format!("{row}: {error}"));
        }
        let needed = [
            base.join("real"),
            base.join("real/data"),
            base.join("real/data/new"),
        ];
        let needed: Vec<&Path> = needed.iter().map(PathBuf::as_path).collect();
        wrong.extend(judge(&row, &synced, &needed, true));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The newest file in `dir` named `<prefix><revision><suffix>`.
fn newest_file(dir: &Path, prefix: &str, suffix: &str) -> PathBuf {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            let revision: u64 = name
                .strip_prefix(prefix)?
                .strip_suffix(suffix)?
                .parse()
                .ok()?;
            Some((revision, path))
        })
        .max_by_key(|(revision, _)| *revision)
        .unwrap_or_else(|| panic!("no {prefix}*{suffix} in {}", dir.display()))
        .1
}

/// A store whose authoritative files are links to files elsewhere has the names their targets'
/// resolution reads made durable before a commit is acknowledged (DUR-5, fix check r2 on
/// DUR-4, R2-F1): the open reads the checkpoint it loads and the segments it replays through
/// such a link, and appends to the active segment through it. A store made and closed, the
/// file then moved to `data/new-parent/` and linked back. With the sync of `data/new-parent`
/// (a target ancestor) failing with a non-permission error, the reopen or its first commit is
/// refused, not acknowledged. The fault cleared, the store reopened (a refusal may poison the
/// log: a reopen settles it), a commit is acknowledged and is there after another reopen, with
/// the earlier acknowledged ones. Whether the refused commit is there isn't judged. Rows,
/// judged apart: the checkpoint loaded (commit 1 then a checkpoint); the active segment
/// (commit 1, no checkpoint); a replayed segment that isn't the active one (commits 1 and 2 in
/// segments of their own, the first linked). Windows runs them on file symlinks where the host
/// may make them, else "not run". Before the fix the commit was acknowledged with
/// `data/new-parent` never synced: a power loss could leave the link dangling, and the
/// acknowledged commits with it.
#[test]
fn a_store_whose_files_are_links_names_their_targets_chain() {
    let mut wrong = Vec::new();
    for row in ["the checkpoint", "the active segment", "a replayed segment"] {
        let configured = || EngineConfig {
            durability: DurabilityConfig {
                // A segment per commit for the replayed row: the first isn't the active one.
                wal_segment_bytes: match row {
                    "a replayed segment" => 1,
                    _ => config().durability.wal_segment_bytes,
                },
                ..config().durability
            },
            ..config()
        };
        let base = tempfile::tempdir().unwrap();
        let base = base.path();
        let store = base.join("store");
        let acknowledged: Vec<u64> = {
            let engine = Engine::open(&store, configured()).unwrap();
            commit(&engine, 1).unwrap();
            match row {
                "the checkpoint" => {
                    engine.checkpoint().unwrap();
                    vec![1]
                }
                "a replayed segment" => {
                    commit(&engine, 2).unwrap();
                    vec![1, 2]
                }
                _ => vec![1],
            }
        };
        let file = match row {
            "the checkpoint" => newest_file(&store, "checkpoint-", ".nck"),
            "the active segment" => newest_file(&store.join("wal"), "", ".wal"),
            _ => {
                let segments = super::wal::list_segments(&store.join("wal")).unwrap();
                assert!(segments.len() >= 2, "{row}: segments {segments:?}");
                segments[0].1.clone()
            }
        };
        let target = base
            .join("data")
            .join("new-parent")
            .join(file.file_name().unwrap());
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::rename(&file, &target).unwrap();
        if !link_file(&target, &file) {
            eprintln!("{row}: not run (no privilege to make a file symlink on this host)");
            continue;
        }
        fault::FAIL_DIR_SYNC_OF.set(Some((
            base.join("data").join("new-parent"),
            std::io::ErrorKind::Other,
        )));
        let refused = match Engine::open(&store, configured()) {
            Err(_) => true,
            Ok(engine) => commit(&engine, 7).is_err(),
        };
        fault::FAIL_DIR_SYNC_OF.set(None);
        if !refused {
            wrong.push(format!(
                "{row}: acknowledged though a target ancestor's sync failed"
            ));
        }
        match Engine::open(&store, configured()) {
            Ok(engine) => {
                if let Err(error) = commit(&engine, 3) {
                    wrong.push(format!("{row}: the commit once the fault cleared: {error}"));
                }
            }
            Err(error) => wrong.push(format!("{row}: the reopen once the fault cleared: {error}")),
        }
        match Engine::open(&store, configured()) {
            Ok(engine) => {
                for n in acknowledged.iter().copied().chain([3]) {
                    if !holds(&engine, n) {
                        wrong.push(format!(
                            "{row}: acknowledged commit {n} gone after a reopen"
                        ));
                    }
                }
            }
            Err(error) => wrong.push(format!("{row}: the last reopen: {error}")),
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The newest segment left after the open's replay is the one `Wal::open` appends to, and is
/// named when it is a link, whatever the replay did to the segments (review r1 on DUR-5, N2:
/// the open finds it without listing the log again). Rows, judged apart:
/// - under a checkpoint: the only segment left holds commit 1, which the checkpoint holds too,
///   so none of its records is replayed; it is linked to `data/new-parent/`;
/// - after a point-in-time cut: commits 1, 2 and 3 in segments of their own, the first linked;
///   an open up to revision 1 removes the other two, so the first is the newest.
///
/// With `data/new-parent`'s sync failing, the open (which changed the store, for the cut) or its
/// first commit is refused. Once it works, a commit is acknowledged and kept after a reopen, with
/// commit 1. Guards: right before the change too, which listed the log again.
#[test]
fn the_newest_segment_left_is_named_after_a_cut_and_under_a_checkpoint() {
    let mut wrong = Vec::new();
    for row in ["under a checkpoint", "after a point-in-time cut"] {
        let small = || EngineConfig {
            durability: DurabilityConfig {
                wal_segment_bytes: 1,
                ..config().durability
            },
            ..config()
        };
        let base = tempfile::tempdir().unwrap();
        let base = base.path();
        let store = base.join("store");
        let wal = store.join("wal");
        let opening = match row {
            "under a checkpoint" => {
                let segment = {
                    let engine = Engine::open(&store, config()).unwrap();
                    commit(&engine, 1).unwrap();
                    let segment = newest_file(&wal, "", ".wal");
                    std::fs::copy(&segment, base.join("held.wal")).unwrap();
                    engine.checkpoint().unwrap();
                    segment
                };
                // The log as a checkpoint's release left it undone: its old segment only.
                for entry in std::fs::read_dir(&wal).unwrap() {
                    std::fs::remove_file(entry.unwrap().path()).unwrap();
                }
                std::fs::copy(base.join("held.wal"), &segment).unwrap();
                config()
            }
            _ => {
                {
                    let engine = Engine::open(&store, small()).unwrap();
                    for n in 1..=3 {
                        commit(&engine, n).unwrap();
                    }
                }
                EngineConfig {
                    durability: DurabilityConfig {
                        recover_until: Some(1),
                        ..small().durability
                    },
                    ..small()
                }
            }
        };
        let file = match row {
            "under a checkpoint" => newest_file(&wal, "", ".wal"),
            _ => super::wal::list_segments(&wal).unwrap()[0].1.clone(),
        };
        let target = base
            .join("data")
            .join("new-parent")
            .join(file.file_name().unwrap());
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::rename(&file, &target).unwrap();
        if !link_file(&target, &file) {
            eprintln!("{row}: not run (no privilege to make a file symlink on this host)");
            continue;
        }
        fault::FAIL_DIR_SYNC_OF.set(Some((
            base.join("data").join("new-parent"),
            std::io::ErrorKind::Other,
        )));
        let refused = match Engine::open(&store, opening.clone()) {
            Err(_) => true,
            Ok(engine) => commit(&engine, 7).is_err(),
        };
        fault::FAIL_DIR_SYNC_OF.set(None);
        if !refused {
            wrong.push(format!(
                "{row}: acknowledged though a target ancestor's sync failed"
            ));
        }
        match Engine::open(&store, opening.clone()) {
            Ok(engine) => {
                if let Err(error) = commit(&engine, 4) {
                    wrong.push(format!("{row}: the commit once the fault cleared: {error}"));
                }
            }
            Err(error) => wrong.push(format!("{row}: the reopen once the fault cleared: {error}")),
        }
        match Engine::open(&store, config()) {
            Ok(engine) => {
                for n in [1, 4] {
                    if !holds(&engine, n) {
                        wrong.push(format!(
                            "{row}: acknowledged commit {n} gone after a reopen"
                        ));
                    }
                }
            }
            Err(error) => wrong.push(format!("{row}: the last reopen: {error}")),
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
