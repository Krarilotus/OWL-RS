//! The bound a pending snapshot charges before it builds a delta's run (DUR-6 slice 2,
//! [`Run::delta_bound`]): engine-v2's uncharged build of the run never holds more at once,
//! and what the run keeps is within it, for every size below the parallel threshold, both
//! layouts, with and without tombstones, and keys spread wide (the packing's worst case).

use super::Layout;
use super::run::Run;
use crate::quad::EncodedQuad;
use crate::term::{TermId, TermKind};

/// SplitMix64, for ids spread over the whole payload width.
fn spread(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *seed;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// `n` distinct quads `layout` holds, every position a different, widely spread id.
fn quads(layout: Layout, n: usize, seed: u64) -> Vec<EncodedQuad> {
    let mut seed = seed;
    let mut id = || TermId::new(TermKind::Iri, (spread(&mut seed) >> 24) | 1);
    (0..n)
        .map(|_| {
            let graph = match layout {
                Layout::Quads => id(),
                Layout::DefaultGraph => TermId::DEFAULT_GRAPH,
            };
            EncodedQuad::new(id(), id(), id(), graph)
        })
        .collect()
}

/// Spec (DUR-6 design note, slice 2, "bound once, build as engine-v2 builds"): for a delta of
/// `n` entries below the parallel threshold, `Run::from_delta` holds at most
/// `Run::delta_bound(layout, n)` bytes at once, and the run it returns keeps at most that.
/// Measured, alone.
#[test]
fn an_uncharged_delta_build_stays_within_the_bound_charged_for_it() {
    if !nrese_exec::measure::alone() {
        return;
    }
    let below = 16 * 1024 - 1;
    assert!(!Run::charged_exactly(below) && Run::charged_exactly(below + 1));
    for layout in [Layout::Quads, Layout::DefaultGraph] {
        for n in [1, 64, 129, 4096, below] {
            for tombstones in [false, true] {
                let all = quads(layout, n, n as u64);
                let (deletes, inserts) = all.split_at(if tombstones { n / 2 } else { 0 });
                let (run, peak) =
                    nrese_exec::measure::measure(|| Run::from_delta(layout, inserts, deletes));
                let bound = Run::delta_bound(layout, n);
                assert!(
                    peak <= bound,
                    "{layout:?}, {n} entries, tombstones {tombstones}: held {peak} bytes, bound {bound}"
                );
                assert!(
                    run.memory_bytes() as usize <= bound,
                    "{layout:?}, {n} entries: keeps {} bytes, bound {bound}",
                    run.memory_bytes()
                );
            }
        }
    }
}
