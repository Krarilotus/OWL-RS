//! `store.index_encoding` (`nrese_engine::IndexEncoding`): checkpoints are written as format
//! 12 with either encoding and reopen with the same statements; checkpoints of formats 9
//! (no palettes) and 10 (palettes) require export and reload into a new directory. The
//! encoding is the engine's (`EngineConfig::index_encoding`): two engines of one process
//! may differ.

use std::fs;
use std::path::Path;

use nrese_engine::{
    DurabilityConfig, Engine, EngineConfig, EngineError, IndexEncoding, QuadPattern,
};
use nrese_rdf::vocab::xsd;
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};

fn config() -> EngineConfig {
    EngineConfig {
        background_maintenance: false,
        durability: DurabilityConfig::default(),
        ..EngineConfig::default()
    }
}

/// Objects alternating between an IRI and an integer: far apart as ids, few per block, so
/// the compact encoding stores them as palettes.
fn quads() -> Vec<Quad> {
    (0..2_000u64)
        .map(|n| {
            let object: Term = match n % 2 {
                0 => NamedNode::new_unchecked("http://example.com/o").into(),
                _ => Literal::new_typed_literal("5", xsd::INTEGER).into(),
            };
            Quad::new(
                NamedNode::new_unchecked(format!("http://example.com/s{}", n / 2)),
                NamedNode::new_unchecked(format!("http://example.com/p{}", n % 2)),
                object,
                GraphName::DefaultGraph,
            )
        })
        .collect()
}

fn magic(dir: &Path) -> Vec<u8> {
    let checkpoint = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|e| e == "nck"))
        .expect("a checkpoint");
    fs::read(checkpoint).unwrap()[..8].to_vec()
}

/// Two engines of one process, one compact and one fast, each write their own: the same
/// statements, a smaller checkpoint from the compact one (the review's L4: no setting of a
/// store is the process's).
#[test]
fn two_engines_of_one_process_keep_their_own_encodings() {
    let open = |encoding: IndexEncoding, dir: &Path| {
        Engine::open(
            dir,
            EngineConfig {
                index_encoding: encoding,
                ..config()
            },
        )
        .unwrap()
    };
    let (fast, compact) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let fast_engine = open(IndexEncoding::Fast, fast.path());
    let compact_engine = open(IndexEncoding::Compact, compact.path());
    // Write after both engines open: the second engine's setting must not change
    // the first engine's later runs or checkpoint.
    for engine in [&fast_engine, &compact_engine] {
        let mut tx = engine.transaction();
        for quad in quads() {
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        engine.checkpoint().unwrap();
    }
    let bytes = |dir: &Path| {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|e| e == "nck"))
            .map(|path| fs::metadata(path).unwrap().len())
            .sum::<u64>()
    };
    let (fast_bytes, compact_bytes) = (bytes(fast.path()), bytes(compact.path()));
    assert!(
        compact_bytes < fast_bytes,
        "compact {compact_bytes} bytes, fast {fast_bytes}"
    );
    for engine in [&fast_engine, &compact_engine] {
        assert_eq!(
            engine
                .snapshot()
                .quads_for_pattern(&QuadPattern::all())
                .count(),
            2_000
        );
    }
}

#[test]
fn checkpoints_of_either_encoding_are_format_12() {
    for (encoding, expected) in [
        (IndexEncoding::Fast, b"NRESECKC"),
        (IndexEncoding::Compact, b"NRESECKC"),
    ] {
        let config = || EngineConfig {
            index_encoding: encoding,
            ..config()
        };
        let dir = tempfile::tempdir().unwrap();
        {
            let engine = Engine::open(dir.path(), config()).unwrap();
            let mut tx = engine.transaction();
            for quad in quads() {
                tx.insert(quad.as_ref());
            }
            tx.commit().unwrap();
            engine.checkpoint().unwrap();
        }
        assert_eq!(magic(dir.path()), expected, "{encoding:?}");
        let engine = Engine::open(dir.path(), config()).unwrap();
        let count = engine
            .snapshot()
            .quads_for_pattern(&QuadPattern::all())
            .count();
        assert_eq!(count, 2_000, "{encoding:?}");
    }
}

/// Old format 9/10 checkpoint bytes stay unchanged and require export and reload.
#[test]
fn checkpoints_before_format_12_need_reload() {
    for name in ["checkpoint-format-9.nck", "checkpoint-format-10.nck"] {
        let dir = tempfile::tempdir().unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
            dir.path().join("checkpoint-00000000000000000001.nck"),
        )
        .unwrap();
        assert!(
            matches!(
                Engine::open(dir.path(), config()),
                Err(EngineError::UnsupportedFormat(_))
            ),
            "{name}"
        );
    }
}
