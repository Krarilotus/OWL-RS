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
/// inferred statement over the inserted terms.
fn commit(engine: &Engine, n: u64) {
    let mut tx = engine.transaction();
    let first = quad(n, n >= 5 && n % 2 == 1);
    tx.insert(first.as_ref());
    tx.insert(quad(n + 1000, false).as_ref());
    if n % 4 == 3 {
        tx.remove(quad(n - 2, false).as_ref());
    }
    let snapshot_ids = tx.inserted().next().map(EncodedTriple::from);
    if let Some(triple) = snapshot_ids {
        tx.insert_inferred(triple);
    }
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
            assert_eq!(contents(&replica), contents(&primary), "at {n}");
        }
    }
    assert_eq!(catch_up(&primary, &replica).unwrap(), 20);
    assert_eq!(contents(&replica), contents(&primary));
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
