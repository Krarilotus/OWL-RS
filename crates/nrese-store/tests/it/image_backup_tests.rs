//! Image backups (image_backup.rs): written from a running store, restored into an empty
//! data directory, checked against their manifest.

use crate::support::Writes;
use std::fs;

use nrese_store::{StoreConfig, StoreService, read_manifest, restore_image};
use tempfile::tempdir;

fn count(service: &StoreService) -> u64 {
    service.stats().expect("stats").quad_count as u64
}

#[test]
fn an_image_restores_the_store_as_it_was() {
    let data = tempdir().unwrap();
    let backups = tempdir().unwrap();
    let store = StoreService::new(StoreConfig::on_disk(data.path())).unwrap();
    store
        .execute_update_str(
            "INSERT DATA { <http://example.com/a> <http://example.com/p> \"one\" , 2 . \
             GRAPH <http://example.com/g> { <http://example.com/b> <http://example.com/p> 3 } }",
        )
        .unwrap();
    let dir = backups.path().join("first");
    let manifest = store.backup_image(&dir).unwrap();
    assert_eq!(manifest.quads, 3);
    assert_eq!(read_manifest(&dir).unwrap(), manifest);
    // Changes after the backup are not in it.
    store
        .execute_update_str("INSERT DATA { <http://example.com/c> <http://example.com/p> 4 }")
        .unwrap();
    assert_eq!(count(&store), 4);
    // A second backup into the same directory is refused.
    assert!(store.backup_image(&dir).is_err());
    drop(store);

    // Into an empty directory: the store opens at the backup's revision and content.
    let restored = tempdir().unwrap();
    let target = restored.path().join("data");
    restore_image(&dir, &target).unwrap();
    let store = StoreService::new(StoreConfig::on_disk(&target)).unwrap();
    assert_eq!(count(&store), 3);
    assert_eq!(store.current_revision(), manifest.revision);
    let answer = store
        .execute_query_str("ASK { GRAPH <http://example.com/g> { ?s ?p 3 } }")
        .unwrap();
    assert!(String::from_utf8_lossy(&answer.payload).contains("true"));
    drop(store);

    // Not over a store, and not from a damaged image.
    assert!(restore_image(&dir, &target).is_err());
    let image = dir.join(&manifest.file);
    let mut bytes = fs::read(&image).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    fs::write(&image, bytes).unwrap();
    assert!(restore_image(&dir, &restored.path().join("other")).is_err());
}

#[test]
fn an_in_memory_store_backs_up_too() {
    let backups = tempdir().unwrap();
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    store
        .execute_update_str("INSERT DATA { <http://example.com/a> <http://example.com/p> 1 }")
        .unwrap();
    let dir = backups.path().join("memory");
    let manifest = store.backup_image(&dir).unwrap();
    let target = backups.path().join("data");
    restore_image(&dir, &target).unwrap();
    let store = StoreService::new(StoreConfig::on_disk(&target)).unwrap();
    assert_eq!(count(&store), 1);
    assert_eq!(store.current_revision(), manifest.revision);
}

/// Point-in-time restore: the image, then the log after it up to a revision; without a
/// revision, as far as the log goes.
#[test]
fn an_image_and_the_log_restore_a_later_revision() {
    let data = tempdir().unwrap();
    let backups = tempdir().unwrap();
    let config = StoreConfig {
        wal_archive: true,
        ..StoreConfig::on_disk(data.path())
    };
    let store = StoreService::new(config).unwrap();
    let insert = |n: u32| {
        store
            .execute_update_str(&format!(
                "INSERT DATA {{ <http://example.com/s{n}> <http://example.com/p> {n} }}"
            ))
            .unwrap()
    };
    insert(1);
    let dir = backups.path().join("image");
    let manifest = store.backup_image(&dir).unwrap();
    let mut revisions = Vec::new();
    for n in 2..6 {
        insert(n);
        revisions.push(store.current_revision());
    }
    drop(store);
    let logs = [data.path().join("wal-archive"), data.path().join("wal")];
    for (i, &revision) in revisions.iter().enumerate() {
        let target = backups.path().join(format!("at-{revision}"));
        let (restored, at) =
            nrese_store::restore_until(&dir, &target, &logs, Some(revision), None).unwrap();
        assert_eq!((restored, at), (manifest.clone(), revision));
        let store = StoreService::new(StoreConfig::on_disk(&target)).unwrap();
        assert_eq!(count(&store), 2 + i as u64);
    }
    let target = backups.path().join("all");
    let (_, at) = nrese_store::restore_until(&dir, &target, &logs, None, None).unwrap();
    assert_eq!(at, *revisions.last().unwrap());
}

