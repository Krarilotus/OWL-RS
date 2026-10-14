//! Guard of the inferred stack built from a rule engine's runs (docs/design/performance.md
//! §0): counted with a counting allocator, so this file is a test binary of its own.

use std::sync::Arc;

use nrese_engine::{Engine, EngineConfig, PairChunks, PredicateRuns, ReadModel, TermId, TermKind};
use nrese_exec::heap;

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

fn iri(payload: u64) -> u64 {
    ((TermKind::Iri as u64) << 60) | payload
}

/// `pairs` pseudo-random `(subject, object)` pairs over the numbers `1..=nodes`, sorted
/// and distinct, packed into words, in chunks of `chunk` (as a reasoner's working set
/// holds them).
fn run(seed: u64, pairs: usize, nodes: u64, chunk: usize) -> Vec<Vec<u64>> {
    let mut x = seed;
    let mut next = move || {
        x = x
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        1 + (x >> 33) % nodes
    };
    let mut all: Vec<u64> = (0..pairs).map(|_| (next() << 32) | next()).collect();
    all.sort_unstable();
    all.dedup();
    all.chunks(chunk).map(<[_]>::to_vec).collect()
}

/// The inferred stack is merged from the runs a reasoner hands over (numbered pairs, a
/// word each, and the table of their ids), a predicate at a time and a wave of keys at a
/// time: the runs, the table and the build together hold the packed permutations, one
/// relation's object order and a wave, never a list of the statements (24 bytes each),
/// their quads (32) or their keys (32), as the build from a list did.
#[test]
fn the_inferred_stack_is_merged_from_runs_without_a_list_of_statements() {
    let engine = Engine::new(EngineConfig {
        inferred_merge_wave_keys: 1 << 14,
        ..EngineConfig::default()
    })
    .unwrap();
    // Two relations, one with its object order and one without, each in two runs (the
    // reasoner's base and recent ones).
    heap::start("inferred stack");
    let start = heap::live();
    const NODES: u64 = 400_000;
    // Number n stands for IRI n, number 0 for none.
    let ids: Arc<[u64]> = (0..=NODES).map(iri).collect::<Vec<_>>().into();
    let numbered = |chunks| PairChunks::Numbers {
        chunks,
        ids: Arc::clone(&ids),
    };
    let relations: Vec<PredicateRuns> = (0..2u64)
        .map(|p| {
            let runs: Vec<Vec<Vec<u64>>> = (0..2)
                .map(|r| run(10 * p + r + 1, 300_000, NODES, 1 << 15))
                .collect();
            let by_object = (p == 0).then(|| {
                runs.iter()
                    .map(|run| {
                        let mut swapped: Vec<u64> =
                            run.iter().flatten().map(|n| n.rotate_left(32)).collect();
                        swapped.sort_unstable();
                        numbered(swapped.chunks(1 << 15).map(<[_]>::to_vec).collect())
                    })
                    .collect()
            });
            PredicateRuns {
                predicate: TermId::from_raw(iri(1_000_000 + p)),
                by_subject: runs.into_iter().map(numbered).collect(),
                by_object,
            }
        })
        .collect();
    let statements: usize = relations.iter().map(PredicateRuns::len).sum();
    let remat = engine.rematerialisation();
    let summary = remat.finish_runs(relations).unwrap();
    let peak = heap::peak() - start;
    let _ = heap::finish();
    let stored = engine.snapshot().len_in(ReadModel::Inferred);
    assert_eq!(summary.inferred_inserted, stored);
    assert!(
        stored as usize * 101 / 100 >= statements,
        "{stored} of {statements}"
    );
    // Per statement, the runs handed over and the table included: 30.8 bytes on 13 Oct
    // 2026. The build from a list of the statements (`40a6da4`): the list's 24 and 64.4
    // beyond it.
    let per_statement = peak as f64 / statements as f64;
    assert!(
        per_statement < 40.0,
        "{per_statement:.1} bytes per statement"
    );
}
