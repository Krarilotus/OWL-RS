//! Guard of the bulk load's spill memory (docs/design/performance.md §0): counted with a
//! counting allocator, so this file is a test binary of its own.

use nrese_engine::{BulkMode, DurabilityConfig, Engine, EngineConfig};
use nrese_exec::heap;
use nrese_rdf::{GraphName, NamedNode, Quad};

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

/// A full chunk goes to the spilling thread as its batches, each freed once written, while
/// the next chunk fills.
#[test]
fn a_spilled_chunk_is_written_without_a_copy() {
    const CHUNK: usize = 4 << 20;
    let dir = tempfile::tempdir().unwrap();
    let config = EngineConfig {
        background_maintenance: false,
        durability: DurabilityConfig {
            bulk_load_memory: Some(3 * CHUNK as u64),
            ..DurabilityConfig::default()
        },
        ..EngineConfig::default()
    };
    let engine = Engine::open(dir.path(), config).unwrap();
    let node = |name: String| NamedNode::new_unchecked(name);
    let batch: Vec<Quad> = (0..4096)
        .map(|i| {
            Quad::new(
                node(format!("urn:s{}", i % 64)),
                node("urn:p".to_owned()),
                node(format!("urn:o{}", i / 64)),
                GraphName::DefaultGraph,
            )
        })
        .collect();
    let load = engine.bulk_load(BulkMode::Replace);
    heap::start("bulk load");
    let before = heap::live();
    // Four chunks of 32-byte quads.
    for _ in 0..4 * CHUNK / 32 / batch.len() {
        load.add(&batch);
    }
    let peak = heap::peak() - before;
    let _ = heap::finish();
    load.finish().unwrap();
    // 5,953,497 bytes on 11 Oct 2026; copying the chunk being written made it 9,099,197.
    assert!(peak < CHUNK * 7 / 4, "{peak} bytes live");
}
