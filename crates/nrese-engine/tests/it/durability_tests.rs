//! E4 gate: recovery to the last acknowledged revision after crashes at every write step.
//!
//! Crashes are simulated at the file level: the engine is dropped (as a killed process
//! would leave its files) and the files are then cut or damaged the way a crash would.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use nrese_engine::{
    BulkMode, DurabilityConfig, Engine, EngineConfig, EngineError, QuadPattern, SyncPolicy,
    TermKind,
};
use nrese_rdf::vocab::xsd;
use nrese_rdf::{GraphName, Literal, NamedNode, Quad};

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

fn quad(n: u64) -> Quad {
    Quad::new(
        NamedNode::new_unchecked(format!("http://example.com/s{}", n % 7)),
        NamedNode::new_unchecked(format!("http://example.com/p{}", n % 3)),
        Literal::new_typed_literal(format!("{n}"), xsd::INTEGER),
        if n.is_multiple_of(2) {
            GraphName::DefaultGraph
        } else {
            NamedNode::new_unchecked("http://example.com/g").into()
        },
    )
}

fn label(n: u64) -> Quad {
    Quad::new(
        NamedNode::new_unchecked(format!("http://example.com/s{n}")),
        NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label"),
        Literal::new_language_tagged_literal_unchecked(format!("label {n}"), "en"),
        GraphName::DefaultGraph,
    )
}

fn contents(engine: &Engine) -> HashSet<Quad> {
    let snapshot = engine.snapshot();
    snapshot
        .quads_for_pattern(&QuadPattern::all())
        .map(|q| snapshot.decode_quad(q).expect("decodable"))
        .collect()
}

/// Commits `batches` (inserts, deletes) and returns the expected contents after each one.
fn commit_all(engine: &Engine, batches: &[(Vec<Quad>, Vec<Quad>)]) -> Vec<HashSet<Quad>> {
    let mut model = contents(engine);
    let mut states = Vec::new();
    for (inserts, deletes) in batches {
        let mut tx = engine.transaction();
        for quad in inserts {
            tx.insert(quad.as_ref());
            model.insert(quad.clone());
        }
        for quad in deletes {
            tx.remove(quad.as_ref());
            model.remove(quad);
        }
        tx.commit().expect("commit");
        states.push(model.clone());
    }
    states
}

fn batches(range: std::ops::Range<u64>) -> Vec<(Vec<Quad>, Vec<Quad>)> {
    range
        .map(|n| {
            let inserts = vec![quad(n), label(n)];
            let deletes = if n % 4 == 3 {
                vec![quad(n - 2)]
            } else {
                Vec::new()
            };
            (inserts, deletes)
        })
        .collect()
}

fn segments(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(dir.join("wal"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "wal"))
        .collect();
    paths.sort();
    paths
}

#[test]
fn reopen_recovers_quads_terms_and_revision() {
    let dir = tempfile::tempdir().unwrap();
    let states = {
        let engine = Engine::open(dir.path(), config()).unwrap();
        commit_all(&engine, &batches(0..20))
    };
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(engine.stats().revision, 20, "revision is persistent");
    assert_eq!(contents(&engine), states[19]);
    // New commits continue the revision sequence and survive another restart.
    commit_all(&engine, &batches(20..21));
    drop(engine);
    assert_eq!(
        Engine::open(dir.path(), config()).unwrap().stats().revision,
        21
    );
}

#[test]
fn checkpoint_plus_wal_recovers_and_releases_covered_segments() {
    let dir = tempfile::tempdir().unwrap();
    let small_segments = EngineConfig {
        durability: DurabilityConfig {
            wal_segment_bytes: 256,
            ..config().durability
        },
        ..config()
    };
    let states = {
        let engine = Engine::open(dir.path(), small_segments).unwrap();
        let mut states = commit_all(&engine, &batches(0..30));
        assert!(segments(dir.path()).len() > 3, "small segments rotate");
        assert_eq!(engine.checkpoint().unwrap(), 30);
        assert_eq!(
            segments(dir.path()).len(),
            1,
            "covered segments are deleted"
        );
        states.extend(commit_all(&engine, &batches(30..40)));
        states
    };
    let engine = Engine::open(dir.path(), small_segments).unwrap();
    assert_eq!(engine.stats().revision, 40);
    assert_eq!(contents(&engine), states[39]);
}

#[test]
fn torn_tail_at_every_byte_recovers_the_previous_revision() {
    let source = tempfile::tempdir().unwrap();
    let (states, full_len, before_last) = {
        let engine = Engine::open(source.path(), config()).unwrap();
        let mut states = commit_all(&engine, &batches(0..5));
        let before_last = fs::metadata(&segments(source.path())[0]).unwrap().len();
        states.extend(commit_all(&engine, &batches(5..6)));
        (
            states,
            fs::metadata(&segments(source.path())[0]).unwrap().len(),
            before_last,
        )
    };
    for cut in before_last..full_len {
        let dir = tempfile::tempdir().unwrap();
        copy_dir(source.path(), dir.path());
        let segment = &segments(dir.path())[0];
        fs::OpenOptions::new()
            .write(true)
            .open(segment)
            .unwrap()
            .set_len(cut)
            .unwrap();

        let engine = Engine::open(dir.path(), config()).unwrap();
        assert_eq!(engine.stats().revision, 5, "cut at {cut}");
        assert_eq!(contents(&engine), states[4], "cut at {cut}");
        // The torn tail is gone, so the log accepts new commits and replays them.
        commit_all(&engine, &batches(5..6));
        drop(engine);
        let engine = Engine::open(dir.path(), config()).unwrap();
        assert_eq!(contents(&engine), states[5], "recommit after cut at {cut}");
    }
}

