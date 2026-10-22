//! The existing N4 archive/open regression's fact-acquisition rows.
use std::fs;

use crate::durability::codec::{CommitRecord, encode_record};
use crate::durability::{sidecar, wal};
use crate::{DurabilityConfig, Engine, EngineConfig, EngineError, SyncPolicy};

pub(super) fn facts_stay_associated_during_acquisition() {
    for (copy, action) in [
        (false, "known"),
        (false, "missing"),
        (false, "prune"),
        (true, "prune"),
        (true, "replace"),
    ] {
        let (root, recipient, destination) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let config = EngineConfig {
            background_maintenance: false,
            durability: DurabilityConfig {
                sync: SyncPolicy::EveryCommit,
                wal_segment_bytes: 1,
                wal_archive: true,
                ..DurabilityConfig::default()
            },
            ..EngineConfig::default()
        };
        let source = Engine::open(root.path(), config.clone()).unwrap();
        let mut frames = Vec::new();
        // Revision3 is another archived segment, so real archive pruning may remove2
        // while retaining the newest archive segment; revision4 remains active.
        for revision in 1..=4 {
            let record = CommitRecord {
                revision,
                committed_micros: Some(revision * 10),
                ..CommitRecord::default()
            };
            encode_record(&record, record.committed_micros, &mut frames).unwrap();
            source.apply_log(&frames).unwrap();
            frames.clear();
            if revision == 1 {
                source.write_image(recipient.path()).unwrap();
            }
        }
        source.checkpoint().unwrap();
        let segment = wal::list_segments(&wal::archive_dir(root.path()))
            .unwrap()
            .into_iter()
            .find(|(first, _)| *first == 2)
            .unwrap()
            .1;
        assert!(segment.is_file());
        if action == "missing" {
            fs::remove_file(sidecar::path_of(&segment)).unwrap();
        }
        let selected = segment.clone();
        let mut once = true;
        wal::acquire::on_open(Some(Box::new(move |path| {
            if path == selected && std::mem::take(&mut once) {
                match action {
                    "prune" => wal::remove_log_segment(path).unwrap(),
                    "replace" => {
                        // Actual copy publication: Unknown fact first, replacement bytes
                        // second. The source's old opened file must not acquire that fact.
                        let unknown = tempfile::tempdir().unwrap();
                        let mut writer = wal::Wal::open(
                            unknown.path(),
                            2,
                            u64::MAX,
                            SyncPolicy::EveryCommit,
                            false,
                            None,
                        )
                        .unwrap();
                        let _ = writer
                            .append(&CommitRecord {
                                revision: 2,
                                committed_micros: None,
                                ..CommitRecord::default()
                            })
                            .unwrap();
                        drop(writer);
                        let unknown = wal::list_segments(unknown.path()).unwrap().remove(0).1;
                        match wal::copy_log_segment(&unknown, path.parent().unwrap()) {
                            Ok(_) => {}
                            // Windows may refuse replacement while the old file is open.
                            // Its already-published Unknown metadata must still invalidate
                            // the outer acquisition; no successful untimed batch is allowed.
                            Err(EngineError::Io(error))
                                if cfg!(windows)
                                    && error.kind() == std::io::ErrorKind::PermissionDenied =>
                            {
                                assert!(path.is_file());
                                assert_eq!(
                                    wal::read_segment(path, 1).unwrap().records[0].committed_micros,
                                    None,
                                    "replacement must have published its Unknown fact before failure"
                                );
                            }
                            Err(error) => panic!("replacement failed: {error}"),
                        }
                    }
                    _ => {}
                }
            }
        })));
        let result = if copy {
            wal::copy_log_segment(&segment, destination.path()).map(|path| {
                let mut batch = super::LogBatch {
                    latest: 4,
                    ..super::LogBatch::default()
                };
                for record in wal::read_segment(&path, 1).unwrap().records {
                    encode_record(&record, record.committed_micros, &mut batch.frames).unwrap();
                    batch.records += 1;
                    batch.last = record.revision;
                }
                batch
            })
        } else {
            source.log_since(1, 1)
        };
        wal::acquire::on_open(None);
        let batch = match result {
            Err(EngineError::LogTruncated { .. }) if action == "prune" => continue,
            Err(EngineError::Io(error))
                if copy
                    && ["prune", "replace"].contains(&action)
                    && error.kind() == std::io::ErrorKind::NotFound =>
            {
                continue;
            }
            Ok(batch) => batch,
            Err(error) => panic!("copy={copy} action={action}: {error}"),
        };
        assert_eq!(batch.records, 1, "copy={copy} action={action}");
        let record = match crate::durability::codec::decode_frame(&batch.frames) {
            crate::durability::codec::Frame::Record(record, _) => record,
            _ => panic!("expected complete record"),
        };
        assert_eq!(
            record.committed_micros,
            (action != "missing").then_some(20),
            "successful acquisition erased known time: copy={copy} action={action}"
        );
        let replica = Engine::open(recipient.path(), config.clone()).unwrap();
        replica.apply_log(&batch.frames).unwrap();
        replica.checkpoint().unwrap();
        drop(replica);
        let mut restore = config;
        restore.durability.recover_until_micros = Some(20);
        match Engine::open(recipient.path(), restore) {
            Ok(reopened) => {
                assert_ne!(action, "missing");
                assert_eq!(reopened.snapshot().revision(), 2);
            }
            Err(EngineError::Configuration(_)) if action == "missing" => {}
            Err(error) => panic!("copy={copy} action={action}: {error}"),
        }
    }

    // Copy cannot certify an empty Known source after its first Unknown append.
    let (root, destination) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut writer = wal::Wal::open(
        root.path(),
        1,
        u64::MAX,
        SyncPolicy::EveryCommit,
        false,
        None,
    )
    .unwrap();
    let segment = wal::list_segments(root.path()).unwrap().remove(0).1;
    wal::acquire::on_open(Some(Box::new(move |_| {
        let _ = writer
            .append(&CommitRecord {
                revision: 1,
                committed_micros: None,
                ..CommitRecord::default()
            })
            .unwrap();
    })));
    let result = wal::copy_log_segment(&segment, destination.path());
    wal::acquire::on_open(None);
    assert!(matches!(result, Err(EngineError::Io(error))
        if error.kind() == std::io::ErrorKind::NotFound));
}
