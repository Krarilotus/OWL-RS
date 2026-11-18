//! Guards of a bulk load's memory while it interns its input (docs/design/performance.md
//! §0): counted with a counting allocator, so this file is a test binary of its own. The
//! tests run one at a time: the allocator's counts are the whole process's.

use std::sync::Mutex;

use nrese_engine::{BulkMode, Engine, EngineConfig};
use nrese_exec::heap;
use nrese_rdf::{GraphName, NamedNode, Quad};

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn node(name: String) -> NamedNode {
    NamedNode::new_unchecked(name)
}

/// `quads` quads over `quads / 64 + 64` subjects and objects named from `from` on.
fn batch(from: usize, quads: usize) -> Vec<Quad> {
    (0..quads)
        .map(|i| {
            Quad::new(
                node(format!("urn:s{}", from + i % 64)),
                node("urn:p".to_owned()),
                node(format!("urn:o{}", from + i / 64)),
                GraphName::DefaultGraph,
            )
        })
        .collect()
}

/// A batch is interned in the arrays the load keeps for its next batches: on the thread
/// adding it, the batch holds its encoded quads (32 bytes each) and nothing per batch
/// beyond, counted for that thread alone ([`heap::Step`]). Allocated afresh per batch, on
/// every thread of a load at once, those arrays (the keys' arena, their table, the slots,
/// the lookups) kept DBpedia core's peak 0.4 GB above main's (18 Oct 2026).
#[test]
fn a_batch_is_interned_in_the_arrays_kept_for_the_next() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|p| p.into_inner());
    const QUADS: usize = 4096;
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let batch = batch(0, QUADS);
    let load = engine.bulk_load(BulkMode::Replace);
    heap::start("bulk load");
    // The first batch makes the arrays and interns the terms; the next find them.
    for _ in 0..8 {
        heap::begin_step();
        load.add(&batch);
        heap::end_step("bulk load: a batch interned");
    }
    let steps = heap::steps();
    let _ = heap::finish();
    load.finish().unwrap();
    let held: Vec<usize> = steps.iter().skip(1).map(|step| step.peak).collect();
    // 131,072 bytes a batch on 18 Oct 2026 (its encoded quads), 828,440 with the arrays
    // allocated per batch.
    for &bytes in &held {
        assert!(
            bytes < 2 * QUADS * 32,
            "a batch held {bytes} bytes beyond its {} bytes of quads: {held:?}",
            QUADS * 32
        );
    }
}

/// The terms new to the dictionary wait in the load's shards with their text, end offset,
/// first position and table entry, not a hash each: the tables grow by hashing the keys
/// again. A hash kept per new term was 8 bytes of the load's peak (0.3 GB of DBpedia
/// core's 40.5 million terms, 18 Oct 2026).
#[test]
fn a_new_term_waits_without_its_hash() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|p| p.into_inner());
    const BATCHES: usize = 16;
    const QUADS: usize = 4096;
    let engine = Engine::new(EngineConfig::default()).unwrap();
    // Two new terms a quad: 131,072 in all (about 512 a shard), besides the predicate.
    let batches: Vec<Vec<Quad>> = (0..BATCHES)
        .map(|b| {
            (b * QUADS..(b + 1) * QUADS)
                .map(|i| {
                    Quad::new(
                        node(format!("urn:s{i}")),
                        node("urn:p".to_owned()),
                        node(format!("urn:o{i}")),
                        GraphName::DefaultGraph,
                    )
                })
                .collect()
        })
        .collect();
    // The second half of the batches: what they add to the shards, the arrays the load
    // keeps for its batches made by then.
    let terms = BATCHES / 2 * QUADS * 2;
    // Each key: a tag byte and the IRI.
    let text: usize = (BATCHES / 2 * QUADS..BATCHES * QUADS)
        .map(|i| 2 * (1 + format!("urn:s{i}").len()))
        .sum();
    let load = engine.bulk_load(BulkMode::Replace);
    heap::start("bulk load");
    for batch in &batches[..BATCHES / 2] {
        load.add(batch);
    }
    let before = heap::live();
    for batch in &batches[BATCHES / 2..] {
        load.add(batch);
    }
    // The encoded quads aside: what the shards hold.
    let pending = heap::live() - before - BATCHES / 2 * QUADS * 32;
    let _ = heap::finish();
    load.finish().unwrap();
    let per_term = (pending - text) as f64 / terms as f64;
    // 34.0 bytes a term beyond its text on 18 Oct 2026 (its end offset, first position,
    // table entry and what the arrays grew by), 45.6 with its hash kept.
    assert!(
        per_term < 40.0,
        "{per_term:.1} bytes a pending term beyond its text ({pending} for {terms} terms, {text} of text)"
    );
}

/// A first load appended to an empty store builds its index from the quads it holds, as a
/// replacing load does: filtered against the empty store, they were copied, a second array
/// of 32 bytes a quad while the index was built (DBpedia core's peak 2 GB higher; fleet
/// pm-ib1801-1, 21 Oct 2026).
#[test]
fn a_first_appended_load_builds_its_index_from_the_quads_it_holds() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|p| p.into_inner());
    const QUADS: usize = 16 * 4096;
    // What the index build holds at its peak, beyond the sorted quads.
    let build = |mode: BulkMode| {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let load = engine.bulk_load(mode);
        for b in 0..16 {
            load.add(&batch(b * 4096, 4096));
        }
        heap::start("bulk load: finish");
        load.finish().unwrap();
        let phases = heap::finish();
        assert_eq!(engine.snapshot().len(), QUADS as u64);
        let phase = |label: &str| {
            *phases
                .iter()
                .find(|phase| phase.label == label)
                .unwrap_or_else(|| panic!("no phase {label}: {phases:?}"))
        };
        phase("load: permutations").peak - phase("load: sorting").live_after
    };
    let (replaced, appended) = (build(BulkMode::Replace), build(BulkMode::Append));
    assert!(
        appended < replaced + QUADS * 32 / 2,
        "an appended load's index build held {appended} bytes, a replacing one's {replaced}"
    );
}