#[test]
fn corruption_before_the_tail_is_an_error_not_data_loss() {
    let dir = tempfile::tempdir().unwrap();
    let small_segments = EngineConfig {
        durability: DurabilityConfig {
            wal_segment_bytes: 256,
            ..config().durability
        },
        ..config()
    };
    {
        let engine = Engine::open(dir.path(), small_segments).unwrap();
        commit_all(&engine, &batches(0..30));
    }
    let first = &segments(dir.path())[0];
    let mut bytes = fs::read(first).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xff;
    fs::write(first, bytes).unwrap();
    match Engine::open(dir.path(), small_segments) {
        Err(EngineError::Corruption(_)) => {}
        other => panic!("expected corruption, got {other:?}"),
    }
}

/// The frames of a WAL segment: (start, end) of each, after the 8-byte magic.
fn frames(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut frames = Vec::new();
    let mut at = 8;
    while at < bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        frames.push((at, at + 8 + len));
        at += 8 + len;
    }
    frames
}

/// Damage to the one segment of a three-commit log, by the sync policy it was written
/// under: truncated where the write never landed whole (or the policy allows losing the last
/// commits), refused as corruption where acknowledged records may be lost, naming the last
/// intact revision; `recover_until` at it then keeps the intact part.
#[test]
fn wal_damage_is_truncated_or_refused_by_its_evidence() {
    type Damage = fn(&mut Vec<u8>, &[(usize, usize)]);
    let flip_first: Damage = |bytes, frames| bytes[frames[0].0 + 12] ^= 1;
    let flip_last: Damage = |bytes, frames| bytes[frames[2].0 + 12] ^= 1;
    // The segment rewritten as version 4 (untimed frames, as a store that keeps integers in
    // its dictionary writes), then its last frame damaged.
    let untimed_flip_last: Damage = |bytes, frames| {
        let mut untimed = b"NRESEWL4".to_vec();
        for &(start, end) in frames {
            let payload = &bytes[start + 8..end - 8];
            untimed.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            untimed.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
            untimed.extend_from_slice(payload);
        }
        let last = untimed.len() - (frames[2].1 - frames[2].0 - 8) + 12;
        untimed[last] ^= 1;
        *bytes = untimed;
    };
    let flip_two: Damage = |bytes, frames| {
        bytes[frames[0].0 + 12] ^= 1;
        bytes[frames[1].0 + 12] ^= 1;
    };
    let long_and_flipped: Damage = |bytes, frames| {
        bytes[frames[0].0..frames[0].0 + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        bytes[frames[0].0 + 12] ^= 1;
    };
    let zero_tail: Damage = |bytes, _| bytes.extend_from_slice(&[0; 40]);
    let zero_end: Damage = |bytes, frames| bytes[frames[2].1 - 8..frames[2].1].fill(0);
    let long_length: Damage = |bytes, frames| {
        bytes[frames[1].0..frames[1].0 + 4].copy_from_slice(&u32::MAX.to_le_bytes())
    };
    let every = SyncPolicy::EveryCommit;
    let buffered = SyncPolicy::OsBuffered;
    // The segment's sidecar: as written, removed (an older segment), or there but unreadable.
    #[derive(Debug, PartialEq)]
    enum Sidecar {
        Kept,
        Missing,
        Unreadable,
    }
    // Opening: at this revision; refused as corruption, naming this last intact revision; or
    // failing on an I/O error.
    #[derive(Debug)]
    enum Expect {
        Opens(u64),
        Refused(u64),
        Fails,
    }
    use {Expect::*, Sidecar::*};
    // (case, policy, sidecar, damage, recover_until, expected)
    type Row = (
        &'static str,
        SyncPolicy,
        Sidecar,
        Damage,
        Option<u64>,
        Expect,
    );
    let rows: [Row; 14] = [
        (
            "damage before intact records",
            every,
            Kept,
            flip_first,
            None,
            Refused(0),
        ),
        (
            "the same, under os-buffered",
            buffered,
            Kept,
            flip_first,
            None,
            Refused(0),
        ),
        (
            "cut before the damage",
            every,
            Kept,
            flip_first,
            Some(0),
            Opens(0),
        ),
        (
            "two damaged records before an intact one, os-buffered",
            buffered,
            Kept,
            flip_two,
            None,
            Refused(0),
        ),
        (
            "a damaged length and payload before intact records",
            every,
            Kept,
            long_and_flipped,
            None,
            Refused(0),
        ),
        ("a zero-filled tail", every, Kept, zero_tail, None, Opens(3)),
        (
            "an untimed (version 4) last record damaged",
            every,
            Kept,
            untimed_flip_last,
            None,
            Refused(2),
        ),
        (
            "a damaged length over an intact record",
            every,
            Kept,
            long_length,
            None,
            Refused(1),
        ),
        (
            "a synced last record damaged",
            every,
            Kept,
            flip_last,
            None,
            Refused(2),
        ),
        (
            "cut before the damaged last record",
            every,
            Kept,
            flip_last,
            Some(2),
            Opens(2),
        ),
        (
            "a last record whose end never landed",
            every,
            Kept,
            zero_end,
            None,
            Opens(2),
        ),
        (
            "a last record damaged under os-buffered",
            buffered,
            Kept,
            flip_last,
            None,
            Opens(2),
        ),
        (
            "a last record damaged, no policy recorded",
            every,
            Missing,
            flip_last,
            None,
            Opens(2),
        ),
        (
            "a last record damaged, its sidecar unreadable",
            every,
            Unreadable,
            flip_last,
            None,
            Fails,
        ),
    ];
    for (case, sync, sidecar, damage, until, expected) in rows {
        let dir = tempfile::tempdir().unwrap();
        let written = EngineConfig {
            durability: DurabilityConfig {
                sync,
                ..config().durability
            },
            ..config()
        };
        {
            let engine = Engine::open(dir.path(), written).unwrap();
            commit_all(&engine, &batches(0..3));
        }
        let segment = &segments(dir.path())[0];
        let mut bytes = fs::read(segment).unwrap();
        let at = frames(&bytes);
        assert_eq!(at.len(), 3, "{case}");
        damage(&mut bytes, &at);
        fs::write(segment, bytes).unwrap();
        let mut meta = segment.clone().into_os_string();
        meta.push(".meta");
        if sidecar != Kept {
            fs::remove_file(&meta).unwrap();
        }
        if sidecar == Unreadable {
            // A directory where the file should be: reading it fails on every platform.
            fs::create_dir(&meta).unwrap();
        }
        let reopen = EngineConfig {
            durability: DurabilityConfig {
                recover_until: until,
                ..written.durability
            },
            ..written
        };
        match (Engine::open(dir.path(), reopen), expected) {
            (Ok(engine), Opens(revision)) => {
                assert_eq!(engine.stats().revision, revision, "{case}")
            }
            (Err(EngineError::Corruption(why)), Refused(intact)) => assert!(
                why.contains(&format!("intact through revision {intact};")),
                "{case}: {why}"
            ),
            (Err(EngineError::Io(_)), Fails) => {}
            (other, expected) => panic!(
                "{case}: expected {expected:?}, got {:?}",
                other.map(|e| e.stats().revision)
            ),
        }
    }
}

/// A segment holds commits of one sync policy: reopening under another starts a new one,
/// and each records its own.
#[test]
fn a_new_sync_policy_starts_a_new_segment() {
    let dir = tempfile::tempdir().unwrap();
    let under = |sync| EngineConfig {
        durability: DurabilityConfig {
            sync,
            ..config().durability
        },
        ..config()
    };
    for (sync, commits) in [
        (SyncPolicy::EveryCommit, 0..2),
        (SyncPolicy::OsBuffered, 2..3),
    ] {
        let engine = Engine::open(dir.path(), under(sync)).unwrap();
        commit_all(&engine, &batches(commits));
    }
    let policies: Vec<String> = segments(dir.path())
        .iter()
        .map(|segment| {
            let mut meta = segment.clone().into_os_string();
            meta.push(".meta");
            fs::read_to_string(meta).unwrap()
        })
        .collect();
    assert_eq!(policies, ["sync=every-commit\n", "sync=os-buffered\n"]);
    let engine = Engine::open(dir.path(), under(SyncPolicy::OsBuffered)).unwrap();
    assert_eq!(engine.stats().revision, 3);
}

#[test]
fn crash_during_checkpoint_keeps_the_previous_state() {
    let dir = tempfile::tempdir().unwrap();
    let states = {
        let engine = Engine::open(dir.path(), config()).unwrap();
        commit_all(&engine, &batches(0..10))
    };
    // A crash while writing the checkpoint leaves a partial temporary file.
    fs::write(
        dir.path().join("checkpoint-00000000000000000010.tmp"),
        b"partial",
    )
    .unwrap();
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(contents(&engine), states[9]);
    assert!(
        !dir.path()
            .join("checkpoint-00000000000000000010.tmp")
            .exists()
    );
}

#[test]
fn crash_between_checkpoint_and_wal_release_skips_covered_records() {
    let dir = tempfile::tempdir().unwrap();
    let saved_wal = tempfile::tempdir().unwrap();
    let states = {
        let engine = Engine::open(dir.path(), config()).unwrap();
        let states = commit_all(&engine, &batches(0..10));
        copy_dir(&dir.path().join("wal"), saved_wal.path());
        engine.checkpoint().unwrap();
        states
    };
    // Restore the pre-checkpoint WAL, as if the release step never ran.
    fs::remove_dir_all(dir.path().join("wal")).unwrap();
    copy_dir(saved_wal.path(), &dir.path().join("wal"));
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(engine.stats().revision, 10);
    assert_eq!(contents(&engine), states[9]);
}

#[test]
fn terms_from_aborted_transactions_keep_the_dictionary_contiguous() {
    let dir = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        {
            let mut tx = engine.transaction();
            tx.insert(label(100).as_ref()); // interns terms, then aborts
        }
        commit_all(&engine, &batches(0..3));
        engine.checkpoint().unwrap();
        {
            let mut tx = engine.transaction();
            tx.insert(label(200).as_ref());
        }
        commit_all(&engine, &batches(3..4));
    }
    let engine = Engine::open(dir.path(), config()).unwrap();
    let snapshot = engine.snapshot();
    let contains = |quad: Quad| {
        snapshot
            .lookup_quad(quad.as_ref())
            .is_some_and(|encoded| snapshot.contains(&encoded))
    };
    for n in 0..4 {
        assert!(contains(label(n)), "label {n}");
    }
    // Aborted quads are absent, although their terms were logged to keep ids contiguous.
    assert!(!contains(label(100)));
    assert!(!contains(label(200)));
}