/// Pruning the archive keeps what a restore after a revision needs.
#[test]
fn pruning_the_archive_keeps_the_segments_after_a_revision() {
    let archive = tempdir().unwrap();
    for first in [1u64, 5, 9, 14] {
        fs::write(archive.path().join(format!("{first:020}.wal")), b"x").unwrap();
    }
    fs::write(archive.path().join("other.txt"), b"x").unwrap();
    // Revision 9 on: the segment from 9 and the newest stay; 1..=4 and 5..=8 go.
    assert_eq!(
        nrese_store::prune_wal_archive(archive.path(), 9).unwrap(),
        2
    );
    let mut left: Vec<String> = fs::read_dir(archive.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            "00000000000000000009.wal",
            "00000000000000000014.wal",
            "other.txt"
        ]
    );
    // The newest segment always stays.
    assert_eq!(
        nrese_store::prune_wal_archive(archive.path(), 100).unwrap(),
        1
    );
    assert!(archive.path().join("00000000000000000014.wal").exists());
}

/// The variable that makes [`a_second_process_opens`] act: the store it opens.
const SECOND_OPEN: &str = "NRESE_TEST_SECOND_OPEN";

/// Run in a second process of this test binary ([`second_process_open`]): opens the engine
/// store named by [`SECOND_OPEN`] and exits 3 if it is refused `Locked` (asserted here, from
/// its own open), 0 if it opens holding one quad, 4 otherwise. In a test run (no variable) it
/// does nothing.
#[test]
fn a_second_process_opens() {
    let Ok(dir) = std::env::var(SECOND_OPEN) else {
        return;
    };
    let config = nrese_engine::EngineConfig {
        background_maintenance: false,
        ..nrese_engine::EngineConfig::default()
    };
    let code = match nrese_engine::Engine::open(std::path::Path::new(&dir), config) {
        Err(nrese_engine::EngineError::Locked(_)) => 3,
        Ok(engine) if engine.stats().quads == 1 => 0,
        Ok(engine) => {
            eprintln!("the second open holds {} quads", engine.stats().quads);
            4
        }
        Err(error) => {
            eprintln!("the second open: {error}");
            4
        }
    };
    std::process::exit(code);
}

/// What a second process of this test binary gets opening the store in `dir`: 3 `Locked`,
/// 0 opened with its quad ([`a_second_process_opens`]).
fn second_process_open(dir: &std::path::Path) -> Option<i32> {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "image_backup_tests::a_second_process_opens",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(SECOND_OPEN, dir)
        .status()
        .unwrap()
        .code()
}

