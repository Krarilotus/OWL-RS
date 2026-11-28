//! Point-in-time restore interruptions, with the image at revision 2 and WAL segments
//! covering revisions 3–5, 6–8 and 9–11. The cutoff falls inside the second segment.

use std::fs;
use std::path::{Path, PathBuf};

use nrese_engine::{EngineError, fault_points};
use nrese_store::{ImageManifest, StoreConfig, StoreError, StoreService};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

const EX: &str = "http://example.org/";
const IMAGE_AT: u64 = 2;
const CUTOFF: u64 = 7;
const LAST: u64 = 11;

fn engine_config() -> nrese_engine::EngineConfig {
    nrese_engine::EngineConfig {
        background_maintenance: false,
        durability: nrese_engine::DurabilityConfig {
            wal_archive: true,
            ..nrese_engine::DurabilityConfig::default()
        },
        ..nrese_engine::EngineConfig::default()
    }
}

fn commit(engine: &nrese_engine::Engine, n: u64) {
    let node = |name: String| nrese_rdf::NamedNode::new_unchecked(format!("{EX}{name}"));
    let quad = nrese_rdf::Quad::new(
        node(format!("s{n}")),
        node("p".to_owned()),
        node(format!("o{n}")),
        nrese_rdf::GraphName::DefaultGraph,
    );
    let mut tx = engine.transaction();
    tx.insert(quad.as_ref());
    tx.commit().unwrap();
    assert_eq!(
        engine.snapshot().revision(),
        n,
        "setup: one revision per commit"
    );
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| {
                    let len = entry.metadata().map_or(0, |m| m.len());
                    format!("{}:{len}", entry.file_name().to_string_lossy())
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The source store, its image backup at [`IMAGE_AT`] and the logs a restore reads.
fn history(root: &Path) -> (PathBuf, Vec<PathBuf>) {
    let data = root.join("data");
    let backup = root.join("backup");
    fs::create_dir_all(&backup).unwrap();
    let engine = nrese_engine::Engine::open(&data, engine_config()).unwrap();
    for n in 1..=IMAGE_AT {
        commit(&engine, n);
    }
    let image = engine.write_image(&backup).unwrap();
    assert_eq!(image.revision, IMAGE_AT);
    let bytes = fs::read(&image.path).unwrap();
    let manifest = ImageManifest {
        format: "nrese-checkpoint".to_owned(),
        file: image
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        revision: image.revision,
        quads: image.quads,
        inferred: image.inferred,
        bytes: bytes.len() as u64,
        sha256: Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
        created_unix: 0,
        nrese_version: String::new(),
    };
    fs::write(
        backup.join(nrese_store::image_backup::MANIFEST),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    // Each checkpoint moves the log to a new segment and archives the ones it covers.
    engine.checkpoint().unwrap();
    for n in IMAGE_AT + 1..=LAST {
        commit(&engine, n);
        if n == 5 || n == 8 {
            engine.checkpoint().unwrap();
        }
    }
    drop(engine);
    let logs = vec![data.join("wal-archive"), data.join("wal")];
    let segments: Vec<String> = logs
        .iter()
        .flat_map(|dir| names(dir))
        .filter_map(|name| name.split_once(".wal:").map(|(first, _)| first.to_owned()))
        .map(|first| first.trim_start_matches('0').to_owned())
        .collect();
    eprintln!("setup: segments {segments:?}");
    assert_eq!(segments, ["1", "3", "6", "9"], "setup: the log's segments");
    (backup, logs)
}

/// The revision an ordinary open serves, checked against its visible quads.
fn served(store: &StoreService) -> String {
    let present: Vec<u64> = (1..=LAST)
        .filter(|n| {
            let answer = store
                .execute_query_str(&format!("ASK {{ <{EX}s{n}> <{EX}p> <{EX}o{n}> }}"))
                .unwrap();
            String::from_utf8_lossy(&answer.payload).contains("true")
        })
        .collect();
    let served = present.len() as u64;
    let revision = store.current_revision();
    let quads = store.stats().unwrap().quad_count;
    let label = if present != (1..=served).collect::<Vec<_>>() {
        "MIXED"
    } else if served == CUTOFF {
        "EXACT"
    } else if served < CUTOFF {
        "PREFIX"
    } else {
        "PAST"
    };
    format!("{label} (content at revision {served}, revision {revision}, {quads} quads)")
}

enum Stop {
    /// The sync of `target/wal` after this many of them passed.
    WalDirSync(usize),
    /// The recovery's cut, after this many of its steps passed.
    Cut(usize),
}

/// Spec: `restore_until` followed by an ordinary open refuses every interrupted copy or cut
/// as an unfinished restore. A completed restore, including a retry into an emptied
/// directory, serves exactly the requested revision.
#[test]
fn an_interrupted_point_in_time_restore_never_serves_another_revision() {
    let root = tempdir().unwrap();
    let (backup, logs) = history(root.path());

    // The restore uninterrupted (setup): the requested revision.
    let control = root.path().join("control");
    check_completed_restore(&backup, &control, &logs);

    let points = [
        ("after-first-segment", Stop::WalDirSync(0)),
        ("between-segments", Stop::WalDirSync(1)),
        ("after-last-segment", Stop::WalDirSync(2)),
        ("open-before-cut", Stop::Cut(0)),
        ("open-cut-segment-truncated", Stop::Cut(1)),
        ("open-after-cut-names-sync", Stop::WalDirSync(3)),
    ];
    let mut wrong = Vec::new();
    for (point, stop) in points {
        let target = root.path().join(point);
        let target_wal = target.join("wal");
        match stop {
            Stop::WalDirSync(passes) => {
                fault_points::fail_dir_sync_after(Some((target_wal.as_path(), passes)));
            }
            Stop::Cut(passes) => fault_points::stop_in_cut_after(Some(passes)),
        }
        let restored = nrese_store::restore_until(&backup, &target, &logs, Some(CUTOFF), None);
        fault_points::fail_dir_sync_after(None);
        fault_points::stop_in_cut_after(None);
        let case = match restored {
            Ok(report) => format!(
                "interruption was not reached (restored revision {})",
                report.1
            ),
            Err(error) if error.to_string().contains("fault point") => {
                match StoreService::new(StoreConfig::on_disk(&target)) {
                    Ok(store) => served(&store),
                    Err(StoreError::Engine(kind)) => format!("REFUSED ({kind:?})"),
                    Err(error) => format!("REFUSED ({error})"),
                }
            }
            Err(error) => format!("restore failed before the interruption: {error}"),
        };
        eprintln!("CASE {point}: {case}");
        // Both proof bases predate the variant, so compare the engine error's name.
        if case != "REFUSED (UnfinishedRestore)" {
            wrong.push(format!("{point}: {case}"));
        }
    }

    let retry = root.path().join("after-first-segment");
    fs::remove_dir_all(&retry).unwrap();
    check_completed_restore(&backup, &retry, &logs);

    assert!(
        wrong.is_empty(),
        "expected an unfinished restore: {wrong:#?}"
    );
}

fn check_completed_restore(backup: &Path, target: &Path, logs: &[PathBuf]) {
    let at = nrese_store::restore_until(backup, target, logs, Some(CUTOFF), None)
        .unwrap()
        .1;
    assert_eq!(at, CUTOFF);
    let store = StoreService::new(StoreConfig::on_disk(target)).unwrap();
    let answer = served(&store);
    eprintln!("CASE completed-restore: {answer}");
    assert_eq!(
        answer,
        format!("EXACT (content at revision {CUTOFF}, revision {CUTOFF}, {CUTOFF} quads)")
    );
    assert!(!target.join("restore.pending").exists());
}

/// Spec: an ordinary open during point-in-time restore returns the store's Locked error.
#[test]
fn an_open_during_restore_finds_the_store_locked() {
    let root = tempdir().unwrap();
    let (backup, logs) = history(root.path());
    let target = root.path().join("target");
    let open_target = target.clone();
    let wal = target.join("wal");
    fault_points::inspect_failed_dir_sync(&wal, 0, move || {
        let opened = StoreService::new(StoreConfig::on_disk(&open_target));
        match opened {
            Err(StoreError::Engine(EngineError::Locked(_))) => {}
            Err(error) => panic!("expected Locked while restoring, got {error}"),
            Ok(store) => panic!("expected Locked while restoring, served {}", served(&store)),
        }
    });
    let error =
        nrese_store::restore_until(&backup, &target, &logs, Some(CUTOFF), None).unwrap_err();
    fault_points::fail_dir_sync_after(None);
    assert!(error.to_string().contains("fault point"), "{error}");
}