/// Files of an older format version are reported as such, not as corruption, so operators
/// know to reload rather than to restore from backup.
#[test]
fn older_format_versions_are_rejected_explicitly() {
    let dir = tempfile::tempdir().unwrap();
    drop(Engine::open(dir.path(), config()).unwrap());
    fs::write(
        dir.path().join("wal").join("00000000000000000001.wal"),
        b"NRESEWL1",
    )
    .unwrap();
    let error = Engine::open(dir.path(), config()).unwrap_err();
    assert!(
        matches!(error, EngineError::UnsupportedFormat(_)),
        "{error}"
    );

    let dir = tempfile::tempdir().unwrap();
    let mut old_checkpoint = b"NRESECK1".to_vec();
    old_checkpoint.extend_from_slice(&crc32(&old_checkpoint).to_le_bytes());
    fs::write(
        dir.path().join("checkpoint-00000000000000000001.nck"),
        old_checkpoint,
    )
    .unwrap();
    let error = Engine::open(dir.path(), config()).unwrap_err();
    assert!(
        matches!(error, EngineError::UnsupportedFormat(_)),
        "{error}"
    );
}

/// CRC-32 (IEEE), bitwise; only used to frame a hand-written test file.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

#[test]
fn a_directory_can_only_be_opened_once() {
    let dir = tempfile::tempdir().unwrap();
    let _engine = Engine::open(dir.path(), config()).unwrap();
    assert!(matches!(
        Engine::open(dir.path(), config()),
        Err(EngineError::Locked(_))
    ));
}

