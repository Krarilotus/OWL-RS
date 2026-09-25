//! E4 gate: recovery to the last acknowledged revision after crashes at every write step.
//!
//! Crashes are simulated at the file level: the engine is dropped (as a killed process
//! would leave its files) and the files are then cut or damaged the way a crash would.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use nrese_engine::{DurabilityConfig, Engine, EngineConfig, EngineError, QuadPattern, SyncPolicy};
use oxrdf::vocab::xsd;
use oxrdf::{GraphName, Literal, NamedNode, Quad};

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
