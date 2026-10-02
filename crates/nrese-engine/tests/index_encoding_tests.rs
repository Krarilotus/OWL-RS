//! `store.index_encoding` (`nrese_engine::IndexEncoding`): checkpoints of blocks without
//! palettes are written as format 9, which binaries before palettes read; with palettes as
//! format 10. Both reopen with the same statements. A process of its own: the encoding is
//! set for the process.

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
fn checkpoints_say_format_10_only_with_palettes() {
    for (encoding, expected) in [
        (IndexEncoding::Fast, b"NRESECK9"),
        (IndexEncoding::Compact, b"NRESECKA"),
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