#[test]
fn background_checkpoints_bound_the_wal() {
    let dir = tempfile::tempdir().unwrap();
    let config = EngineConfig {
        background_maintenance: true,
        durability: DurabilityConfig {
            wal_segment_bytes: 512,
            checkpoint_after_wal_bytes: 2048,
            sync: SyncPolicy::OsBuffered,
            verify_on_open: false,
            map_checkpoints: true,
            bulk_load_memory: None,
            wal_archive: false,
            recover_until: None,
            recover_until_micros: None,
        },
        ..EngineConfig::default()
    };
    let states = {
        let engine = Engine::open(dir.path(), config).unwrap();
        commit_all(&engine, &batches(0..200))
    };
    let checkpoints = fs::read_dir(dir.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "nck")
        })
        .count();
    assert_eq!(checkpoints, 1, "exactly one checkpoint is kept");
    assert!(
        segments(dir.path()).len() < 20,
        "covered WAL segments were released"
    );
    let engine = Engine::open(dir.path(), config).unwrap();
    assert_eq!(contents(&engine), states[199]);
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else if path.file_name().is_some_and(|n| n != "LOCK") {
            fs::copy(&path, &target).unwrap();
        }
    }
}

/// A checkpoint of a stack with several runs and tombstones (no compaction) stores the
/// merged, visible state; restart reads it back as one run (format 5).
#[test]
fn checkpoint_of_many_runs_with_tombstones_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let config = EngineConfig {
        compaction: nrese_engine::CompactionPolicy {
            fanout: 0,
            ..nrese_engine::CompactionPolicy::default()
        },
        ..config()
    };
    let engine = Engine::open(dir.path(), config).unwrap();
    let mut batches = batches(0..400);
    // Delete quads inserted by earlier commits: tombstones in later runs.
    for (i, (_, deletes)) in batches.iter_mut().enumerate().skip(1) {
        deletes.extend((0..(i as u64 * 3)).step_by(7).map(quad));
    }
    let states = commit_all(&engine, &batches);
    assert!(engine.stats().runs > 4, "runs: {}", engine.stats().runs);
    engine.checkpoint().unwrap();
    let expected = states.last().unwrap().clone();
    assert_eq!(contents(&engine), expected);
    drop(engine);
    let reopened = Engine::open(dir.path(), config).unwrap();
    assert_eq!(contents(&reopened), expected);
    assert_eq!(reopened.stats().runs, 1, "one run per stack after restart");
}

/// An `xsd:int` literal, and a quad with it.
fn int(n: u64) -> Literal {
    Literal::new_typed_literal(n.to_string(), xsd::INT)
}

fn with_int(n: u64) -> Quad {
    Quad::new(
        NamedNode::new_unchecked("http://example.com/s"),
        NamedNode::new_unchecked("http://example.com/age"),
        int(n),
        GraphName::DefaultGraph,
    )
}

