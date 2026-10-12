//! Guard of the bulk load's spill memory (docs/design/performance.md §0): counted with a
//! counting allocator, so this file is a test binary of its own.

use nrese_engine::{BulkMode, DurabilityConfig, Engine, EngineConfig};
use nrese_exec::heap;
use nrese_rdf::{GraphName, NamedNode, Quad};

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

/// A full chunk goes to the spilling thread as its batches, each freed once written, while
/// the next chunk fills. Two properties, apart:
/// - **Written without a copy:** the spilling thread holds no more than its I/O buffer
///   beyond the chunk it was handed while it writes one, counted for that thread alone
///   ([`heap::Step`]), so however far it falls behind the loader.
/// - **Within the budget:** the load holds at most `bulk_load_memory`. A chunk filling, one
///   being written and, where the spilling thread falls behind, the next waiting for it;
///   the budget allows a third chunk (its sorted copy, at the end).
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
    load.finish().unwrap();
    let written = heap::steps();
    let _ = heap::finish();
    let written: Vec<usize> = written
        .iter()
        .filter(|step| step.label == "spill: a chunk written")
        .map(|step| step.peak)
        .collect();
    assert!(
        written.len() >= 3,
        "{written:?}: the chunks weren't spilled"
    );
    // 1,048,692 bytes each on 13 Oct 2026 (the 1 MiB I/O buffer), also with the spilling
    // thread held back 400 ms a chunk; concatenating the chunk to write it, 5,243,020,
    // while the load's peak stayed within its budget (10,945,526).
    for &held in &written {
        assert!(
            held < CHUNK / 2,
            "a chunk written with {held} bytes held: {written:?}"
        );
    }
    // 5,953,886 bytes on 13 Oct 2026 with the spilling thread keeping up; 9,452,478 held
    // back 400 ms a chunk (9,452,410 under three builds' load on 12 Oct).
    assert!(
        peak < 3 * CHUNK,
        "{peak} bytes live, past the load's budget"
    );
}
