//! Time-cutoff restore requires a contiguous, timed log after the image.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nrese_store::{ImageManifest, StoreConfig, StoreService};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

const EX: &str = "http://example.org/";

fn engine(dir: &Path, archive: bool) -> nrese_engine::Engine {
    let config = nrese_engine::EngineConfig {
        background_maintenance: false,
        durability: nrese_engine::DurabilityConfig {
            wal_archive: archive,
            ..nrese_engine::DurabilityConfig::default()
        },
        ..nrese_engine::EngineConfig::default()
    };
    nrese_engine::Engine::open(dir, config).unwrap()
}

fn commit(engine: &nrese_engine::Engine, n: u64, object: &str) {
    let node = |name: String| nrese_rdf::NamedNode::new_unchecked(format!("{EX}{name}"));
    let quad = nrese_rdf::Quad::new(
        node(format!("s{n}")),
        node("p".to_owned()),
        node(object.to_owned()),
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

fn commit_plain(engine: &nrese_engine::Engine, n: u64) {
    commit(engine, n, &format!("o{n}"));
}

/// An image of the latest snapshot in `backup`, with its manifest.
fn image(engine: &nrese_engine::Engine, backup: &Path) -> u64 {
    fs::create_dir_all(backup).unwrap();
    let image = engine.write_image(backup).unwrap();
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
    image.revision
}

fn segment(dir: &Path, first: u64) -> PathBuf {
    dir.join(format!("{first:020}.wal"))
}

/// The first revisions of the segments in `dir`, as named.
fn segments(dir: &Path) -> Vec<u64> {
    let mut firsts: Vec<u64> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    name.strip_suffix(".wal")?.parse().ok()
                })
                .collect()
        })
        .unwrap_or_default();
    firsts.sort_unstable();
    firsts
}

fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

/// CRC-32 (IEEE), as the log's frames carry it.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// A version 6 segment rewritten as a version 5 one (`NRESEWL5`, the format before commit
/// times): the same records, each without its time (the payload's last 8 bytes), reframed.
fn untimed(v6: &[u8]) -> Vec<u8> {
    assert_eq!(&v6[..8], b"NRESEWL6", "setup: a version 6 segment");
    let mut out = b"NRESEWL5".to_vec();
    let mut pos = 8;
    while pos < v6.len() {
        let len = u32::from_le_bytes(v6[pos..pos + 4].try_into().unwrap()) as usize;
        let payload = &v6[pos + 8..pos + 8 + len];
        let kept = &payload[..len - 8];
        out.extend_from_slice(&(kept.len() as u32).to_le_bytes());
        out.extend_from_slice(&crc32(kept).to_le_bytes());
        out.extend_from_slice(kept);
        pos += 8 + len;
    }
    assert_eq!(pos, v6.len(), "setup: whole frames");
    out
}

fn ask(store: &StoreService, n: u64, object: &str) -> bool {
    let answer = store
        .execute_query_str(&format!("ASK {{ <{EX}s{n}> <{EX}p> <{EX}{object}> }}"))
        .unwrap();
    String::from_utf8_lossy(&answer.payload).contains("true")
}

/// What an ordinary open of `target` serves: the revision its content holds, of 1..=`last`.
fn opens_at(target: &Path, last: u64) -> String {
    let store = match StoreService::new(StoreConfig::on_disk(target)) {
        Ok(store) => store,
        Err(error) => return format!("open refused ({})", error.to_string().replace('\n', " ")),
    };
    let present: Vec<u64> = (1..=last)
        .filter(|&n| ask(&store, n, &format!("o{n}")))
        .collect();
    let served = present.len() as u64;
    let content = match present == (1..=served).collect::<Vec<_>>() {
        true => format!("content at revision {served}"),
        false => format!("content MIXED {present:?}"),
    };
    format!(
        "{content}, revision {}, {} quads",
        store.current_revision(),
        store.stats().unwrap().quad_count
    )
}

fn check(row: &str, case: &str, reason: &str) {
    eprintln!("CASE {row}: {case}");
    assert!(
        case.starts_with("REFUSED"),
        "{row}: the restore was not refused: {case}"
    );
    assert!(case.contains(reason), "{row}: {case}");
}