fn commit_one(engine: &Engine, quad: &Quad) {
    let mut tx = engine.transaction();
    tx.insert(quad.as_ref());
    tx.commit().unwrap();
}

/// How `xsd:int` literals are encoded in `engine`.
fn int_kind(engine: &Engine, n: u64) -> Option<TermKind> {
    engine
        .snapshot()
        .lookup(int(n).as_ref().into())
        .map(|id| id.kind())
}

/// The magic of the newest WAL segment.
fn newest_segment_magic(dir: &Path) -> Vec<u8> {
    let newest = segments(dir).pop().unwrap();
    fs::read(newest).unwrap()[..8].to_vec()
}

/// A new store inlines integer-derived literals, through the WAL, checkpoints and restarts.
#[test]
fn new_stores_inline_integer_derived_literals() {
    let dir = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        commit_one(&engine, &with_int(5));
        assert_eq!(int_kind(&engine, 5), Some(TermKind::DerivedInteger));
    }
    assert_eq!(newest_segment_magic(dir.path()), b"NRESEWL6");
    // A store whose newest segment is of version 5 starts a version 6 one at the next
    // commit, not appending timed records to the old format.
    let newest = segments(dir.path()).pop().unwrap();
    let mut bytes = fs::read(&newest).unwrap();
    bytes[..8].copy_from_slice(b"NRESEWL5");
    fs::write(&newest, bytes).unwrap();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        assert_eq!(int_kind(&engine, 5), Some(TermKind::DerivedInteger));
        commit_one(&engine, &with_int(7));
    }
    assert_eq!(segments(dir.path()).len(), 2);
    assert_eq!(newest_segment_magic(dir.path()), b"NRESEWL6");
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        assert_eq!(int_kind(&engine, 5), Some(TermKind::DerivedInteger));
        assert_eq!(int_kind(&engine, 7), Some(TermKind::DerivedInteger));
        engine.checkpoint().unwrap();
        commit_one(&engine, &with_int(6));
    }
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(int_kind(&engine, 6), Some(TermKind::DerivedInteger));
    let expected: HashSet<Quad> = [with_int(5), with_int(6), with_int(7)]
        .into_iter()
        .collect();
    assert_eq!(contents(&engine), expected);
}

/// Checkpoints in format 4 (stacks as quad lists) are still read. Such a store keeps
/// integer-derived literals in its dictionary for good: through new checkpoints (format 8
/// records it), new WAL segments (version 4) and restarts.
#[test]
fn format_4_checkpoints_are_read() {
    let dir = tempfile::tempdir().unwrap();
    let iri = |index: u64| (1u64 << 60) | index;
    let integer = {
        let engine = Engine::new(config()).unwrap();
        let mut tx = engine.transaction();
        tx.insert(quad(0).as_ref());
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        snapshot
            .lookup(
                Literal::new_typed_literal("0", xsd::INTEGER)
                    .as_ref()
                    .into(),
            )
            .unwrap()
            .raw()
    };
    let mut file = b"NRESECK4".to_vec();
    file.extend_from_slice(&7u64.to_le_bytes()); // revision
    let keys: [&[u8]; 2] = [b"Ihttp://example.com/s0", b"Ihttp://example.com/p0"];
    file.extend_from_slice(&(keys.len() as u64).to_le_bytes());
    for key in keys {
        file.extend_from_slice(&(key.len() as u32).to_le_bytes());
        file.extend_from_slice(key);
    }
    file.extend_from_slice(&1u64.to_le_bytes()); // one asserted quad
    for component in [iri(0), iri(1), integer, 0] {
        file.extend_from_slice(&component.to_le_bytes());
    }
    file.extend_from_slice(&0u64.to_le_bytes()); // no inferred triples
    let crc = crc32(&file);
    file.extend_from_slice(&crc.to_le_bytes());
    fs::write(dir.path().join("checkpoint-00000000000000000007.nck"), file).unwrap();
    let engine = Engine::open(dir.path(), config()).unwrap();
    let mut expected: HashSet<Quad> = [quad(0)].into_iter().collect();
    assert_eq!(contents(&engine), expected);
    assert_eq!(engine.stats().revision, 7);
    // Integer-derived literals stay dictionary entries in this store.
    commit_one(&engine, &with_int(5));
    expected.insert(with_int(5));
    assert_eq!(int_kind(&engine, 5), Some(TermKind::TypedLiteral));
    assert_eq!(newest_segment_magic(dir.path()), b"NRESEWL4");
    engine.checkpoint().unwrap();
    commit_one(&engine, &with_int(6));
    expected.insert(with_int(6));
    drop(engine);
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(int_kind(&engine, 5), Some(TermKind::TypedLiteral));
    assert_eq!(int_kind(&engine, 6), Some(TermKind::TypedLiteral));
    assert_eq!(contents(&engine), expected);
}

