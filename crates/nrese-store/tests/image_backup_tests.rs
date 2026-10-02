//! Image backups (image_backup.rs): written from a running store, restored into an empty
//! data directory, checked against their manifest.

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
            nrese_store::restore_until(&dir, &target, &logs, Some(revision)).unwrap();
        assert_eq!((restored, at), (manifest.clone(), revision));
        let store = StoreService::new(StoreConfig::on_disk(&target)).unwrap();
        assert_eq!(count(&store), 2 + i as u64);
    }
    let target = backups.path().join("all");
    let (_, at) = nrese_store::restore_until(&dir, &target, &logs, None).unwrap();
    assert_eq!(at, *revisions.last().unwrap());
}