/// A refused image placement whose image is a live store's `LOCK` (the image may have any
/// name) leaves the store locked against another process (review r1, R1-F1). The placement
/// hashes the file, so opens and closes it, and refuses: the hash doesn't match. Before the
/// fix the store's lock was a record lock, which that close dropped: a second process opened
/// the store beside the first. Once the first is dropped, the second opens with its commit.
/// Windows: a guard (a handle's lock is its own).
#[test]
fn a_refused_placement_of_a_lock_file_leaves_the_store_locked() {
    let data = tempdir().unwrap();
    let config = nrese_engine::EngineConfig {
        background_maintenance: false,
        ..nrese_engine::EngineConfig::default()
    };
    let engine = nrese_engine::Engine::open(data.path(), config).unwrap();
    let mut tx = engine.transaction();
    let quad = nrese_rdf::Quad::new(
        nrese_rdf::NamedNode::new_unchecked("http://example.com/s"),
        nrese_rdf::NamedNode::new_unchecked("http://example.com/p"),
        nrese_rdf::NamedNode::new_unchecked("http://example.com/o"),
        nrese_rdf::GraphName::DefaultGraph,
    );
    tx.insert(quad.as_ref());
    tx.commit().unwrap();
    let manifest = nrese_store::ImageManifest {
        format: "nrese-checkpoint".into(),
        file: "checkpoint-00000000000000000001.nck".into(),
        revision: 1,
        quads: 1,
        inferred: 0,
        bytes: 0,
        sha256: "0".repeat(64),
        created_unix: 0,
        nrese_version: "test".into(),
    };
    let target = tempdir().unwrap();
    let refused = nrese_store::image_backup::place_image(
        &data.path().join("LOCK"),
        manifest,
        &target.path().join("restored"),
    );
    assert!(
        matches!(refused, Err(nrese_store::StoreError::Configuration(_))),
        "the placement wasn't refused: {refused:?}"
    );
    assert_eq!(
        second_process_open(data.path()),
        Some(3),
        "a second process opened the store beside a live engine"
    );
    drop(engine);
    assert_eq!(
        second_process_open(data.path()),
        Some(0),
        "the released store didn't open in a second process with its commit"
    );
}

/// A restore names every directory it creates durably, through the placement owner
/// (`restore_image` → `place_image` → `nrese_engine::place::create_dir_all`; review r1,
/// R1-F2): into `sealed/a/b`, where `sealed` exists but can't be opened (mode 0o311), so its
/// sync, which must name `a`, fails, and the restore must refuse before it returns. Before the
/// fix only `a` and `b` were synced, so it returned. Guards: into `open/a/b` it restores, the
/// store opens with its quad and takes a commit; into an existing empty directory too. Unix
/// only (permissions); as root, where the mode doesn't keep the directory closed, the first
/// row isn't checked.
#[cfg(unix)]
#[test]
fn a_restore_names_every_directory_it_creates() {
    use std::os::unix::fs::PermissionsExt as _;
    let base = tempdir().unwrap();
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    store
        .execute_update_str("INSERT DATA { <http://example.com/a> <http://example.com/p> 1 }")
        .unwrap();
    let backup = base.path().join("backup");
    store.backup_image(&backup).unwrap();
    let mut wrong = Vec::new();

    let sealed = base.path().join("sealed");
    fs::create_dir(&sealed).unwrap();
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o311)).unwrap();
    if fs::read_dir(&sealed).is_ok() {
        eprintln!("row sealed: not checked (the directory opens: root?)");
    } else {
        // Tried twice (sol-loop on review r1, S1): the first refusal mustn't leave `a` for the
        // second to take as durably named.
        for attempt in ["sealed", "sealed, tried again"] {
            let restored = restore_image(&backup, &sealed.join("a").join("b"));
            eprintln!(
                "row {attempt}: {}",
                if restored.is_err() { "ok" } else { "FAILED" }
            );
            if restored.is_ok() {
                wrong.push(format!(
                    "{attempt}: restored under a directory whose sync fails"
                ));
            }
        }
    }
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o755)).unwrap();

    for (row, target) in [
        (
            "two missing levels (guard)",
            base.path().join("open").join("a").join("b"),
        ),
        ("an existing directory (guard)", {
            let existing = base.path().join("existing");
            fs::create_dir(&existing).unwrap();
            existing
        }),
    ] {
        let opened = restore_image(&backup, &target).map(|_| {
            let store = StoreService::new(StoreConfig::on_disk(&target)).unwrap();
            store
                .execute_update_str(
                    "INSERT DATA { <http://example.com/b> <http://example.com/p> 2 }",
                )
                .unwrap();
            count(&store)
        });
        let ok = matches!(opened, Ok(2));
        eprintln!("row {row}: {}", if ok { "ok" } else { "FAILED" });
        if !ok {
            wrong.push(format!("{row}: {opened:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