/// A reopened store uses its checkpoint in place: the index data and the dictionary stay
/// in the mapped file (no heap copy), and the store keeps growing on top of them, through
/// the WAL and through further checkpoints.
#[test]
fn checkpoints_are_used_in_place_and_grow() {
    let dir = tempfile::tempdir().unwrap();
    let first = {
        let engine = Engine::open(dir.path(), config()).unwrap();
        let states = commit_all(&engine, &batches(0..20));
        engine.checkpoint().unwrap();
        states[19].clone()
    };
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(contents(&engine), first);
    let stats = engine.stats();
    assert_eq!(stats.index_bytes, 0, "no index data on the heap: {stats:?}");
    assert!(stats.index_mapped_bytes > 0, "{stats:?}");
    assert!(stats.dictionary.mapped_bytes > 0, "{stats:?}");
    // New terms go to the heap part of the dictionary; old ones are still found.
    let mut expected = first.clone();
    let mut tx = engine.transaction();
    for n in 1000..1040 {
        tx.insert(label(n).as_ref());
        tx.insert(quad(n % 20).as_ref());
        expected.insert(label(n));
        expected.insert(quad(n % 20));
    }
    tx.commit().unwrap();
    assert_eq!(contents(&engine), expected);
    drop(engine);
    // Recovery: the mapped checkpoint plus the WAL.
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(contents(&engine), expected);
    // A second checkpoint of mapped and heap terms, then reopened with a full check.
    engine.checkpoint().unwrap();
    drop(engine);
    let verified = EngineConfig {
        durability: DurabilityConfig {
            verify_on_open: true,
            ..config().durability
        },
        ..config()
    };
    let engine = Engine::open(dir.path(), verified).unwrap();
    assert_eq!(contents(&engine), expected);
}

/// With `verify_on_open`, damage anywhere in a checkpoint is refused at open (without, the
/// open reads only the structure, and finds damage only where it reads).
#[test]
fn verify_on_open_refuses_a_damaged_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        commit_all(&engine, &batches(0..20));
        engine.checkpoint().unwrap();
    }
    let checkpoint = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "nck"))
        .unwrap();
    let mut bytes = fs::read(&checkpoint).unwrap();
    let at = bytes.len() * 3 / 4;
    bytes[at] ^= 0x01;
    fs::write(&checkpoint, bytes).unwrap();
    let verified = EngineConfig {
        durability: DurabilityConfig {
            verify_on_open: true,
            ..config().durability
        },
        ..config()
    };
    match Engine::open(dir.path(), verified) {
        Err(EngineError::Corruption(_)) => {}
        other => panic!("expected corruption, got {other:?}"),
    }
}

/// A running engine serves what it has checkpointed from the file, as after a restart: no
/// index data stays on the heap, snapshots taken before keep their data, and the store
/// grows on top and recovers.
#[test]
fn checkpoints_are_served_mapped_while_running() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path(), config()).unwrap();
    let states = commit_all(&engine, &batches(0..20));
    let before = engine.snapshot();
    engine.checkpoint().unwrap();
    let stats = engine.stats();
    assert_eq!(stats.index_bytes, 0, "no index data on the heap: {stats:?}");
    assert!(stats.index_mapped_bytes > 0, "{stats:?}");
    assert!(stats.dictionary.mapped_bytes > 0, "{stats:?}");
    assert_eq!(contents(&engine), states[19]);
    let old: HashSet<Quad> = before
        .quads_for_pattern(&QuadPattern::all())
        .map(|q| before.decode_quad(q).expect("decodable"))
        .collect();
    assert_eq!(old, states[19]);
    drop(before);
    // A checkpoint of an unchanged store writes nothing (its file is in use).
    engine.checkpoint().unwrap();
    let states = commit_all(&engine, &batches(20..40));
    assert_eq!(contents(&engine), states[19]);
    engine.checkpoint().unwrap();
    assert_eq!(engine.stats().index_bytes, 0);
    let states = commit_all(&engine, &batches(40..45));
    drop(engine);
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(contents(&engine), states[4]);
}

/// A durable bulk load installs its checkpoint, mapped; with `map_checkpoints` off the
/// data stays on the heap.
#[test]
fn bulk_loads_are_served_from_their_checkpoint() {
    for map in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let config = EngineConfig {
            durability: DurabilityConfig {
                map_checkpoints: map,
                ..config().durability
            },
            ..config()
        };
        let engine = Engine::open(dir.path(), config).unwrap();
        let data: Vec<Quad> = (0..500).flat_map(|n| [quad(n), label(n)]).collect();
        let load = engine.bulk_load(BulkMode::Append);
        load.add(&data);
        load.finish().unwrap();
        let expected: HashSet<Quad> = data.into_iter().collect();
        assert_eq!(contents(&engine), expected);
        let stats = engine.stats();
        assert_eq!(stats.index_bytes == 0, map, "{stats:?}");
        assert_eq!(stats.dictionary.mapped_bytes > 0, map, "{stats:?}");
        let states = commit_all(&engine, &batches(1000..1010));
        assert_eq!(contents(&engine), states[9]);
        drop(engine);
        let engine = Engine::open(dir.path(), config).unwrap();
        assert_eq!(contents(&engine), states[9]);
    }
}

