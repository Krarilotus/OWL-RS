//! E4 gate: recovery to the last acknowledged revision after crashes at every write step.
//!
//! Crashes are simulated at the file level: the engine is dropped (as a killed process
//! would leave its files) and the files are then cut or damaged the way a crash would.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use nrese_engine::{
    BulkMode, DurabilityConfig, Engine, EngineConfig, EngineError, QuadPattern, SyncPolicy,
    TermKind, WalDamage,
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
        let engine = Engine::open(dir.path(), small_segments.clone()).unwrap();
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

/// Where each frame of a WAL segment starts, after its 8-byte magic.
fn frame_starts(bytes: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut at = 8;
    while at + 8 <= bytes.len() {
        starts.push(at);
        at += 8 + u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    }
    starts
}

/// Four acknowledged commits, one record each, in the store's only segment.
fn four_records() -> (tempfile::TempDir, Vec<HashSet<Quad>>, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let states = {
        let engine = Engine::open(dir.path(), config()).unwrap();
        commit_all(&engine, &batches(0..4))
    };
    let bytes = fs::read(&segments(dir.path())[0]).unwrap();
    assert_eq!(frame_starts(&bytes).len(), 4);
    (dir, states, bytes)
}

/// Damage in the newest segment that a crash mid-append doesn't leave (owner, 26 Oct:
/// refused by default) fails the open, and leaves the log as it was: a flipped byte in a
/// record with records after it (B-302: they were cut with it), a garbage header (its
/// length runs past the end), zeros over a frame boundary with an intact record after
/// them, and a flipped byte in the last record, which is complete. Each row is judged on
/// its own.
#[test]
fn damage_in_the_last_segment_is_refused_and_left_in_place() {
    let (source, _, original) = four_records();
    let starts = frame_starts(&original);
    let (second, third, last) = (starts[1], starts[2], starts[3]);
    type Damage = Box<dyn Fn(&mut Vec<u8>)>;
    let damages: [(&str, Damage); 4] = [
        (
            "a flipped byte in record 2 of 4",
            Box::new(move |bytes: &mut Vec<u8>| bytes[second + 20] ^= 0x10),
        ),
        (
            "a garbage header over record 2",
            Box::new(move |bytes: &mut Vec<u8>| bytes[second..second + 8].fill(0xff)),
        ),
        (
            "zeros from record 2 into record 3",
            Box::new(move |bytes: &mut Vec<u8>| bytes[second + 10..third + 12].fill(0)),
        ),
        (
            "a flipped byte in the last record",
            Box::new(move |bytes: &mut Vec<u8>| bytes[last + 20] ^= 0x10),
        ),
    ];
    let mut failures = Vec::new();
    for (case, damage) in damages {
        let dir = tempfile::tempdir().unwrap();
        copy_dir(source.path(), dir.path());
        let segment = &segments(dir.path())[0];
        let mut bytes = original.clone();
        damage(&mut bytes);
        fs::write(segment, &bytes).unwrap();
        let mut wrong = None;
        for attempt in 1..=2 {
            match Engine::open(dir.path(), config()) {
                Err(EngineError::Corruption(_)) => {}
                Ok(engine) => {
                    wrong = Some(format!(
                        "opened at revision {} (attempt {attempt})",
                        engine.stats().revision
                    ))
                }
                Err(other) => wrong = Some(format!("{other} (attempt {attempt})")),
            }
            if wrong.is_none() && fs::read(segment).unwrap() != bytes {
                wrong = Some(format!("the log changed (attempt {attempt})"));
            }
            if wrong.is_some() {
                break;
            }
        }
        eprintln!(
            "row {case}: {}",
            if wrong.is_some() { "FAILED" } else { "ok" }
        );
        failures.extend(wrong.map(|w| format!("{case}: {w}")));
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Guard (right on the base too): a size extension that never got its data (zeros to the
/// end of the file) is a torn tail, so every record recovers.
#[test]
fn a_zero_filled_tail_is_torn() {
    let (dir, states, mut bytes) = four_records();
    bytes.extend_from_slice(&[0; 300]);
    fs::write(&segments(dir.path())[0], &bytes).unwrap();
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(contents(&engine), states[3]);
}

/// What opening cuts from a torn tail is kept, byte for byte, in `wal-cut/`: a zero-filled
/// extension and a last record cut short alike.
#[test]
fn a_cut_torn_tail_is_kept() {
    let (source, _, original) = four_records();
    let last = frame_starts(&original)[3];
    let mut zeros = original.clone();
    zeros.extend_from_slice(&[0; 300]);
    let tails = [
        ("zeros to the end", zeros, original.len()),
        (
            "the last record cut short",
            original[..last + 20].to_vec(),
            last,
        ),
    ];
    let mut failures = Vec::new();
    for (case, bytes, at) in tails {
        let dir = tempfile::tempdir().unwrap();
        copy_dir(source.path(), dir.path());
        let segment = segments(dir.path())[0].clone();
        fs::write(&segment, &bytes).unwrap();
        drop(Engine::open(dir.path(), config()).unwrap());
        let kept = fs::read(cut_copy(dir.path(), &segment, at)).ok();
        let ok = kept.as_deref() == Some(&bytes[at..]);
        eprintln!("row {case}: {}", if ok { "ok" } else { "FAILED" });
        if !ok {
            failures.push(format!("{case}: kept {:?} bytes", kept.map(|k| k.len())));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Where opening keeps the bytes it cut from `segment` at `offset`.
fn cut_copy(dir: &Path, segment: &Path, offset: usize) -> PathBuf {
    let name = segment.file_name().unwrap().to_string_lossy();
    dir.join("wal-cut").join(format!("{name}.{offset}"))
}

/// With the prefix kept (`WalDamage::KeepPrefix`, opted into), damage in record 2 of 4
/// opens the store at revision 1. Records 2 to 4 are kept byte for byte in the copy, and
/// the store takes commits again, across a restart.
#[test]
fn keeping_the_prefix_opens_up_to_the_damage_and_keeps_the_rest() {
    let (dir, states, mut bytes) = four_records();
    let second = frame_starts(&bytes)[1];
    bytes[second + 20] ^= 0x10;
    let segment = segments(dir.path())[0].clone();
    fs::write(&segment, &bytes).unwrap();
    let keep = EngineConfig {
        durability: DurabilityConfig {
            wal_damage: WalDamage::KeepPrefix,
            ..config().durability
        },
        ..config()
    };
    let engine = Engine::open(dir.path(), keep.clone()).unwrap();
    assert_eq!(engine.stats().revision, 1);
    assert_eq!(contents(&engine), states[0]);
    assert_eq!(
        fs::read(cut_copy(dir.path(), &segment, second)).unwrap(),
        bytes[second..]
    );
    let again = commit_all(&engine, &batches(1..2));
    drop(engine);
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(contents(&engine), again[0]);
}

/// A later cut at the same offset never replaces an earlier copy (sol-loop r1 S2): after a
/// prefix kept at record 2, the next commit is written where record 2 was, and a crash
/// tears it. Cutting that torn tail keeps it under another name; the copy of the damaged
/// records 2 to 4, possibly the only one of acknowledged commits, stays as it was.
#[test]
fn a_later_cut_at_the_same_offset_keeps_the_earlier_copy() {
    let (dir, _, mut bytes) = four_records();
    let second = frame_starts(&bytes)[1];
    bytes[second + 20] ^= 0x10;
    let segment = segments(dir.path())[0].clone();
    fs::write(&segment, &bytes).unwrap();
    let keep = EngineConfig {
        durability: DurabilityConfig {
            wal_damage: WalDamage::KeepPrefix,
            ..config().durability
        },
        ..config()
    };
    let engine = Engine::open(dir.path(), keep).unwrap();
    commit_all(&engine, &batches(1..2));
    drop(engine);
    fs::OpenOptions::new()
        .write(true)
        .open(&segment)
        .unwrap()
        .set_len(second as u64 + 20)
        .unwrap();
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(engine.stats().revision, 1);
    assert_eq!(
        fs::read(cut_copy(dir.path(), &segment, second)).unwrap(),
        bytes[second..],
        "the earlier copy as it was"
    );
    assert_eq!(fs::read_dir(dir.path().join("wal-cut")).unwrap().count(), 2);
}

/// A segment's name is a lower bound of its records only: bulk loads advance the revision
/// by checkpoints alone, and an empty active segment keeps its name (sol-loop r1 S1). Here
/// 30 replacements leave the store at revision 30 with its first segment, named 1, empty;
/// two small commits (31 and 32, one quad of known terms each) follow in it, far more
/// revisions past the name than frames fit. A garbage length over record 31 still finds
/// record 32 intact after it: the open refuses, the log as it was.
#[test]
fn damage_is_found_in_a_segment_named_far_below_its_records() {
    let node = |n: &str| NamedNode::new_unchecked(format!("http://example.com/{n}"));
    let triple = |o: u64| {
        Quad::new(
            node("s"),
            node("p"),
            node(&format!("o{o}")),
            GraphName::DefaultGraph,
        )
    };
    let dir = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        for n in 0..30 {
            let load = engine.bulk_load(BulkMode::Replace);
            load.add(&[triple(n)]);
            load.finish().unwrap();
        }
        assert_eq!(engine.stats().revision, 30);
        for o in [0, 1] {
            let mut tx = engine.transaction();
            tx.insert(triple(o).as_ref());
            tx.commit().unwrap();
        }
    }
    let segment = segments(dir.path())[0].clone();
    let mut bytes = fs::read(&segment).unwrap();
    let starts = frame_starts(&bytes);
    assert_eq!(starts.len(), 2, "both commits in the first segment");
    bytes[starts[0]..starts[0] + 4].fill(0xff);
    fs::write(&segment, &bytes).unwrap();
    match Engine::open(dir.path(), config()) {
        Err(EngineError::Corruption(_)) => {}
        Ok(engine) => panic!("opened at revision {}", engine.stats().revision),
        Err(other) => panic!("{other}"),
    }
    assert_eq!(fs::read(&segment).unwrap(), bytes, "the log as it was");
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
        let engine = Engine::open(dir.path(), small_segments.clone()).unwrap();
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

/// Damage in a segment the checkpoint wholly covers, one a failed release left behind,
/// neither refuses the open nor costs a record after it (F7): its records are all in the
/// checkpoint. Here records 1 to 10 are in a leftover segment, the checkpoint is at 10, and
/// records 11 and 12 are in the next segment; the leftover's middle is garbled. Before the fix
/// the open refused it as damage outside the last segment, and the repair that refusal named
/// (`recover_until`) cut away the later segments.
#[test]
fn damage_in_a_covered_leftover_segment_costs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let saved_wal = tempfile::tempdir().unwrap();
    let states = {
        let engine = Engine::open(dir.path(), config()).unwrap();
        let mut states = commit_all(&engine, &batches(0..10));
        copy_dir(&dir.path().join("wal"), saved_wal.path());
        engine.checkpoint().unwrap();
        states.extend(commit_all(&engine, &batches(10..12)));
        states
    };
    // The covered segment back, as if its release had failed, garbled in the middle.
    for entry in fs::read_dir(saved_wal.path()).unwrap() {
        let path = entry.unwrap().path();
        let mut bytes = fs::read(&path).unwrap();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0xff;
        fs::write(
            dir.path().join("wal").join(path.file_name().unwrap()),
            bytes,
        )
        .unwrap();
    }
    assert!(segments(dir.path()).len() >= 2);
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(engine.stats().revision, 12);
    assert_eq!(contents(&engine), states[11]);
}

/// A store closed and reopened while another thread of the process starts a child process
/// opens again (open row IB1312-8ba4, the store lock's). On Linux the child is forked holding a
/// copy of every open descriptor, the store's `LOCK` among them, until it execs; here it is held
/// in that window (a `pre_exec` hook that marks it is there and sleeps 3 s) while the store
/// is dropped and reopened. A lock held by the descriptor's open file description (`flock`)
/// is then still held by the child, and the reopen fails with `Locked`. On Windows the same
/// sequence with a child that just lives a while (handles are not inherited unless marked).
#[test]
fn a_reopen_is_not_locked_out_by_a_child_process() {
    let dir = tempfile::tempdir().unwrap();
    let marks = tempfile::tempdir().unwrap();
    let mark = marks.path().join("forked");
    let engine = Engine::open(dir.path(), config()).unwrap();
    commit_all(&engine, &batches(0..1));
    let spawner = {
        let marker = fs::File::create(&mark).unwrap();
        std::thread::spawn(move || child_in_its_window(marker))
    };
    // The child exists, holding what it inherited, before the store is dropped.
    let started = std::time::Instant::now();
    while fs::metadata(&mark).unwrap().len() == 0 {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "the child never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    drop(engine);
    let reopened = Engine::open(dir.path(), config());
    let ended = spawner.join().unwrap();
    assert!(
        reopened.is_ok(),
        "reopened while a child process was starting: {:?}",
        reopened.err()
    );
    assert!(ended, "the child ran");
}

/// Guard (right on the base too): a store an earlier binary has open is refused. An earlier
/// binary holds `flock` on `LOCK` for as long as it runs; here a second open file description
/// in this process holds it (`flock` locks of two descriptions conflict, in one process too).
/// Released, the store opens.
#[test]
fn a_store_an_earlier_binary_has_open_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    drop(Engine::open(dir.path(), config()).unwrap());
    let earlier = fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join("LOCK"))
        .unwrap();
    earlier.try_lock().unwrap();
    let refused = Engine::open(dir.path(), config());
    assert!(
        matches!(refused, Err(EngineError::Locked(_))),
        "opened beside an earlier binary: {:?}",
        refused.err()
    );
    earlier.unlock().unwrap();
    drop(earlier);
    Engine::open(dir.path(), config()).unwrap();
}

/// The variable that makes [`a_second_process_opens`] act: the store it opens.
const SECOND_OPEN: &str = "NRESE_TEST_SECOND_OPEN";

/// Run in a second process of this test binary ([`second_process_open`]): opens the store
/// named by [`SECOND_OPEN`] and exits 3 if it is `Locked`, 0 if it opens holding `quad(0)`, 4
/// otherwise. In a test run (no variable) it does nothing.
#[test]
fn a_second_process_opens() {
    let Ok(dir) = std::env::var(SECOND_OPEN) else {
        return;
    };
    let code = match Engine::open(Path::new(&dir), config()) {
        Err(EngineError::Locked(_)) => 3,
        Ok(engine) if contents(&engine).contains(&quad(0)) => 0,
        other => {
            eprintln!(
                "the second open: {:?}",
                other.map(|engine| contents(&engine))
            );
            4
        }
    };
    std::process::exit(code);
}

/// What a second process of this test binary gets opening the store in `dir`: 3 `Locked`,
/// 0 opened with `quad(0)` ([`a_second_process_opens`]).
fn second_process_open(dir: &Path) -> Option<i32> {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "durability_tests::a_second_process_opens",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(SECOND_OPEN, dir)
        .status()
        .unwrap()
        .code()
}

/// A store stays locked against another process whatever else this process does with its
/// lock file (review r1, R1-F1): here `LOCK` is read, so opened and closed, while the store is
/// open. Before the fix the store's lock was a record lock, which any close of the file by the
/// process drops: a second process opened the store too, and both could commit the same
/// revision, one of them lost at the next reopen. Once the first engine is dropped, the second
/// process opens and recovers its commit. Windows: a guard (a handle's lock is its own).
#[test]
fn a_store_stays_locked_when_its_lock_file_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path(), config()).unwrap();
    commit_all(&engine, &[(vec![quad(0)], Vec::new())]);
    // Opened and closed; read where the platform lets it (Windows refuses a read of the
    // locked range, which leaves the open and the close).
    {
        use std::io::Read as _;
        let mut lock = fs::File::open(dir.path().join("LOCK")).unwrap();
        let _ = lock.read_to_end(&mut Vec::new());
    }
    assert_eq!(
        second_process_open(dir.path()),
        Some(3),
        "a second process opened the store beside a live engine"
    );
    drop(engine);
    assert_eq!(
        second_process_open(dir.path()),
        Some(0),
        "the released store didn't open in a second process with its commit"
    );
}

/// Starts a child that marks `marker` once it exists and holds the process's descriptors
/// for 3 s before it runs (Linux: in its fork-to-exec window); whether it ran.
#[cfg(unix)]
fn child_in_its_window(marker: fs::File) -> bool {
    use std::io::Write as _;
    use std::os::unix::process::CommandExt as _;
    let mut command = std::process::Command::new("true");
    // SAFETY: the hook runs in the forked child before exec and only writes to an already
    // open file and sleeps (system calls, no allocation and no lock), which is safe there.
    unsafe {
        command.pre_exec(move || {
            (&marker).write_all(b"x")?;
            std::thread::sleep(std::time::Duration::from_secs(3));
            Ok(())
        });
    }
    command.status().is_ok_and(|status| status.success())
}

/// Windows: a child that lives 3 s, started while the store is open.
#[cfg(windows)]
fn child_in_its_window(marker: fs::File) -> bool {
    use std::io::Write as _;
    let mut child = std::process::Command::new("cmd")
        .args(["/C", "ping -n 4 127.0.0.1 > NUL"])
        .spawn()
        .unwrap();
    (&marker).write_all(b"x").unwrap();
    child.wait().is_ok_and(|status| status.success())
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
            wal_damage: WalDamage::Refuse,
        },
        ..EngineConfig::default()
    };
    let states = {
        let engine = Engine::open(dir.path(), config.clone()).unwrap();
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
    let engine = Engine::open(dir.path(), config.clone()).unwrap();
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
        let engine = Engine::open(dir.path(), config.clone()).unwrap();
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
    let engine = Engine::open(dir.path(), config.clone()).unwrap();
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

/// A reopen gives a new asserted identity (P2-C1, DUR's condition): it is never persisted,
/// so a cache keyed by it can't take one process's state for another's.
#[test]
fn a_reopen_gives_a_new_asserted_identity() {
    let dir = tempfile::tempdir().unwrap();
    let before = {
        let engine = Engine::open(dir.path(), config()).unwrap();
        commit_all(&engine, &batches(0..2));
        engine.snapshot().asserted_identity()
    };
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_ne!(engine.snapshot().asserted_identity(), before);
}

/// A change a transaction makes, for a table of rows.
type Change = dyn Fn(&mut nrese_engine::Transaction<'_>);

/// `Transaction::asserted_identity_after` names what the commit publishes (P2-C1, DUR's
/// conditions of 8 Nov): a new id for a commit that changes asserted statements, the
/// base's for an inferred-only one, an empty one, and one inserting and then removing the
/// same statement (no net change); the same on every call; equal to the commit's summary
/// and to the published snapshot's. In memory and durable.
#[test]
fn the_asserted_identity_after_a_transaction_is_what_its_commit_publishes() {
    let dir = tempfile::tempdir().unwrap();
    let engines = [
        ("in memory", Engine::new(config()).unwrap()),
        ("durable", Engine::open(dir.path(), config()).unwrap()),
    ];
    let mut wrong = Vec::new();
    for (kind, engine) in &engines {
        let rows: [(&str, &Change, bool); 4] = [
            (
                "asserted",
                &|tx| {
                    tx.insert(quad(1).as_ref());
                },
                true,
            ),
            (
                "inferred only",
                &|tx| {
                    let [s, p, o] = ["s", "p", "o"].map(|n| {
                        tx.intern(
                            NamedNode::new_unchecked(format!("http://example.com/{n}"))
                                .as_ref()
                                .into(),
                        )
                    });
                    tx.insert_inferred(nrese_engine::EncodedTriple::new(s, p, o));
                },
                false,
            ),
            ("empty", &|_| {}, false),
            (
                "inserted, then removed",
                &|tx| {
                    tx.insert(quad(2).as_ref());
                    tx.remove(quad(2).as_ref());
                },
                false,
            ),
        ];
        for (name, change, new) in rows {
            let base = engine.snapshot().asserted_identity();
            let mut tx = engine.transaction();
            change(&mut tx);
            let after = tx.asserted_identity_after();
            let again = tx.asserted_identity_after();
            let summary = tx.commit().unwrap();
            let published = engine.snapshot().asserted_identity();
            let row = format!("{kind}, {name}");
            if after != again {
                wrong.push(format!("{row}: {after:?}, then {again:?}"));
            }
            if (after != base) != new {
                wrong.push(format!("{row}: new {}, expected {new}", after != base));
            }
            if summary.asserted != after || published != after {
                wrong.push(format!(
                    "{row}: said {after:?}, summary {:?}, published {published:?}",
                    summary.asserted
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