/// Spec: `restore_until` rejects a WAL gap even at a record past the time cutoff.
/// the image at 10, revision 11 (committed before T) missing from the supplied log,
/// whose first record after the image is 12 (committed after T). Restored by time T: a gap.
#[test]
fn a_time_cutoff_with_a_gap_in_the_log_is_refused() {
    let root = tempdir().unwrap();
    let (data, backup) = (root.path().join("data"), root.path().join("backup"));
    let e = engine(&data, true);
    for n in 1..=10 {
        commit_plain(&e, n);
    }
    assert_eq!(image(&e, &backup), 10);
    e.checkpoint().unwrap();
    let before_last_commit = now_micros();
    commit_plain(&e, 11);
    e.checkpoint().unwrap();
    std::thread::sleep(Duration::from_millis(30));
    let t = now_micros();
    std::thread::sleep(Duration::from_millis(30));
    commit_plain(&e, 12);
    commit_plain(&e, 13);
    drop(e);
    let (archive, live) = (data.join("wal-archive"), data.join("wal"));
    assert_eq!(segments(&archive), [1, 11], "setup: the archive");
    assert_eq!(segments(&live), [12], "setup: the live log");
    // Control (setup): the whole log, cut at T, gives 11.
    let control = root.path().join("control");
    let (_, at, report) =
        nrese_store::restore_until(&backup, &control, &[archive, live.clone()], None, Some(t))
            .unwrap();
    assert_eq!(at, 11, "setup: T falls between revisions 11 and 12");
    // Spec: the report names the last kept commit and why recovery stopped.
    assert_eq!(report.stop, nrese_store::RestoreStop::PastTime);
    assert!((before_last_commit..=t).contains(&report.last_commit_micros.unwrap()));

    let supplied = root.path().join("supplied");
    fs::create_dir_all(&supplied).unwrap();
    fs::copy(segment(&live, 12), segment(&supplied, 12)).unwrap();
    let target = root.path().join("target");
    let case = match nrese_store::restore_until(&backup, &target, &[supplied], None, Some(t)) {
        Err(error) => format!("REFUSED ({error})"),
        Ok(report) => format!(
            "SERVED (the restore returned revision {}; an ordinary open: {})",
            report.1,
            opens_at(&target, 13)
        ),
    };
    check("time-cut-hides-gap", &case, "WAL gap: expected revision 11");
}

/// Spec: `restore_until` rejects untimed records after the image under a time cutoff.
/// the image at 2, then records 3-5 without a time (a version 5 segment); 4 and 5
/// were committed after T. Restored by time T: the time of what is kept is unknown.
#[test]
fn untimed_records_under_a_time_cutoff_are_refused() {
    let root = tempdir().unwrap();
    let (data, backup) = (root.path().join("data"), root.path().join("backup"));
    let e = engine(&data, true);
    commit_plain(&e, 1);
    commit_plain(&e, 2);
    assert_eq!(image(&e, &backup), 2);
    e.checkpoint().unwrap();
    commit_plain(&e, 3);
    std::thread::sleep(Duration::from_millis(30));
    let t = now_micros();
    std::thread::sleep(Duration::from_millis(30));
    commit_plain(&e, 4);
    commit_plain(&e, 5);
    drop(e);
    let live = data.join("wal");
    assert_eq!(segments(&live), [3], "setup: the live log");
    // Control (setup): the timed segment, cut at T, gives 3.
    let control = root.path().join("control");
    let at = nrese_store::restore_until(&backup, &control, &[live.clone()], None, Some(t))
        .unwrap()
        .1;
    assert_eq!(at, 3, "setup: T falls between revisions 3 and 4");

    let supplied = root.path().join("supplied");
    fs::create_dir_all(&supplied).unwrap();
    let bytes = fs::read(segment(&live, 3)).unwrap();
    fs::write(segment(&supplied, 3), untimed(&bytes)).unwrap();
    let target = root.path().join("target");
    let case = match nrese_store::restore_until(&backup, &target, &[supplied], None, Some(t)) {
        Err(error) => format!("REFUSED ({error})"),
        Ok(report) => format!(
            "SERVED (the restore returned revision {}; an ordinary open: {}; revisions 4 and 5 \
             were committed after T)",
            report.1,
            opens_at(&target, 5)
        ),
    };
    check(
        "untimed-records-pass-time-cut",
        &case,
        "restore by revision",
    );
}
