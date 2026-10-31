//! Read replicas (`Engine::log_since`, `Engine::apply_log`): a replica started from a
//! primary's image and fed its log holds what the primary holds, revision by revision,
//! inferred statements and named graphs included; it survives a restart; a log a
//! checkpoint has covered sends it back to an image; records out of order are refused.

use std::collections::HashSet;

use nrese_engine::{
    DurabilityConfig, EncodedTriple, Engine, EngineConfig, EngineError, QuadPattern, ReadModel,
    SyncPolicy,
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

fn quad(n: u64, named: bool) -> Quad {
    Quad::new(
        NamedNode::new_unchecked(format!("http://example.com/s{}", n % 11)),
        NamedNode::new_unchecked(format!("http://example.com/p{}", n % 3)),
        Literal::new_typed_literal(format!("value {n}"), xsd::STRING),
        if named {
            NamedNode::new_unchecked("http://example.com/g").into()
        } else {
            GraphName::DefaultGraph
        },
    )
}

/// Everything an engine holds, decoded: asserted quads and inferred triples.
fn contents(engine: &Engine) -> (HashSet<Quad>, HashSet<String>) {
    let snapshot = engine.snapshot();
    let asserted = snapshot
        .quads_for_pattern_in(ReadModel::Asserted, &QuadPattern::all())
        .map(|q| snapshot.decode_quad(q).expect("decodable"))
        .collect();
    let inferred = snapshot
        .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
        .map(|q| snapshot.decode_quad(q).expect("decodable").to_string())
        .collect();
    (asserted, inferred)
}

/// Commits a batch: inserts (the first named graph at commit 5), a delete, and an
/// inferred statement over the first insert's terms that nothing asserts: commit `n`
/// leaves `n + 1` inferred statements.
fn commit(engine: &Engine, n: u64) {
    let mut tx = engine.transaction();
    let first = quad(n, n >= 5 && n % 2 == 1);
    tx.insert(first.as_ref());
    tx.insert(quad(n + 1000, false).as_ref());
    if n % 4 == 3 {
        tx.remove(quad(n - 2, false).as_ref());
    }
    let predicate = NamedNode::new_unchecked("http://example.com/inferred");
    let inferred = EncodedTriple {
        subject: tx.intern(first.subject.as_ref().into()),
        predicate: tx.intern(predicate.as_ref().into()),
        object: tx.intern(first.object.as_ref()),
    };
    assert!(tx.insert_inferred(inferred), "commit {n}: inferred");
    tx.commit().expect("commit");
}

/// Feeds the replica the primary's log until it has caught up, in small batches.
fn catch_up(primary: &Engine, replica: &Engine) -> Result<u64, EngineError> {
    loop {
        let at = replica.stats().revision;
        let batch = primary.log_since(at, 300)?;
        if batch.records == 0 {
            return Ok(at);
        }
        let reached = replica.apply_log(&batch.frames)?;
        assert_eq!(reached, batch.last);
    }
}

#[test]
fn a_replica_holds_what_the_primary_holds() {
    let (primary_dir, replica_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let primary = Engine::open(primary_dir.path(), config()).unwrap();
    for n in 0..3 {
        commit(&primary, n);
    }
    // The replica starts from an image of revision 3.
    primary.write_image(replica_dir.path()).unwrap();
    let replica = Engine::open(replica_dir.path(), config()).unwrap();
    assert_eq!(replica.stats().revision, 3);
    for n in 3..20 {
        commit(&primary, n);
        if n % 6 == 0 {
            catch_up(&primary, &replica).unwrap();
            let held = contents(&primary);
            assert_eq!(held.1.len(), n as usize + 1, "inferred at {n}");
            assert_eq!(contents(&replica), held, "at {n}");
        }
    }
    assert_eq!(catch_up(&primary, &replica).unwrap(), 20);
    let held = contents(&primary);
    assert_eq!(held.1.len(), 20, "inferred");
    assert_eq!(contents(&replica), held);
    assert_eq!(
        replica.stats().dictionary.terms,
        primary.stats().dictionary.terms
    );
    // Nothing more to ship.
    let empty = primary.log_since(20, 1 << 20).unwrap();
    assert_eq!((empty.records, empty.last, empty.latest), (0, 20, 20));
    // A batch applied twice changes nothing.
    let again = primary.log_since(10, 1 << 20).unwrap();
    assert_eq!(replica.apply_log(&again.frames).unwrap(), 20);
    assert_eq!(contents(&replica), contents(&primary));

    // The replica keeps what it applied across a restart (its own log).
    let expected = contents(&primary);
    drop(replica);
    let replica = Engine::open(replica_dir.path(), config()).unwrap();
    assert_eq!(replica.stats().revision, 20);
    assert_eq!(contents(&replica), expected);

    // A checkpoint covers the log: a replica behind it starts from an image again.
    commit(&primary, 20);
    primary.checkpoint().unwrap();
    commit(&primary, 21);
    match primary.log_since(20, 1 << 20) {
        Err(EngineError::LogTruncated { oldest }) => assert!(oldest >= 20, "{oldest}"),
        other => panic!("expected a truncated log, got {other:?}"),
    }
    // From the checkpoint's revision on, the log still serves.
    let tail = primary.log_since(21, 1 << 20).unwrap();
    assert_eq!((tail.records, tail.last), (1, 22));
}

#[test]
fn records_out_of_order_are_refused() {
    let (primary_dir, replica_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let primary = Engine::open(primary_dir.path(), config()).unwrap();
    commit(&primary, 0);
    primary.write_image(replica_dir.path()).unwrap();
    let replica = Engine::open(replica_dir.path(), config()).unwrap();
    for n in 1..5 {
        commit(&primary, n);
    }
    // Revisions 3 to 5 without 2: a gap.
    let gap = primary.log_since(2, 1 << 20).unwrap();
    assert!(matches!(
        replica.apply_log(&gap.frames),
        Err(EngineError::Corruption(_))
    ));
    assert_eq!(replica.stats().revision, 1, "nothing applied");
    // Garbage isn't a record.
    assert!(replica.apply_log(b"not a record at all").is_err());
    // In order, it applies.
    assert_eq!(catch_up(&primary, &replica).unwrap(), 5);
    // An in-memory primary has no log to ship.
    let memory = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = memory.transaction();
    tx.insert(quad(1, false).as_ref());
    tx.commit().unwrap();
    assert!(matches!(
        memory.log_since(0, 1024),
        Err(EngineError::LogTruncated { .. })
    ));
}

/// A complete record in the log whose commit isn't published (still syncing, or its sync
/// failed) isn't shipped: the feed stops at the revision published when it was asked.
#[test]
fn the_feed_stops_at_the_published_revision() {
    let (primary_dir, twin_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let primary = Engine::open(primary_dir.path(), config()).unwrap();
    let twin = Engine::open(twin_dir.path(), config()).unwrap();
    for n in 0..2 {
        commit(&primary, n);
        commit(&twin, n);
    }
    // The twin's third record, as the primary's log would hold it before publishing it.
    commit(&twin, 2);
    let unpublished = twin.log_since(2, 1 << 20).unwrap();
    assert_eq!(unpublished.last, 3);
    let wal = primary_dir.path().join("wal");
    let active = std::fs::read_dir(&wal)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .max()
        .unwrap();
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(active)
        .unwrap()
        .write_all(&unpublished.frames)
        .unwrap();
    let batch = primary.log_since(0, 1 << 20).unwrap();
    assert_eq!((batch.records, batch.last, batch.latest), (2, 2, 2));
}
