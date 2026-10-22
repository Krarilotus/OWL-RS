//! EEV-06: a checkpoint's time belongs to its snapshot, including portable replacements.
use super::{BulkMode, Engine, EngineError, config, copy_dir, fs, quad};

/// A valid replication frame with a selected source time, or the legacy untimed payload.
fn at_time(mut frame: Vec<u8>, time: Option<u64>) -> Vec<u8> {
    let end = frame.len();
    match time {
        Some(time) => frame[end - 8..].copy_from_slice(&time.to_le_bytes()),
        None => frame.truncate(end - 8),
    }
    let len = frame.len() - 8;
    frame[..4].copy_from_slice(&(len as u32).to_le_bytes());
    let crc = crc32fast::hash(&frame[8..]);
    frame[4..8].copy_from_slice(&crc.to_le_bytes());
    frame
}

#[test]
fn time_restore_never_installs_a_later_or_unknown_history() {
    let producer_dir = tempfile::tempdir().unwrap();
    let producer = Engine::open(producer_dir.path(), config()).unwrap();
    let mut records = Vec::new();
    for n in 1..=3 {
        let mut tx = producer.transaction();
        tx.insert(quad(n).as_ref());
        tx.commit().unwrap();
        records.push(producer.log_since(n - 1, usize::MAX).unwrap().frames);
    }

    // The same portable filename and statements are replaced by a different source time.
    // No old external fact, inode association or content-only certificate can vouch for it.
    let images = tempfile::tempdir().unwrap();
    let mut previous = None;
    for stamp in [100, 200] {
        let source_dir = tempfile::tempdir().unwrap();
        let source = Engine::open(source_dir.path(), config()).unwrap();
        source
            .apply_log(&at_time(records[0].clone(), Some(stamp)))
            .unwrap();
        let image = source.write_image(images.path()).unwrap();
        let bytes = fs::read(&image.path).unwrap();
        if let Some(previous) = previous {
            assert_ne!(
                bytes, previous,
                "replacement must carry its own snapshot time"
            );
        }
        previous = Some(bytes);
        for cutoff in [99, 100, 150, 200] {
            let target = tempfile::tempdir().unwrap();
            copy_dir(images.path(), target.path());
            let mut cfg = config();
            cfg.durability.recover_until_micros = Some(cutoff);
            match Engine::open(target.path(), cfg) {
                Ok(restored) => {
                    assert!(
                        stamp <= cutoff,
                        "restored commit {stamp} after cutoff {cutoff}"
                    );
                    assert_eq!(restored.snapshot().revision(), 1);
                }
                Err(EngineError::Configuration(_)) => assert!(stamp > cutoff),
                Err(error) => panic!("unexpected restore error: {error}"),
            }
        }
    }

    // A proven base and first later timed record can establish a cut before Unknown.
    // A cut depending on the untimed boundary refuses, before modifying any supplied log.
    let source_dir = tempfile::tempdir().unwrap();
    let source = Engine::open(source_dir.path(), config()).unwrap();
    source
        .apply_log(&at_time(records[0].clone(), Some(100)))
        .unwrap();
    source.checkpoint().unwrap();
    source
        .apply_log(&at_time(records[1].clone(), Some(200)))
        .unwrap();
    source
        .apply_log(&at_time(records[2].clone(), None))
        .unwrap();
    drop(source);
    for (cutoff, succeeds) in [(150, true), (200, false)] {
        let target = tempfile::tempdir().unwrap();
        copy_dir(source_dir.path(), target.path());
        let mut cfg = config();
        cfg.durability.recover_until_micros = Some(cutoff);
        match Engine::open(target.path(), cfg) {
            Ok(restored) => {
                assert!(succeeds);
                assert_eq!(restored.snapshot().revision(), 1);
            }
            Err(EngineError::Configuration(_)) => {
                assert!(!succeeds);
                let intact = Engine::open(target.path(), config()).unwrap();
                assert_eq!(intact.snapshot().revision(), 3);
            }
            Err(error) => panic!("unexpected restore error: {error}"),
        }
    }

    // Unknown survives later local commits, Replace, a portable image, and restart.
    let source = Engine::open(source_dir.path(), config()).unwrap();
    let mut tx = source.transaction();
    tx.insert(quad(4).as_ref());
    tx.commit().unwrap();
    let load = source.bulk_load(BulkMode::Replace);
    load.add(&[quad(5)]);
    load.finish().unwrap();
    let target = tempfile::tempdir().unwrap();
    source.write_image(target.path()).unwrap();
    let mut cfg = config();
    cfg.durability.recover_until_micros = Some(u64::MAX);
    assert!(matches!(
        Engine::open(target.path(), cfg),
        Err(EngineError::Configuration(_))
    ));
    assert_eq!(
        Engine::open(target.path(), config())
            .unwrap()
            .snapshot()
            .revision(),
        5
    );

    // Damage to the fact cannot assert a time when full-image verification is disabled.
    let target = tempfile::tempdir().unwrap();
    copy_dir(images.path(), target.path());
    let image = fs::read_dir(target.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut bytes = fs::read(&image).unwrap();
    bytes[24] ^= 1;
    fs::write(&image, bytes).unwrap();
    let mut cfg = config();
    cfg.durability.recover_until_micros = Some(u64::MAX);
    assert!(matches!(
        Engine::open(target.path(), cfg),
        Err(EngineError::Configuration(_))
    ));
    assert_eq!(
        Engine::open(target.path(), config())
            .unwrap()
            .snapshot()
            .revision(),
        1
    );

    // A previously observed future source time clamps a rolled-back local wall clock.
    let target = tempfile::tempdir().unwrap();
    let source = Engine::open(target.path(), config()).unwrap();
    source
        .apply_log(&at_time(records[0].clone(), Some(u64::MAX)))
        .unwrap();
    let mut tx = source.transaction();
    tx.insert(quad(4).as_ref());
    tx.commit().unwrap();
    let batch = source.log_since(1, usize::MAX).unwrap();
    assert_eq!(
        &batch.frames[batch.frames.len() - 8..],
        &u64::MAX.to_le_bytes()
    );
    source.checkpoint().unwrap();
    drop(source);
    let mut cfg = config();
    cfg.durability.recover_until_micros = Some(u64::MAX - 1);
    assert!(matches!(
        Engine::open(target.path(), cfg),
        Err(EngineError::Configuration(_))
    ));
}