/// A bulk load past its memory budget spills sorted chunks and merges them: the same
/// contents and counts as one in memory, from several threads with duplicates across
/// batches, nothing left in the directory, and the same after a restart. Appending to a
/// store that has data doesn't spill (its version isn't streamed) and stays correct.
#[test]
fn bulk_loads_past_their_budget_spill_and_merge() {
    let dir = tempfile::tempdir().unwrap();
    let config = EngineConfig {
        durability: DurabilityConfig {
            // 21 quads per chunk.
            bulk_load_memory: Some(2048),
            ..config().durability
        },
        ..config()
    };
    let engine = Engine::open(dir.path(), config).unwrap();
    commit_all(&engine, &batches(0..5));
    let data: Vec<Quad> = (0..900).flat_map(|n| [quad(n), label(n)]).collect();
    let load = engine.bulk_load(BulkMode::Replace);
    std::thread::scope(|scope| {
        for part in data.chunks(250) {
            let load = &load;
            scope.spawn(move || {
                for batch in part.chunks(20) {
                    load.add(batch);
                    // Every quad twice, in other batches.
                    load.add(&batch[..batch.len() / 2]);
                    load.add(&batch[batch.len() / 2..]);
                }
            });
        }
    });
    let summary = load.finish().unwrap();
    let mut expected: HashSet<Quad> = data.iter().cloned().collect();
    assert_eq!(summary.inserted, expected.len() as u64);
    assert_eq!(contents(&engine), expected);
    assert_eq!(engine.stats().index_bytes, 0);
    assert!(!dir.path().join("bulk-spill").exists());
    // Appending to data: in memory.
    let more: Vec<Quad> = (2000..2100).map(label).collect();
    let load = engine.bulk_load(BulkMode::Append);
    for batch in more.chunks(10) {
        load.add(batch);
    }
    let summary = load.finish().unwrap();
    assert_eq!(summary.inserted, 100);
    expected.extend(more);
    assert_eq!(contents(&engine), expected);
    drop(engine);
    let engine = Engine::open(dir.path(), config).unwrap();
    assert_eq!(contents(&engine), expected);
}

/// A store of default-graph quads (bulk-loaded, then committed to, with deletes) turns into
/// a quad store at its first quad in a named graph: its mapped checkpoint and its runs
/// alike, the same contents before and after, and after restarts on either side of a
/// checkpoint.
#[test]
fn default_graph_stores_take_named_graphs_later() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path(), config()).unwrap();
    let data: Vec<Quad> = (0..300).map(label).collect();
    let load = engine.bulk_load(BulkMode::Append);
    load.add(&data);
    load.finish().unwrap();
    let defaults: Vec<(Vec<Quad>, Vec<Quad>)> = (300..320)
        .map(|n| (vec![label(n)], vec![label(n - 300)]))
        .collect();
    let states = commit_all(&engine, &defaults);
    // A quad in a named graph (odd), and more deletes of mapped and committed quads.
    let named = vec![(vec![quad(1), quad(3)], vec![label(50), label(301)])];
    let mut states = [states, commit_all(&engine, &named)].concat();
    assert_eq!(contents(&engine), *states.last().unwrap());
    // Through the WAL alone.
    drop(engine);
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(contents(&engine), *states.last().unwrap());
    // Through a checkpoint of the quad layout, with a named quad deleted after it.
    engine.checkpoint().unwrap();
    states.extend(commit_all(&engine, &[(vec![label(999)], vec![quad(1)])]));
    drop(engine);
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(contents(&engine), *states.last().unwrap());
}

/// Point-in-time restore: an image at one revision plus the archived and the live WAL
/// segments recover any later revision; the log is cut there, new commits continue from it,
/// and a restart sees the same. Revisions before the image, or after the log's end, are
/// refused.
#[test]
fn an_image_and_the_archived_log_restore_any_later_revision() {
    let dir = tempfile::tempdir().unwrap();
    let archiving = EngineConfig {
        durability: DurabilityConfig {
            wal_archive: true,
            wal_segment_bytes: 256,
            ..config().durability
        },
        ..config()
    };
    let engine = Engine::open(dir.path(), archiving).unwrap();
    let mut states = commit_all(&engine, &batches(0..10));
    let images = tempfile::tempdir().unwrap();
    let image = engine.write_image(images.path()).unwrap();
    assert_eq!(image.revision, 10);
    engine.checkpoint().unwrap();
    states.extend(commit_all(&engine, &batches(10..30)));
    engine.checkpoint().unwrap();
    states.extend(commit_all(&engine, &batches(30..35)));
    drop(engine);
    let archive = dir.path().join("wal-archive");
    assert!(
        fs::read_dir(&archive).unwrap().count() > 0,
        "segments archived"
    );

    // A copy: the image, then the archived segments and the live ones.
    let restore = |until: Option<u64>| {
        let target = tempfile::tempdir().unwrap();
        fs::copy(
            &image.path,
            target.path().join(image.path.file_name().unwrap()),
        )
        .unwrap();
        let wal = target.path().join("wal");
        fs::create_dir_all(&wal).unwrap();
        for source in [archive.clone(), dir.path().join("wal")] {
            for entry in fs::read_dir(source).unwrap() {
                let entry = entry.unwrap();
                fs::copy(entry.path(), wal.join(entry.file_name())).unwrap();
            }
        }
        let config = EngineConfig {
            durability: DurabilityConfig {
                recover_until: until,
                ..config().durability
            },
            ..config()
        };
        (Engine::open(target.path(), config), target)
    };
    for until in [10u64, 11, 17, 29, 35] {
        let (engine, target) = restore(Some(until));
        let engine = engine.unwrap();
        assert_eq!(engine.snapshot().revision(), until);
        assert_eq!(
            contents(&engine),
            states[until as usize - 1],
            "revision {until}"
        );
        // New commits continue from there, and a restart sees them.
        let mut next = states[until as usize - 1].clone();
        next.extend(
            commit_all(&engine, &[(vec![label(9000)], vec![])])
                .pop()
                .unwrap(),
        );
        assert_eq!(engine.snapshot().revision(), until + 1);
        drop(engine);
        let engine = Engine::open(target.path(), config()).unwrap();
        assert_eq!(contents(&engine), next, "revision {until}, reopened");
    }
    // Everything: no cut.
    let (engine, _target) = restore(None);
    assert_eq!(contents(&engine.unwrap()), states[34]);
    // Before the image, after the end of the log.
    assert!(restore(Some(9)).0.is_err());
    assert!(restore(Some(36)).0.is_err());
}

