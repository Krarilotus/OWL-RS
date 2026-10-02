//! `store.index_encoding` (`nrese_engine::IndexEncoding`): checkpoints are written as format
//! 11 with either encoding and reopen with the same statements; checkpoints of formats 9
//! (no palettes) and 10 (palettes), written by the engine before format 11, still open. A
//! process of its own: the encoding is set for the process.

use std::fs;
use std::path::Path;

use nrese_engine::{
    DurabilityConfig, Engine, EngineConfig, IndexEncoding, QuadPattern, set_index_encoding,
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

#[test]
fn checkpoints_of_either_encoding_are_format_11() {
    for (encoding, expected) in [
        (IndexEncoding::Fast, b"NRESECKB"),
        (IndexEncoding::Compact, b"NRESECKB"),
    ] {
        set_index_encoding(encoding);
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

/// Checkpoints the engine wrote before format 11 (the statements of [`quads`]; format 9
/// without palettes, format 10 with) open, and their next checkpoint is format 11.
#[test]
fn checkpoints_before_format_11_open() {
    for name in ["checkpoint-format-9.nck", "checkpoint-format-10.nck"] {
        let dir = tempfile::tempdir().unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
            dir.path().join("checkpoint-00000000000000000001.nck"),
        )
        .unwrap();
        {
            let engine = Engine::open(dir.path(), config()).unwrap();
            let snapshot = engine.snapshot();
            assert_eq!(
                snapshot.quads_for_pattern(&QuadPattern::all()).count(),
                2_000,
                "{name}"
            );
            for quad in quads() {
                assert!(
                    snapshot.lookup_quad(quad.as_ref()).is_some_and(|q| snapshot.contains(&q)),
                    "{name}: {quad}"
                );
            }
            let mut tx = engine.transaction();
            tx.insert(
                Quad::new(
                    NamedNode::new_unchecked("http://example.com/new"),
                    NamedNode::new_unchecked("http://example.com/p0"),
                    NamedNode::new_unchecked("http://example.com/o"),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            );
            tx.commit().unwrap();
            engine.checkpoint().unwrap();
        }
        let checkpoints: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|e| e == "nck"))
            .collect();
        let newest = checkpoints.iter().max().unwrap();
        assert_eq!(&fs::read(newest).unwrap()[..8], b"NRESECKB", "{name}");
        let engine = Engine::open(dir.path(), config()).unwrap();
        assert_eq!(
            engine.snapshot().quads_for_pattern(&QuadPattern::all()).count(),
            2_001,
            "{name}"
        );
    }
}