/// Point-in-time restore by time: the commits made up to a time, from the log's commit
/// times.
#[test]
fn the_log_restores_the_state_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path(), config()).unwrap();
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64
    };
    let mut states = commit_all(&engine, &batches(0..5));
    std::thread::sleep(std::time::Duration::from_millis(5));
    let middle = now();
    std::thread::sleep(std::time::Duration::from_millis(5));
    states.extend(commit_all(&engine, &batches(5..9)));
    drop(engine);
    let copy = tempfile::tempdir().unwrap();
    let wal = copy.path().join("wal");
    fs::create_dir_all(&wal).unwrap();
    for entry in fs::read_dir(dir.path().join("wal")).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), wal.join(entry.file_name())).unwrap();
    }
    let config = EngineConfig {
        durability: DurabilityConfig {
            recover_until_micros: Some(middle),
            ..config().durability
        },
        ..config()
    };
    let engine = Engine::open(copy.path(), config).unwrap();
    assert_eq!(engine.snapshot().revision(), 5);
    assert_eq!(contents(&engine), states[4]);
}

/// A spill directory left by a crashed load goes when the store opens.
#[test]
fn opening_removes_a_crashed_loads_spill() {
    let dir = tempfile::tempdir().unwrap();
    drop(Engine::open(dir.path(), config()).unwrap());
    let spill = dir.path().join("bulk-spill");
    std::fs::create_dir_all(&spill).unwrap();
    std::fs::write(spill.join("chunk-00000-0.keys"), b"partial").unwrap();
    drop(Engine::open(dir.path(), config()).unwrap());
    assert!(!spill.exists());
}

/// A bulk replacement of a durable store with commits in its WAL: the loaded quads only,
/// served from the checkpoint, and so after a restart; later commits go to the WAL.
#[test]
fn bulk_replacement_is_streamed_into_its_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path(), config()).unwrap();
    commit_all(&engine, &batches(0..30));
    let data: Vec<Quad> = (100..400).flat_map(|n| [quad(n), label(n)]).collect();
    let load = engine.bulk_load(BulkMode::Replace);
    load.add(&data);
    load.finish().unwrap();
    let mut expected: HashSet<Quad> = data.into_iter().collect();
    assert_eq!(contents(&engine), expected);
    assert_eq!(engine.stats().index_bytes, 0);
    let mut tx = engine.transaction();
    tx.insert(label(5000).as_ref());
    tx.commit().unwrap();
    expected.insert(label(5000));
    drop(engine);
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(contents(&engine), expected);
}

/// A checkpoint orders the dictionary's texts (format 7): two equal texts of 256 KiB, a
/// simple literal's and a typed one's, are ordered as any others, and a prefix search finds
/// both after reopening. (The order refined them a level per shared 8 bytes, which
/// overflowed the checkpoint's worker stacks.)
#[test]
fn a_checkpoint_orders_equal_long_texts() {
    use nrese_engine::{Placement, StringTest};
    let dir = tempfile::tempdir().unwrap();
    let text = "abcdefgh".repeat(32 << 10);
    let objects = [
        Literal::new_simple_literal(&text),
        Literal::new_typed_literal(&text, NamedNode::new_unchecked("http://example.com/type")),
    ];
    let engine = Engine::open(dir.path(), config()).unwrap();
    let mut tx = engine.transaction();
    for (i, object) in objects.iter().enumerate() {
        let subject = NamedNode::new_unchecked(format!("http://example.com/s{i}"));
        let predicate = NamedNode::new_unchecked("http://example.com/p");
        tx.insert(Quad::new(subject, predicate, object.clone(), GraphName::DefaultGraph).as_ref());
    }
    tx.commit().unwrap();
    engine.checkpoint().unwrap();
    drop(engine);
    let snapshot = Engine::open(dir.path(), config()).unwrap().snapshot();
    let test = StringTest {
        needle: "abcdefghabcdefgh",
        placement: Placement::Start,
        iris: false,
        strings: true,
        lang_strings: false,
        typed: true,
        language: None,
        ascii_case_insensitive: false,
    };
    assert!(snapshot.text_order_ready());
    assert_eq!(snapshot.matching_strings(&test).len(), 2);
}
